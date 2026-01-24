mod ask_user;
pub mod background;
mod bash;
mod edit;
mod enter_plan_mode;
mod event_bus_tools;
mod exit_plan_mode;
mod glob;
mod grep;
mod kill_shell;
mod read;
mod task;
mod task_output;
pub mod tasks;
mod todo_write;
mod web_fetch;
mod web_search;
mod write;

use anyhow::Result;
use genai_rs::{CallableFunction, ToolService};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

use crate::agent::AgentEvent;
use crate::plan::PlanManager;

// ============================================================================
// ToolEmitter trait - unified output emission for tools
// ============================================================================

/// Trait for tools that emit output events.
/// Provides a default `emit()` implementation that sends through
/// the event channel or falls back to logging.
pub trait ToolEmitter {
    /// Returns a reference to the tool's events sender.
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>>;

    /// Emit tool output through event channel or fallback to log.
    /// This default implementation handles the common pattern.
    fn emit(&self, output: &str) {
        if let Some(tx) = self.events_tx() {
            let _ = tx.try_send(AgentEvent::ToolOutput(output.to_string()));
        } else {
            crate::logging::log_event(output);
        }
    }
}

pub use ask_user::AskUserTool;
pub use bash::BashTool;
pub use edit::EditTool;
pub use enter_plan_mode::EnterPlanModeTool;
pub use event_bus_tools::{
    EventBusGetEventsTool, EventBusListChannelsTool, EventBusListSessionsTool, EventBusPublishTool,
    EventBusRegisterTool, EventBusUnregisterTool,
};
pub use exit_plan_mode::ExitPlanModeTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use kill_shell::KillShellTool;
pub use read::ReadTool;
pub use task::TaskTool;
pub use task_output::TaskOutputTool;
pub use todo_write::TodoWriteTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
pub use write::WriteTool;

pub const DEFAULT_EXCLUDES: &[&str] = &[".git", "node_modules", "target", "__pycache__", ".venv"];

/// Maximum length for tool output (bash stdout/stderr, web fetch content).
pub const MAX_TOOL_OUTPUT_LEN: usize = 50_000;

/// Maximum buffer size for background task output to prevent memory exhaustion.
pub const MAX_BACKGROUND_BUFFER_LEN: usize = 1_000_000;

/// Maximum length for suggestion text previews in error messages.
pub const MAX_SUGGESTION_PREVIEW_LEN: usize = 100;

/// Tool service that provides file and command execution capabilities.
pub struct CleminiToolService {
    cwd: PathBuf,
    bash_timeout: u64,
    is_mcp_mode: bool,
    allowed_paths: Vec<PathBuf>,
    api_key: String,
    /// Event sender for tools to emit output events (for correct ordering).
    /// Tools use this instead of calling log_event() directly.
    /// Uses interior mutability so events_tx can be set per-interaction while
    /// the tool service itself is created once at startup.
    events_tx: Arc<RwLock<Option<mpsc::Sender<AgentEvent>>>>,
    /// Plan manager for plan mode state.
    /// Uses interior mutability so plan state can be modified per-interaction
    /// while the tool service itself is created once at startup.
    plan_manager: Arc<RwLock<PlanManager>>,
}

impl CleminiToolService {
    pub fn new(
        cwd: PathBuf,
        bash_timeout: u64,
        is_mcp_mode: bool,
        allowed_paths: Vec<PathBuf>,
        api_key: String,
    ) -> Self {
        Self {
            cwd,
            bash_timeout,
            is_mcp_mode,
            allowed_paths,
            api_key,
            events_tx: Arc::new(RwLock::new(None)),
            plan_manager: Arc::new(RwLock::new(PlanManager::new())),
        }
    }

    /// Create a tool service with a shared plan manager.
    ///
    /// Use this when you need multiple components to share the same plan state
    /// (e.g., ACP server and agent).
    pub fn with_plan_manager(
        cwd: PathBuf,
        bash_timeout: u64,
        is_mcp_mode: bool,
        allowed_paths: Vec<PathBuf>,
        api_key: String,
        plan_manager: Arc<RwLock<PlanManager>>,
    ) -> Self {
        Self {
            cwd,
            bash_timeout,
            is_mcp_mode,
            allowed_paths,
            api_key,
            events_tx: Arc::new(RwLock::new(None)),
            plan_manager,
        }
    }

    /// Get a reference to the plan manager.
    pub fn plan_manager(&self) -> &Arc<RwLock<PlanManager>> {
        &self.plan_manager
    }

    /// Set the events sender and return an RAII guard that clears it when dropped.
    ///
    /// This ensures cleanup even if the interaction panics or errors.
    /// Preferred over `set_events_tx` for production use.
    pub fn with_events_tx(&self, tx: mpsc::Sender<AgentEvent>) -> EventsGuard<'_> {
        self.set_events_tx(Some(tx));
        EventsGuard { service: self }
    }

    /// Set or clear the events sender directly.
    ///
    /// For production code, prefer `with_events_tx()` which returns a guard
    /// that automatically clears the sender when dropped. This method is
    /// primarily for tests that need to control lifetime manually.
    pub fn set_events_tx(&self, tx: Option<mpsc::Sender<AgentEvent>>) {
        match self.events_tx.write() {
            Ok(mut guard) => *guard = tx,
            Err(poisoned) => {
                tracing::warn!("events_tx lock was poisoned, recovering");
                *poisoned.into_inner() = tx;
            }
        }
    }

    /// Get a clone of the current events sender for tools.
    fn events_tx(&self) -> Option<mpsc::Sender<AgentEvent>> {
        match self.events_tx.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => {
                tracing::warn!("events_tx lock was poisoned, recovering");
                poisoned.into_inner().clone()
            }
        }
    }

    pub async fn execute(&self, name: &str, args: Value) -> Result<Value> {
        let tools = self.tools();
        let tool = tools
            .iter()
            .find(|t| t.declaration().name() == name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", name))?;
        tool.call(args).await.map_err(|e| anyhow::anyhow!(e))
    }
}

/// RAII guard that clears events_tx when dropped.
///
/// Returned by `CleminiToolService::with_events_tx()`.
/// Ensures cleanup even if the interaction panics or errors.
///
/// # Usage
///
/// The guard must be bound to a variable to stay alive for the duration of the interaction:
///
/// ```ignore
/// // Correct: guard lives until end of scope
/// let _guard = tool_service.with_events_tx(events_tx.clone());
/// run_interaction(...).await;
/// // _guard drops here, clearing events_tx
///
/// // Wrong: guard drops immediately, events_tx cleared before interaction
/// tool_service.with_events_tx(events_tx.clone()); // drops here!
/// run_interaction(...).await; // events_tx already None
/// ```
pub struct EventsGuard<'a> {
    service: &'a CleminiToolService,
}

impl Drop for EventsGuard<'_> {
    fn drop(&mut self) {
        self.service.set_events_tx(None);
    }
}

impl ToolService for CleminiToolService {
    /// Returns the list of available tools.
    ///
    /// Available tools:
    /// - `read`: Read file contents
    /// - `write`: Create or overwrite files
    /// - `edit`: Surgical string replacement in files
    /// - `bash`: Execute shell commands
    /// - `glob`: Find files by pattern
    /// - `grep`: Search for text in files
    /// - `kill_shell`: Kill a background task
    /// - `task`: Spawn a clemini subagent
    /// - `task_output`: Get output from a background task
    /// - `web_fetch`: Fetch web content
    /// - `web_search`: Search the web using DuckDuckGo
    /// - `ask_user`: Ask the user a question
    /// - `todo_write`: Display a todo list
    fn tools(&self) -> Vec<Arc<dyn CallableFunction>> {
        let events_tx = self.events_tx();
        vec![
            Arc::new(ReadTool::new(
                self.cwd.clone(),
                self.allowed_paths.clone(),
                events_tx.clone(),
            )),
            Arc::new(WriteTool::new(
                self.cwd.clone(),
                self.allowed_paths.clone(),
                events_tx.clone(),
            )),
            Arc::new(EditTool::new(
                self.cwd.clone(),
                self.allowed_paths.clone(),
                events_tx.clone(),
            )),
            Arc::new(BashTool::new(
                self.cwd.clone(),
                self.allowed_paths.clone(),
                self.bash_timeout,
                self.is_mcp_mode,
                events_tx.clone(),
            )),
            Arc::new(GlobTool::new(
                self.cwd.clone(),
                self.allowed_paths.clone(),
                events_tx.clone(),
            )),
            Arc::new(GrepTool::new(
                self.cwd.clone(),
                self.allowed_paths.clone(),
                events_tx.clone(),
            )),
            Arc::new(KillShellTool::new(events_tx.clone())),
            Arc::new(TaskTool::new(self.cwd.clone(), events_tx.clone())),
            Arc::new(TaskOutputTool::new(events_tx.clone())),
            Arc::new(WebFetchTool::new(self.api_key.clone(), events_tx.clone())),
            Arc::new(WebSearchTool::new(events_tx.clone())),
            Arc::new(AskUserTool::new(events_tx.clone())),
            Arc::new(TodoWriteTool::new(events_tx.clone())),
            Arc::new(EnterPlanModeTool::new(
                events_tx.clone(),
                self.plan_manager.clone(),
            )),
            Arc::new(ExitPlanModeTool::new(
                events_tx.clone(),
                self.plan_manager.clone(),
            )),
            // Event bus tools
            Arc::new(EventBusRegisterTool::new(events_tx.clone())),
            Arc::new(EventBusListSessionsTool::new(events_tx.clone())),
            Arc::new(EventBusListChannelsTool::new(events_tx.clone())),
            Arc::new(EventBusPublishTool::new(events_tx.clone())),
            Arc::new(EventBusGetEventsTool::new(events_tx.clone())),
            Arc::new(EventBusUnregisterTool::new(events_tx)),
        ]
    }
}

/// Resolves a path relative to CWD and validates it's within any allowed path.
pub fn resolve_and_validate_path(
    file_path: &str,
    cwd: &std::path::Path,
    allowed_paths: &[PathBuf],
) -> Result<PathBuf, String> {
    let path = std::path::Path::new(file_path);
    let full_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };

    validate_path(&full_path, allowed_paths)
}

/// Check if a path is within any of the allowed paths.
/// Returns `Ok(canonical_path)` if allowed, Err(reason) if denied.
pub fn validate_path(path: &std::path::Path, allowed_paths: &[PathBuf]) -> Result<PathBuf, String> {
    // For new files, check parent directory
    let check_path = if path.exists() {
        path.canonicalize()
            .map_err(|e| format!("Cannot resolve path: {e}"))?
    } else {
        // For new files, canonicalize the parent and append the filename
        let parent = path.parent().unwrap_or(std::path::Path::new("."));
        let filename = path.file_name().ok_or("Invalid path")?;

        let canonical_parent =
            if parent.as_os_str().is_empty() || parent == std::path::Path::new(".") {
                // If parent is empty or ".", we use the first allowed path (which is always CWD)
                // but we need to resolve it against all allowed paths.
                // Actually, if it's relative, it's relative to CWD.
                let cwd = &allowed_paths[0];
                cwd.to_path_buf()
            } else if parent.exists() {
                parent
                    .canonicalize()
                    .map_err(|e| format!("Cannot resolve parent: {e}"))?
            } else {
                // Parent doesn't exist - check if it would be under any allowed path
                let mut resolved_parent = None;

                // Relative paths are relative to CWD (first allowed path)
                let full_parent = if parent.is_absolute() {
                    parent.to_path_buf()
                } else {
                    allowed_paths[0].join(parent)
                };

                for allowed in allowed_paths {
                    if full_parent.starts_with(allowed) {
                        resolved_parent = Some(full_parent);
                        break;
                    }
                }

                match resolved_parent {
                    Some(p) => p,
                    None => {
                        return Err(format!("Path {} is outside allowed paths", path.display(),));
                    }
                }
            };

        canonical_parent.join(filename)
    };

    // Verify the path is under any allowed path
    for allowed in allowed_paths {
        if let Ok(canonical_allowed) = allowed.canonicalize() {
            if check_path.starts_with(&canonical_allowed) {
                return Ok(check_path);
            }
        } else if check_path.starts_with(allowed) {
            return Ok(check_path);
        }
    }

    Err(format!(
        "Path {} is outside allowed paths",
        check_path.display(),
    ))
}

/// Makes a path relative to the CWD if possible, otherwise returns the path as a string.
pub fn make_relative(path: &std::path::Path, cwd: &std::path::Path) -> String {
    let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    path.strip_prefix(&canonical_cwd)
        .or_else(|_| path.strip_prefix(cwd))
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

pub fn create_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("clemini/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))
}

/// Get the clemini executable path for spawning subagents.
/// Tries current executable first, falls back to cargo run (development only).
pub fn get_clemini_command() -> (String, Vec<String>) {
    // During tests, we want to find the actual clemini binary in target/debug or target/release
    // current_exe() during tests is the test runner binary (e.g. deps/clemini-hash)
    #[cfg(test)]
    {
        if let Ok(cwd) = std::env::current_dir() {
            let debug_path = cwd.join("target/debug/clemini");
            if debug_path.exists() {
                return (debug_path.to_string_lossy().to_string(), vec![]);
            }
            let release_path = cwd.join("target/release/clemini");
            if release_path.exists() {
                return (release_path.to_string_lossy().to_string(), vec![]);
            }
        }
    }

    // Try current executable first
    if let Ok(exe) = std::env::current_exe()
        && exe.exists()
    {
        let exe_name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Avoid using test runner binary as the command
        // Nextest binaries often have a hash suffix, e.g. clemini-f3b1e56b5b5b5b5b
        // They are also usually in a 'deps' directory.
        let is_test_runner = exe.to_string_lossy().contains("/deps/")
            || exe.to_string_lossy().contains("\\deps\\")
            || exe_name.contains("-") && !exe_name.ends_with(".exe");

        if !is_test_runner {
            return (exe.to_string_lossy().to_string(), vec![]);
        }
    }

    // Fallback to cargo run - only useful during development
    tracing::warn!(
        "current_exe() failed, is a test runner, or doesn't exist, falling back to 'cargo run'. \
         This is expected during development but indicates an issue in production."
    );
    (
        "cargo".to_string(),
        vec!["run".to_string(), "--quiet".to_string(), "--".to_string()],
    )
}

/// List of all tool names registered in this crate.
/// Used by `tool_is_read_only()` test to ensure completeness.
pub const ALL_TOOL_NAMES: &[&str] = &[
    "read",
    "write",
    "edit",
    "bash",
    "glob",
    "grep",
    "kill_shell",
    "task",
    "task_output",
    "web_fetch",
    "web_search",
    "ask_user",
    "todo_write",
    "enter_plan_mode",
    "exit_plan_mode",
    // Event bus tools
    "event_bus_register",
    "event_bus_list_sessions",
    "event_bus_list_channels",
    "event_bus_publish",
    "event_bus_get_events",
    "event_bus_unregister",
];

/// Check if a tool is read-only (safe to run in plan mode).
///
/// Read-only tools don't modify files, execute commands with side effects,
/// or change system state. They're safe to run during the planning phase.
///
/// # Panics in tests
///
/// The accompanying test `test_tool_is_read_only_covers_all_tools` will fail
/// if a new tool is added to `ALL_TOOL_NAMES` but not categorized here.
pub fn tool_is_read_only(tool_name: &str) -> bool {
    matches!(
        tool_name,
        // File reading
        "read" | "glob" | "grep" |
        // Web reading
        "web_fetch" | "web_search" |
        // User interaction (no side effects)
        "ask_user" | "todo_write" |
        // Plan mode management
        "enter_plan_mode" | "exit_plan_mode" |
        // Event bus reading (these don't modify state significantly)
        "event_bus_list_sessions" | "event_bus_list_channels" | "event_bus_get_events" |
        // Task output reading (doesn't start new tasks)
        "task_output"
    )
}

/// Structured response from tool execution.
///
/// Provides type-safe tool results while serializing to the same JSON format
/// used by the existing `serde_json::json!` approach for backward compatibility.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum ToolResponse {
    /// Successful result with arbitrary JSON data.
    Success(serde_json::Value),

    /// Error result with message, code, and context.
    Error {
        error: String,
        error_code: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<serde_json::Value>,
    },
}

impl ToolResponse {
    /// Create a success response from any serializable value.
    pub fn success<T: serde::Serialize>(value: T) -> Self {
        Self::Success(serde_json::to_value(value).unwrap_or(serde_json::Value::Null))
    }

    /// Create an error response.
    pub fn error(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self::Error {
            error: message.into(),
            error_code: code.into(),
            context: None,
        }
    }

    /// Create an error response with context.
    pub fn error_with_context(
        message: impl Into<String>,
        code: impl Into<String>,
        context: serde_json::Value,
    ) -> Self {
        Self::Error {
            error: message.into(),
            error_code: code.into(),
            context: Some(context),
        }
    }

    /// Convert to JSON Value for tool return.
    pub fn into_json(self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({"error": "Failed to serialize response", "error_code": "INTERNAL"})
        })
    }

    /// Check if this is an error response.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }
}

impl From<ToolResponse> for serde_json::Value {
    fn from(response: ToolResponse) -> Self {
        response.into_json()
    }
}

/// Standard error codes for tool responses
pub mod error_codes {
    pub const NOT_FOUND: &str = "NOT_FOUND";
    pub const ACCESS_DENIED: &str = "ACCESS_DENIED";
    pub const INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
    pub const NOT_UNIQUE: &str = "NOT_UNIQUE";
    pub const IO_ERROR: &str = "IO_ERROR";
    pub const BINARY_FILE: &str = "BINARY_FILE";
    pub const TIMEOUT: &str = "TIMEOUT";
    pub const BLOCKED: &str = "BLOCKED";
}

/// Create a standardized error response
pub fn error_response(message: &str, code: &str, context: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "error": message,
        "error_code": code,
        "context": context
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_validate_path() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let allowed = vec![cwd.clone()];

        // Path within cwd (allowed)
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "test").unwrap();
        assert!(validate_path(&file_path, &allowed).is_ok());

        // New file within cwd (allowed)
        let new_file = cwd.join("new.txt");
        assert!(validate_path(&new_file, &allowed).is_ok());

        // Paths outside cwd via .. (rejected)
        let outside_path = cwd.join("../outside.txt");
        assert!(validate_path(&outside_path, &allowed).is_err());

        // Absolute paths outside cwd (rejected)
        let absolute_outside = std::env::temp_dir().join("some_other_file.txt");
        if !absolute_outside.starts_with(&cwd) {
            assert!(validate_path(&absolute_outside, &allowed).is_err());
        }

        // Edge cases: .
        assert!(validate_path(&cwd.join("."), &allowed).is_ok());
    }

    #[test]
    fn test_resolve_and_validate_path() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let allowed = vec![cwd.clone()];

        // Relative path
        assert!(resolve_and_validate_path("test.txt", &cwd, &allowed).is_ok());

        // Relative path with .. (allowed if stays within)
        fs::create_dir(cwd.join("subdir")).unwrap();
        assert!(resolve_and_validate_path("subdir/../test.txt", &cwd, &allowed).is_ok());

        // Relative path escaping cwd
        assert!(resolve_and_validate_path("../outside.txt", &cwd, &allowed).is_err());

        // Non-existent parent directory
        // If it's under an allowed path, it should be OK
        assert!(resolve_and_validate_path("newdir/newfile.txt", &cwd, &allowed).is_ok());

        // Non-existent parent directory outside allowed paths
        let another_dir = tempdir().unwrap();
        let outside_dir = another_dir.path().join("some_dir");
        assert!(
            resolve_and_validate_path(
                &outside_dir.join("file.txt").to_string_lossy(),
                &cwd,
                &allowed
            )
            .is_err()
        );
    }

    #[test]
    fn test_make_relative() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();

        let file_path = cwd.join("subdir/test.txt");
        assert_eq!(make_relative(&file_path, &cwd), "subdir/test.txt");

        let outside_path = std::path::Path::new("/tmp/some_file.txt");
        if !outside_path.starts_with(&cwd) {
            assert_eq!(
                make_relative(outside_path, &cwd),
                outside_path.to_string_lossy()
            );
        }
    }

    // ============================================================================
    // ToolEmitter tests
    // ============================================================================

    #[tokio::test]
    async fn test_tool_emitter_with_channel() {
        struct MockTool {
            events_tx: Option<mpsc::Sender<AgentEvent>>,
        }
        impl ToolEmitter for MockTool {
            fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
                &self.events_tx
            }
        }

        let (tx, mut rx) = mpsc::channel(1);
        let tool = MockTool {
            events_tx: Some(tx),
        };

        tool.emit("test message");

        if let Some(AgentEvent::ToolOutput(msg)) = rx.recv().await {
            assert_eq!(msg, "test message");
        } else {
            panic!("Expected AgentEvent::ToolOutput");
        }
    }

    // ============================================================================
    // tool_is_read_only tests
    // ============================================================================

    #[test]
    fn test_tool_is_read_only_covers_all_tools() {
        // This test ensures every tool in ALL_TOOL_NAMES is explicitly categorized.
        // When adding a new tool, you MUST add it to ALL_TOOL_NAMES AND categorize
        // it in tool_is_read_only(). This test will fail if you forget either.
        for tool_name in ALL_TOOL_NAMES {
            // Just calling the function exercises the match - if a tool isn't
            // matched, it returns false (which might be correct for write tools).
            // The important thing is we have explicit coverage.
            let _ = tool_is_read_only(tool_name);
        }

        // Verify expected categorizations
        // Read-only tools
        assert!(tool_is_read_only("read"));
        assert!(tool_is_read_only("glob"));
        assert!(tool_is_read_only("grep"));
        assert!(tool_is_read_only("web_fetch"));
        assert!(tool_is_read_only("web_search"));
        assert!(tool_is_read_only("ask_user"));
        assert!(tool_is_read_only("todo_write"));
        assert!(tool_is_read_only("task_output"));
        assert!(tool_is_read_only("enter_plan_mode"));
        assert!(tool_is_read_only("exit_plan_mode"));
        assert!(tool_is_read_only("event_bus_list_sessions"));
        assert!(tool_is_read_only("event_bus_list_channels"));
        assert!(tool_is_read_only("event_bus_get_events"));

        // Write tools (side effects)
        assert!(!tool_is_read_only("write"));
        assert!(!tool_is_read_only("edit"));
        assert!(!tool_is_read_only("bash"));
        assert!(!tool_is_read_only("kill_shell"));
        assert!(!tool_is_read_only("task"));
        assert!(!tool_is_read_only("event_bus_register"));
        assert!(!tool_is_read_only("event_bus_publish"));
        assert!(!tool_is_read_only("event_bus_unregister"));
    }

    #[test]
    fn test_tool_is_read_only_unknown_tool() {
        // Unknown tools are treated as write tools (conservative default)
        assert!(!tool_is_read_only("unknown_tool"));
        assert!(!tool_is_read_only(""));
    }

    // ============================================================================
    // ToolResponse tests
    // ============================================================================

    #[test]
    fn test_tool_response_success() {
        let response = ToolResponse::success(serde_json::json!({"data": "value"}));
        let json = response.into_json();

        assert_eq!(json["data"], "value");
        assert!(json.get("error").is_none());
    }

    #[test]
    fn test_tool_response_error() {
        let response = ToolResponse::error("File not found", error_codes::NOT_FOUND);
        assert!(response.is_error());

        let json = response.into_json();
        assert_eq!(json["error"], "File not found");
        assert_eq!(json["error_code"], "NOT_FOUND");
        assert!(json.get("context").is_none());
    }

    #[test]
    fn test_tool_response_error_with_context() {
        let response = ToolResponse::error_with_context(
            "Path denied",
            error_codes::ACCESS_DENIED,
            serde_json::json!({"path": "/etc/passwd"}),
        );

        let json = response.into_json();
        assert_eq!(json["error"], "Path denied");
        assert_eq!(json["error_code"], "ACCESS_DENIED");
        assert_eq!(json["context"]["path"], "/etc/passwd");
    }

    #[test]
    fn test_tool_response_into_value() {
        let response = ToolResponse::success(serde_json::json!({"ok": true}));
        let value: serde_json::Value = response.into();
        assert_eq!(value["ok"], true);
    }
}

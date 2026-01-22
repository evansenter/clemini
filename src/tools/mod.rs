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
pub const MAX_TOOL_OUTPUT_LEN: usize = 50_000;

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
        }
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
            Arc::new(EnterPlanModeTool::new(events_tx.clone())),
            Arc::new(ExitPlanModeTool::new(events_tx.clone())),
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
    // Try current executable first
    if let Ok(exe) = std::env::current_exe()
        && exe.exists()
    {
        return (exe.to_string_lossy().to_string(), vec![]);
    }
    // Fallback to cargo run - only useful during development
    tracing::warn!(
        "current_exe() failed or doesn't exist, falling back to 'cargo run'. \
         This is expected during development but indicates an issue in production."
    );
    (
        "cargo".to_string(),
        vec!["run".to_string(), "--quiet".to_string(), "--".to_string()],
    )
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
}

mod ask_user;
mod bash;
mod edit;
mod glob;
mod grep;
mod read;
mod todo_write;
mod web_fetch;
mod web_search;
mod write;

use anyhow::Result;
use genai_rs::{CallableFunction, ToolService};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

pub use ask_user::AskUserTool;
pub use bash::BashTool;
pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use todo_write::TodoWriteTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
pub use write::WriteTool;

pub const DEFAULT_EXCLUDES: &[&str] = &[".git", "node_modules", "target", "__pycache__", ".venv"];

/// Tool service that provides file and command execution capabilities.
pub struct CleminiToolService {
    cwd: PathBuf,
    bash_timeout: u64,
    is_mcp_mode: bool,
}

impl CleminiToolService {
    pub fn new(cwd: PathBuf, bash_timeout: u64, is_mcp_mode: bool) -> Self {
        Self { cwd, bash_timeout, is_mcp_mode }
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
    /// - `web_fetch`: Fetch web content
    /// - `web_search`: Search the web using DuckDuckGo
    /// - `ask_user`: Ask the user a question
    /// - `todo_write`: Display a todo list
    fn tools(&self) -> Vec<Arc<dyn CallableFunction>> {
        vec![
            Arc::new(ReadTool::new(self.cwd.clone())),
            Arc::new(WriteTool::new(self.cwd.clone())),
            Arc::new(EditTool::new(self.cwd.clone())),
            Arc::new(BashTool::new(self.cwd.clone(), self.bash_timeout, self.is_mcp_mode)),
            Arc::new(GlobTool::new(self.cwd.clone())),
            Arc::new(GrepTool::new(self.cwd.clone())),
            Arc::new(WebFetchTool::new(self.cwd.clone())),
            Arc::new(WebSearchTool::new(self.cwd.clone())),
            Arc::new(AskUserTool::new()),
            Arc::new(TodoWriteTool::new()),
        ]
    }
}

/// Resolves a path relative to CWD and validates it's within CWD.
pub fn resolve_and_validate_path(
    file_path: &str,
    cwd: &std::path::Path,
) -> Result<PathBuf, String> {
    let path = std::path::Path::new(file_path);
    let full_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };

    validate_path(&full_path, cwd)
}

/// Check if a path is within the allowed working directory.
/// Returns `Ok(canonical_path)` if allowed, Err(reason) if denied.
pub fn validate_path(path: &std::path::Path, cwd: &std::path::Path) -> Result<PathBuf, String> {
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
                cwd.to_path_buf()
            } else if parent.exists() {
                parent
                    .canonicalize()
                    .map_err(|e| format!("Cannot resolve parent: {e}"))?
            } else {
                // Parent doesn't exist - check if it would be under cwd
                let full_parent = if parent.is_absolute() {
                    parent.to_path_buf()
                } else {
                    cwd.join(parent)
                };
                // Do a simple prefix check since we can't canonicalize
                if !full_parent.starts_with(cwd) {
                    return Err(format!(
                        "Path {} is outside working directory {}",
                        path.display(),
                        cwd.display()
                    ));
                }
                full_parent
            };

        canonical_parent.join(filename)
    };

    // Verify the path is under cwd
    let canonical_cwd = cwd
        .canonicalize()
        .map_err(|e| format!("Cannot resolve cwd: {e}"))?;

    if !check_path.starts_with(&canonical_cwd) {
        return Err(format!(
            "Path {} is outside working directory {}",
            check_path.display(),
            canonical_cwd.display()
        ));
    }

    Ok(check_path)
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

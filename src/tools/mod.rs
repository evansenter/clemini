mod bash;
mod edit;
mod glob;
mod grep;
mod read;
mod write;

use genai_rs::{CallableFunction, ToolService};
use std::path::PathBuf;
use std::sync::Arc;

pub use bash::BashTool;
pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use write::WriteTool;

/// Tool service that provides file and command execution capabilities.
pub struct CleminiToolService {
    cwd: PathBuf,
}

impl CleminiToolService {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
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
    fn tools(&self) -> Vec<Arc<dyn CallableFunction>> {
        vec![
            Arc::new(ReadTool::new(self.cwd.clone())),
            Arc::new(WriteTool::new(self.cwd.clone())),
            Arc::new(EditTool::new(self.cwd.clone())),
            Arc::new(BashTool::new(self.cwd.clone())),
            Arc::new(GlobTool::new(self.cwd.clone())),
            Arc::new(GrepTool::new(self.cwd.clone())),
        ]
    }
}

/// Check if a path is within the allowed working directory.
/// Returns Ok(canonical_path) if allowed, Err(reason) if denied.
pub fn validate_path(path: &std::path::Path, cwd: &std::path::Path) -> Result<PathBuf, String> {
    // For new files, check parent directory
    let check_path = if path.exists() {
        path.canonicalize()
            .map_err(|e| format!("Cannot resolve path: {}", e))?
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
                    .map_err(|e| format!("Cannot resolve parent: {}", e))?
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
        .map_err(|e| format!("Cannot resolve cwd: {}", e))?;

    if !check_path.starts_with(&canonical_cwd) {
        return Err(format!(
            "Path {} is outside working directory {}",
            check_path.display(),
            canonical_cwd.display()
        ));
    }

    Ok(check_path)
}

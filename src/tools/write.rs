use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;

use super::validate_path;

pub struct WriteTool {
    cwd: PathBuf,
}

impl WriteTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve_path(&self, file_path: &str) -> PathBuf {
        let path = PathBuf::from(file_path);
        if path.is_absolute() {
            path
        } else {
            self.cwd.join(path)
        }
    }
}

#[async_trait]
impl CallableFunction for WriteTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "write_file".to_string(),
            "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Creates parent directories as needed.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "file_path": {
                        "type": "string",
                        "description": "The path to the file to write (absolute or relative to cwd)"
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file"
                    }
                }),
                vec!["file_path".to_string(), "content".to_string()],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing file_path".to_string()))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing content".to_string()))?;

        let raw_path = self.resolve_path(file_path);

        // Safety check - must be within cwd
        let path = match validate_path(&raw_path, &self.cwd) {
            Ok(p) => p,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Access denied: {}", e)
                }));
            }
        };

        eprintln!("[write: {} ({} bytes)]", path.display(), content.len());

        // Create parent directories if needed
        if let Some(parent) = path.parent()
            && !parent.exists()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Ok(json!({
                "error": format!("Failed to create directory {}: {}", parent.display(), e)
            }));
        }

        match tokio::fs::write(&path, content).await {
            Ok(()) => Ok(json!({
                "path": path.display().to_string(),
                "bytes_written": content.len(),
                "success": true
            })),
            Err(e) => Ok(json!({
                "error": format!("Failed to write {}: {}", path.display(), e)
            })),
        }
    }
}

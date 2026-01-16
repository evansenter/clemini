use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;

use super::resolve_and_validate_path;

pub struct ReadTool {
    cwd: PathBuf,
}

impl ReadTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl CallableFunction for ReadTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "read_file".to_string(),
            "Read the contents of a file. Returns the file contents as text.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "file_path": {
                        "type": "string",
                        "description": "The path to the file to read (absolute or relative to cwd)"
                    }
                }),
                vec!["file_path".to_string()],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing file_path".to_string()))?;

        // Resolve and validate path
        let path = match resolve_and_validate_path(file_path, &self.cwd) {
            Ok(p) => p,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Access denied: {}. Only files within the current working directory can be accessed.", e)
                }));
            }
        };

        // Logging is handled by main.rs event loop with timing info

        match tokio::fs::read_to_string(&path).await {
            Ok(contents) => Ok(json!({
                "path": path.display().to_string(),
                "contents": contents,
                "size_bytes": contents.len()
            })),
            Err(e) => Ok(json!({
                "error": format!("Failed to read {}: {}. Ensure the file exists and is not a directory.", path.display(), e)
            })),
        }
    }
}

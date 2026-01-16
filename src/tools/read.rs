use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;

use super::validate_path;

pub struct ReadTool {
    cwd: PathBuf,
}

impl ReadTool {
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

        let path = self.resolve_path(file_path);

        // Safety check - must be within cwd
        let path = match validate_path(&path, &self.cwd) {
            Ok(p) => p,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Access denied: {}", e)
                }));
            }
        };

        eprintln!("[read: {}]", path.display());

        match tokio::fs::read_to_string(&path).await {
            Ok(contents) => Ok(json!({
                "path": path.display().to_string(),
                "contents": contents,
                "size_bytes": contents.len()
            })),
            Err(e) => Ok(json!({
                "error": format!("Failed to read {}: {}", path.display(), e)
            })),
        }
    }
}

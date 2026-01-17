use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;
use tracing::instrument;

use super::{error_codes, error_response, resolve_and_validate_path};

pub struct WriteTool {
    cwd: PathBuf,
    allowed_paths: Vec<PathBuf>,
}

impl WriteTool {
    pub fn new(cwd: PathBuf, allowed_paths: Vec<PathBuf>) -> Self {
        Self { cwd, allowed_paths }
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

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing file_path".to_string()))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing content".to_string()))?;

        // Resolve and validate path
        let path = match resolve_and_validate_path(file_path, &self.cwd, &self.allowed_paths) {
            Ok(p) => p,
            Err(e) => {
                return Ok(error_response(
                    &format!("Access denied: {}. Path must be within allowed paths.", e),
                    error_codes::ACCESS_DENIED,
                    json!({"path": file_path}),
                ));
            }
        };

        // Logging is handled by main.rs event loop with timing info

        // Create parent directories if needed
        if let Some(parent) = path.parent()
            && !parent.exists()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Ok(error_response(
                &format!(
                    "Failed to create directory {}: {}. Check permissions for the parent directory.",
                    parent.display(),
                    e
                ),
                error_codes::IO_ERROR,
                json!({"path": parent.display().to_string()}),
            ));
        }

        let metadata = tokio::fs::metadata(&path).await.ok();
        let previous_size = metadata.as_ref().map(|m| m.len());
        let exists = metadata.is_some();

        match tokio::fs::write(&path, content).await {
            Ok(()) => {
                let mut response = json!({
                    "path": path.display().to_string(),
                    "bytes_written": content.len(),
                    "success": true
                });

                if exists {
                    response["overwritten"] = json!(true);
                    if let Some(size) = previous_size {
                        response["previous_size"] = json!(size);
                    }
                } else {
                    response["created"] = json!(true);
                }

                Ok(response)
            }
            Err(e) => Ok(error_response(
                &format!(
                    "Failed to write {}: {}. Check file permissions.",
                    path.display(),
                    e
                ),
                error_codes::IO_ERROR,
                json!({"path": path.display().to_string()}),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_write_tool_success() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let allowed = vec![cwd.clone()];
        let tool = WriteTool::new(cwd.clone(), allowed);
        let file_path = "test.txt";
        let content = "hello world";

        let args = json!({
            "file_path": file_path,
            "content": content
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert!(result["created"].as_bool().unwrap());
        assert_eq!(result["bytes_written"], content.len());

        let saved_content = fs::read_to_string(cwd.join(file_path)).unwrap();
        assert_eq!(saved_content, content);
    }

    #[tokio::test]
    async fn test_write_tool_overwrite() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        let old_content = "old content";
        fs::write(&file_path, old_content).unwrap();

        let tool = WriteTool::new(cwd.clone(), vec![cwd.clone()]);
        let args = json!({
            "file_path": "test.txt",
            "content": "new content"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert!(result["overwritten"].as_bool().unwrap());
        assert_eq!(result["previous_size"], old_content.len());

        let saved_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(saved_content, "new content");
    }

    #[tokio::test]
    async fn test_write_tool_outside_cwd() {
        let dir = tempdir().unwrap();
        let tool = WriteTool::new(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let args = json!({
            "file_path": "../outside.txt",
            "content": "data"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("Access denied"));
        assert_eq!(result["error_code"], error_codes::ACCESS_DENIED);
        assert!(result["context"]["path"].is_string());
    }
}

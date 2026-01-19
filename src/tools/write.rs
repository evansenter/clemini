use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::instrument;

use super::{ToolEmitter, error_codes, error_response, resolve_and_validate_path};
use crate::agent::AgentEvent;

pub struct WriteTool {
    cwd: PathBuf,
    allowed_paths: Vec<PathBuf>,
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl WriteTool {
    pub fn new(
        cwd: PathBuf,
        allowed_paths: Vec<PathBuf>,
        events_tx: Option<mpsc::Sender<AgentEvent>>,
    ) -> Self {
        Self {
            cwd,
            allowed_paths,
            events_tx,
        }
    }
}

impl ToolEmitter for WriteTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for WriteTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "write_file".to_string(),
            "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Creates parent directories as needed. Returns: {success, bytes_written, created?, overwritten?, backup_created?}".to_string(),
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
                    },
                    "backup": {
                        "type": "boolean",
                        "description": "Whether to create a backup of the existing file (as {filename}.bak) before overwriting. (default: false)"
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

        let backup = args
            .get("backup")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

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

        let mut backup_created = false;
        if backup && exists {
            let mut backup_path_os = path.clone().into_os_string();
            backup_path_os.push(".bak");
            let backup_path = PathBuf::from(backup_path_os);

            if let Err(e) = tokio::fs::copy(&path, &backup_path).await {
                return Ok(error_response(
                    &format!(
                        "Failed to create backup at {}: {}. Overwrite aborted.",
                        backup_path.display(),
                        e
                    ),
                    error_codes::IO_ERROR,
                    json!({"path": backup_path.display().to_string()}),
                ));
            }
            backup_created = true;
        }

        match tokio::fs::write(&path, content).await {
            Ok(()) => {
                let mut response = json!({
                    "path": path.display().to_string(),
                    "bytes_written": content.len(),
                    "success": true
                });

                if exists {
                    response["overwritten"] = json!(true);
                    if backup_created {
                        response["backup_created"] = json!(true);
                    }
                    if let Some(size) = previous_size {
                        response["previous_size"] = json!(size);
                    }
                } else {
                    response["created"] = json!(true);
                }

                // Emit visual output
                let line_count = content.lines().count();
                let action = if exists { "overwritten" } else { "created" };
                let msg = format!("  {} lines {}", line_count, action)
                    .dimmed()
                    .to_string();
                self.emit(&msg);

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
        let tool = WriteTool::new(cwd.clone(), allowed, None);
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

        let tool = WriteTool::new(cwd.clone(), vec![cwd.clone()], None);
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
        let tool = WriteTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            None,
        );
        let args = json!({
            "file_path": "../outside.txt",
            "content": "data"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("Access denied"));
        assert_eq!(result["error_code"], error_codes::ACCESS_DENIED);
        assert!(result["context"]["path"].is_string());
    }

    #[tokio::test]
    async fn test_write_tool_overwrite_with_backup() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        let old_content = "old content";
        fs::write(&file_path, old_content).unwrap();

        let tool = WriteTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "content": "new content",
            "backup": true
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert!(result["overwritten"].as_bool().unwrap());
        assert!(result["backup_created"].as_bool().unwrap());

        let saved_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(saved_content, "new content");

        let backup_path = cwd.join("test.txt.bak");
        assert!(backup_path.exists());
        let backup_content = fs::read_to_string(&backup_path).unwrap();
        assert_eq!(backup_content, old_content);
    }

    #[tokio::test]
    async fn test_write_tool_overwrite_no_backup_default() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        let old_content = "old content";
        fs::write(&file_path, old_content).unwrap();

        let tool = WriteTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "content": "new content"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert!(result["overwritten"].as_bool().unwrap());
        assert!(result["backup_created"].is_null());

        let backup_path = cwd.join("test.txt.bak");
        assert!(!backup_path.exists());
    }

    #[tokio::test]
    async fn test_write_tool_backup_failure() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        let old_content = "old content";
        fs::write(&file_path, old_content).unwrap();

        // Create a directory where the backup file should be
        let backup_path = cwd.join("test.txt.bak");
        fs::create_dir(&backup_path).unwrap();

        let tool = WriteTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "content": "new content",
            "backup": true
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].is_string());
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("Failed to create backup")
        );
        assert_eq!(result["error_code"], error_codes::IO_ERROR);

        // Ensure original file was NOT overwritten
        let saved_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(saved_content, old_content);
    }
}

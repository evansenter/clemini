use crate::tools::{error_codes, error_response};
use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tracing::instrument;

pub struct KillShellTool;

impl KillShellTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CallableFunction for KillShellTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "kill_shell".to_string(),
            "Kill a background bash task started with run_in_background=true. Returns: {task_id, status, success}".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "task_id": {
                        "type": "string",
                        "description": "The task ID to kill (returned by bash with run_in_background=true)"
                    }
                }),
                vec!["task_id".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let task_id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing task_id".to_string()))?;

        let mut child = {
            let mut tasks = crate::tools::bash::BACKGROUND_TASKS.lock().unwrap();
            tasks.remove(task_id)
        };

        if let Some(mut child) = child.take() {
            match child.kill().await {
                Ok(_) => {
                    crate::log_event(&format!("[kill_shell] Killed task {}", task_id));
                    Ok(json!({
                        "task_id": task_id,
                        "status": "killed",
                        "success": true
                    }))
                }
                Err(e) => Ok(error_response(
                    &format!("Failed to kill task {}: {}", task_id, e),
                    error_codes::IO_ERROR,
                    json!({ "task_id": task_id }),
                )),
            }
        } else {
            Ok(error_response(
                &format!("Task {} not found", task_id),
                error_codes::NOT_FOUND,
                json!({ "task_id": task_id }),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::bash::BashTool;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_kill_shell_success() {
        let dir = tempdir().unwrap();
        let bash = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
        );

        // Start a background task
        let bash_result = bash
            .call(json!({
                "command": "sleep 100",
                "run_in_background": true
            }))
            .await
            .unwrap();

        let task_id = bash_result["task_id"].as_str().unwrap();

        // Kill it
        let kill_tool = KillShellTool::new();
        let kill_result = kill_tool.call(json!({ "task_id": task_id })).await.unwrap();

        assert!(kill_result["success"].as_bool().unwrap());
        assert_eq!(kill_result["status"], "killed");

        // Verify it's gone from the map
        let tasks = crate::tools::bash::BACKGROUND_TASKS.lock().unwrap();
        assert!(!tasks.contains_key(task_id));
    }

    #[tokio::test]
    async fn test_kill_shell_not_found() {
        let kill_tool = KillShellTool::new();
        let result = kill_tool
            .call(json!({ "task_id": "non-existent" }))
            .await
            .unwrap();

        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
    }
}

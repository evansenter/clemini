use crate::agent::AgentEvent;
use crate::tools::tasks::{TASKS, Task};
use crate::tools::{ToolEmitter, error_codes, error_response};
use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::instrument;

pub struct KillShellTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl KillShellTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for KillShellTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for KillShellTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "kill_shell".to_string(),
            "Kill a background task. Works for both bash tasks (run_in_background=true) and ACP subagent tasks. Returns: {task_id, status, success}".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "task_id": {
                        "type": "string",
                        "description": "The task ID to kill (e.g., 'bg-1' for bash, 'acp-1' for subagent)"
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

        let task = {
            let mut tasks = TASKS.lock().unwrap();
            tasks.remove(task_id)
        };

        if let Some(mut task) = task {
            let task_type = task.task_type();

            // For ACP tasks, send cancel signal first to allow graceful shutdown
            if let Task::Acp(ref acp) = task
                && let Some(cancel_tx) = acp.cancel_tx()
            {
                // Non-blocking send - if receiver is gone, that's fine
                let _ = cancel_tx.try_send(());
            }

            // Extract child process based on task type
            let child = match &mut task {
                Task::Background(bg) => bg.take_child(),
                Task::Acp(acp) => acp.take_child(),
            };

            if let Some(mut child) = child {
                match child.kill().await {
                    Ok(_) => {
                        self.emit(&format!("  {}", "killed".dimmed()));
                        Ok(json!({
                            "task_id": task_id,
                            "task_type": task_type,
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
                // Task object exists but child is missing (already finished?)
                Ok(error_response(
                    &format!("Task {} already finished or process missing", task_id),
                    error_codes::NOT_FOUND,
                    json!({ "task_id": task_id }),
                ))
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
        let bash = BashTool::new_without_confirmation_tracking(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
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
        let kill_tool = KillShellTool::new(None);
        let kill_result = kill_tool.call(json!({ "task_id": task_id })).await.unwrap();

        assert!(kill_result["success"].as_bool().unwrap());
        assert_eq!(kill_result["status"], "killed");

        // Verify it's gone from the unified registry
        let tasks = TASKS.lock().unwrap();
        assert!(!tasks.contains_key(task_id));
    }

    #[tokio::test]
    async fn test_kill_shell_not_found() {
        let kill_tool = KillShellTool::new(None);
        let result = kill_tool
            .call(json!({ "task_id": "non-existent" }))
            .await
            .unwrap();

        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
    }
}

use crate::agent::AgentEvent;
use crate::tools::background::BACKGROUND_TASKS;
use crate::tools::{ToolEmitter, error_codes, error_response};
use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, sleep};
use tracing::instrument;

pub struct TaskOutputTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl TaskOutputTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for TaskOutputTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for TaskOutputTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "task_output".to_string(),
            "Get the output and status of a background task. Returns: {task_id, status, exit_code, stdout, stderr}".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "task_id": {
                        "type": "string",
                        "description": "The task ID to check"
                    },
                    "wait": {
                        "type": "boolean",
                        "description": "If true, wait for the task to complete (up to timeout). (default: false)"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Maximum time to wait in seconds if wait=true. (default: 30)"
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

        let wait = args.get("wait").and_then(|v| v.as_bool()).unwrap_or(false);

        let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

        // First check
        {
            let mut tasks = BACKGROUND_TASKS.lock().unwrap();
            if let Some(task) = tasks.get_mut(task_id) {
                task.update_status();
            } else {
                return Ok(error_response(
                    &format!("Task {} not found", task_id),
                    error_codes::NOT_FOUND,
                    json!({ "task_id": task_id }),
                ));
            }
        }

        if wait {
            let start = Instant::now();
            let duration = Duration::from_secs(timeout_secs);

            loop {
                let completed = {
                    let mut tasks = BACKGROUND_TASKS.lock().unwrap();
                    if let Some(task) = tasks.get_mut(task_id) {
                        task.update_status();
                        task.completed.load(std::sync::atomic::Ordering::SeqCst)
                    } else {
                        // Task disappeared?
                        return Ok(error_response(
                            &format!("Task {} not found during wait", task_id),
                            error_codes::NOT_FOUND,
                            json!({ "task_id": task_id }),
                        ));
                    }
                };

                if completed {
                    break;
                }

                if start.elapsed() >= duration {
                    break;
                }

                sleep(Duration::from_millis(200)).await;
            }
        }

        // Fetch final result
        let tasks = BACKGROUND_TASKS.lock().unwrap();
        if let Some(task) = tasks.get(task_id) {
            let completed = task.completed.load(std::sync::atomic::Ordering::SeqCst);
            let status = if completed { "completed" } else { "running" };

            let exit_code = task.exit_code.load(std::sync::atomic::Ordering::SeqCst);
            let stdout = task.stdout_buffer.lock().unwrap().clone();
            let stderr = task.stderr_buffer.lock().unwrap().clone();

            let mut resp = json!({
                "task_id": task_id,
                "status": status,
                "stdout": stdout,
                "stderr": stderr,
            });

            if completed {
                resp["exit_code"] = json!(exit_code);
            }

            Ok(resp)
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
    async fn test_task_output_tool_basic() {
        let dir = tempdir().unwrap();
        let bash = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );

        // Start a background task
        let bash_result = bash
            .call(json!({
                "command": "echo 'hello background'",
                "run_in_background": true
            }))
            .await
            .unwrap();

        let task_id = bash_result["task_id"].as_str().unwrap();

        // Check output
        let tool = TaskOutputTool::new(None);

        // Wait a bit for the process to actually run and output
        sleep(Duration::from_millis(500)).await;

        let result = tool
            .call(json!({ "task_id": task_id, "wait": true, "timeout": 5 }))
            .await
            .unwrap();

        assert_eq!(result["task_id"].as_str().unwrap(), task_id);
        assert_eq!(result["status"].as_str().unwrap(), "completed");
        assert_eq!(result["exit_code"].as_i64().unwrap(), 0);
        assert!(
            result["stdout"]
                .as_str()
                .unwrap()
                .contains("hello background")
        );
    }

    #[tokio::test]
    async fn test_task_output_tool_running() {
        let dir = tempdir().unwrap();
        let bash = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );

        // Start a long running background task
        let bash_result = bash
            .call(json!({
                "command": "sleep 5",
                "run_in_background": true
            }))
            .await
            .unwrap();

        let task_id = bash_result["task_id"].as_str().unwrap();

        let tool = TaskOutputTool::new(None);
        let result = tool
            .call(json!({ "task_id": task_id, "wait": false }))
            .await
            .unwrap();

        assert_eq!(result["status"].as_str().unwrap(), "running");

        // Clean up - extract child before dropping lock to avoid holding across await
        let child = {
            let mut tasks = BACKGROUND_TASKS.lock().unwrap();
            tasks.remove(task_id).and_then(|mut task| task.child.take())
        };
        if let Some(mut child) = child {
            let _ = child.kill().await;
        }
    }
}

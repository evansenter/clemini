use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::instrument;

use super::bash::{BACKGROUND_TASKS, NEXT_TASK_ID};
use crate::agent::AgentEvent;

pub struct TaskTool {
    cwd: PathBuf,
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl TaskTool {
    pub fn new(cwd: PathBuf, events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { cwd, events_tx }
    }

    fn emit(&self, output: &str) {
        if let Some(tx) = &self.events_tx {
            let _ = tx.try_send(AgentEvent::ToolOutput(output.to_string()));
        } else {
            crate::logging::log_event(output);
        }
    }

    /// Get the clemini executable path.
    /// Tries current executable first, falls back to cargo run (development only).
    fn get_clemini_command() -> (String, Vec<String>) {
        // Try current executable first
        if let Ok(exe) = std::env::current_exe()
            && exe.exists()
        {
            return (exe.to_string_lossy().to_string(), vec![]);
        }
        // Fallback to cargo run - only useful during development
        tracing::warn!(
            "current_exe() failed or doesn't exist, falling back to 'cargo run'. \
             This is expected during development but indicates an issue in production."
        );
        (
            "cargo".to_string(),
            vec!["run".to_string(), "--quiet".to_string(), "--".to_string()],
        )
    }
}

#[async_trait]
impl CallableFunction for TaskTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "task".to_string(),
            "Spawn a clemini subagent to handle a delegated task. Use for parallel work, \
             long-running operations, or breaking down complex tasks. \
             Limitations: subagent cannot use interactive tools (ask_user) and has its own sandbox based on cwd. \
             Returns: {task_id, status} for background, {status, stdout, stderr, exit_code} for foreground."
                .to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "prompt": {
                        "type": "string",
                        "description": "The task/prompt to give to the subagent"
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Run in background (default: false). If true, returns immediately with task_id. Use kill_shell to terminate."
                    }
                }),
                vec!["prompt".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing prompt".to_string()))?;
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let (cmd, mut cmd_args) = Self::get_clemini_command();
        cmd_args.extend(["-p".to_string(), prompt.to_string()]);
        // Note: subagent gets its own sandbox based on cwd. It does not inherit the parent's
        // allowed_paths - this is intentional as the subagent operates as an independent instance.
        cmd_args.extend(["--cwd".to_string(), self.cwd.to_string_lossy().to_string()]);

        if background {
            // Background mode: spawn detached, store in registry
            let task_id = NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst).to_string();

            // Note: subprocess inherits environment including GEMINI_API_KEY (required for subagent)
            let child = Command::new(&cmd)
                .args(&cmd_args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| {
                    FunctionError::ExecutionError(format!("Failed to spawn task: {}", e).into())
                })?;

            BACKGROUND_TASKS
                .lock()
                .unwrap()
                .insert(task_id.clone(), child);

            self.emit(&format!("  task {} running in background", task_id));

            Ok(json!({
                "task_id": task_id,
                "status": "running",
                "prompt": prompt
            }))
        } else {
            // Foreground mode: wait for completion, capture output
            self.emit("  running subagent...");

            let output = Command::new(&cmd)
                .args(&cmd_args)
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .map_err(|e| {
                    FunctionError::ExecutionError(format!("Failed to run task: {}", e).into())
                })?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            if exit_code == 0 {
                self.emit("  subagent completed successfully");
            } else {
                self.emit(&format!("  subagent exited with code {}", exit_code));
            }

            Ok(json!({
                "status": if exit_code == 0 { "completed" } else { "failed" },
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_get_clemini_command() {
        let (cmd, args) = TaskTool::get_clemini_command();
        // Should either be the current exe or cargo
        assert!(!cmd.is_empty());
        // If it's cargo, should have the run args
        if cmd == "cargo" {
            assert!(args.contains(&"run".to_string()));
            assert!(args.contains(&"--".to_string()));
        }
    }

    #[test]
    fn test_task_tool_declaration() {
        let dir = tempdir().unwrap();
        let tool = TaskTool::new(dir.path().to_path_buf(), None);
        let decl = tool.declaration();

        assert_eq!(decl.name(), "task");
        assert!(decl.description().contains("subagent"));

        let params = decl.parameters();
        assert!(params.required().contains(&"prompt".to_string()));
    }
}

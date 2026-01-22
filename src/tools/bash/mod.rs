//! Bash command execution tool.
//!
//! This module provides safe bash command execution with:
//! - Pattern-based safety validation (blocked and caution patterns)
//! - Confirmation flow for destructive commands
//! - Background task support
//! - Streaming output capture
//! - Timeout handling

mod safety;

pub use safety::{is_blocked, needs_caution};

use crate::agent::AgentEvent;
use crate::tools::background::BackgroundTask;
use crate::tools::tasks::register_background_task;
use crate::tools::{MAX_TOOL_OUTPUT_LEN, ToolEmitter, error_codes, error_response};
use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::instrument;

pub struct BashTool {
    cwd: PathBuf,
    allowed_paths: Vec<PathBuf>,
    timeout_secs: u64,
    is_mcp_mode: bool,
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl BashTool {
    pub fn new(
        cwd: PathBuf,
        allowed_paths: Vec<PathBuf>,
        timeout_secs: u64,
        is_mcp_mode: bool,
        events_tx: Option<mpsc::Sender<AgentEvent>>,
    ) -> Self {
        Self {
            cwd,
            allowed_paths,
            timeout_secs,
            is_mcp_mode,
            events_tx,
        }
    }

    fn truncate_output(output: String, max_len: usize) -> String {
        if output.len() > max_len {
            // Find last valid UTF-8 boundary at or before max_len
            let mut end = max_len;
            while end > 0 && !output.is_char_boundary(end) {
                end -= 1;
            }
            format!(
                "{}...\n[truncated, {} bytes total]",
                &output[..end],
                output.len()
            )
        } else {
            output
        }
    }

    fn confirm_execution(&self, command: &str) -> bool {
        let msg = format!(
            "\n⚠️  This command may be destructive:\n    {}",
            command.bold()
        );
        eprintln!("{}", msg);
        self.emit(&msg);

        eprint!("Proceed? [y/N] ");
        let _ = io::stderr().flush();

        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_ok() {
            let answer = answer.trim().to_lowercase();
            answer == "y" || answer == "yes"
        } else {
            false
        }
    }
}

impl ToolEmitter for BashTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for BashTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "bash".to_string(),
            "Execute a bash command and return the output. Use for builds, tests, git, and shell commands. Returns: {stdout, stderr, exit_code} or {task_id, status} when run_in_background=true".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute (e.g., 'cargo test', 'gh issue view 42', 'git status')"
                    },
                    "description": {
                        "type": "string",
                        "description": "Human-readable description of what the command does (shown in logs)"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": format!("Maximum time to wait for the command (default: {})", self.timeout_secs)
                    },
                    "confirmed": {
                        "type": "boolean",
                        "description": "Set to true only after user explicitly approves the command in conversation. First call should always omit this or use false. Destructive commands return needs_confirmation until approved. (default: false)"
                    },
                    "working_directory": {
                        "type": "string",
                        "description": "Directory to run the command in (must be within allowed paths). (default: current working directory)"
                    },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "If true, run the command in the background and return a task_id immediately. (default: false)"
                    }
                }),
                vec!["command".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing command".to_string()))?;

        let description = args.get("description").and_then(|v| v.as_str());

        let timeout_secs = args
            .get("timeout_seconds")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(self.timeout_secs);

        let confirmed = args
            .get("confirmed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let run_in_background = args
            .get("run_in_background")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let working_dir = if let Some(wd) = args.get("working_directory").and_then(|v| v.as_str()) {
            match crate::tools::resolve_and_validate_path(wd, &self.cwd, &self.allowed_paths) {
                Ok(path) => path,
                Err(e) => {
                    return Ok(error_response(
                        &format!("Invalid working_directory: {}", e),
                        error_codes::ACCESS_DENIED,
                        json!({
                            "command": command,
                            "working_directory": wd
                        }),
                    ));
                }
            }
        } else {
            self.cwd.clone()
        };

        // Safety check
        if let Some(pattern) = is_blocked(command) {
            let msg = format!(
                "  {} {}",
                format!("BLOCKED (matches pattern: {pattern}):").red(),
                command.dimmed()
            );
            self.emit(&msg);
            return Ok(error_response(
                &format!("Command blocked: matches pattern '{}'", pattern),
                error_codes::BLOCKED,
                json!({
                    "command": command,
                    "description": description
                }),
            ));
        }

        if needs_caution(command) {
            if self.is_mcp_mode {
                if !confirmed {
                    let msg = format!(
                        "  {} {}",
                        "CAUTION (requesting MCP confirmation):".yellow(),
                        command.dimmed()
                    );
                    self.emit(&msg);
                    let mut resp = json!({
                        "needs_confirmation": true,
                        "command": command,
                        "message": format!("This command may be destructive: {}. Please confirm execution.", command)
                    });
                    if let Some(desc) = description {
                        resp["description"] = json!(desc);
                    }
                    return Ok(resp);
                }
            } else if !confirmed && !self.confirm_execution(command) {
                let msg = format!("  {} {}", "CANCELLED:".red(), command.dimmed());
                self.emit(&msg);
                return Ok(error_response(
                    "Command cancelled by user",
                    error_codes::BLOCKED,
                    json!({
                        "command": command,
                        "description": description
                    }),
                ));
            }
            let msg = format!(
                "  {} {}",
                "CAUTION (user confirmed):".yellow(),
                command.dimmed()
            );
            self.emit(&msg);
        }

        if run_in_background {
            let child = Command::new("bash")
                .arg("-c")
                .arg(command)
                .current_dir(&working_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    FunctionError::ExecutionError(format!("Failed to spawn process: {}", e).into())
                })?;

            // Register in unified task registry with namespaced ID (bg-1, bg-2, etc.)
            let task_id = register_background_task(BackgroundTask::new(child));

            let mut response = json!({
                "command": command,
                "task_id": task_id,
                "status": "running"
            });
            if let Some(desc) = description {
                response["description"] = json!(desc);
            }
            return Ok(response);
        }

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                FunctionError::ExecutionError(format!("Failed to spawn process: {}", e).into())
            })?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        let mut captured_stdout = String::new();
        let mut captured_stderr = String::new();

        let mut logged_stdout_lines = 0;
        let mut logged_stderr_lines = 0;
        const MAX_LOG_LINES: usize = 10;

        let mut stdout_done = false;
        let mut stderr_done = false;
        let mut process_exited = false;
        let mut exit_status_final = None;

        let timeout_duration = std::time::Duration::from_secs(timeout_secs);

        let timed_out = match tokio::time::timeout(timeout_duration, async {
            loop {
                if process_exited && stdout_done && stderr_done {
                    break;
                }

                tokio::select! {
                    line = stdout_reader.next_line(), if !stdout_done => {
                        match line {
                            Ok(Some(line)) => {
                                if logged_stdout_lines < MAX_LOG_LINES {
                                    self.emit(&format!("  {}", line.dimmed()));
                                    logged_stdout_lines += 1;
                                } else if logged_stdout_lines == MAX_LOG_LINES {
                                    self.emit(&format!("  {}", "[...more stdout...]".dimmed()));
                                    logged_stdout_lines += 1;
                                }
                                captured_stdout.push_str(&line);
                                captured_stdout.push('\n');
                            }
                            _ => {
                                stdout_done = true;
                            }
                        }
                    }
                    line = stderr_reader.next_line(), if !stderr_done => {
                        match line {
                            Ok(Some(line)) => {
                                if logged_stderr_lines < MAX_LOG_LINES {
                                    self.emit(&format!("  {}", line.dimmed()));
                                    logged_stderr_lines += 1;
                                } else if logged_stderr_lines == MAX_LOG_LINES {
                                    self.emit(&format!("  {}", "[...more stderr...]".dimmed()));
                                    logged_stderr_lines += 1;
                                }
                                captured_stderr.push_str(&line);
                                captured_stderr.push('\n');
                            }
                            _ => {
                                stderr_done = true;
                            }
                        }
                    }
                    status = child.wait(), if !process_exited => {
                        process_exited = true;
                        exit_status_final = status.ok();
                    }
                }
            }
        })
        .await
        {
            Ok(_) => false,
            Err(_) => {
                let _ = child.kill().await;
                true
            }
        };

        if timed_out {
            return Ok(error_response(
                &format!("Command timed out after {} seconds", timeout_secs),
                error_codes::TIMEOUT,
                json!({
                    "command": command,
                    "description": description,
                    "timeout_seconds": timeout_secs,
                    "stdout": captured_stdout,
                    "stderr": captured_stderr,
                }),
            ));
        }

        let exit_code = exit_status_final.and_then(|s| s.code()).unwrap_or(-1);
        let success = exit_status_final.map(|s| s.success()).unwrap_or(false);

        // Truncate very long output
        let max_len = MAX_TOOL_OUTPUT_LEN;
        let stdout_truncated = Self::truncate_output(captured_stdout, max_len);
        let stderr_truncated = Self::truncate_output(captured_stderr, max_len);

        let mut response = json!({
            "command": command,
            "exit_code": exit_code,
            "stdout": stdout_truncated,
            "stderr": stderr_truncated,
            "success": success
        });

        if let Some(desc) = description {
            response["description"] = json!(desc);
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tasks::{TASKS, Task};
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_bash_tool_success() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );
        let args = json!({ "command": "echo 'hello world'" });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "hello world");
    }

    #[tokio::test]
    async fn test_bash_tool_description() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );
        let args = json!({
            "command": "echo 'test'",
            "description": "testing description"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(
            result["description"].as_str().unwrap(),
            "testing description"
        );
    }

    #[tokio::test]
    async fn test_bash_tool_failure() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );
        let args = json!({ "command": "exit 1" });

        let result = tool.call(args).await.unwrap();
        assert!(!result["success"].as_bool().unwrap());
        assert_eq!(result["exit_code"], 1);
    }

    #[tokio::test]
    async fn test_bash_tool_timeout() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            1,
            false,
            None,
        );
        let args = json!({ "command": "sleep 2" });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("timed out"));
        assert_eq!(result["error_code"], error_codes::TIMEOUT);
    }

    #[tokio::test]
    async fn test_bash_tool_stderr() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );
        let args = json!({ "command": "echo 'error message' >&2" });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["stderr"].as_str().unwrap().trim(), "error message");
    }

    #[tokio::test]
    async fn test_bash_tool_cwd() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );
        let args = json!({ "command": "pwd" });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        let pwd = result["stdout"].as_str().unwrap().trim();
        // Handle potential symlinks in temp dir
        let expected = dir.path().canonicalize().unwrap();
        let actual = std::path::Path::new(pwd).canonicalize().unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn test_bash_tool_working_directory() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );

        // Run pwd in subdir
        let args = json!({
            "command": "pwd",
            "working_directory": "subdir"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        let pwd = result["stdout"].as_str().unwrap().trim();
        let expected = subdir.canonicalize().unwrap();
        let actual = std::path::Path::new(pwd).canonicalize().unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn test_bash_tool_invalid_working_directory() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );

        // Try to run in a directory outside allowed paths
        let args = json!({
            "command": "pwd",
            "working_directory": "/tmp"
        });

        let result = tool.call(args).await.unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("outside allowed paths")
        );
        assert_eq!(result["error_code"], error_codes::ACCESS_DENIED);
    }

    #[tokio::test]
    async fn test_bash_tool_background() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );
        let args = json!({
            "command": "sleep 10",
            "run_in_background": true
        });

        let result = tool.call(args).await.unwrap();
        assert_eq!(result["status"], "running");
        assert!(result["task_id"].is_string());

        let task_id = result["task_id"].as_str().unwrap().to_string();

        // Task IDs now have "bg-" prefix
        assert!(task_id.starts_with("bg-"));

        // Check if it's in the unified TASKS registry
        {
            let tasks = TASKS.lock().unwrap();
            assert!(tasks.contains_key(&task_id));
        }

        // Cleanup: kill the background process
        let mut task = {
            let mut tasks = TASKS.lock().unwrap();
            tasks.remove(&task_id)
        };
        if let Some(Task::Background(ref mut bg)) = task
            && let Some(mut child) = bg.take_child()
        {
            let _ = child.kill().await;
        }
    }

    #[tokio::test]
    async fn test_bash_tool_background_unique_ids() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            5,
            false,
            None,
        );

        let args1 = json!({
            "command": "sleep 10",
            "run_in_background": true
        });
        let args2 = json!({
            "command": "sleep 10",
            "run_in_background": true
        });

        let result1 = tool.call(args1).await.unwrap();
        let result2 = tool.call(args2).await.unwrap();

        let id1 = result1["task_id"].as_str().unwrap();
        let id2 = result2["task_id"].as_str().unwrap();

        // Both should have "bg-" prefix and be unique
        assert!(id1.starts_with("bg-"));
        assert!(id2.starts_with("bg-"));
        assert_ne!(id1, id2);

        // Cleanup - extract children before dropping lock to avoid holding across await
        let (mut task1, mut task2) = {
            let mut tasks = TASKS.lock().unwrap();
            (tasks.remove(id1), tasks.remove(id2))
        };
        if let Some(Task::Background(ref mut bg)) = task1
            && let Some(mut child) = bg.take_child()
        {
            let _ = child.kill().await;
        }
        if let Some(Task::Background(ref mut bg)) = task2
            && let Some(mut child) = bg.take_child()
        {
            let _ = child.kill().await;
        }
    }

    #[test]
    fn test_truncate_output_utf8() {
        // Multi-byte character: "🦀" is 4 bytes [240, 159, 166, 128]
        let input = "abc🦀def".to_string();

        // Truncate in middle of "🦀" (at index 5 or 6)
        let truncated = BashTool::truncate_output(input.clone(), 5);
        // Should truncate at index 3 (before 🦀)
        assert!(truncated.starts_with("abc..."));

        let truncated = BashTool::truncate_output(input, 7);
        // Should truncate at index 7 (after 🦀)
        assert!(truncated.starts_with("abc🦀..."));
    }
}

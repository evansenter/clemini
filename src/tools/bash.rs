use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use regex::Regex;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::LazyLock;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::instrument;

/// Blocked command patterns that are always rejected.
static BLOCKED_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Destructive filesystem operations
        Regex::new(r"rm\s+(-[rfRF]+\s+)*[/~](\s|$)").unwrap(),
        Regex::new(r"rm\s+(-[rfRF]+\s+)*/\*").unwrap(),
        Regex::new(r"rm\s+(-[rfRF]+\s+)*~").unwrap(),
        // Disk/device operations
        Regex::new(r"dd\s+.*if=").unwrap(),
        Regex::new(r"mkfs").unwrap(),
        Regex::new(r">\s*/dev/sd").unwrap(),
        Regex::new(r">\s*/dev/nvme").unwrap(),
        // Permission bombs
        Regex::new(r"chmod\s+(-[rR]+\s+)*777\s+/").unwrap(),
        Regex::new(r"chown\s+(-[rR]+\s+)*.*\s+/").unwrap(),
        // Fork bomb
        Regex::new(r":\(\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;").unwrap(),
        // Dangerous redirects
        Regex::new(r">\s*/etc/").unwrap(),
        Regex::new(r">\s*/boot/").unwrap(),
        // History/config manipulation
        Regex::new(r">\s*~/\.bash").unwrap(),
        Regex::new(r">\s*~/\.profile").unwrap(),
        Regex::new(r">\s*~/\.zsh").unwrap(),
    ]
});

/// Commands that require extra caution (logged but allowed).
static CAUTION_PATTERNS: LazyLock<Vec<&str>> = LazyLock::new(|| {
    vec![
        "sudo", "su ", "rm ", "mv ", "chmod", "chown", "kill", "pkill", "killall",
    ]
});

pub struct BashTool {
    cwd: PathBuf,
    timeout_secs: u64,
}

impl BashTool {
    pub fn new(cwd: PathBuf, timeout_secs: u64) -> Self {
        Self { cwd, timeout_secs }
    }

    fn is_blocked(command: &str) -> Option<String> {
        for pattern in BLOCKED_PATTERNS.iter() {
            if pattern.is_match(command) {
                return Some(pattern.as_str().to_string());
            }
        }
        None
    }

    fn needs_caution(command: &str) -> bool {
        CAUTION_PATTERNS
            .iter()
            .any(|pattern| command.contains(pattern))
    }
}

#[async_trait]
impl CallableFunction for BashTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "bash".to_string(),
            "Execute a bash command and return the output. Use this for running builds, tests, git operations, and other shell commands.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": format!("Maximum time to wait for the command (default: {})", self.timeout_secs)
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

        let timeout_secs = args
            .get("timeout_seconds")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(self.timeout_secs);

        // Safety check
        if let Some(pattern) = Self::is_blocked(command) {
            let msg = format!("[bash BLOCKED: {command}] (matches pattern: {pattern})");
            eprintln!("{}", msg);
            crate::log_event(&msg);
            return Ok(json!({
                "error": format!("Command blocked: matches pattern '{pattern}'"),
                "command": command
            }));
        }

        if Self::needs_caution(command) {
            let msg = format!("[bash CAUTION: {command}]");
            eprintln!("{}", msg);
            crate::log_event(&msg);
        } else {
            let msg = format!("[bash] running: \"{}\"", command).dimmed();
            eprintln!("{}", msg);
            crate::log_event(&msg.to_string());
        }

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| FunctionError::ExecutionError(format!("Failed to spawn process: {}", e).into()))?;

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
                                    let dimmed = line.dimmed();
                                    eprintln!("{}", dimmed);
                                    crate::log_event(&dimmed.to_string());
                                    logged_stdout_lines += 1;
                                } else if logged_stdout_lines == MAX_LOG_LINES {
                                    let msg = "[...more stdout...]";
                                    eprintln!("{}", msg.dimmed());
                                    crate::log_event(msg);
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
                                    let dimmed = line.dimmed();
                                    eprintln!("{}", dimmed);
                                    crate::log_event(&dimmed.to_string());
                                    logged_stderr_lines += 1;
                                } else if logged_stderr_lines == MAX_LOG_LINES {
                                    let msg = "[...more stderr...]";
                                    eprintln!("{}", msg.dimmed());
                                    crate::log_event(msg);
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
        }).await {
            Ok(_) => false,
            Err(_) => {
                let _ = child.kill().await;
                true
            }
        };

        if timed_out {
            return Ok(json!({
                "error": format!("Command timed out after {} seconds", timeout_secs),
                "command": command,
                "stdout": captured_stdout,
                "stderr": captured_stderr,
            }));
        }

        let exit_code = exit_status_final.and_then(|s| s.code()).unwrap_or(-1);
        let success = exit_status_final.map(|s| s.success()).unwrap_or(false);

        // Truncate very long output
        let max_len = 50000;
        let stdout_truncated = if captured_stdout.len() > max_len {
            format!(
                "{}...\n[truncated, {} bytes total]",
                &captured_stdout[..max_len],
                captured_stdout.len()
            )
        } else {
            captured_stdout
        };

        let stderr_truncated = if captured_stderr.len() > max_len {
            format!(
                "{}...\n[truncated, {} bytes total]",
                &captured_stderr[..max_len],
                captured_stderr.len()
            )
        } else {
            captured_stderr
        };

        Ok(json!({
            "command": command,
            "exit_code": exit_code,
            "stdout": stdout_truncated,
            "stderr": stderr_truncated,
            "success": success
        }))
    }
}

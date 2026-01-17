use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use regex::Regex;
use serde_json::{Value, json};
use std::io::{self, Write};
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

/// Commands that require extra caution (requires user confirmation).
static CAUTION_PATTERNS: LazyLock<Vec<&str>> = LazyLock::new(|| {
    vec![
        "sudo",
        "su ",
        "rm ",
        "mv ",
        "chmod",
        "chown",
        "kill",
        "pkill",
        "killall",
        "git push --force",
        "git push -f",
        "git reset --hard",
        "cargo publish",
        "npm publish",
        "docker rm",
        "docker rmi",
    ]
});

pub struct BashTool {
    cwd: PathBuf,
    timeout_secs: u64,
    is_mcp_mode: bool,
}

impl BashTool {
    pub fn new(cwd: PathBuf, timeout_secs: u64, is_mcp_mode: bool) -> Self {
        Self {
            cwd,
            timeout_secs,
            is_mcp_mode,
        }
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
        crate::log_event(&msg);

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
                        "description": "Set to true to confirm execution of a destructive command (required for caution commands in MCP mode)"
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

        // Safety check
        if let Some(pattern) = Self::is_blocked(command) {
            let msg = format!("[bash BLOCKED: {command}] (matches pattern: {pattern})");
            crate::log_event(&msg);
            return Ok(json!({
                "error": format!("Command blocked: matches pattern '{pattern}'"),
                "command": command
            }));
        }

        if Self::needs_caution(command) {
            if self.is_mcp_mode {
                if !confirmed {
                    let msg = format!("[bash CAUTION: {command}] (requesting MCP confirmation)");
                    crate::log_event(&msg);
                    return Ok(json!({
                        "needs_confirmation": true,
                        "command": command,
                        "message": format!("This command may be destructive: {}. Please confirm execution.", command)
                    }));
                }
            } else if !confirmed && !self.confirm_execution(command) {
                let msg = format!("[bash CANCELLED: {command}]");
                crate::log_event(&msg);
                return Ok(json!({
                    "error": "Command cancelled by user",
                    "command": command
                }));
            }
            let msg = format!("[bash CAUTION: {}] (user confirmed)", command);
            crate::log_event(&msg);
        } else {
            let log_msg = if let Some(desc) = description {
                format!("[bash] {}: \"{}\"", desc, command)
            } else {
                format!("[bash] running: \"{}\"", command)
            };
            crate::log_event_raw(&log_msg.dimmed().italic().to_string());
        }

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
                                    crate::log_event_raw(&line.dimmed().italic().to_string());
                                    logged_stdout_lines += 1;
                                } else if logged_stdout_lines == MAX_LOG_LINES {
                                    crate::log_event_raw(&"[...more stdout...]".dimmed().italic().to_string());
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
                                    crate::log_event_raw(&line.dimmed().italic().to_string());
                                    logged_stderr_lines += 1;
                                } else if logged_stderr_lines == MAX_LOG_LINES {
                                    crate::log_event_raw(&"[...more stderr...]".dimmed().italic().to_string());
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
        let stdout_truncated = Self::truncate_output(captured_stdout, max_len);
        let stderr_truncated = Self::truncate_output(captured_stderr, max_len);

        Ok(json!({
            "command": command,
            "exit_code": exit_code,
            "stdout": stdout_truncated,
            "stderr": stderr_truncated,
            "success": success
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_bash_tool_success() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(dir.path().to_path_buf(), 5, false);
        let args = json!({ "command": "echo 'hello world'" });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "hello world");
    }

    #[tokio::test]
    async fn test_bash_tool_failure() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(dir.path().to_path_buf(), 5, false);
        let args = json!({ "command": "exit 1" });

        let result = tool.call(args).await.unwrap();
        assert!(!result["success"].as_bool().unwrap());
        assert_eq!(result["exit_code"], 1);
    }

    #[tokio::test]
    async fn test_bash_tool_timeout() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(dir.path().to_path_buf(), 1, false);
        let args = json!({ "command": "sleep 2" });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("timed out"));
    }

    #[test]
    fn test_bash_tool_blocked_patterns() {
        assert!(BashTool::is_blocked("rm -rf /").is_some());
        assert!(BashTool::is_blocked("rm -rf /*").is_some());
        assert!(BashTool::is_blocked("rm -rf ~").is_some());
        assert!(BashTool::is_blocked("dd if=/dev/zero of=/dev/sda").is_some());
        assert!(BashTool::is_blocked("mkfs.ext4 /dev/sda1").is_some());
        assert!(BashTool::is_blocked("chmod 777 /").is_some());
        assert!(BashTool::is_blocked("chmod -R 777 /").is_some());
        assert!(BashTool::is_blocked("chown user /").is_some());
        assert!(BashTool::is_blocked(":(){ :|:& };:").is_some());
        assert!(BashTool::is_blocked("echo 'malicious' > /etc/passwd").is_some());
        assert!(BashTool::is_blocked("ls -l").is_none());
    }

    #[test]
    fn test_bash_tool_needs_caution() {
        assert!(BashTool::needs_caution("sudo apt update"));
        assert!(BashTool::needs_caution("rm file.txt"));
        assert!(BashTool::needs_caution("git push --force"));
        assert!(BashTool::needs_caution("git reset --hard HEAD"));
        assert!(!BashTool::needs_caution("ls -l"));
    }

    #[tokio::test]
    async fn test_bash_tool_stderr() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(dir.path().to_path_buf(), 5, false);
        let args = json!({ "command": "echo 'error message' >&2" });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["stderr"].as_str().unwrap().trim(), "error message");
    }

    #[tokio::test]
    async fn test_bash_tool_cwd() {
        let dir = tempdir().unwrap();
        let tool = BashTool::new(dir.path().to_path_buf(), 5, false);
        let args = json!({ "command": "pwd" });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        let pwd = result["stdout"].as_str().unwrap().trim();
        // Handle potential symlinks in temp dir
        let expected = dir.path().canonicalize().unwrap();
        let actual = std::path::Path::new(pwd).canonicalize().unwrap();
        assert_eq!(actual, expected);
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

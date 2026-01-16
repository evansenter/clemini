use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use regex::Regex;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::LazyLock;
use tokio::process::Command;

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
}

impl BashTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
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
                        "description": "Maximum time to wait for the command (default: 60)"
                    }
                }),
                vec!["command".to_string()],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing command".to_string()))?;

        let timeout_secs = args
            .get("timeout_seconds")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(60);

        // Safety check
        if let Some(pattern) = Self::is_blocked(command) {
            eprintln!("[bash BLOCKED: {command}] (matches pattern: {pattern})");
            return Ok(json!({
                "error": format!("Command blocked: matches pattern '{pattern}'"),
                "command": command
            }));
        }

        if Self::needs_caution(command) {
            eprintln!("[bash CAUTION: {command}]");
        }

        // Logging is handled by main.rs event loop with timing info

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            Command::new("bash")
                .arg("-c")
                .arg(command)
                .current_dir(&self.cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);

                // Truncate very long output
                let max_len = 50000;
                let stdout_truncated = if stdout.len() > max_len {
                    format!(
                        "{}...\n[truncated, {} bytes total]",
                        &stdout[..max_len],
                        stdout.len()
                    )
                } else {
                    stdout.to_string()
                };

                let stderr_truncated = if stderr.len() > max_len {
                    format!(
                        "{}...\n[truncated, {} bytes total]",
                        &stderr[..max_len],
                        stderr.len()
                    )
                } else {
                    stderr.to_string()
                };

                Ok(json!({
                    "command": command,
                    "exit_code": exit_code,
                    "stdout": stdout_truncated,
                    "stderr": stderr_truncated,
                    "success": output.status.success()
                }))
            }
            Ok(Err(e)) => Ok(json!({
                "error": format!("Failed to execute command: {}", e),
                "command": command
            })),
            Err(_) => Ok(json!({
                "error": format!("Command timed out after {} seconds", timeout_secs),
                "command": command
            })),
        }
    }
}

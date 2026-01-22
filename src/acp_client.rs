//! ACP client implementation for spawning subagents.
//!
//! This module provides the client side of ACP communication, enabling
//! clemini to spawn subagents and receive structured events from them.
//!
//! # Architecture
//!
//! The task tool spawns clemini subagents via `AcpSubagent::spawn()`.
//! The subagent runs with `--acp-server` and communicates via ACP protocol.
//!
//! See also:
//! - `src/acp.rs` for the server side
//! - `src/tools/task.rs` for the task tool that uses this

use acp::Agent as AcpAgent;
use agent_client_protocol as acp;
use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::tools::get_clemini_command;

use crate::tools::tasks::{TASKS, Task, register_acp_task};

/// Represents an active ACP subagent task.
pub struct AcpTask {
    /// Whether the task has completed.
    completed: Arc<AtomicBool>,

    /// Accumulated output from the subagent.
    output_buffer: Arc<Mutex<String>>,

    /// Error message if the task failed.
    error: Arc<Mutex<Option<String>>>,

    /// The child process handle.
    child: Option<Child>,

    /// Channel to signal cancellation.
    cancel_tx: Option<mpsc::Sender<()>>,
}

impl AcpTask {
    /// Create a new ACP task from a spawned child process.
    pub fn new(child: Child, cancel_tx: mpsc::Sender<()>) -> Self {
        Self {
            completed: Arc::new(AtomicBool::new(false)),
            output_buffer: Arc::new(Mutex::new(String::new())),
            error: Arc::new(Mutex::new(None)),
            child: Some(child),
            cancel_tx: Some(cancel_tx),
        }
    }

    /// Check if the task has completed.
    pub fn is_completed(&self) -> bool {
        self.completed.load(Ordering::SeqCst)
    }

    /// Mark the task as completed.
    pub fn mark_completed(&self) {
        self.completed.store(true, Ordering::SeqCst);
    }

    /// Get the accumulated output.
    pub fn output(&self) -> String {
        match self.output_buffer.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => {
                tracing::warn!("output_buffer lock was poisoned, recovering");
                poisoned.into_inner().clone()
            }
        }
    }

    /// Get the error message if any.
    pub fn error(&self) -> Option<String> {
        match self.error.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => {
                tracing::warn!("error lock was poisoned, recovering");
                poisoned.into_inner().clone()
            }
        }
    }

    /// Set the error message.
    pub fn set_error(&self, error: String) {
        match self.error.lock() {
            Ok(mut guard) => *guard = Some(error),
            Err(poisoned) => {
                tracing::warn!("error lock was poisoned, recovering");
                *poisoned.into_inner() = Some(error);
            }
        }
    }

    /// Take the child process (for killing).
    pub fn take_child(&mut self) -> Option<Child> {
        self.child.take()
    }

    /// Check if the child process is still available.
    pub fn has_child(&self) -> bool {
        self.child.is_some()
    }

    /// Get the cancellation sender.
    pub fn cancel_tx(&self) -> Option<&mpsc::Sender<()>> {
        self.cancel_tx.as_ref()
    }

    /// Get clones of the internal buffers for spawning background work.
    /// Returns (completed_flag, output_buffer, error_buffer).
    #[allow(clippy::type_complexity)]
    pub fn internal_buffers(
        &self,
    ) -> (
        Arc<AtomicBool>,
        Arc<Mutex<String>>,
        Arc<Mutex<Option<String>>>,
    ) {
        (
            self.completed.clone(),
            self.output_buffer.clone(),
            self.error.clone(),
        )
    }
}

/// Client implementation that receives session notifications from subagent.
struct SubagentClient {
    /// Buffer to accumulate output.
    output_buffer: Arc<Mutex<String>>,

    /// Flag to mark completion (reserved for future use).
    #[allow(dead_code)]
    completed: Arc<AtomicBool>,
}

#[async_trait(?Send)]
impl acp::Client for SubagentClient {
    async fn request_permission(
        &self,
        _args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        // Subagents operate autonomously - no interactive permissions
        Err(acp::Error::method_not_found())
    }

    async fn write_text_file(
        &self,
        _args: acp::WriteTextFileRequest,
    ) -> acp::Result<acp::WriteTextFileResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn read_text_file(
        &self,
        _args: acp::ReadTextFileRequest,
    ) -> acp::Result<acp::ReadTextFileResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn create_terminal(
        &self,
        _args: acp::CreateTerminalRequest,
    ) -> acp::Result<acp::CreateTerminalResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn terminal_output(
        &self,
        _args: acp::TerminalOutputRequest,
    ) -> acp::Result<acp::TerminalOutputResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn release_terminal(
        &self,
        _args: acp::ReleaseTerminalRequest,
    ) -> acp::Result<acp::ReleaseTerminalResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn wait_for_terminal_exit(
        &self,
        _args: acp::WaitForTerminalExitRequest,
    ) -> acp::Result<acp::WaitForTerminalExitResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn kill_terminal_command(
        &self,
        _args: acp::KillTerminalCommandRequest,
    ) -> acp::Result<acp::KillTerminalCommandResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        match args.update {
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk { content, .. }) => {
                // ContentBlock is #[non_exhaustive], catch-all required
                let text = match content {
                    acp::ContentBlock::Text(text_content) => text_content.text,
                    acp::ContentBlock::Image(_) => "[image]".into(),
                    acp::ContentBlock::Audio(_) => "[audio]".into(),
                    acp::ContentBlock::ResourceLink(link) => format!("[link: {}]", link.uri),
                    acp::ContentBlock::Resource(_) => "[resource]".into(),
                    other => {
                        tracing::debug!("Unhandled ACP content block: {:?}", other);
                        "[unknown content]".into()
                    }
                };
                let mut buffer = self.output_buffer.lock().unwrap();
                buffer.push_str(&text);
            }
            acp::SessionUpdate::ToolCall(tool_call) => {
                let mut buffer = self.output_buffer.lock().unwrap();
                buffer.push_str(&format!("\n[tool: {}]\n", tool_call.title));
            }
            acp::SessionUpdate::ToolCallUpdate(update) => {
                let mut buffer = self.output_buffer.lock().unwrap();
                if let Some(status) = &update.fields.status {
                    buffer.push_str(&format!("[status: {:?}]\n", status));
                }
            }
            other => {
                // Log unknown update types at debug level for diagnostics
                tracing::debug!("Ignoring unhandled ACP session update: {:?}", other);
            }
        }
        Ok(())
    }

    async fn ext_method(&self, _args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn ext_notification(&self, _args: acp::ExtNotification) -> acp::Result<()> {
        Ok(())
    }
}

/// Spawn a clemini subagent and run a prompt.
///
/// Returns the task output for foreground mode, or task_id for background mode.
pub async fn spawn_subagent(prompt: &str, cwd: &Path, background: bool) -> Result<SubagentResult> {
    // Get clemini executable path
    let (cmd, mut cmd_args) = get_clemini_command();
    cmd_args.extend([
        "--acp-server".to_string(),
        "--cwd".to_string(),
        cwd.to_string_lossy().to_string(),
    ]);

    // Spawn the subprocess with stderr captured for debugging
    let mut child = Command::new(&cmd)
        .args(&cmd_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("No stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("No stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("No stderr"))?;

    // Create cancel channel - tx stored in task for kill_shell, rx used by background runner
    let (cancel_tx, cancel_rx) = mpsc::channel::<()>(1);

    // Create the ACP task (owns the internal buffers)
    let task = AcpTask::new(child, cancel_tx);

    // Get clones of the internal buffers for the client and background work
    let (completed, output_buffer, error_buffer) = task.internal_buffers();

    // Spawn task to capture stderr
    let error_buffer_clone = error_buffer.clone();
    tokio::task::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut stderr = stderr;
        let mut buf = String::new();
        if stderr.read_to_string(&mut buf).await.is_ok() && !buf.is_empty() {
            match error_buffer_clone.lock() {
                Ok(mut guard) => *guard = Some(buf),
                Err(poisoned) => *poisoned.into_inner() = Some(buf),
            }
        }
    });

    let client = SubagentClient {
        output_buffer: output_buffer.clone(),
        completed: completed.clone(),
    };

    if background {
        // Background mode: register task in unified registry and return immediately
        let task_id = register_acp_task(task);

        // Get references to track completion
        let task_completed = completed;
        let task_error = error_buffer;

        // Spawn the ACP communication in the background
        let prompt = prompt.to_string();
        let task_id_clone = task_id.clone();
        let cwd_owned = cwd.to_path_buf();
        let mut cancel_rx = cancel_rx;
        tokio::task::spawn_local(async move {
            // Race between ACP session completion and cancellation signal
            let result = tokio::select! {
                result = run_acp_session(
                    client,
                    stdin.compat_write(),
                    stdout.compat(),
                    &prompt,
                    &cwd_owned,
                ) => result,
                _ = cancel_rx.recv() => {
                    Err(anyhow::anyhow!("Task cancelled"))
                }
            };

            task_completed.store(true, Ordering::SeqCst);

            if let Err(e) = result {
                // Store error in dedicated field
                match task_error.lock() {
                    Ok(mut guard) => *guard = Some(e.to_string()),
                    Err(poisoned) => *poisoned.into_inner() = Some(e.to_string()),
                }
            }

            // Clean up child from registry
            if let Ok(mut tasks) = TASKS.lock()
                && let Some(Task::Acp(task)) = tasks.get_mut(&task_id_clone)
            {
                task.take_child();
            }
        });

        Ok(SubagentResult::Background { task_id })
    } else {
        // Foreground mode: wait for completion
        let result =
            run_acp_session(client, stdin.compat_write(), stdout.compat(), prompt, cwd).await;

        completed.store(true, Ordering::SeqCst);

        let output = match output_buffer.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let stderr_output = match error_buffer.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };

        match result {
            Ok(_) => Ok(SubagentResult::Completed { output }),
            Err(e) => {
                // Combine error with stderr if available
                let error_msg = if let Some(stderr) = stderr_output {
                    format!("{}\nstderr: {}", e, stderr)
                } else {
                    e.to_string()
                };
                Ok(SubagentResult::Failed {
                    output,
                    error: error_msg,
                })
            }
        }
    }
}

/// Result from spawning a subagent.
pub enum SubagentResult {
    /// Background task started.
    Background { task_id: String },
    /// Foreground task completed successfully.
    Completed { output: String },
    /// Foreground task failed.
    Failed { output: String, error: String },
}

/// Run the ACP session with the subagent.
///
/// Cancellation is handled at the caller level via tokio::select! racing this
/// function against a cancel channel. When cancelled, the caller kills the
/// child process.
async fn run_acp_session<W, R>(
    client: SubagentClient,
    outgoing: W,
    incoming: R,
    prompt: &str,
    cwd: &Path,
) -> Result<()>
where
    W: futures_util::AsyncWrite + Unpin + 'static,
    R: futures_util::AsyncRead + Unpin + 'static,
{
    let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });

    // Spawn IO handler
    tokio::task::spawn_local(handle_io);

    // Initialize
    conn.initialize(
        acp::InitializeRequest::new(acp::ProtocolVersion::LATEST).client_info(
            acp::Implementation::new(
                env!("CARGO_PKG_NAME").to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
            ),
        ),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Initialize failed: {}", e))?;

    // Create session
    let session = conn
        .new_session(acp::NewSessionRequest::new(cwd.to_path_buf()))
        .await
        .map_err(|e| anyhow::anyhow!("New session failed: {}", e))?;

    // Send prompt
    conn.prompt(acp::PromptRequest::new(
        session.session_id.to_string(),
        vec![acp::ContentBlock::Text(acp::TextContent::new(
            prompt.to_string(),
        ))],
    ))
    .await
    .map_err(|e| anyhow::anyhow!("Prompt failed: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn test_acp_task_initial_state() {
        let (tx, _rx) = mpsc::channel(1);
        let child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();

        let task = AcpTask::new(child, tx);

        assert!(!task.is_completed());
        assert!(task.output().is_empty());
        assert!(task.error().is_none());
        assert!(task.has_child());
    }

    #[test]
    fn test_get_clemini_command() {
        let (cmd, args) = crate::tools::get_clemini_command();
        assert!(!cmd.is_empty());
        // If it's cargo, should have the run args
        if cmd == "cargo" {
            assert!(args.contains(&"run".to_string()));
            assert!(args.contains(&"--".to_string()));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_acp_task_set_error() {
        let (tx, _rx) = mpsc::channel(1);
        let child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();

        let task = AcpTask::new(child, tx);

        assert!(task.error().is_none());
        task.set_error("test error".to_string());
        assert_eq!(task.error(), Some("test error".to_string()));
    }
}

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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Global counter for generating unique ACP task IDs.
pub static NEXT_ACP_TASK_ID: AtomicUsize = AtomicUsize::new(1);

/// Registry of active ACP subagent tasks.
pub static ACP_TASKS: LazyLock<Mutex<std::collections::HashMap<String, AcpTask>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Represents an active ACP subagent task.
pub struct AcpTask {
    /// Whether the task has completed.
    pub completed: Arc<AtomicBool>,

    /// Accumulated output from the subagent.
    pub output_buffer: Arc<Mutex<String>>,

    /// The child process handle.
    pub child: Option<Child>,

    /// Channel to signal cancellation.
    #[allow(dead_code)]
    pub cancel_tx: Option<mpsc::Sender<()>>,
}

impl AcpTask {
    fn new(child: Child, cancel_tx: mpsc::Sender<()>) -> Self {
        Self {
            completed: Arc::new(AtomicBool::new(false)),
            output_buffer: Arc::new(Mutex::new(String::new())),
            child: Some(child),
            cancel_tx: Some(cancel_tx),
        }
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
                let text = match content {
                    acp::ContentBlock::Text(text_content) => text_content.text,
                    acp::ContentBlock::Image(_) => "[image]".into(),
                    acp::ContentBlock::Audio(_) => "[audio]".into(),
                    acp::ContentBlock::ResourceLink(link) => format!("[link: {}]", link.uri),
                    acp::ContentBlock::Resource(_) => "[resource]".into(),
                    _ => "[unknown content]".into(),
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
            _ => {}
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
    let task_id = NEXT_ACP_TASK_ID.fetch_add(1, Ordering::SeqCst).to_string();

    // Get clemini executable path
    let (cmd, mut cmd_args) = get_clemini_command();
    cmd_args.extend([
        "--acp-server".to_string(),
        "--cwd".to_string(),
        cwd.to_string_lossy().to_string(),
    ]);

    // Spawn the subprocess
    let mut child = Command::new(&cmd)
        .args(&cmd_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
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

    let output_buffer = Arc::new(Mutex::new(String::new()));
    let completed = Arc::new(AtomicBool::new(false));

    let client = SubagentClient {
        output_buffer: output_buffer.clone(),
        completed: completed.clone(),
    };

    let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);

    if background {
        // Background mode: register task and return immediately
        let task = AcpTask::new(child, cancel_tx);
        let task_output = task.output_buffer.clone();
        let task_completed = task.completed.clone();

        ACP_TASKS.lock().unwrap().insert(task_id.clone(), task);

        // Spawn the ACP communication in the background
        let prompt = prompt.to_string();
        let task_id_clone = task_id.clone();
        let cwd_owned = cwd.to_path_buf();
        tokio::task::spawn_local(async move {
            let result = run_acp_session(
                client,
                stdin.compat_write(),
                stdout.compat(),
                &prompt,
                &cwd_owned,
                &mut cancel_rx,
            )
            .await;

            task_completed.store(true, Ordering::SeqCst);

            if let Err(e) = result {
                let mut buffer = task_output.lock().unwrap();
                buffer.push_str(&format!("\n[error: {}]", e));
            }

            // Clean up child from registry
            if let Some(task) = ACP_TASKS.lock().unwrap().get_mut(&task_id_clone) {
                task.child = None;
            }
        });

        Ok(SubagentResult::Background { task_id })
    } else {
        // Foreground mode: wait for completion
        let result = run_acp_session(
            client,
            stdin.compat_write(),
            stdout.compat(),
            prompt,
            cwd,
            &mut cancel_rx,
        )
        .await;

        completed.store(true, Ordering::SeqCst);

        let output = output_buffer.lock().unwrap().clone();

        match result {
            Ok(_) => Ok(SubagentResult::Completed { output }),
            Err(e) => Ok(SubagentResult::Failed {
                output,
                error: e.to_string(),
            }),
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
async fn run_acp_session<W, R>(
    client: SubagentClient,
    outgoing: W,
    incoming: R,
    prompt: &str,
    cwd: &Path,
    _cancel_rx: &mut mpsc::Receiver<()>,
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

/// Get the clemini executable path.
fn get_clemini_command() -> (String, Vec<String>) {
    // Try current executable first
    if let Ok(exe) = std::env::current_exe()
        && exe.exists()
    {
        return (exe.to_string_lossy().to_string(), vec![]);
    }
    // Fallback to cargo run - only useful during development
    (
        "cargo".to_string(),
        vec!["run".to_string(), "--quiet".to_string(), "--".to_string()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acp_task_id_increments() {
        let id1 = NEXT_ACP_TASK_ID.fetch_add(1, Ordering::SeqCst);
        let id2 = NEXT_ACP_TASK_ID.fetch_add(1, Ordering::SeqCst);
        assert_eq!(id2, id1 + 1);
    }

    #[tokio::test]
    async fn test_acp_task_initial_state() {
        let (tx, _rx) = mpsc::channel(1);
        let child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();

        let task = AcpTask::new(child, tx);

        assert!(!task.completed.load(Ordering::SeqCst));
        assert!(task.output_buffer.lock().unwrap().is_empty());
        assert!(task.child.is_some());
    }

    #[test]
    fn test_get_clemini_command() {
        let (cmd, args) = get_clemini_command();
        assert!(!cmd.is_empty());
        // If it's cargo, should have the run args
        if cmd == "cargo" {
            assert!(args.contains(&"run".to_string()));
            assert!(args.contains(&"--".to_string()));
        }
    }
}

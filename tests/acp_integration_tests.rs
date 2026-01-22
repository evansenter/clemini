//! Integration tests for ACP (Agent Client Protocol) communication.
//!
//! These tests verify that clemini can spawn subagents via ACP and communicate
//! with them correctly. They require a built clemini binary.
//!
//! # Running Tests
//!
//! ```bash
//! cargo build --release
//! cargo test --test acp_integration_tests -- --include-ignored --nocapture
//! ```

use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

/// Get the path to the clemini binary.
/// Uses the debug binary in target/debug if it exists.
fn get_clemini_binary() -> Option<String> {
    // Try debug build first (faster for development)
    let debug_path = std::env::current_dir().ok()?.join("target/debug/clemini");
    if debug_path.exists() {
        return Some(debug_path.to_string_lossy().to_string());
    }

    // Try release build
    let release_path = std::env::current_dir().ok()?.join("target/release/clemini");
    if release_path.exists() {
        return Some(release_path.to_string_lossy().to_string());
    }

    None
}

/// Test that clemini can be started in ACP server mode.
/// This is a basic smoke test to verify the binary works.
#[tokio::test]
#[ignore = "Requires built clemini binary and GEMINI_API_KEY"]
async fn test_acp_server_starts() {
    let Some(binary) = get_clemini_binary() else {
        println!("Skipping: clemini binary not found. Run 'cargo build' first.");
        return;
    };

    if std::env::var("GEMINI_API_KEY").is_err() {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    // Start clemini in ACP server mode
    let mut child = Command::new(&binary)
        .arg("--acp-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn clemini");

    // Give it a moment to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check if it's still running (hasn't crashed immediately)
    match child.try_wait() {
        Ok(None) => {
            // Process is still running - good!
            println!("clemini started in ACP server mode successfully");
        }
        Ok(Some(status)) => {
            // Process exited - read stderr to see why
            let stderr = child.stderr.take().unwrap();
            let mut reader = BufReader::new(stderr).lines();
            let mut errors = Vec::new();
            while let Ok(Some(line)) = reader.next_line().await {
                errors.push(line);
            }
            panic!(
                "clemini exited immediately with status {:?}\nStderr:\n{}",
                status,
                errors.join("\n")
            );
        }
        Err(e) => {
            panic!("Failed to check process status: {}", e);
        }
    }

    // Clean up
    let _ = child.kill().await;
}

/// Test that we can send an ACP initialize request and get a response.
/// This verifies the basic ACP protocol handshake works.
#[tokio::test]
#[ignore = "Requires built clemini binary and GEMINI_API_KEY"]
async fn test_acp_initialize() {
    let Some(binary) = get_clemini_binary() else {
        println!("Skipping: clemini binary not found. Run 'cargo build' first.");
        return;
    };

    if std::env::var("GEMINI_API_KEY").is_err() {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    use acp::Agent as AcpAgent;
    use agent_client_protocol as acp;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    // Start clemini in ACP server mode
    let mut child = Command::new(&binary)
        .arg("--acp-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn clemini");

    let stdin = child.stdin.take().expect("No stdin");
    let stdout = child.stdout.take().expect("No stdout");

    // Create a minimal client that just handles notifications
    struct TestClient;

    #[async_trait::async_trait(?Send)]
    impl acp::Client for TestClient {
        async fn request_permission(
            &self,
            _: acp::RequestPermissionRequest,
        ) -> acp::Result<acp::RequestPermissionResponse> {
            Err(acp::Error::method_not_found())
        }
        async fn write_text_file(
            &self,
            _: acp::WriteTextFileRequest,
        ) -> acp::Result<acp::WriteTextFileResponse> {
            Err(acp::Error::method_not_found())
        }
        async fn read_text_file(
            &self,
            _: acp::ReadTextFileRequest,
        ) -> acp::Result<acp::ReadTextFileResponse> {
            Err(acp::Error::method_not_found())
        }
        async fn create_terminal(
            &self,
            _: acp::CreateTerminalRequest,
        ) -> acp::Result<acp::CreateTerminalResponse> {
            Err(acp::Error::method_not_found())
        }
        async fn terminal_output(
            &self,
            _: acp::TerminalOutputRequest,
        ) -> acp::Result<acp::TerminalOutputResponse> {
            Err(acp::Error::method_not_found())
        }
        async fn release_terminal(
            &self,
            _: acp::ReleaseTerminalRequest,
        ) -> acp::Result<acp::ReleaseTerminalResponse> {
            Err(acp::Error::method_not_found())
        }
        async fn wait_for_terminal_exit(
            &self,
            _: acp::WaitForTerminalExitRequest,
        ) -> acp::Result<acp::WaitForTerminalExitResponse> {
            Err(acp::Error::method_not_found())
        }
        async fn kill_terminal_command(
            &self,
            _: acp::KillTerminalCommandRequest,
        ) -> acp::Result<acp::KillTerminalCommandResponse> {
            Err(acp::Error::method_not_found())
        }
        async fn session_notification(&self, _: acp::SessionNotification) -> acp::Result<()> {
            Ok(())
        }
        async fn ext_method(&self, _: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
            Err(acp::Error::method_not_found())
        }
        async fn ext_notification(&self, _: acp::ExtNotification) -> acp::Result<()> {
            Ok(())
        }
    }

    // Create ACP connection
    let (conn, handle_io) =
        acp::ClientSideConnection::new(TestClient, stdin.compat_write(), stdout.compat(), |fut| {
            tokio::task::spawn_local(fut);
        });

    // Spawn IO handler
    tokio::task::spawn_local(handle_io);

    // Initialize with timeout
    let init_result = timeout(
        Duration::from_secs(5),
        conn.initialize(
            acp::InitializeRequest::new(acp::ProtocolVersion::LATEST).client_info(
                acp::Implementation::new("test-client".to_string(), "0.1.0".to_string()),
            ),
        ),
    )
    .await;

    match init_result {
        Ok(Ok(response)) => {
            println!("ACP initialize successful!");
            if let Some(info) = response.agent_info {
                println!("Agent: {} v{}", info.name, info.version);
            } else {
                println!("Agent info not provided");
            }
        }
        Ok(Err(e)) => {
            panic!("ACP initialize failed: {:?}", e);
        }
        Err(_) => {
            panic!("ACP initialize timed out");
        }
    }

    // Clean up
    let _ = child.kill().await;
}

/// Test the spawn_subagent function directly.
/// This doesn't require the binary but does require API key for the subagent.
#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY and built clemini binary"]
async fn test_spawn_subagent_foreground() {
    if std::env::var("GEMINI_API_KEY").is_err() {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    if get_clemini_binary().is_none() {
        println!("Skipping: clemini binary not found. Run 'cargo build' first.");
        return;
    }

    // LocalSet is required for spawn_local used in ACP communication
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let cwd = std::env::current_dir().unwrap();

            // Spawn a simple subagent task
            let result = clemini::spawn_subagent(
                "Just say 'hello' and nothing else.",
                &cwd,
                false, // foreground
            )
            .await;

            match result {
                Ok(clemini::SubagentResult::Completed { output }) => {
                    println!("Subagent completed with output: {}", output);
                    assert!(
                        output.to_lowercase().contains("hello"),
                        "Expected 'hello' in output"
                    );
                }
                Ok(clemini::SubagentResult::Failed { output, error }) => {
                    panic!("Subagent failed: {}\nOutput: {}", error, output);
                }
                Ok(clemini::SubagentResult::Background { task_id }) => {
                    panic!(
                        "Expected foreground result, got background task_id: {}",
                        task_id
                    );
                }
                Err(e) => {
                    panic!("spawn_subagent failed: {}", e);
                }
            }
        })
        .await;
}

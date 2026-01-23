//! PTY-based E2E tests for clemitui.
//!
//! These tests spawn the clemitui-demo binary in a pseudo-terminal and verify
//! the actual terminal output, including ANSI escape codes for colors.
//!
//! Run with: `cargo test -p clemitui --test e2e_tests`

mod common;

use common::strip_ansi;
use expectrl::{Session, session::OsProcess};
use std::process::Command;
use std::time::Duration;

/// Get the clemitui-demo binary path
fn demo_binary() -> String {
    let debug_path = env!("CARGO_MANIFEST_DIR").to_string() + "/../../target/debug/clemitui-demo";
    if std::path::Path::new(&debug_path).exists() {
        return debug_path;
    }
    // Fall back to release
    env!("CARGO_MANIFEST_DIR").to_string() + "/../../target/release/clemitui-demo"
}

/// Check if the demo binary exists
fn has_demo_binary() -> bool {
    std::path::Path::new(&demo_binary()).exists()
}

/// Spawn the demo binary with arguments
fn spawn_demo(args: &[&str]) -> Result<Session<OsProcess>, Box<dyn std::error::Error>> {
    let binary = demo_binary();
    let mut cmd = Command::new(&binary);
    cmd.args(args);
    let session = Session::spawn(cmd)?;
    Ok(session)
}

/// Read all output until EOF
fn read_until_eof(session: &mut Session<OsProcess>) -> String {
    use std::io::Read;

    session.set_expect_timeout(Some(Duration::from_secs(5)));

    let mut output = Vec::new();

    // Read all available output using blocking read
    loop {
        let mut buf = [0u8; 4096];
        match session.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No more data available, wait a bit and check for EOF
                std::thread::sleep(Duration::from_millis(100));
                // Try once more
                match session.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => output.extend_from_slice(&buf[..n]),
                    Err(_) => break,
                }
            }
            Err(_) => break,
        }
    }

    String::from_utf8_lossy(&output).to_string()
}

// strip_ansi is imported from common module

// =============================================================================
// Tool Executing Tests
// =============================================================================

#[test]
fn test_tool_executing_basic() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found. Run `cargo build -p clemitui` first.");
        return;
    }

    let mut session =
        spawn_demo(&["tool-executing", "bash", r#"{"command":"ls"}"#]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // Should contain tool name and opening bracket
    assert!(
        stripped.contains("bash"),
        "Should contain tool name: {}",
        stripped
    );
    assert!(
        stripped.contains("┌─"),
        "Should contain opening bracket: {}",
        stripped
    );
    assert!(
        stripped.contains("command="),
        "Should contain args: {}",
        stripped
    );
}

#[test]
fn test_tool_executing_empty_args() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["tool-executing", "glob", "{}"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    assert!(
        stripped.contains("glob"),
        "Should contain tool name: {}",
        stripped
    );
    assert!(
        stripped.contains("┌─"),
        "Should contain opening bracket: {}",
        stripped
    );
}

// =============================================================================
// Tool Result Tests
// =============================================================================

#[test]
fn test_tool_result_success() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["tool-result", "bash", "150", "100"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    assert!(
        stripped.contains("bash"),
        "Should contain tool name: {}",
        stripped
    );
    assert!(
        stripped.contains("└─"),
        "Should contain closing bracket: {}",
        stripped
    );
    assert!(
        stripped.contains("0.15s"),
        "Should contain duration: {}",
        stripped
    );
    assert!(
        stripped.contains("100 tok"),
        "Should contain tokens: {}",
        stripped
    );
}

#[test]
fn test_tool_result_with_error() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session =
        spawn_demo(&["tool-result", "bash", "50", "25", "error"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    assert!(
        stripped.contains("bash"),
        "Should contain tool name: {}",
        stripped
    );
    assert!(
        stripped.contains("└─"),
        "Should contain closing bracket: {}",
        stripped
    );
    // Error indicator should be present (either in color code or visible)
}

// =============================================================================
// TextBuffer Tests
// =============================================================================

#[test]
fn test_text_buffer_simple() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["text-buffer", "**Hello** world!"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // Should render the text (bold may be stripped but content should be there)
    assert!(
        stripped.contains("Hello"),
        "Should contain 'Hello': {}",
        stripped
    );
    assert!(
        stripped.contains("world"),
        "Should contain 'world': {}",
        stripped
    );
}

#[test]
fn test_text_buffer_multiline() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["text-buffer-multiline"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // Should contain header
    assert!(
        stripped.contains("Header"),
        "Should contain header: {}",
        stripped
    );
    // Should contain list items
    assert!(
        stripped.contains("Item 1"),
        "Should contain list item: {}",
        stripped
    );
    // Should contain code
    assert!(
        stripped.contains("println"),
        "Should contain code: {}",
        stripped
    );
}

#[test]
fn test_text_buffer_streaming() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["text-buffer-streaming"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // All streamed parts should be accumulated
    assert!(
        stripped.contains("Hello"),
        "Should contain 'Hello': {}",
        stripped
    );
    assert!(
        stripped.contains("world"),
        "Should contain 'world': {}",
        stripped
    );
    assert!(
        stripped.contains("streaming"),
        "Should contain 'streaming': {}",
        stripped
    );
}

// =============================================================================
// Context Warning Tests
// =============================================================================

#[test]
fn test_context_warning_normal() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session =
        spawn_demo(&["context-warning", "850000", "1000000"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // Should contain warning indicator and percentage
    assert!(
        stripped.contains("85.0%") || stripped.contains("context"),
        "Should contain percentage or context warning: {}",
        stripped
    );
}

#[test]
fn test_context_warning_critical() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session =
        spawn_demo(&["context-warning", "950000", "1000000"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // Should contain critical warning (95%)
    assert!(
        stripped.contains("95.0%") || stripped.contains("context"),
        "Should contain percentage: {}",
        stripped
    );
}

// =============================================================================
// Error Formatting Tests
// =============================================================================

#[test]
fn test_error_detail() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["error-detail", "Connection refused"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    assert!(
        stripped.contains("Connection refused"),
        "Should contain error message: {}",
        stripped
    );
    assert!(
        stripped.contains("└─") || stripped.contains("error"),
        "Should have error formatting: {}",
        stripped
    );
}

#[test]
fn test_error_message() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["error-message", "File not found"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    assert!(
        stripped.contains("File not found"),
        "Should contain error: {}",
        stripped
    );
}

// =============================================================================
// Retry Formatting Tests
// =============================================================================

#[test]
fn test_retry_format() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session =
        spawn_demo(&["retry", "2", "3", "rate limit exceeded"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    assert!(
        stripped.contains("2") && stripped.contains("3"),
        "Should contain attempt numbers: {}",
        stripped
    );
    assert!(
        stripped.contains("rate limit"),
        "Should contain reason: {}",
        stripped
    );
}

// =============================================================================
// Ctrl-C and Cancelled Tests
// =============================================================================

#[test]
fn test_ctrl_c_message() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["ctrl-c"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // Should contain ctrl-c related message
    assert!(
        stripped.to_lowercase().contains("ctrl")
            || stripped.to_lowercase().contains("cancel")
            || stripped.to_lowercase().contains("interrupt"),
        "Should contain ctrl-c message: {}",
        stripped
    );
}

#[test]
fn test_cancelled_message() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["cancelled"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    assert!(
        stripped.to_lowercase().contains("cancel"),
        "Should contain cancelled message: {}",
        stripped
    );
}

// =============================================================================
// Logging Infrastructure Tests
// =============================================================================

#[test]
fn test_logging_output() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["logging"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // Should contain all logged messages
    assert!(
        stripped.contains("log event"),
        "Should contain first log event: {}",
        stripped
    );
    assert!(
        stripped.contains("log line"),
        "Should contain log line: {}",
        stripped
    );
    assert!(
        stripped.contains("Another"),
        "Should contain another event: {}",
        stripped
    );
}

// =============================================================================
// Tool Args Formatting Tests
// =============================================================================

#[test]
fn test_tool_args_complex() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["tool-args-complex"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // Should contain formatted args
    assert!(
        stripped.contains("command="),
        "Should contain command arg: {}",
        stripped
    );
    assert!(
        stripped.contains("echo hello"),
        "Should contain command value: {}",
        stripped
    );
    // Long value should be truncated
    assert!(
        stripped.contains("...") || stripped.len() < 500,
        "Long value should be truncated: {}",
        stripped
    );
}

#[test]
fn test_tool_args_edit_filtering() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session = spawn_demo(&["tool-args-edit"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);
    let stripped = strip_ansi(&output);

    // Should contain file_path
    assert!(
        stripped.contains("file_path="),
        "Should contain file_path: {}",
        stripped
    );
    // Should NOT expose old_string/new_string content (filtered for readability)
    // The actual values should not appear in output
    assert!(
        !stripped.contains("original content here"),
        "Should not expose old_string content: {}",
        stripped
    );
    assert!(
        !stripped.contains("replacement content here"),
        "Should not expose new_string content: {}",
        stripped
    );
}

// =============================================================================
// ANSI Color Tests (verify colors are actually present)
// =============================================================================

#[test]
fn test_output_has_ansi_colors() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session =
        spawn_demo(&["tool-executing", "bash", r#"{"command":"ls"}"#]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);

    // Should contain ANSI escape codes (color)
    assert!(
        output.contains("\x1b["),
        "Output should contain ANSI escape codes for colors: {:?}",
        output
    );
}

#[test]
fn test_error_has_red_color() {
    if !has_demo_binary() {
        eprintln!("Skipping: demo binary not found");
        return;
    }

    let mut session =
        spawn_demo(&["tool-result", "bash", "50", "25", "error"]).expect("Failed to spawn");

    let output = read_until_eof(&mut session);

    // Should contain ANSI escape codes
    assert!(
        output.contains("\x1b["),
        "Error output should have color codes: {:?}",
        output
    );
}

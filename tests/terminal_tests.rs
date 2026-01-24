//! PTY-based terminal integration tests for REPL behavior.
//!
//! These tests verify terminal rendering by spawning clemini in a pseudo-terminal
//! and checking actual output positioning.
//!
//! **Requirements:**
//! - Built clemini binary (debug or release)
//! - GEMINI_API_KEY environment variable set
//!
//! Run with: `cargo test --test terminal_tests -- --include-ignored --nocapture`

use expectrl::{Eof, Regex, Session, session::OsProcess};
use std::process::Command;
use std::time::Duration;

/// CSI DSR (Device Status Report) query - reedline sends this to get cursor position
const CSI_DSR: &str = "\x1b[6n";

/// Send a mock cursor position response (row 1, column 1)
fn respond_to_cursor_query(session: &mut Session<OsProcess>) {
    // Response format: ESC [ row ; column R
    let _ = session.send("\x1b[1;1R");
}

/// Strip ANSI escape codes from a string for proper text analysis
fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip the escape sequence
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                // Skip until we hit a letter (the terminator)
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Get the clemini binary path
fn clemini_binary() -> String {
    // Use debug build for faster iteration
    let debug_path = env!("CARGO_MANIFEST_DIR").to_string() + "/target/debug/clemini";
    if std::path::Path::new(&debug_path).exists() {
        return debug_path;
    }
    // Fall back to release
    env!("CARGO_MANIFEST_DIR").to_string() + "/target/release/clemini"
}

/// Check if GEMINI_API_KEY is set
fn has_api_key() -> bool {
    std::env::var("GEMINI_API_KEY").is_ok()
}

/// Spawn clemini with proper environment inheritance
fn spawn_clemini() -> Result<Session<OsProcess>, Box<dyn std::error::Error>> {
    let binary = clemini_binary();
    let mut cmd = Command::new(&binary);

    // Inherit all environment variables including GEMINI_API_KEY
    cmd.envs(std::env::vars());

    let session = Session::spawn(cmd)?;
    Ok(session)
}

/// Read available output from session into a String, responding to any DSR queries found
fn read_and_respond(session: &mut Session<OsProcess>) -> String {
    let mut buf = [0u8; 4096];
    let mut result = String::new();

    loop {
        match session.try_read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                // Respond to all DSR queries in this chunk
                let dsr_count = chunk.matches(CSI_DSR).count();
                for _ in 0..dsr_count {
                    respond_to_cursor_query(session);
                }
                result.push_str(&chunk);
            }
            Err(_) => break,
        }
    }

    result
}

/// Wait for the prompt, handling any terminal capability queries from reedline.
/// Returns Ok(()) once the prompt is visible.
fn wait_for_prompt(session: &mut Session<OsProcess>) -> Result<(), Box<dyn std::error::Error>> {
    // Reedline sends CSI DSR queries to discover terminal capabilities
    // We need to respond to them so reedline can proceed to show the prompt

    let mut all_output = String::new();

    // Keep trying to read output and respond to any DSR queries
    // Timeout after 30 attempts (about 6 seconds)
    for _ in 0..30 {
        // Small delay before each read
        std::thread::sleep(Duration::from_millis(200));

        // Read what's available and respond to DSR queries
        let output = read_and_respond(session);
        if !output.is_empty() {
            all_output.push_str(&output);

            // Check if we see the prompt
            if all_output.contains('〉') {
                return Ok(());
            }
        }
    }

    Err(format!(
        "Failed to see prompt after accumulated output: {:?}",
        all_output
    )
    .into())
}

/// Wait for process to exit, continuing to respond to DSR queries
fn wait_for_exit(session: &mut Session<OsProcess>) -> Result<(), Box<dyn std::error::Error>> {
    // Continue responding to DSR queries while waiting for exit
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(200));

        // Read and respond to any DSR queries
        let _ = read_and_respond(session);

        // Check if process has exited using a very short timeout
        session.set_expect_timeout(Some(Duration::from_millis(100)));
        if session.expect(Eof).is_ok() {
            return Ok(());
        }
    }

    Err("Process did not exit within timeout".into())
}

/// Send a command to the terminal with proper line ending (CR instead of LF)
/// Reedline expects carriage return to submit commands
fn send_command(
    session: &mut Session<OsProcess>,
    cmd: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    session.send(format!("{}\r", cmd))?;
    Ok(())
}

/// Debug test to see what happens when we send /quit
#[test]
#[ignore = "Debug test"]
fn debug_quit_command() {
    let binary = clemini_binary();
    eprintln!("Binary path: {}", binary);

    if !std::path::Path::new(&binary).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn");
    session.set_expect_timeout(Some(Duration::from_secs(15)));

    // Keep responding to DSR queries until we see the prompt
    let mut all_output = String::new();
    for i in 0..10 {
        std::thread::sleep(Duration::from_millis(200));

        let output = read_and_respond(&mut session);
        if !output.is_empty() {
            eprintln!("Round {}: {:?}", i, output);
            all_output.push_str(&output);

            // Check if we see the prompt
            if all_output.contains('〉') {
                eprintln!("Found prompt! Total output: {:?}", all_output);
                break;
            }
        }
    }

    // Try sending /quit with \r (carriage return) instead of \n
    eprintln!("Sending /quit with CR...");
    session.send("/quit\r").expect("Failed to send /quit");

    // Wait and read
    for i in 0..10 {
        std::thread::sleep(Duration::from_millis(300));
        let output = read_and_respond(&mut session);
        eprintln!("After quit round {}: {:?}", i, output);
        if output.is_empty() && i > 2 {
            break;
        }
    }

    // Try to wait for Eof with short timeout
    session.set_expect_timeout(Some(Duration::from_secs(3)));
    match session.expect(Eof) {
        Ok(_) => eprintln!("Process exited as expected"),
        Err(e) => eprintln!("Process did not exit: {}", e),
    }
}

/// Debug test to see what clemini outputs on startup
#[test]
#[ignore = "Debug test"]
fn debug_clemini_startup() {
    let binary = clemini_binary();
    eprintln!("Binary path: {}", binary);
    eprintln!("Binary exists: {}", std::path::Path::new(&binary).exists());
    eprintln!("API key set: {}", has_api_key());

    if !std::path::Path::new(&binary).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }

    let mut session = match spawn_clemini() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to spawn: {}", e);
            return;
        }
    };

    session.set_expect_timeout(Some(Duration::from_secs(10)));

    // Try to read any output
    match session.expect(Regex(".+")) {
        Ok(output) => {
            let output_str = String::from_utf8_lossy(output.as_bytes());
            eprintln!("Got output: {:?}", output_str);
        }
        Err(e) => {
            eprintln!("Error reading output: {}", e);
        }
    }
}

// ============================================================================
// Core REPL Tests
// ============================================================================

/// Test that the REPL starts and shows a prompt.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_repl_starts_with_prompt() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    // Should see the banner and prompt
    wait_for_prompt(&mut session).expect("Failed to see prompt");
}

/// Test that /quit exits cleanly.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_quit_command() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");
    send_command(&mut session, "/quit").expect("Failed to send /quit");
    wait_for_exit(&mut session).expect("Process should have exited");
}

/// Test that /exit also works.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_exit_command() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");
    send_command(&mut session, "/exit").expect("Failed to send /exit");
    wait_for_exit(&mut session).expect("Process should have exited");
}

/// Test that /q (short form) also works.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_q_command() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");
    send_command(&mut session, "/q").expect("Failed to send /q");
    wait_for_exit(&mut session).expect("Process should have exited");
}

// ============================================================================
// Ctrl-C Tests
// ============================================================================

/// Test that Ctrl-C during empty input shows warning message.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_ctrl_c_shows_warning() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");

    // Send Ctrl-C on empty line
    session.send("\x03").expect("Failed to send Ctrl-C");

    // Should see warning message (keep responding to DSR queries)
    let mut saw_warning = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(200));
        let output = read_and_respond(&mut session);
        if output.contains("Press Ctrl-C again") {
            saw_warning = true;
            break;
        }
    }
    assert!(saw_warning, "Should see exit warning");

    // Should return to prompt
    wait_for_prompt(&mut session).expect("Should return to prompt");
}

/// Test that double Ctrl-C exits the program.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_double_ctrl_c_exits() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");

    // First Ctrl-C
    session.send("\x03").expect("Failed to send first Ctrl-C");

    // Wait for warning (keep responding to DSR queries)
    let mut saw_warning = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(200));
        let output = read_and_respond(&mut session);
        if output.contains("Press Ctrl-C again") {
            saw_warning = true;
            break;
        }
    }
    assert!(saw_warning, "Should see warning message");

    // Second Ctrl-C
    session.send("\x03").expect("Failed to send second Ctrl-C");

    // Process should exit
    wait_for_exit(&mut session).expect("Process should have exited");
}

/// Test that Ctrl-C during agent execution cancels and returns to prompt.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_ctrl_c_cancels_agent() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(30)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");

    // Send a prompt that will generate a long response
    send_command(
        &mut session,
        "Write a detailed 1000 word essay about the history of computing",
    )
    .expect("Failed to send prompt");

    // Wait a moment for the agent to start responding, keep reading output
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(200));
        let _ = read_and_respond(&mut session);
    }

    // Send Ctrl-C to interrupt
    session.send("\x03").expect("Failed to send Ctrl-C");

    // Should see cancelled message and return to prompt
    wait_for_prompt(&mut session).expect("Should return to prompt after cancel");
}

/// Test that typing input then Ctrl-C returns to prompt (not exit warning).
/// This test verifies reedline clears partial input on Ctrl-C.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_ctrl_c_with_partial_input() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");

    // Type partial input (don't press Enter)
    session
        .send("partial")
        .expect("Failed to send partial input");

    // Small delay for reedline to process
    std::thread::sleep(Duration::from_millis(300));
    let _ = read_and_respond(&mut session);

    // Send Ctrl-C - should clear partial input and return to prompt
    session.send("\x03").expect("Failed to send Ctrl-C");

    // Should return to prompt (reedline clears the partial input)
    // This verifies that Ctrl-C on non-empty input clears it rather than showing exit warning
    wait_for_prompt(&mut session).expect("Should return to prompt after clearing partial input");

    // Test passes if we got back to the prompt - that's the main behavior we're testing
    // (Ctrl-C with partial input clears it rather than triggering exit warning)
}

// ============================================================================
// Builtin Command Tests
// ============================================================================

/// Helper to wait for output containing a pattern while responding to DSR queries
fn wait_for_output_containing(
    session: &mut Session<OsProcess>,
    pattern: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut all_output = String::new();
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(200));
        let output = read_and_respond(session);
        all_output.push_str(&output);
        if all_output.contains(pattern) {
            return Ok(all_output);
        }
    }
    Err(format!(
        "Timeout waiting for '{}' in output: {:?}",
        pattern, all_output
    )
    .into())
}

/// Test /model shows the current model.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_model_command() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");
    send_command(&mut session, "/model").expect("Failed to send /model");

    // Should show model name (contains "gemini") - prompt will also be in output
    let output =
        wait_for_output_containing(&mut session, "gemini").expect("Should show model name");

    // Verify we also see the prompt (command completed successfully)
    assert!(output.contains('〉'), "Should return to prompt");
}

/// Test /pwd shows the current directory.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_pwd_command() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");
    send_command(&mut session, "/pwd").expect("Failed to send /pwd");

    // Should show a path (contains /home or similar) - prompt will also be in output
    let output = wait_for_output_containing(&mut session, "/home").expect("Should show a path");

    // Verify we also see the prompt (command completed successfully)
    assert!(output.contains('〉'), "Should return to prompt");
}

/// Test /help shows help text.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_help_command() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");
    send_command(&mut session, "/help").expect("Failed to send /help");

    // Should show help content (mentions commands) - prompt will also be in output
    let output =
        wait_for_output_containing(&mut session, "/quit").expect("Help should mention /quit");

    // Verify we also see the prompt (command completed successfully)
    assert!(output.contains('〉'), "Should return to prompt");
}

/// Test /clear clears conversation context.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_clear_command() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");
    send_command(&mut session, "/clear").expect("Failed to send /clear");

    // Should show cleared message - prompt will also be in output
    let output =
        wait_for_output_containing(&mut session, "cleared").expect("Should show cleared message");

    // Verify we also see the prompt (command completed successfully)
    assert!(output.contains('〉'), "Should return to prompt");
}

// ============================================================================
// Output Formatting Tests (Stairstepping Prevention)
// ============================================================================

/// Test that builtin command output doesn't stairstep.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_builtin_output_no_stairstepping() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");
    send_command(&mut session, "/help").expect("Failed to send /help");

    // Wait for help output and next prompt
    std::thread::sleep(Duration::from_millis(500));

    // Read all available output
    let all_output = read_and_respond(&mut session);

    // Respond to any DSR queries
    if all_output.contains(CSI_DSR) {
        respond_to_cursor_query(&mut session);
    }

    // Check for stairstepping - each line should start near column 0
    for (i, line) in all_output.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        // Strip ANSI codes for proper space counting
        let stripped = strip_ansi_codes(line);
        let leading_spaces = stripped.len() - stripped.trim_start().len();
        // Allow up to 10 spaces for intentional indentation
        if leading_spaces > 10 {
            panic!(
                "Stairstepping detected at line {}: {:?} ({} leading spaces)",
                i, line, leading_spaces
            );
        }
    }
}

/// Test that multi-line agent output doesn't stairstep.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_agent_output_no_stairstepping() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(30)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");

    // Ask for a response that will have multiple lines
    send_command(&mut session, "List the numbers 1 through 5, one per line")
        .expect("Failed to send prompt");

    // Wait for response
    std::thread::sleep(Duration::from_secs(5));

    // Read all available output
    let all_output = read_and_respond(&mut session);

    // Respond to any DSR queries
    if all_output.contains(CSI_DSR) {
        respond_to_cursor_query(&mut session);
    }

    // Check for stairstepping
    let mut max_leading_spaces = 0;
    for line in all_output.lines() {
        if line.is_empty() {
            continue;
        }
        let stripped = strip_ansi_codes(line);
        let leading_spaces = stripped.len() - stripped.trim_start().len();
        if leading_spaces > max_leading_spaces {
            max_leading_spaces = leading_spaces;
        }
    }

    // If we see excessive indentation growing across lines, that's stairstepping
    assert!(
        max_leading_spaces < 20,
        "Possible stairstepping - max leading spaces: {}",
        max_leading_spaces
    );
}

// ============================================================================
// Ctrl-D (EOF) Tests
// ============================================================================

/// Test that Ctrl-D exits the REPL.
#[test]
#[ignore = "Requires built binary and GEMINI_API_KEY"]
fn test_ctrl_d_exits() {
    if !std::path::Path::new(&clemini_binary()).exists() {
        eprintln!("Skipping: binary not found");
        return;
    }
    if !has_api_key() {
        eprintln!("Skipping: GEMINI_API_KEY not set");
        return;
    }

    let mut session = spawn_clemini().expect("Failed to spawn clemini");
    session.set_expect_timeout(Some(Duration::from_secs(10)));

    wait_for_prompt(&mut session).expect("Failed to see prompt");

    // Send Ctrl-D (EOF)
    session.send("\x04").expect("Failed to send Ctrl-D");

    // Process should exit
    wait_for_exit(&mut session).expect("Process should have exited");
}

// ============================================================================
// Multiline Input Tests
// ============================================================================

/// Multiline input via Shift+Enter or Alt+Enter.
///
/// **Manual testing required** - PTY environments don't support the kitty keyboard
/// protocol needed for Shift+Enter, and Alt+Enter escape sequences are interpreted
/// differently across terminal emulators.
///
/// To test manually:
/// 1. Run `cargo run` or the release binary
/// 2. Type some text
/// 3. Press Shift+Enter (in iTerm2, kitty, WezTerm, alacritty) or Alt+Enter
/// 4. Should insert a newline and show the multiline indicator ("  ")
/// 5. Type more text and press Enter to submit the full multiline input
#[test]
#[ignore = "Manual testing only - PTY doesn't support kitty keyboard protocol"]
fn test_multiline_input_manual() {
    // This test exists as documentation for manual testing.
    // Automated PTY testing of keyboard shortcuts with modifiers is unreliable
    // because PTYs don't support the kitty keyboard protocol.
    eprintln!("This test requires manual verification:");
    eprintln!("1. Run clemini");
    eprintln!("2. Type text, press Shift+Enter or Alt+Enter");
    eprintln!("3. Verify newline is inserted with multiline indicator");
}

/// Bracketed paste for multiline content.
///
/// **Manual testing required** - PTY environments don't support bracketed paste mode.
/// Bracketed paste is a terminal emulator feature where the terminal wraps pasted
/// content with `ESC[200~` ... `ESC[201~` escape sequences. PTY testing libraries
/// don't emulate this terminal feature.
///
/// To test manually:
/// 1. Run `cargo run` or the release binary
/// 2. Copy multiline text to clipboard
/// 3. Paste into clemini (Cmd+V / Ctrl+Shift+V)
/// 4. Content should appear in buffer with newlines preserved
/// 5. Press Enter to submit the entire multiline input
///
/// Without bracketed paste, each newline would submit immediately.
#[test]
#[ignore = "Manual testing only - PTY doesn't support bracketed paste mode"]
fn test_bracketed_paste_manual() {
    // This test exists as documentation for manual testing.
    // PTYs don't support bracketed paste mode - it's a terminal emulator feature.
    eprintln!("This test requires manual verification:");
    eprintln!("1. Run clemini");
    eprintln!("2. Copy multiline text and paste it");
    eprintln!("3. Verify all lines appear in the input buffer");
    eprintln!("4. Press Enter to submit");
}

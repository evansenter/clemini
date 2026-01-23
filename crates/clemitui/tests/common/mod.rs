//! Shared test helpers for clemitui tests.
//!
//! This module provides common utilities used across test files to reduce
//! duplication and ensure consistent test behavior.

use clemitui::{OutputSink, TextBuffer, format_tool_executing, format_tool_result};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// =============================================================================
// ANSI Stripping
// =============================================================================

/// Strip ANSI escape codes for content verification in tests.
///
/// This allows tests to verify text content without being affected by
/// color codes or other terminal formatting.
pub fn strip_ansi(s: &str) -> String {
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

// =============================================================================
// RAII Guards
// =============================================================================

/// RAII guard that disables colored output for tests.
///
/// This ensures colors are disabled during the test and automatically
/// restored when the guard is dropped, even if the test panics.
///
/// # Example
///
/// ```ignore
/// #[test]
/// fn my_test() {
///     let _guard = DisableColors::new();
///     // ... test code with colors disabled ...
/// } // colors automatically restored here
/// ```
pub struct DisableColors;

impl DisableColors {
    /// Create a new guard that disables colored output.
    pub fn new() -> Self {
        colored::control::set_override(false);
        Self
    }
}

impl Default for DisableColors {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DisableColors {
    fn drop(&mut self) {
        colored::control::unset_override();
    }
}

/// RAII guard for logging state cleanup.
///
/// Ensures logging is disabled when the guard is dropped, preventing
/// test pollution even if a test panics.
pub struct LoggingGuard;

impl Drop for LoggingGuard {
    fn drop(&mut self) {
        clemitui::disable_logging();
    }
}

// =============================================================================
// Tool Execution Helpers
// =============================================================================

/// Format a complete tool execution block (executing + result + newline).
///
/// This combines the common pattern of formatting both the tool start
/// and completion lines, with ANSI codes stripped for testing.
///
/// # Arguments
///
/// * `name` - Tool name (e.g., "bash", "grep", "edit")
/// * `args` - Tool arguments as JSON value
/// * `duration_ms` - Execution duration in milliseconds
/// * `tokens` - Token count for the result
/// * `has_error` - Whether the tool execution resulted in an error
///
/// # Returns
///
/// A string containing the formatted tool block with ANSI codes stripped.
pub fn format_tool_block(
    name: &str,
    args: &Value,
    duration_ms: u64,
    tokens: u32,
    has_error: bool,
) -> String {
    let mut result = String::new();
    result.push_str(&strip_ansi(&format_tool_executing(name, args)));
    result.push_str(&strip_ansi(&format_tool_result(
        name,
        Duration::from_millis(duration_ms),
        tokens,
        has_error,
    )));
    result.push('\n');
    result
}

/// Flush TextBuffer content to output string with ANSI codes stripped.
///
/// This handles the common pattern of flushing a TextBuffer and appending
/// the result to an output string for verification.
pub fn flush_to_output(buffer: &mut TextBuffer, output: &mut String) {
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }
}

// =============================================================================
// Test Capture Sink
// =============================================================================

/// A test sink that captures all logged output for verification.
///
/// This implements `OutputSink` and stores all emitted messages in a
/// thread-safe vector that can be inspected after the test.
///
/// # Example
///
/// ```ignore
/// let (sink, captured) = CaptureSink::new();
/// set_output_sink(Arc::new(sink));
/// enable_logging();
///
/// log_event("test message");
///
/// let logs = captured.lock().unwrap();
/// assert!(logs.iter().any(|l| l.contains("test message")));
/// ```
pub struct CaptureSink {
    /// The captured messages, wrapped in Arc<Mutex> for thread safety.
    pub captured: Arc<Mutex<Vec<String>>>,
}

impl CaptureSink {
    /// Create a new capture sink and return both the sink and a handle
    /// to the captured messages.
    pub fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = Self {
            captured: captured.clone(),
        };
        (sink, captured)
    }
}

impl Default for CaptureSink {
    fn default() -> Self {
        Self::new().0
    }
}

impl OutputSink for CaptureSink {
    fn emit(&self, message: &str) {
        self.captured.lock().unwrap().push(message.to_string());
    }

    fn emit_line(&self, message: &str) {
        self.captured.lock().unwrap().push(message.to_string());
    }
}

// =============================================================================
// Test Assertions
// =============================================================================

/// Assert that output contains a tool execution marker.
///
/// Verifies that the output contains the opening bracket (`┌─`) followed
/// by the tool name, indicating a tool execution was formatted.
#[allow(dead_code)]
pub fn assert_has_tool_executing(output: &str, tool_name: &str) {
    assert!(
        output.contains(&format!("┌─ {}", tool_name)),
        "Output should contain tool executing marker for '{}'. Output:\n{}",
        tool_name,
        output
    );
}

/// Assert that output contains a tool result marker.
///
/// Verifies that the output contains the closing bracket (`└─`) followed
/// by the tool name, indicating a tool result was formatted.
#[allow(dead_code)]
pub fn assert_has_tool_result(output: &str, tool_name: &str) {
    assert!(
        output.contains(&format!("└─ {}", tool_name)),
        "Output should contain tool result marker for '{}'. Output:\n{}",
        tool_name,
        output
    );
}

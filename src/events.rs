//! Event handling and formatting for UI layers.
//!
//! This module is the canonical location for:
//! - `EventHandler` trait - UI implementations handle `AgentEvent`s
//! - Formatting functions - `format_tool_*`, `format_error_detail`, `format_context_warning`
//! - Streaming text rendering - `render_streaming_chunk`, `flush_streaming_buffer`
//!
//! # Design
//!
//! The agent emits `AgentEvent`s through a channel. Each UI mode implements
//! `EventHandler` to process these events appropriately:
//!
//! - `TerminalEventHandler`: For plain REPL and non-interactive modes
//! - TUI mode: Uses `AppEvent` internally (handled separately)
//!
//! All handlers use the shared formatting functions to ensure consistent output.
//!
//! # Future (#59)
//!
//! When we move to streaming-first architecture, the handler will consume
//! `Stream<Item = AgentEvent>` instead of individual events, but the trait
//! methods remain the same.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use colored::Colorize;
use serde_json::Value;
use termimad::MadSkin;

use crate::logging::log_event;

// ============================================================================
// Markdown Rendering Infrastructure
// ============================================================================

/// Termimad skin for markdown rendering. Left-aligns headers.
/// Used by streaming functions and exported for main.rs TerminalSink.
pub static SKIN: LazyLock<MadSkin> = LazyLock::new(|| {
    let mut skin = MadSkin::default();
    for h in &mut skin.headers {
        h.align = termimad::Alignment::Left;
    }
    skin
});

/// Render text with markdown formatting but without line wrapping.
/// Uses a very large width to effectively disable termimad's wrapping.
pub fn text_nowrap(text: &str) -> String {
    use termimad::FmtText;
    FmtText::from(&SKIN, text, Some(10000)).to_string()
}

/// Buffer for streaming text - accumulates until newlines, then renders with markdown
static STREAMING_BUFFER: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));

// ============================================================================
// Unified Streaming Text Rendering
// ============================================================================
//
// These functions provide a unified approach to streaming text rendering.
// All UI modes (Terminal, TUI, MCP) use these to ensure consistent behavior:
//
// 1. render_streaming_chunk() - Buffer text, render complete lines with markdown
// 2. flush_streaming_buffer() - Flush remaining text at end of stream
// 3. write_to_streaming_log() - Write rendered text to log files
//
// Usage pattern in EventHandler.on_text_delta():
//   if let Some(rendered) = render_streaming_chunk(text) {
//       // Display rendered text (mode-specific: print, channel, etc.)
//       write_to_streaming_log(&rendered);
//   }

/// Split text at the last newline, returning (complete_lines, remainder).
/// Returns None if there's no newline in the text.
fn split_at_last_newline(text: &str) -> Option<(&str, &str)> {
    text.rfind('\n').map(|pos| {
        let complete = &text[..=pos];
        let remaining = &text[pos + 1..];
        (complete, remaining)
    })
}

/// Write streaming text directly to a log file (no newline added).
fn write_streaming_to_log_file(path: impl Into<PathBuf>, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.into())?;
    write!(file, "{}", text)?;
    Ok(())
}

/// Buffer streaming text and render complete lines with markdown.
/// Returns rendered text for complete lines, or None if still buffering.
/// Call `flush_streaming_buffer()` when streaming completes.
pub fn render_streaming_chunk(text: &str) -> Option<String> {
    let Ok(mut buffer) = STREAMING_BUFFER.lock() else {
        return None;
    };

    buffer.push_str(text);

    // Find the last newline - everything before it can be rendered
    let (complete, remaining) = split_at_last_newline(&buffer)?;
    let complete = complete.to_string();
    let remaining = remaining.to_string();
    *buffer = remaining;

    // Render complete lines with markdown
    colored::control::set_override(true);
    Some(text_nowrap(&complete))
}

/// Flush any remaining buffered streaming text with markdown rendering.
/// Returns rendered text, or None if buffer was empty.
/// Call this when streaming is complete (e.g., before tool execution or on_complete).
/// Output is normalized to end with exactly `\n\n` for consistent spacing.
pub fn flush_streaming_buffer() -> Option<String> {
    let Ok(mut buffer) = STREAMING_BUFFER.lock() else {
        return None;
    };

    if buffer.is_empty() {
        return None;
    }

    let text = std::mem::take(&mut *buffer);

    // Render with markdown
    colored::control::set_override(true);
    let rendered = text_nowrap(&text);

    // Normalize trailing newlines to exactly \n\n
    let trimmed = rendered.trim_end_matches('\n');
    if trimmed.is_empty() {
        None
    } else {
        Some(format!("{}\n\n", trimmed))
    }
}

/// Write rendered streaming text to log files.
/// Used by EventHandlers after rendering with `render_streaming_chunk()`.
pub fn write_to_streaming_log(rendered: &str) {
    // Skip logging during tests unless explicitly enabled
    if !crate::logging::is_logging_enabled() {
        return;
    }

    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".clemini/logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let today = chrono::Local::now().format("%Y-%m-%d");
    let log_path = log_dir.join(format!("clemini.log.{}", today));

    let _ = write_streaming_to_log_file(&log_path, rendered);

    if let Ok(path) = std::env::var("CLEMINI_LOG") {
        let _ = write_streaming_to_log_file(PathBuf::from(path), rendered);
    }
}

// ============================================================================
// Formatting helpers (UI concerns, used by EventHandler implementations)
// ============================================================================

/// Format function call arguments for display.
pub fn format_tool_args(tool_name: &str, args: &Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };

    let mut parts = Vec::new();
    for (k, v) in obj {
        // Skip large strings for the edit tool as they are shown in the diff
        if tool_name == "edit" && (k == "old_string" || k == "new_string") {
            continue;
        }
        // Skip todos for todo_write as they are rendered below
        if tool_name == "todo_write" && k == "todos" {
            continue;
        }
        // Skip question/options for ask_user as they are rendered below
        if tool_name == "ask_user" && (k == "question" || k == "options") {
            continue;
        }

        let val_str = match v {
            Value::String(s) => {
                let trimmed = s.replace('\n', " ");
                if trimmed.len() > 80 {
                    format!("\"{}...\"", &trimmed[..77])
                } else {
                    format!("\"{trimmed}\"")
                }
            }
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".to_string(),
            _ => "...".to_string(),
        };
        parts.push(format!("{k}={val_str}"));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("{} ", parts.join(" "))
    }
}

/// Format tool executing line for display.
pub fn format_tool_executing(name: &str, args: &Value) -> String {
    let args_str = format_tool_args(name, args);
    format!("┌─ {} {}", name.cyan(), args_str)
}

/// Rough token estimate: ~4 chars per token.
pub fn estimate_tokens(value: &Value) -> u32 {
    (value.to_string().len() / 4) as u32
}

/// Format tool result for display.
pub fn format_tool_result(
    name: &str,
    duration: Duration,
    estimated_tokens: u32,
    has_error: bool,
) -> String {
    let error_suffix = if has_error {
        " ERROR".bright_red().bold().to_string()
    } else {
        String::new()
    };
    let elapsed_secs = duration.as_secs_f32();

    let duration_str = if elapsed_secs < 0.001 {
        format!("{:.3}s", elapsed_secs)
    } else {
        format!("{:.2}s", elapsed_secs)
    };

    format!(
        "└─ {} {} ~{} tok{}",
        name.cyan(),
        duration_str.yellow(),
        estimated_tokens,
        error_suffix
    )
}

/// Format context warning message.
pub fn format_context_warning(percentage: f64) -> String {
    if percentage > 95.0 {
        format!(
            "WARNING: Context window at {:.1}%. Use /clear to reset.",
            percentage
        )
    } else {
        format!("WARNING: Context window at {:.1}%.", percentage)
    }
}

/// Format error detail line for display (shown below tool result on error).
pub fn format_error_detail(error_message: &str) -> String {
    format!("  └─ error: {}", error_message.dimmed())
}

/// Handler for agent events. UI modes implement this to process events.
pub trait EventHandler {
    /// Handle streaming text (should append to current line, not create new line).
    fn on_text_delta(&mut self, text: &str);

    /// Handle tool starting execution.
    fn on_tool_executing(&mut self, name: &str, args: &Value);

    /// Handle tool completion.
    fn on_tool_result(
        &mut self,
        name: &str,
        duration: Duration,
        tokens: u32,
        has_error: bool,
        error_message: Option<&str>,
    );

    /// Handle context window warning.
    fn on_context_warning(&mut self, percentage: f64);

    /// Handle interaction complete (optional, default no-op).
    fn on_complete(&mut self) {}

    /// Handle cancellation (optional, default no-op).
    fn on_cancelled(&mut self) {}

    /// Handle tool output (emitted by tools for visual display).
    /// Default implementation logs the output.
    fn on_tool_output(&mut self, output: &str) {
        // Tool output is pre-formatted with ANSI codes, skip markdown rendering
        crate::logging::log_event_raw(output);
    }
}

/// Event handler for terminal output (plain REPL and non-interactive modes).
pub struct TerminalEventHandler {
    stream_enabled: bool,
}

impl TerminalEventHandler {
    pub fn new(stream_enabled: bool) -> Self {
        Self { stream_enabled }
    }
}

impl EventHandler for TerminalEventHandler {
    fn on_text_delta(&mut self, text: &str) {
        // Use unified streaming: buffer, render markdown, then display + log
        if let Some(rendered) = render_streaming_chunk(text) {
            if self.stream_enabled {
                print!("{}", rendered);
                let _ = io::stdout().flush();
            }
            write_to_streaming_log(&rendered);
        }
    }

    fn on_tool_executing(&mut self, name: &str, args: &Value) {
        // Flush streaming buffer before tool output (normalizes to \n\n)
        if let Some(rendered) = flush_streaming_buffer() {
            if self.stream_enabled {
                print!("{}", rendered);
                let _ = io::stdout().flush();
            }
            write_to_streaming_log(&rendered);
        } else {
            // No buffered content - add blank line for spacing
            // (streaming may have written complete lines already)
            log_event("");
        }
        log_event(&format_tool_executing(name, args));
    }

    fn on_tool_result(
        &mut self,
        name: &str,
        duration: Duration,
        tokens: u32,
        has_error: bool,
        error_message: Option<&str>,
    ) {
        log_event(&format_tool_result(name, duration, tokens, has_error));
        if let Some(err_msg) = error_message {
            log_event(&format_error_detail(err_msg));
        }
        log_event(""); // Blank line after tool result
    }

    fn on_context_warning(&mut self, percentage: f64) {
        let msg = format_context_warning(percentage);
        eprintln!("{}", msg.bright_red().bold());
    }

    fn on_complete(&mut self) {
        // Flush any remaining buffered text with unified streaming (normalizes to \n\n)
        if let Some(rendered) = flush_streaming_buffer() {
            if self.stream_enabled {
                print!("{}", rendered);
                let _ = io::stdout().flush();
            }
            write_to_streaming_log(&rendered);
        } else {
            // No buffered content - add blank line for spacing before OUT
            log_event("");
        }
    }
}

/// Dispatch an AgentEvent to the appropriate handler method.
///
/// This is a convenience function that matches on the event type and calls
/// the corresponding handler method.
pub fn dispatch_event<H: EventHandler>(handler: &mut H, event: &crate::agent::AgentEvent) {
    use crate::agent::AgentEvent;

    match event {
        AgentEvent::TextDelta(text) => handler.on_text_delta(text),
        AgentEvent::ToolExecuting(calls) => {
            for call in calls {
                handler.on_tool_executing(&call.name, &call.args);
            }
        }
        AgentEvent::ToolResult(result) => {
            let tokens = estimate_tokens(&result.args) + estimate_tokens(&result.result);
            let has_error = result.is_error();
            let error_message = if has_error {
                result.error_message()
            } else {
                None
            };
            handler.on_tool_result(
                &result.name,
                result.duration,
                tokens,
                has_error,
                error_message,
            );
        }
        AgentEvent::ContextWarning { percentage, .. } => {
            handler.on_context_warning(*percentage);
        }
        AgentEvent::Complete { .. } => handler.on_complete(),
        AgentEvent::Cancelled => handler.on_cancelled(),
        AgentEvent::ToolOutput(output) => handler.on_tool_output(output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Test handler that records all calls for verification.
    struct RecordingHandler {
        events: Rc<RefCell<Vec<String>>>,
        stream_enabled: bool,
    }

    impl RecordingHandler {
        fn new(stream_enabled: bool) -> (Self, Rc<RefCell<Vec<String>>>) {
            let events = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    events: events.clone(),
                    stream_enabled,
                },
                events,
            )
        }
    }

    impl EventHandler for RecordingHandler {
        fn on_text_delta(&mut self, text: &str) {
            if self.stream_enabled {
                self.events
                    .borrow_mut()
                    .push(format!("text_delta:{}", text));
            }
        }

        fn on_tool_executing(&mut self, name: &str, args: &Value) {
            self.events
                .borrow_mut()
                .push(format!("tool_executing:{}:{}", name, args));
        }

        fn on_tool_result(
            &mut self,
            name: &str,
            duration: Duration,
            tokens: u32,
            has_error: bool,
            error_message: Option<&str>,
        ) {
            self.events.borrow_mut().push(format!(
                "tool_result:{}:{}ms:{}tok:error={}:{}",
                name,
                duration.as_millis(),
                tokens,
                has_error,
                error_message.unwrap_or("")
            ));
        }

        fn on_context_warning(&mut self, percentage: f64) {
            self.events
                .borrow_mut()
                .push(format!("context_warning:{:.1}", percentage));
        }

        fn on_complete(&mut self) {
            self.events.borrow_mut().push("complete".to_string());
        }

        fn on_cancelled(&mut self) {
            self.events.borrow_mut().push("cancelled".to_string());
        }

        fn on_tool_output(&mut self, output: &str) {
            self.events
                .borrow_mut()
                .push(format!("tool_output:{}", output));
        }
    }

    // =========================================
    // EventHandler contract tests
    // =========================================

    #[test]
    fn test_text_delta_respects_stream_enabled() {
        // With streaming enabled
        let (mut handler, events) = RecordingHandler::new(true);
        handler.on_text_delta("Hello");
        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("text_delta:Hello"));

        // With streaming disabled
        let (mut handler, events) = RecordingHandler::new(false);
        handler.on_text_delta("Hello");
        assert_eq!(events.borrow().len(), 0); // No event recorded
    }

    #[test]
    fn test_tool_executing_records_name_and_args() {
        let (mut handler, events) = RecordingHandler::new(true);
        let args = serde_json::json!({"path": "test.rs"});
        handler.on_tool_executing("read_file", &args);

        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("tool_executing:read_file"));
        assert!(events.borrow()[0].contains("test.rs"));
    }

    #[test]
    fn test_tool_result_records_all_fields() {
        let (mut handler, events) = RecordingHandler::new(true);
        handler.on_tool_result("write_file", Duration::from_millis(50), 100, false, None);

        assert_eq!(events.borrow().len(), 1);
        let event = &events.borrow()[0];
        assert!(event.contains("tool_result:write_file"));
        assert!(event.contains("50ms"));
        assert!(event.contains("100tok"));
        assert!(event.contains("error=false"));
    }

    #[test]
    fn test_tool_result_with_error() {
        let (mut handler, events) = RecordingHandler::new(true);
        handler.on_tool_result(
            "bash",
            Duration::from_millis(10),
            25,
            true,
            Some("permission denied"),
        );

        assert_eq!(events.borrow().len(), 1);
        let event = &events.borrow()[0];
        assert!(event.contains("error=true"));
        assert!(event.contains("permission denied"));
    }

    #[test]
    fn test_context_warning_records_percentage() {
        let (mut handler, events) = RecordingHandler::new(true);
        handler.on_context_warning(85.5);

        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("context_warning:85.5"));
    }

    // =========================================
    // dispatch_event tests
    // =========================================

    #[test]
    fn test_dispatch_text_delta() {
        use crate::agent::AgentEvent;

        let (mut handler, events) = RecordingHandler::new(true);
        let event = AgentEvent::TextDelta("Hello world".to_string());
        dispatch_event(&mut handler, &event);

        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("text_delta:Hello world"));
    }

    #[test]
    fn test_dispatch_tool_executing() {
        use crate::agent::AgentEvent;
        use genai_rs::OwnedFunctionCallInfo;

        let (mut handler, events) = RecordingHandler::new(true);
        let call = OwnedFunctionCallInfo {
            name: "grep".to_string(),
            id: Some("123".to_string()),
            args: serde_json::json!({"pattern": "test"}),
        };
        let event = AgentEvent::ToolExecuting(vec![call]);
        dispatch_event(&mut handler, &event);

        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("tool_executing:grep"));
    }

    #[test]
    fn test_dispatch_context_warning() {
        use crate::agent::AgentEvent;

        let (mut handler, events) = RecordingHandler::new(true);
        let event = AgentEvent::ContextWarning {
            used: 900_000,
            limit: 1_000_000,
            percentage: 90.0,
        };
        dispatch_event(&mut handler, &event);

        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("context_warning:90.0"));
    }

    #[test]
    fn test_dispatch_complete() {
        // Test that on_complete is called - we verify via the handler trait method
        // without needing to construct a full InteractionResponse
        let (mut handler, events) = RecordingHandler::new(true);
        handler.on_complete();

        assert_eq!(events.borrow().len(), 1);
        assert_eq!(events.borrow()[0], "complete");
    }

    #[test]
    fn test_dispatch_cancelled() {
        use crate::agent::AgentEvent;

        let (mut handler, events) = RecordingHandler::new(true);
        let event = AgentEvent::Cancelled;
        dispatch_event(&mut handler, &event);

        assert_eq!(events.borrow().len(), 1);
        assert_eq!(events.borrow()[0], "cancelled");
    }

    // =========================================
    // Full flow tests
    // =========================================

    #[test]
    fn test_typical_interaction_flow() {
        use crate::agent::AgentEvent;
        use genai_rs::{FunctionExecutionResult, OwnedFunctionCallInfo};

        let (mut handler, events) = RecordingHandler::new(true);

        // Text delta (streaming)
        dispatch_event(
            &mut handler,
            &AgentEvent::TextDelta("I'll search.\n".to_string()),
        );

        // Tool executing
        let call = OwnedFunctionCallInfo {
            name: "grep".to_string(),
            id: Some("1".to_string()),
            args: serde_json::json!({"pattern": "fn main"}),
        };
        dispatch_event(&mut handler, &AgentEvent::ToolExecuting(vec![call]));

        // Tool result
        let result = FunctionExecutionResult::new(
            "grep".to_string(),
            "1".to_string(),
            serde_json::json!({"pattern": "fn main"}),
            serde_json::json!({"matches": ["src/main.rs:1"]}),
            Duration::from_millis(25),
        );
        dispatch_event(&mut handler, &AgentEvent::ToolResult(result));

        // More text
        dispatch_event(
            &mut handler,
            &AgentEvent::TextDelta("Found it!".to_string()),
        );

        // Complete (call directly to avoid constructing InteractionResponse)
        handler.on_complete();

        // Verify flow
        let events = events.borrow();
        assert_eq!(events.len(), 5);
        assert!(events[0].contains("text_delta"));
        assert!(events[1].contains("tool_executing"));
        assert!(events[2].contains("tool_result"));
        assert!(events[3].contains("text_delta"));
        assert_eq!(events[4], "complete");
    }

    // =========================================
    // Format helper tests
    // =========================================

    #[test]
    fn test_format_tool_args_empty() {
        assert_eq!(format_tool_args("test", &serde_json::json!({})), "");
        assert_eq!(format_tool_args("test", &serde_json::json!(null)), "");
        assert_eq!(
            format_tool_args("test", &serde_json::json!("not an object")),
            ""
        );
    }

    #[test]
    fn test_format_tool_args_types() {
        let args = serde_json::json!({
            "bool": true,
            "num": 42,
            "null": null,
            "str": "hello"
        });
        let formatted = format_tool_args("test", &args);
        // serde_json::Map is sorted by key
        assert_eq!(formatted, "bool=true null=null num=42 str=\"hello\" ");
    }

    #[test]
    fn test_format_tool_args_complex_types() {
        let args = serde_json::json!({
            "arr": [1, 2],
            "obj": {"a": 1}
        });
        let formatted = format_tool_args("test", &args);
        assert_eq!(formatted, "arr=... obj=... ");
    }

    #[test]
    fn test_format_tool_args_truncation() {
        let long_str = "a".repeat(100);
        let args = serde_json::json!({"long": long_str});
        let formatted = format_tool_args("test", &args);
        let expected_val = format!("\"{}...\"", "a".repeat(77));
        assert_eq!(formatted, format!("long={} ", expected_val));
    }

    #[test]
    fn test_format_tool_args_newlines() {
        let args = serde_json::json!({"text": "hello\nworld"});
        let formatted = format_tool_args("test", &args);
        assert_eq!(formatted, "text=\"hello world\" ");
    }

    #[test]
    fn test_format_tool_args_edit_filtering() {
        let args = serde_json::json!({
            "file_path": "test.rs",
            "old_string": "old content",
            "new_string": "new content"
        });
        let formatted = format_tool_args("edit", &args);
        assert_eq!(formatted, "file_path=\"test.rs\" ");
    }

    #[test]
    fn test_format_tool_args_todo_write_filtering() {
        let args = serde_json::json!({
            "todos": [
                {"content": "Task 1", "status": "pending"},
                {"content": "Task 2", "status": "completed"}
            ]
        });
        let formatted = format_tool_args("todo_write", &args);
        // todos should be filtered out since they're rendered below
        assert_eq!(formatted, "");
    }

    #[test]
    fn test_format_tool_args_ask_user_filtering() {
        let args = serde_json::json!({
            "question": "What is your favorite color?",
            "options": ["red", "blue", "green"]
        });
        let formatted = format_tool_args("ask_user", &args);
        // question and options should be filtered out since they're rendered below
        assert_eq!(formatted, "");
    }

    #[test]
    fn test_estimate_tokens() {
        // ~4 chars per token
        assert_eq!(estimate_tokens(&serde_json::json!("hello")), 1); // "hello" = 7 chars / 4 = 1
        assert_eq!(estimate_tokens(&serde_json::json!({"key": "value"})), 3); // {"key":"value"} = 15 chars / 4 = 3
    }

    #[test]
    fn test_format_tool_executing_basic() {
        // Disable colors for predictable test output
        colored::control::set_override(false);
        let args = serde_json::json!({"file_path": "test.rs"});
        let formatted = format_tool_executing("read_file", &args);
        assert!(formatted.contains("┌─"));
        assert!(formatted.contains("read_file"));
        assert!(formatted.contains("file_path=\"test.rs\""));
    }

    #[test]
    fn test_format_tool_executing_empty_args() {
        colored::control::set_override(false);
        let formatted = format_tool_executing("list_files", &serde_json::json!({}));
        assert!(formatted.contains("┌─"));
        assert!(formatted.contains("list_files"));
    }

    #[test]
    fn test_dispatch_tool_result_includes_args_and_result_tokens() {
        use crate::agent::AgentEvent;
        use genai_rs::FunctionExecutionResult;

        let (mut handler, events) = RecordingHandler::new(true);

        // Create a result with known args and result sizes
        let args = serde_json::json!({"file_path": "/path/to/file.txt", "old_string": "hello", "new_string": "world"});
        let result_data = serde_json::json!({"success": true, "bytes": 100});

        let args_tokens = estimate_tokens(&args);
        let result_tokens = estimate_tokens(&result_data);
        let expected_total = args_tokens + result_tokens;

        let result = FunctionExecutionResult::new(
            "edit".to_string(),
            "call-1".to_string(),
            args,
            result_data,
            Duration::from_millis(10),
        );

        dispatch_event(&mut handler, &AgentEvent::ToolResult(result));

        // Verify the token count passed to handler includes both args AND result
        let events = events.borrow();
        assert_eq!(events.len(), 1);
        // The recording format is: "tool_result:name:duration:tokens:error:msg"
        assert!(events[0].contains(&format!("{}tok", expected_total)));
    }

    #[test]
    fn test_format_tool_result_duration() {
        colored::control::set_override(false);

        // < 1ms (100us) -> 3 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_micros(100), 10, false),
            "└─ test 0.000s ~10 tok"
        );

        // < 1ms (900us) -> 3 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_micros(900), 10, false),
            "└─ test 0.001s ~10 tok"
        );

        // >= 1ms (1.1ms) -> 2 decimals (shows 0.00s due to threshold)
        assert_eq!(
            format_tool_result("test", Duration::from_micros(1100), 10, false),
            "└─ test 0.00s ~10 tok"
        );

        // >= 1ms (20ms) -> 2 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_millis(20), 10, false),
            "└─ test 0.02s ~10 tok"
        );

        // >= 1ms (1450ms) -> 2 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_millis(1450), 10, false),
            "└─ test 1.45s ~10 tok"
        );

        colored::control::unset_override();
    }

    #[test]
    fn test_format_tool_result_error() {
        colored::control::set_override(false);

        let res = format_tool_result("test", Duration::from_millis(10), 25, true);
        assert_eq!(res, "└─ test 0.01s ~25 tok ERROR");

        let res = format_tool_result("test", Duration::from_millis(10), 25, false);
        assert_eq!(res, "└─ test 0.01s ~25 tok");

        colored::control::unset_override();
    }

    #[test]
    fn test_format_context_warning_normal() {
        let msg = format_context_warning(85.0);
        assert!(msg.contains("85.0%"));
        assert!(!msg.contains("/clear"));
    }

    #[test]
    fn test_format_context_warning_critical() {
        let msg = format_context_warning(96.0);
        assert!(msg.contains("96.0%"));
        assert!(msg.contains("/clear"));
    }

    #[test]
    fn test_format_context_warning_boundary() {
        // Exactly 95% - not critical
        let msg = format_context_warning(95.0);
        assert!(!msg.contains("/clear"));

        // Just over 95% - critical
        let msg = format_context_warning(95.1);
        assert!(msg.contains("/clear"));
    }

    #[test]
    fn test_format_error_detail() {
        colored::control::set_override(false);
        let detail = format_error_detail("permission denied");
        assert_eq!(detail, "  └─ error: permission denied");
        colored::control::unset_override();
    }

    // =========================================
    // Line buffering tests for streaming log
    // =========================================

    #[test]
    fn test_split_at_last_newline_no_newline() {
        assert!(split_at_last_newline("hello world").is_none());
    }

    #[test]
    fn test_split_at_last_newline_single_newline() {
        let (complete, remaining) = split_at_last_newline("hello\n").unwrap();
        assert_eq!(complete, "hello\n");
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_split_at_last_newline_with_remainder() {
        let (complete, remaining) = split_at_last_newline("hello\nworld").unwrap();
        assert_eq!(complete, "hello\n");
        assert_eq!(remaining, "world");
    }

    #[test]
    fn test_split_at_last_newline_multiple_lines() {
        let (complete, remaining) = split_at_last_newline("line1\nline2\npartial").unwrap();
        assert_eq!(complete, "line1\nline2\n");
        assert_eq!(remaining, "partial");
    }

    #[test]
    fn test_split_at_last_newline_ends_with_newline() {
        let (complete, remaining) = split_at_last_newline("line1\nline2\n").unwrap();
        assert_eq!(complete, "line1\nline2\n");
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_streaming_buffer_basic() {
        // Clear buffer before test
        STREAMING_BUFFER.lock().unwrap().clear();

        // No newline: should buffer and return None
        let out1 = render_streaming_chunk("Hello ");
        assert!(out1.is_none());
        assert_eq!(*STREAMING_BUFFER.lock().unwrap(), "Hello ");

        // Newline: should render up to the newline
        let out2 = render_streaming_chunk("world!\nNext line");
        let rendered = out2.unwrap();
        assert!(rendered.contains("Hello world!"));
        assert!(rendered.ends_with('\n'));
        assert_eq!(*STREAMING_BUFFER.lock().unwrap(), "Next line");

        // Flush: should render remaining
        let out3 = flush_streaming_buffer();
        let flushed = out3.unwrap();
        assert!(flushed.contains("Next line"));
        assert!(STREAMING_BUFFER.lock().unwrap().is_empty());
    }

    #[test]
    fn test_streaming_multiple_lines() {
        STREAMING_BUFFER.lock().unwrap().clear();

        let out = render_streaming_chunk("Line 1\nLine 2\nPartial");
        let rendered = out.unwrap();
        assert!(rendered.contains("Line 1"));
        assert!(rendered.contains("Line 2"));
        assert_eq!(*STREAMING_BUFFER.lock().unwrap(), "Partial");

        let out2 = flush_streaming_buffer();
        assert!(out2.unwrap().contains("Partial"));
    }

    #[test]
    fn test_flush_empty_buffer() {
        STREAMING_BUFFER.lock().unwrap().clear();
        let out = flush_streaming_buffer();
        assert!(out.is_none());
    }

    #[test]
    fn test_flush_normalizes_to_double_newline() {
        // flush_streaming_buffer should normalize output to end with exactly \n\n
        // This is critical for consistent spacing before tool calls and OUT lines

        // Case 1: Text with no trailing newline -> normalized to \n\n
        STREAMING_BUFFER.lock().unwrap().clear();
        STREAMING_BUFFER.lock().unwrap().push_str("Hello world");
        let out = flush_streaming_buffer().unwrap();
        assert!(
            out.ends_with("\n\n"),
            "Should end with \\n\\n, got: {:?}",
            out
        );
        assert!(!out.ends_with("\n\n\n"), "Should not have triple newline");

        // Case 2: Text with single trailing newline -> normalized to \n\n
        STREAMING_BUFFER.lock().unwrap().clear();
        STREAMING_BUFFER.lock().unwrap().push_str("Hello world\n");
        let out = flush_streaming_buffer().unwrap();
        assert!(
            out.ends_with("\n\n"),
            "Should end with \\n\\n, got: {:?}",
            out
        );

        // Case 3: Text with double trailing newline -> stays \n\n
        STREAMING_BUFFER.lock().unwrap().clear();
        STREAMING_BUFFER.lock().unwrap().push_str("Hello world\n\n");
        let out = flush_streaming_buffer().unwrap();
        assert!(
            out.ends_with("\n\n"),
            "Should end with \\n\\n, got: {:?}",
            out
        );
        assert!(!out.ends_with("\n\n\n"), "Should not have triple newline");
    }

    #[test]
    fn test_flush_returns_none_for_whitespace_only() {
        // If buffer only contains whitespace/newlines, flush should return None
        STREAMING_BUFFER.lock().unwrap().clear();
        STREAMING_BUFFER.lock().unwrap().push_str("\n\n");
        let out = flush_streaming_buffer();
        assert!(out.is_none(), "Whitespace-only buffer should return None");
    }

    // =========================================
    // Spacing contract documentation tests
    // =========================================

    /// Documents the spacing contract for tool execution and completion.
    ///
    /// The handlers use this pattern:
    /// - If flush_streaming_buffer() returns Some -> content normalized to \n\n -> no extra blank
    /// - If flush_streaming_buffer() returns None -> add blank line manually
    ///
    /// This ensures exactly one blank line before tool calls and OUT lines regardless of
    /// whether the model text ended with a newline (rendered immediately) or not (buffered).
    #[test]
    fn test_spacing_contract_documentation() {
        // This test documents the expected behavior, not the implementation.
        // The actual handlers implement this logic.

        // Scenario 1: Model sends "I'll read the file" (no trailing newline)
        // - Text is buffered
        // - Tool starts, flush returns "I'll read the file\n\n"
        // - No extra blank line needed
        // Result: "I'll read the file\n\n┌─ read_file..."

        // Scenario 2: Model sends "I'll read the file\n" (with trailing newline)
        // - Text is rendered immediately by render_streaming_chunk
        // - Buffer is empty
        // - Tool starts, flush returns None
        // - Handler adds blank line
        // Result: "I'll read the file\n\n┌─ read_file..."

        // Both scenarios produce the same visual output: one blank line before tool.
        assert!(true, "Spacing contract documented");
    }
}

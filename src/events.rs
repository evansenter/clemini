//! Event handling and formatting for UI layers.
//!
//! This module is the canonical location for:
//! - `EventHandler` trait - UI implementations handle `AgentEvent`s
//! - Formatting functions - pure functions for formatting events
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
//! Each handler owns its own text buffer for streaming text accumulation.
//!
//! # Pure Formatters
//!
//! Type-aligned pure formatters take genai-rs types directly:
//! - `format_call()` - OwnedFunctionCallInfo → String
//! - `format_result()` - FunctionExecutionResult → String
//!
//! Lower-level formatters for individual fields:
//! - `format_tool_executing()`, `format_tool_result()`, etc.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

use colored::Colorize;
use genai_rs::{FunctionExecutionResult, OwnedFunctionCallInfo};
use serde_json::Value;
use termimad::MadSkin;

use crate::logging::{log_event, log_event_raw};

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
pub fn render_markdown_nowrap(text: &str) -> String {
    use termimad::FmtText;
    FmtText::from(&SKIN, text, Some(10000)).to_string()
}

// ============================================================================
// Text Buffer (shared across EventHandler implementations)
// ============================================================================

/// Buffer for accumulating streaming text until event boundaries.
///
/// Text is buffered via `push()` at each TextDelta event, then flushed with
/// markdown rendering at event boundaries (tool executing, complete).
/// The `flush()` method normalizes trailing newlines to exactly `\n\n`.
#[derive(Debug, Default)]
pub struct TextBuffer(String);

impl TextBuffer {
    /// Create a new empty text buffer.
    pub fn new() -> Self {
        Self(String::new())
    }

    /// Append text to the buffer.
    pub fn push(&mut self, text: &str) {
        self.0.push_str(text);
    }

    /// Flush buffered text with markdown rendering, normalized to `\n\n`.
    /// Returns rendered text, or None if buffer was empty or whitespace-only.
    pub fn flush(&mut self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }

        let text = std::mem::take(&mut self.0);
        let rendered = render_markdown_nowrap(&text);

        // Normalize trailing newlines to exactly \n\n
        let trimmed = rendered.trim_end_matches('\n');
        if trimmed.is_empty() {
            None
        } else {
            Some(format!("{}\n\n", trimmed))
        }
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ============================================================================
// File Logging
// ============================================================================

/// Write streaming text directly to a log file (no newline added).
fn write_streaming_to_log_file(path: impl Into<PathBuf>, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.into())?;
    write!(file, "{}", text)?;
    Ok(())
}

/// Write rendered text to log files.
/// Used by EventHandlers after flushing with `TextBuffer::flush()`.
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

// ============================================================================
// Type-aligned pure formatters (take genai-rs types directly)
// ============================================================================

/// Pure: Format a function call for display.
/// Takes the genai-rs type directly for clean consumer API.
pub fn format_call(call: &OwnedFunctionCallInfo) -> String {
    format_tool_executing(&call.name, &call.args)
}

/// Pure: Format a function execution result for display.
/// Takes the genai-rs type directly, computing tokens internally.
pub fn format_result(result: &FunctionExecutionResult) -> String {
    let tokens = estimate_tokens(&result.args) + estimate_tokens(&result.result);
    let has_error = result.is_error();
    format_tool_result(&result.name, result.duration, tokens, has_error)
}

/// Handler for agent events. UI modes implement this to process events.
pub trait EventHandler {
    /// Handle streaming text (should append to current line, not create new line).
    fn on_text_delta(&mut self, text: &str);

    /// Handle tool starting execution.
    fn on_tool_executing(&mut self, call: &OwnedFunctionCallInfo);

    /// Handle tool completion.
    fn on_tool_result(&mut self, result: &FunctionExecutionResult);

    /// Handle context window warning.
    fn on_context_warning(&mut self, warning: &crate::agent::ContextWarning);

    /// Handle interaction complete (optional, default no-op).
    fn on_complete(&mut self, _interaction_id: Option<&str>, _response: &genai_rs::InteractionResponse) {}

    /// Handle cancellation (optional, default no-op).
    fn on_cancelled(&mut self) {}

    /// Handle tool output (emitted by tools for visual display).
    /// Default implementation is no-op; logging is handled by dispatch_event.
    fn on_tool_output(&mut self, _output: &str) {}
}

/// Event handler for terminal output (plain REPL and non-interactive modes).
pub struct TerminalEventHandler {
    stream_enabled: bool,
    text_buffer: TextBuffer,
}

impl TerminalEventHandler {
    pub fn new(stream_enabled: bool) -> Self {
        Self {
            stream_enabled,
            text_buffer: TextBuffer::new(),
        }
    }
}

impl EventHandler for TerminalEventHandler {
    fn on_text_delta(&mut self, text: &str) {
        self.text_buffer.push(text);
    }

    fn on_tool_executing(&mut self, _call: &OwnedFunctionCallInfo) {
        // Flush buffer before tool output (normalizes to \n\n for spacing)
        // Logging is handled by dispatch_event() after this method returns
        if let Some(rendered) = self.text_buffer.flush() {
            if self.stream_enabled {
                print!("{}", rendered);
                let _ = io::stdout().flush();
            }
            write_to_streaming_log(&rendered);
        }
    }

    fn on_tool_result(&mut self, _result: &FunctionExecutionResult) {
        // Logging is handled by dispatch_event() after this method returns
    }

    fn on_context_warning(&mut self, _warning: &crate::agent::ContextWarning) {
        // Logging is handled by dispatch_event() after this method returns
    }

    fn on_complete(&mut self, _interaction_id: Option<&str>, _response: &genai_rs::InteractionResponse) {
        // Flush any remaining buffered text (normalizes to \n\n)
        if let Some(rendered) = self.text_buffer.flush() {
            if self.stream_enabled {
                print!("{}", rendered);
                let _ = io::stdout().flush();
            }
            write_to_streaming_log(&rendered);
        }
    }
}

/// Dispatch an AgentEvent to the appropriate handler method.
///
/// This function handles logging centrally so handlers don't need to duplicate
/// log_event calls. The order is: handler method first (to flush buffers), then log.
pub fn dispatch_event<H: EventHandler>(handler: &mut H, event: &crate::agent::AgentEvent) {
    use crate::agent::AgentEvent;

    match event {
        AgentEvent::TextDelta(text) => handler.on_text_delta(text),
        AgentEvent::ToolExecuting(calls) => {
            for call in calls {
                handler.on_tool_executing(call);
                // Unified logging: after handler (so buffer flushes first)
                log_event(&format_call(call));
            }
        }
        AgentEvent::ToolResult(result) => {
            handler.on_tool_result(result);
            // Unified logging: after handler
            log_event(&format_result(result));
            if let Some(err_msg) = result.error_message() {
                log_event(&format_error_detail(err_msg));
            }
            log_event(""); // Blank line after tool result
        }
        AgentEvent::ContextWarning(warning) => {
            handler.on_context_warning(warning);
            // Unified logging: after handler
            log_event(&format_context_warning(warning.percentage()));
        }
        AgentEvent::Complete { interaction_id, response } => {
            handler.on_complete(interaction_id.as_deref(), response);
        }
        AgentEvent::Cancelled => handler.on_cancelled(),
        AgentEvent::ToolOutput(output) => {
            handler.on_tool_output(output);
            // Unified logging: tool output is pre-formatted with ANSI codes
            log_event_raw(output);
        }
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

        fn on_tool_executing(&mut self, call: &OwnedFunctionCallInfo) {
            self.events
                .borrow_mut()
                .push(format!("tool_executing:{}:{}", call.name, call.args));
        }

        fn on_tool_result(&mut self, result: &FunctionExecutionResult) {
            let tokens = estimate_tokens(&result.args) + estimate_tokens(&result.result);
            self.events.borrow_mut().push(format!(
                "tool_result:{}:{}ms:{}tok:error={}:{}",
                result.name,
                result.duration.as_millis(),
                tokens,
                result.is_error(),
                result.error_message().unwrap_or("")
            ));
        }

        fn on_context_warning(&mut self, warning: &crate::agent::ContextWarning) {
            self.events
                .borrow_mut()
                .push(format!("context_warning:{:.1}", warning.percentage()));
        }

        fn on_complete(&mut self, _interaction_id: Option<&str>, _response: &genai_rs::InteractionResponse) {
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
        let call = OwnedFunctionCallInfo {
            name: "read_file".to_string(),
            args: serde_json::json!({"path": "test.rs"}),
            id: None,
        };
        handler.on_tool_executing(&call);

        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("tool_executing:read_file"));
        assert!(events.borrow()[0].contains("test.rs"));
    }

    #[test]
    fn test_tool_result_records_all_fields() {
        let (mut handler, events) = RecordingHandler::new(true);
        let result = FunctionExecutionResult::new(
            "write_file".to_string(),
            "call-1".to_string(),
            serde_json::json!({}),
            serde_json::json!({"success": true}),
            Duration::from_millis(50),
        );
        handler.on_tool_result(&result);

        assert_eq!(events.borrow().len(), 1);
        let event = &events.borrow()[0];
        assert!(event.contains("tool_result:write_file"));
        assert!(event.contains("50ms"));
        assert!(event.contains("error=false"));
    }

    #[test]
    fn test_tool_result_with_error() {
        let (mut handler, events) = RecordingHandler::new(true);
        let result = FunctionExecutionResult::new(
            "bash".to_string(),
            "call-1".to_string(),
            serde_json::json!({}),
            serde_json::json!({"error": "permission denied"}),
            Duration::from_millis(10),
        );
        handler.on_tool_result(&result);

        assert_eq!(events.borrow().len(), 1);
        let event = &events.borrow()[0];
        assert!(event.contains("error=true"));
        assert!(event.contains("permission denied"));
    }

    #[test]
    fn test_context_warning_records_percentage() {
        let (mut handler, events) = RecordingHandler::new(true);
        handler.on_context_warning(&crate::agent::ContextWarning::new(855_000, 1_000_000));

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
        let event = AgentEvent::ContextWarning(crate::agent::ContextWarning::new(900_000, 1_000_000));
        dispatch_event(&mut handler, &event);

        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("context_warning:90.0"));
    }

    #[test]
    fn test_dispatch_complete() {
        use genai_rs::{InteractionResponse, InteractionStatus};

        let (mut handler, events) = RecordingHandler::new(true);
        let response = InteractionResponse {
            id: Some("test-id".to_string()),
            model: None,
            agent: None,
            input: vec![],
            outputs: vec![],
            status: InteractionStatus::Completed,
            usage: None,
            tools: None,
            grounding_metadata: None,
            url_context_metadata: None,
            previous_interaction_id: None,
            created: None,
            updated: None,
        };
        handler.on_complete(Some("test-id"), &response);

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

        // Complete
        use genai_rs::{InteractionResponse, InteractionStatus};
        let response = InteractionResponse {
            id: Some("test-id".to_string()),
            model: None,
            agent: None,
            input: vec![],
            outputs: vec![],
            status: InteractionStatus::Completed,
            usage: None,
            tools: None,
            grounding_metadata: None,
            url_context_metadata: None,
            previous_interaction_id: None,
            created: None,
            updated: None,
        };
        handler.on_complete(Some("test-id"), &response);

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
    // TextBuffer tests
    // =========================================

    #[test]
    fn test_text_buffer_accumulates() {
        let mut buffer = TextBuffer::new();

        // Buffer text chunks
        buffer.push("Hello ");
        buffer.push("world!");

        // Flush returns rendered content
        let out = buffer.flush();
        assert!(out.is_some());
        assert!(out.unwrap().contains("Hello world!"));

        // Buffer is now empty
        assert!(buffer.flush().is_none());
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_text_buffer_flush_empty() {
        let mut buffer = TextBuffer::new();
        assert!(buffer.is_empty());
        let out = buffer.flush();
        assert!(out.is_none());
    }

    #[test]
    fn test_text_buffer_flush_normalizes_to_double_newline() {
        // flush() should normalize output to end with exactly \n\n
        // This is critical for consistent spacing before tool calls

        // Case 1: Text with no trailing newline -> normalized to \n\n
        let mut buffer = TextBuffer::new();
        buffer.push("Hello world");
        let out = buffer.flush().unwrap();
        assert!(
            out.ends_with("\n\n"),
            "Should end with \\n\\n, got: {:?}",
            out
        );
        assert!(!out.ends_with("\n\n\n"), "Should not have triple newline");

        // Case 2: Text with single trailing newline -> normalized to \n\n
        let mut buffer = TextBuffer::new();
        buffer.push("Hello world\n");
        let out = buffer.flush().unwrap();
        assert!(
            out.ends_with("\n\n"),
            "Should end with \\n\\n, got: {:?}",
            out
        );

        // Case 3: Text with double trailing newline -> stays \n\n
        let mut buffer = TextBuffer::new();
        buffer.push("Hello world\n\n");
        let out = buffer.flush().unwrap();
        assert!(
            out.ends_with("\n\n"),
            "Should end with \\n\\n, got: {:?}",
            out
        );
        assert!(!out.ends_with("\n\n\n"), "Should not have triple newline");
    }

    #[test]
    fn test_text_buffer_flush_returns_none_for_whitespace_only() {
        // If buffer only contains whitespace/newlines, flush should return None
        let mut buffer = TextBuffer::new();
        buffer.push("\n\n");
        let out = buffer.flush();
        assert!(out.is_none(), "Whitespace-only buffer should return None");
    }

    #[test]
    fn test_text_buffer_default() {
        // TextBuffer implements Default
        let buffer = TextBuffer::default();
        assert!(buffer.is_empty());
    }

    // =========================================
    // EventHandler spacing tests
    // =========================================

    #[test]
    fn test_terminal_event_handler_spacing_contract() {
        // This specifically tests the spacing contract for TerminalEventHandler:
        // when text is buffered and then a tool executes, the buffer must be flushed.
        let mut handler = TerminalEventHandler::new(false); // stream disabled to avoid stdout pollution
        handler.on_text_delta("Some text");

        // At this point, text is in buffer.
        assert!(!handler.text_buffer.is_empty());

        // on_tool_executing should flush the buffer
        let call = OwnedFunctionCallInfo {
            name: "tool".to_string(),
            args: serde_json::json!({}),
            id: None,
        };
        handler.on_tool_executing(&call);

        // Buffer should be empty now
        assert!(handler.text_buffer.is_empty());
    }
}

//! Event handling for UI layers.
//!
//! This module provides the `EventHandler` trait that UI implementations use to
//! handle `AgentEvent`s. This decouples the agent from UI concerns and makes
//! event handling testable.
//!
//! # Design
//!
//! The agent emits `AgentEvent`s through a channel. Each UI mode implements
//! `EventHandler` to process these events appropriately:
//!
//! - `TerminalEventHandler`: For plain REPL and non-interactive modes
//! - TUI mode: Uses `AppEvent` internally (handled separately)
//!
//! # Future (#59)
//!
//! When we move to streaming-first architecture, the handler will consume
//! `Stream<Item = AgentEvent>` instead of individual events, but the trait
//! methods remain the same.

use std::io::{self, Write};
use std::time::Duration;

use colored::Colorize;
use serde_json::Value;

use crate::log_event;

// ============================================================================
// Formatting helpers (UI concerns, used by EventHandler implementations)
// ============================================================================

/// Format function call arguments for display.
pub fn format_tool_args(args: &Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };

    let mut parts = Vec::new();
    for (k, v) in obj {
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
        "[{}] {}, ~{} tok{}",
        name.cyan(),
        duration_str.yellow(),
        estimated_tokens,
        error_suffix
    )
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
        if self.stream_enabled {
            print!("{}", text);
            let _ = io::stdout().flush();
        }
    }

    fn on_tool_executing(&mut self, name: &str, args: &Value) {
        let args_str = format_tool_args(args);
        log_event(&format!(
            "{} {} {}",
            "🔧".dimmed(),
            name.cyan(),
            args_str.dimmed()
        ));
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
            log_event(&format!("  └─ error: {}", err_msg.dimmed()));
        }
    }

    fn on_context_warning(&mut self, percentage: f64) {
        let msg = if percentage > 95.0 {
            format!(
                "WARNING: Context window at {:.1}%. Use /clear to reset.",
                percentage
            )
        } else {
            format!("WARNING: Context window at {:.1}%.", percentage)
        };
        eprintln!("{}", msg.bright_red().bold());
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
        assert_eq!(format_tool_args(&serde_json::json!({})), "");
        assert_eq!(format_tool_args(&serde_json::json!(null)), "");
        assert_eq!(format_tool_args(&serde_json::json!("not an object")), "");
    }

    #[test]
    fn test_format_tool_args_types() {
        let args = serde_json::json!({
            "bool": true,
            "num": 42,
            "null": null,
            "str": "hello"
        });
        let formatted = format_tool_args(&args);
        // serde_json::Map is sorted by key
        assert_eq!(formatted, "bool=true null=null num=42 str=\"hello\" ");
    }

    #[test]
    fn test_format_tool_args_complex_types() {
        let args = serde_json::json!({
            "arr": [1, 2],
            "obj": {"a": 1}
        });
        let formatted = format_tool_args(&args);
        assert_eq!(formatted, "arr=... obj=... ");
    }

    #[test]
    fn test_format_tool_args_truncation() {
        let long_str = "a".repeat(100);
        let args = serde_json::json!({"long": long_str});
        let formatted = format_tool_args(&args);
        let expected_val = format!("\"{}...\"", "a".repeat(77));
        assert_eq!(formatted, format!("long={} ", expected_val));
    }

    #[test]
    fn test_format_tool_args_newlines() {
        let args = serde_json::json!({"text": "hello\nworld"});
        let formatted = format_tool_args(&args);
        assert_eq!(formatted, "text=\"hello world\" ");
    }

    #[test]
    fn test_estimate_tokens() {
        // ~4 chars per token
        assert_eq!(estimate_tokens(&serde_json::json!("hello")), 1); // "hello" = 7 chars / 4 = 1
        assert_eq!(estimate_tokens(&serde_json::json!({"key": "value"})), 3); // {"key":"value"} = 15 chars / 4 = 3
    }

    #[test]
    fn test_format_tool_result_duration() {
        colored::control::set_override(false);

        // < 1ms (100us) -> 3 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_micros(100), 10, false),
            "[test] 0.000s, ~10 tok"
        );

        // < 1ms (900us) -> 3 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_micros(900), 10, false),
            "[test] 0.001s, ~10 tok"
        );

        // >= 1ms (1.1ms) -> 2 decimals (shows 0.00s due to threshold)
        assert_eq!(
            format_tool_result("test", Duration::from_micros(1100), 10, false),
            "[test] 0.00s, ~10 tok"
        );

        // >= 1ms (20ms) -> 2 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_millis(20), 10, false),
            "[test] 0.02s, ~10 tok"
        );

        // >= 1ms (1450ms) -> 2 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_millis(1450), 10, false),
            "[test] 1.45s, ~10 tok"
        );

        colored::control::unset_override();
    }

    #[test]
    fn test_format_tool_result_error() {
        colored::control::set_override(false);

        let res = format_tool_result("test", Duration::from_millis(10), 25, true);
        assert_eq!(res, "[test] 0.01s, ~25 tok ERROR");

        let res = format_tool_result("test", Duration::from_millis(10), 25, false);
        assert_eq!(res, "[test] 0.01s, ~25 tok");

        colored::control::unset_override();
    }
}

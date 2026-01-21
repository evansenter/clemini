//! Event handling for UI layers.
//!
//! This module is the canonical location for:
//! - `EventHandler` trait - UI implementations handle `AgentEvent`s
//! - `TerminalEventHandler` - For REPL and non-interactive modes
//! - `dispatch_event()` - Central event dispatch with logging
//!
//! # Design
//!
//! The agent emits `AgentEvent`s through a channel. Each UI mode implements
//! `EventHandler` to process these events appropriately:
//!
//! - `TerminalEventHandler`: For REPL and non-interactive modes (this module)
//! - `McpEventHandler`: For MCP server mode (in mcp.rs)
//!
//! All handlers use the shared formatting functions from `crate::format`.
//! Each handler owns its own `TextBuffer` for streaming text accumulation.

use std::time::Duration;

use genai_rs::{FunctionExecutionResult, OwnedFunctionCallInfo};

use crate::logging::log_event;

// Re-export text rendering infrastructure from format module
pub use crate::format::{SKIN, TextBuffer, render_markdown_nowrap};

// ============================================================================
// EventHandler trait and implementations
// ============================================================================

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
    fn on_complete(
        &mut self,
        _interaction_id: Option<&str>,
        _response: &genai_rs::InteractionResponse,
    ) {
    }

    /// Handle cancellation (optional, default no-op).
    fn on_cancelled(&mut self) {}

    /// Handle tool output (emitted by tools for visual display).
    /// Default implementation is no-op; logging is handled by dispatch_event.
    fn on_tool_output(&mut self, _output: &str) {}

    /// Handle API retry notification.
    fn on_retry(&mut self, _attempt: u32, _max_attempts: u32, _delay: Duration, _error: &str) {}
}

/// Event handler for terminal output (plain REPL and non-interactive modes).
///
/// All text output goes through `log_event_line()` which uses the OutputSink.
/// Text is accumulated in `TextBuffer` and flushed at event boundaries.
pub struct TerminalEventHandler {
    text_buffer: TextBuffer,
    model: String,
}

impl TerminalEventHandler {
    pub fn new(model: String) -> Self {
        Self {
            text_buffer: TextBuffer::new(),
            model,
        }
    }
}

impl EventHandler for TerminalEventHandler {
    fn on_text_delta(&mut self, text: &str) {
        self.text_buffer.push(text);
    }

    fn on_tool_executing(&mut self, _call: &OwnedFunctionCallInfo) {
        // Flush buffer before tool output
        if let Some(rendered) = self.text_buffer.flush() {
            crate::logging::log_event_line(&rendered);
        }
    }

    fn on_tool_result(&mut self, _result: &FunctionExecutionResult) {
        // Logging is handled by dispatch_event() after this method returns
    }

    fn on_context_warning(&mut self, _warning: &crate::agent::ContextWarning) {
        // Logging is handled by dispatch_event() after this method returns
    }

    fn on_complete(
        &mut self,
        interaction_id: Option<&str>,
        _response: &genai_rs::InteractionResponse,
    ) {
        // Flush any remaining buffered text
        if let Some(rendered) = self.text_buffer.flush() {
            crate::logging::log_event_line(&rendered);
        }

        // Print interaction ID and model for session continuity
        if let Some(id) = interaction_id {
            crate::logging::log_event(&crate::format::format_interaction_complete(id, &self.model));
        }
    }

    fn on_retry(&mut self, _attempt: u32, _max_attempts: u32, _delay: Duration, _error: &str) {
        // Flush buffer before retry message
        if let Some(rendered) = self.text_buffer.flush() {
            crate::logging::log_event_line(&rendered);
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
                // Tool executing is start of block - no trailing blank line
                // (tool output and result will follow)
                crate::logging::log_event_line(&crate::format::format_call(call));
            }
        }
        AgentEvent::ToolResult(result) => {
            handler.on_tool_result(result);
            // Unified logging: complete visual block
            log_event(&crate::format::format_result_block(result));
        }
        AgentEvent::ContextWarning(warning) => {
            handler.on_context_warning(warning);
            // Unified logging: after handler
            log_event(&crate::format::format_context_warning(warning.percentage()));
        }
        AgentEvent::Complete {
            interaction_id,
            response,
        } => {
            handler.on_complete(interaction_id.as_deref(), response);
        }
        AgentEvent::Cancelled => handler.on_cancelled(),
        AgentEvent::ToolOutput(output) => {
            handler.on_tool_output(output);
            // Tool output lines don't get trailing blank line (they're part of a block)
            // Add newline since tool output doesn't include its own
            crate::logging::log_event_line(&format!("{}\n", output));
        }
        AgentEvent::Retry {
            attempt,
            max_attempts,
            delay,
            error,
        } => {
            handler.on_retry(*attempt, *max_attempts, *delay, error);
            crate::logging::log_event(&crate::format::format_retry(
                *attempt,
                *max_attempts,
                *delay,
                error,
            ));
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
    }

    impl RecordingHandler {
        fn new() -> (Self, Rc<RefCell<Vec<String>>>) {
            let events = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    events: events.clone(),
                },
                events,
            )
        }
    }

    /// Create a minimal InteractionResponse for testing.
    fn test_response(id: &str) -> genai_rs::InteractionResponse {
        genai_rs::InteractionResponse {
            id: Some(id.to_string()),
            model: None,
            agent: None,
            input: vec![],
            outputs: vec![],
            status: genai_rs::InteractionStatus::Completed,
            usage: None,
            tools: None,
            grounding_metadata: None,
            url_context_metadata: None,
            previous_interaction_id: None,
            created: None,
            updated: None,
        }
    }

    impl EventHandler for RecordingHandler {
        fn on_text_delta(&mut self, text: &str) {
            self.events
                .borrow_mut()
                .push(format!("text_delta:{}", text));
        }

        fn on_tool_executing(&mut self, call: &OwnedFunctionCallInfo) {
            self.events
                .borrow_mut()
                .push(format!("tool_executing:{}:{}", call.name, call.args));
        }

        fn on_tool_result(&mut self, result: &FunctionExecutionResult) {
            let tokens = crate::format::estimate_tokens(&result.args)
                + crate::format::estimate_tokens(&result.result);
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

        fn on_complete(
            &mut self,
            interaction_id: Option<&str>,
            _response: &genai_rs::InteractionResponse,
        ) {
            self.events
                .borrow_mut()
                .push(format!("complete:{}", interaction_id.unwrap_or("none")));
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
    fn test_text_delta_records_text() {
        let (mut handler, events) = RecordingHandler::new();
        handler.on_text_delta("Hello");
        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("text_delta:Hello"));
    }

    #[test]
    fn test_tool_executing_records_name_and_args() {
        let (mut handler, events) = RecordingHandler::new();
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
        let (mut handler, events) = RecordingHandler::new();
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
        let (mut handler, events) = RecordingHandler::new();
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
        let (mut handler, events) = RecordingHandler::new();
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

        let (mut handler, events) = RecordingHandler::new();
        let event = AgentEvent::TextDelta("Hello world".to_string());
        dispatch_event(&mut handler, &event);

        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("text_delta:Hello world"));
    }

    #[test]
    fn test_dispatch_tool_executing() {
        use crate::agent::AgentEvent;
        use genai_rs::OwnedFunctionCallInfo;

        let (mut handler, events) = RecordingHandler::new();
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

        let (mut handler, events) = RecordingHandler::new();
        let event =
            AgentEvent::ContextWarning(crate::agent::ContextWarning::new(900_000, 1_000_000));
        dispatch_event(&mut handler, &event);

        assert_eq!(events.borrow().len(), 1);
        assert!(events.borrow()[0].contains("context_warning:90.0"));
    }

    #[test]
    fn test_dispatch_complete() {
        let (mut handler, events) = RecordingHandler::new();
        let response = test_response("test-id");
        handler.on_complete(Some("test-id"), &response);

        assert_eq!(events.borrow().len(), 1);
        assert_eq!(events.borrow()[0], "complete:test-id");
    }

    #[test]
    fn test_dispatch_cancelled() {
        use crate::agent::AgentEvent;

        let (mut handler, events) = RecordingHandler::new();
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

        let (mut handler, events) = RecordingHandler::new();

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
        let response = test_response("test-id");
        handler.on_complete(Some("test-id"), &response);

        // Verify flow
        let events = events.borrow();
        assert_eq!(events.len(), 5);
        assert!(events[0].contains("text_delta"));
        assert!(events[1].contains("tool_executing"));
        assert!(events[2].contains("tool_result"));
        assert!(events[3].contains("text_delta"));
        assert_eq!(events[4], "complete:test-id");
    }

    #[test]
    fn test_dispatch_tool_result_includes_args_and_result_tokens() {
        use crate::agent::AgentEvent;
        use genai_rs::FunctionExecutionResult;

        let (mut handler, events) = RecordingHandler::new();

        // Create a result with known args and result sizes
        let args = serde_json::json!({"file_path": "/path/to/file.txt", "old_string": "hello", "new_string": "world"});
        let result_data = serde_json::json!({"success": true, "bytes": 100});

        let args_tokens = crate::format::estimate_tokens(&args);
        let result_tokens = crate::format::estimate_tokens(&result_data);
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

    // =========================================
    // EventHandler spacing tests
    // =========================================

    #[test]
    fn test_terminal_event_handler_spacing_contract() {
        crate::logging::disable_logging();

        // This specifically tests the spacing contract for TerminalEventHandler:
        // when text is buffered and then a tool executes, the buffer must be flushed.
        let mut handler = TerminalEventHandler::new("test-model".to_string());
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

    #[test]
    fn test_terminal_event_handler_on_complete_flushes() {
        crate::logging::disable_logging();

        let mut handler = TerminalEventHandler::new("test-model".to_string());
        handler.on_text_delta("Final thoughts");

        assert!(!handler.text_buffer.is_empty());

        let response = test_response("test-id");
        handler.on_complete(Some("test-id"), &response);

        assert!(handler.text_buffer.is_empty());
    }

    #[test]
    fn test_terminal_event_handler_on_retry_flushes() {
        crate::logging::disable_logging();

        let mut handler = TerminalEventHandler::new("test-model".to_string());
        handler.on_text_delta("Trying...");

        assert!(!handler.text_buffer.is_empty());

        handler.on_retry(1, 3, Duration::from_secs(1), "error");

        assert!(handler.text_buffer.is_empty());
    }
}

//! Terminal UI utilities for ACP-compatible AI agents.
//!
//! clemitui provides formatting and logging utilities for AI coding assistants
//! that speak the Agent Client Protocol (ACP). It handles:
//!
//! - Streaming text rendering with markdown support
//! - Tool execution display (start/result formatting)
//! - Session logging infrastructure
//!
//! This crate is intentionally minimal, providing only primitive formatting
//! functions that take simple types (strings, durations, token counts). This
//! allows it to be used by any ACP-compatible agent without model-specific
//! dependencies.
//!
//! # Modules
//!
//! - [`mod@format`] - Pure formatting functions for tool output, warnings, etc.
//! - [`logging`] - OutputSink trait and global logging infrastructure
//! - [`text_buffer`] - Streaming text accumulation with markdown rendering

pub mod format;
pub mod logging;
pub mod text_buffer;

// Re-export commonly used types
pub use format::{
    estimate_tokens, format_cancelled, format_context_warning, format_ctrl_c, format_error_detail,
    format_error_message, format_retry, format_tool_args, format_tool_executing,
    format_tool_result,
};
pub use logging::{
    OutputSink, disable_logging, enable_logging, is_logging_enabled, log_event, log_event_line,
    set_output_sink,
};
pub use text_buffer::TextBuffer;

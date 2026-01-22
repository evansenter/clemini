//! Logging infrastructure for clemini.
//!
//! This module re-exports the logging infrastructure from `clemitui`.
//! Concrete sink implementations (FileSink, TerminalSink) are provided
//! by main.rs since they have UI-specific dependencies.

// Re-export everything from clemitui::logging
pub use clemitui::logging::{
    OutputSink, disable_logging, get_output_sink, is_logging_enabled, log_event, log_event_line,
    reset_output_sink, set_output_sink,
};

//! Logging infrastructure for clemini.
//!
//! This module provides the core logging interfaces used throughout the crate.
//! Concrete sink implementations (FileSink, TerminalSink) are provided
//! by main.rs since they have UI-specific dependencies.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Flag to disable logging (opt-out). Defaults to false (logging enabled).
/// Tests can set this to true to prevent log file writes.
static LOGGING_DISABLED: AtomicBool = AtomicBool::new(false);

/// Disable logging to files. Call this in tests to prevent log writes.
pub fn disable_logging() {
    LOGGING_DISABLED.store(true, Ordering::SeqCst);
}

/// Check if logging is enabled. Returns true unless explicitly disabled via `disable_logging()`.
pub fn is_logging_enabled() -> bool {
    !LOGGING_DISABLED.load(Ordering::SeqCst)
}

/// Trait for output sinks that handle logging and display.
pub trait OutputSink: Send + Sync {
    /// Emit a complete block with trailing blank line for visual separation.
    fn emit(&self, message: &str);
    /// Emit a line without trailing blank line (for multi-line tool output).
    fn emit_line(&self, message: &str);
}

static OUTPUT_SINK: OnceLock<Arc<dyn OutputSink>> = OnceLock::new();

/// Set the global output sink. Called once at startup by main.rs.
pub fn set_output_sink(sink: Arc<dyn OutputSink>) {
    let _ = OUTPUT_SINK.set(sink);
}

/// Get the current output sink (for advanced use cases).
pub fn get_output_sink() -> Option<&'static Arc<dyn OutputSink>> {
    OUTPUT_SINK.get()
}

/// Log a complete block with trailing blank line for visual separation.
pub fn log_event(message: &str) {
    if let Some(sink) = OUTPUT_SINK.get() {
        sink.emit(message);
    }
    // No fallback - OUTPUT_SINK is always set in production before logging.
    // Skipping prevents test pollution of shared log files.
}

/// Log a line without trailing blank line (for multi-line tool output).
pub fn log_event_line(message: &str) {
    if let Some(sink) = OUTPUT_SINK.get() {
        sink.emit_line(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disable_logging() {
        // Once disabled, logging stays disabled for the test process
        disable_logging();
        assert!(!is_logging_enabled());
    }
}

//! Logging infrastructure for clemitui.
//!
//! This module provides the core logging interfaces used throughout the crate.
//! Concrete sink implementations (FileSink, TerminalSink) are provided by the
//! application using clemitui since they have environment-specific dependencies.
//!
//! # Usage
//!
//! ```no_run
//! use clemitui::{OutputSink, set_output_sink, log_event};
//! use std::sync::Arc;
//!
//! struct StdoutSink;
//!
//! impl OutputSink for StdoutSink {
//!     fn emit(&self, message: &str) {
//!         // Print message with trailing blank line for visual separation
//!         println!("{}\n", message);
//!     }
//!     fn emit_line(&self, message: &str) {
//!         // Print message without extra spacing (for continuous output)
//!         println!("{}", message);
//!     }
//! }
//!
//! // Set up the sink at application startup
//! set_output_sink(Arc::new(StdoutSink));
//!
//! // Now logging works throughout the application
//! log_event("Tool completed successfully");
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// Flag to disable logging (opt-out). Defaults to false (logging enabled).
/// Tests can set this to true to prevent log file writes.
static LOGGING_DISABLED: AtomicBool = AtomicBool::new(false);

/// Disable logging to files. Call this in tests to prevent log writes.
pub fn disable_logging() {
    LOGGING_DISABLED.store(true, Ordering::SeqCst);
}

/// Re-enable logging after it was disabled. Primarily for test cleanup.
pub fn enable_logging() {
    LOGGING_DISABLED.store(false, Ordering::SeqCst);
}

/// Check if logging is enabled. Returns true unless explicitly disabled via `disable_logging()`.
pub fn is_logging_enabled() -> bool {
    !LOGGING_DISABLED.load(Ordering::SeqCst)
}

/// Trait for output sinks that handle logging and display.
///
/// Implement this trait to control where log messages go (stdout, file, etc.).
///
/// # Methods
///
/// * `emit` - For complete blocks that should have visual separation (trailing blank line)
/// * `emit_line` - For continuous output without separation (e.g., multi-line tool output)
pub trait OutputSink: Send + Sync {
    /// Emit a complete block with trailing blank line for visual separation.
    fn emit(&self, message: &str);
    /// Emit a line without trailing blank line (for multi-line tool output).
    fn emit_line(&self, message: &str);
}

static OUTPUT_SINK: RwLock<Option<Arc<dyn OutputSink>>> = RwLock::new(None);

/// Set the global output sink. Called once at startup by main.rs.
/// Can be called multiple times (e.g., in tests) - replaces the previous sink.
pub fn set_output_sink(sink: Arc<dyn OutputSink>) {
    if let Ok(mut guard) = OUTPUT_SINK.write() {
        *guard = Some(sink);
    }
}

/// Get the current output sink (for advanced use cases).
pub fn get_output_sink() -> Option<Arc<dyn OutputSink>> {
    OUTPUT_SINK.read().ok().and_then(|guard| guard.clone())
}

/// Log a complete block with trailing blank line for visual separation.
pub fn log_event(message: &str) {
    if !is_logging_enabled() {
        return;
    }
    if let Some(sink) = get_output_sink() {
        sink.emit(message);
    }
    // No fallback - OUTPUT_SINK is always set in production before logging.
    // Skipping prevents test pollution of shared log files.
}

/// Log a line without trailing blank line (for multi-line tool output).
pub fn log_event_line(message: &str) {
    if !is_logging_enabled() {
        return;
    }
    if let Some(sink) = get_output_sink() {
        sink.emit_line(message);
    }
}

/// Reset the output sink (for testing). Clears the current sink.
pub fn reset_output_sink() {
    if let Ok(mut guard) = OUTPUT_SINK.write() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn test_disable_logging() {
        // Once disabled, logging stays disabled for the test process
        disable_logging();
        assert!(!is_logging_enabled());
    }

    /// Mock OutputSink that captures emitted messages for testing.
    struct MockSink {
        emits: Mutex<Vec<String>>,
        lines: Mutex<Vec<String>>,
    }

    impl MockSink {
        fn new() -> Self {
            Self {
                emits: Mutex::new(Vec::new()),
                lines: Mutex::new(Vec::new()),
            }
        }

        fn emits(&self) -> Vec<String> {
            self.emits.lock().unwrap().clone()
        }

        fn lines(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }
    }

    impl OutputSink for MockSink {
        fn emit(&self, message: &str) {
            self.emits.lock().unwrap().push(message.to_string());
        }

        fn emit_line(&self, message: &str) {
            self.lines.lock().unwrap().push(message.to_string());
        }
    }

    #[test]
    fn test_output_sink_set_get_reset() {
        // Reset to clean state first
        reset_output_sink();
        assert!(get_output_sink().is_none());

        // Set a sink
        let sink = Arc::new(MockSink::new());
        set_output_sink(sink.clone());
        assert!(get_output_sink().is_some());

        // Reset clears the sink
        reset_output_sink();
        assert!(get_output_sink().is_none());
    }

    #[test]
    fn test_output_sink_replacement() {
        enable_logging(); // Ensure logging is enabled (may have been disabled by other tests)
        reset_output_sink();

        // Set first sink and emit
        let sink1 = Arc::new(MockSink::new());
        set_output_sink(sink1.clone());
        log_event("message1");
        assert_eq!(sink1.emits(), vec!["message1"]);

        // Replace with second sink
        let sink2 = Arc::new(MockSink::new());
        set_output_sink(sink2.clone());
        log_event("message2");

        // First sink didn't get second message
        assert_eq!(sink1.emits(), vec!["message1"]);
        // Second sink got its message
        assert_eq!(sink2.emits(), vec!["message2"]);

        reset_output_sink();
    }

    #[test]
    fn test_log_event_routes_to_emit() {
        enable_logging(); // Ensure logging is enabled (may have been disabled by other tests)
        reset_output_sink();

        let sink = Arc::new(MockSink::new());
        set_output_sink(sink.clone());

        log_event("block message");
        assert_eq!(sink.emits(), vec!["block message"]);
        assert!(sink.lines().is_empty());

        reset_output_sink();
    }

    #[test]
    fn test_log_event_line_routes_to_emit_line() {
        enable_logging(); // Ensure logging is enabled (may have been disabled by other tests)
        reset_output_sink();

        let sink = Arc::new(MockSink::new());
        set_output_sink(sink.clone());

        log_event_line("line message");
        assert!(sink.emits().is_empty());
        assert_eq!(sink.lines(), vec!["line message"]);

        reset_output_sink();
    }

    #[test]
    fn test_log_event_noop_when_no_sink() {
        reset_output_sink();

        // Should not panic when no sink is set
        log_event("message");
        log_event_line("line");

        // Just verify no panic occurred
    }
}

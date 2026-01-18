//! Logging infrastructure for clemini.
//!
//! This module provides the core logging interfaces used throughout the crate.
//! Concrete sink implementations (FileSink, TerminalSink, TuiSink) are provided
//! by main.rs since they have UI-specific dependencies.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

static TEST_LOGGING_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable or disable logging to files during tests.
pub fn set_test_logging_enabled(enabled: bool) {
    TEST_LOGGING_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Check if logging is enabled (always true in production, controlled in tests).
pub fn is_logging_enabled() -> bool {
    if cfg!(test) {
        TEST_LOGGING_ENABLED.load(Ordering::SeqCst)
            || std::env::var("CLEMINI_ALLOW_TEST_LOGS").is_ok()
    } else {
        true
    }
}

/// Trait for output sinks that handle logging and display.
pub trait OutputSink: Send + Sync {
    /// Emit a complete message/line.
    fn emit(&self, message: &str, render_markdown: bool);
    /// Emit streaming text (no newline, no markdown). Used for model response streaming.
    fn emit_streaming(&self, text: &str);
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

/// Log to human-readable file with ANSI colors preserved.
/// Uses same naming as rolling::daily: clemini.log.YYYY-MM-DD
pub fn log_event(message: &str) {
    if let Some(sink) = OUTPUT_SINK.get() {
        sink.emit(message, true);
    }
    // No fallback - OUTPUT_SINK is always set in production before logging.
    // Skipping prevents test pollution of shared log files.
}

/// Log without markdown rendering (for protocol messages with long content).
pub fn log_event_raw(message: &str) {
    if let Some(sink) = OUTPUT_SINK.get() {
        sink.emit(message, false);
    }
    // No fallback - see log_event comment
}

/// Emit streaming text (for model response streaming).
pub fn emit_streaming(text: &str) {
    if let Some(sink) = OUTPUT_SINK.get() {
        sink.emit_streaming(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_logging_disabled_by_default() {
        let original_env = std::env::var("CLEMINI_ALLOW_TEST_LOGS");
        unsafe { std::env::remove_var("CLEMINI_ALLOW_TEST_LOGS") };

        set_test_logging_enabled(false);
        assert!(!is_logging_enabled());

        if let Ok(val) = original_env {
            unsafe { std::env::set_var("CLEMINI_ALLOW_TEST_LOGS", val) };
        }
    }

    #[test]
    #[serial]
    fn test_set_test_logging_enabled() {
        let original_env = std::env::var("CLEMINI_ALLOW_TEST_LOGS");
        unsafe { std::env::remove_var("CLEMINI_ALLOW_TEST_LOGS") };

        set_test_logging_enabled(true);
        assert!(is_logging_enabled());

        set_test_logging_enabled(false);
        assert!(!is_logging_enabled());

        if let Ok(val) = original_env {
            unsafe { std::env::set_var("CLEMINI_ALLOW_TEST_LOGS", val) };
        }
    }

    #[test]
    #[serial]
    fn test_env_var_enables_logging() {
        let original_env = std::env::var("CLEMINI_ALLOW_TEST_LOGS");

        set_test_logging_enabled(false);
        unsafe { std::env::set_var("CLEMINI_ALLOW_TEST_LOGS", "1") };
        assert!(is_logging_enabled());

        unsafe { std::env::remove_var("CLEMINI_ALLOW_TEST_LOGS") };
        assert!(!is_logging_enabled());

        if let Ok(val) = original_env {
            unsafe { std::env::set_var("CLEMINI_ALLOW_TEST_LOGS", val) };
        }
    }
}

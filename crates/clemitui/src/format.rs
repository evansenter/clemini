//! Pure formatting functions for UI output.
//!
//! All colored/styled output uses `format_*` helper functions defined here.
//! This keeps formatting testable, centralized, and out of business logic.
//!
//! # Categories
//!
//! ## Tool Output Formatters
//! - [`format_tool_executing`] - Tool start line (`┌─ name args`)
//! - [`format_tool_result`] - Tool completion line (`└─ name duration ~tokens tok`)
//! - [`format_tool_args`] - Format tool arguments as key=value pairs
//! - [`format_error_detail`] - Error detail line (indented)
//!
//! ## Other Formatters
//! - [`format_context_warning`] - Context window warnings
//! - [`format_retry`] - API retry messages

use std::time::Duration;

use colored::Colorize;
use serde_json::Value;

// ============================================================================
// Constants
// ============================================================================

/// Maximum argument display length before truncation.
const MAX_ARG_DISPLAY_LEN: usize = 80;

// ============================================================================
// Tool Argument Formatting
// ============================================================================

/// Format function call arguments for display.
///
/// Converts a JSON object of arguments into a space-separated `key=value` string.
/// Long strings are truncated, and certain tool-specific keys are filtered out
/// (e.g., `old_string`/`new_string` for edit tool).
///
/// # Example
///
/// ```
/// use clemitui::format_tool_args;
/// use serde_json::json;
///
/// let args = json!({"file_path": "test.rs", "line": 42});
/// let formatted = format_tool_args("read", &args);
/// assert!(formatted.contains("file_path="));
/// ```
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
                if trimmed.len() > MAX_ARG_DISPLAY_LEN {
                    format!("\"{}...\"", &trimmed[..MAX_ARG_DISPLAY_LEN - 3])
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

// ============================================================================
// Tool Execution Formatting
// ============================================================================

/// Format tool executing line for display.
///
/// Produces a line like `┌─ tool_name arg1=val1 arg2=val2`.
/// Includes trailing newline for use with `emit_line`.
///
/// # Example
///
/// ```
/// use clemitui::format_tool_executing;
/// use serde_json::json;
///
/// let line = format_tool_executing("read_file", &json!({"path": "test.rs"}));
/// assert!(line.contains("┌─"));
/// assert!(line.contains("read_file"));
/// ```
pub fn format_tool_executing(name: &str, args: &Value) -> String {
    let args_str = format_tool_args(name, args);
    format!("┌─ {} {}\n", name.cyan(), args_str)
}

/// Format tool result for display.
///
/// Produces a line like `└─ tool_name 0.25s ~100 tok` or with ` ERROR` suffix.
///
/// # Arguments
///
/// * `name` - Tool name
/// * `duration` - How long the tool took to execute
/// * `estimated_tokens` - Approximate token count for the result
/// * `has_error` - Whether the tool result contains an error
///
/// # Example
///
/// ```
/// use clemitui::format_tool_result;
/// use std::time::Duration;
///
/// let line = format_tool_result("bash", Duration::from_millis(250), 100, false);
/// assert!(line.contains("└─"));
/// assert!(line.contains("bash"));
/// assert!(line.contains("0.25s"));
/// ```
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

/// Format error detail line for display (shown below tool result on error).
///
/// Produces an indented line like `  └─ error: message`.
pub fn format_error_detail(error_message: &str) -> String {
    format!("  └─ error: {}", error_message.dimmed())
}

// ============================================================================
// Other Formatters
// ============================================================================

/// Format context warning message.
///
/// Shows a warning when context window usage is high. At >95%, suggests using
/// `/clear` to reset.
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

/// Format API retry message.
///
/// Shows retry information including attempt count and delay.
pub fn format_retry(attempt: u32, max_attempts: u32, delay: Duration, error: &str) -> String {
    format!(
        "[{}: retrying in {}s (attempt {}/{})]",
        error.bright_yellow(),
        delay.as_secs(),
        attempt,
        max_attempts
    )
}

/// Format an error message (red).
pub fn format_error_message(msg: &str) -> String {
    format!("{}", msg.red())
}

/// Format ctrl-c received message.
pub fn format_ctrl_c() -> &'static str {
    "[ctrl-c received]"
}

/// Format task cancelled/aborted message.
pub fn format_cancelled() -> String {
    format!("{} task cancelled by client", "ABORTED".red())
}

// ============================================================================
// Token Estimation
// ============================================================================

/// Approximate characters per token for estimation.
const CHARS_PER_TOKEN: usize = 4;

/// Rough token estimate based on JSON string length.
///
/// Uses a simple heuristic of ~4 characters per token.
pub fn estimate_tokens(value: &Value) -> u32 {
    (value.to_string().len() / CHARS_PER_TOKEN) as u32
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================
    // Tool args formatting tests
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
        assert_eq!(formatted, "");
    }

    #[test]
    fn test_format_tool_args_ask_user_filtering() {
        let args = serde_json::json!({
            "question": "What is your favorite color?",
            "options": ["red", "blue", "green"]
        });
        let formatted = format_tool_args("ask_user", &args);
        assert_eq!(formatted, "");
    }

    // =========================================
    // Tool executing format tests
    // =========================================

    #[test]
    fn test_format_tool_executing_basic() {
        colored::control::set_override(false);
        let args = serde_json::json!({"file_path": "test.rs"});
        let formatted = format_tool_executing("read_file", &args);
        assert!(formatted.contains("┌─"));
        assert!(formatted.contains("read_file"));
        assert!(formatted.contains("file_path=\"test.rs\""));
        assert!(formatted.ends_with('\n'), "must end with newline");
        colored::control::unset_override();
    }

    #[test]
    fn test_format_tool_executing_empty_args() {
        colored::control::set_override(false);
        let formatted = format_tool_executing("list_files", &serde_json::json!({}));
        assert!(formatted.contains("┌─"));
        assert!(formatted.contains("list_files"));
        assert!(formatted.ends_with('\n'), "must end with newline");
        colored::control::unset_override();
    }

    // =========================================
    // Tool result format tests
    // =========================================

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(&serde_json::json!("hello")), 1);
        assert_eq!(estimate_tokens(&serde_json::json!({"key": "value"})), 3);
    }

    #[test]
    fn test_format_tool_result_duration() {
        colored::control::set_override(false);

        // < 1ms -> 3 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_micros(100), 10, false),
            "└─ test 0.000s ~10 tok"
        );

        // >= 1ms -> 2 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_millis(20), 10, false),
            "└─ test 0.02s ~10 tok"
        );

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
    fn test_format_error_detail() {
        colored::control::set_override(false);
        let detail = format_error_detail("permission denied");
        assert_eq!(detail, "  └─ error: permission denied");
        colored::control::unset_override();
    }

    // =========================================
    // Context warning format tests
    // =========================================

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

    // =========================================
    // Retry format tests
    // =========================================

    #[test]
    fn test_format_retry() {
        colored::control::set_override(false);

        let msg = format_retry(1, 3, Duration::from_secs(2), "rate limit exceeded");
        assert!(msg.contains("rate limit exceeded"));
        assert!(msg.contains("2s"));
        assert!(msg.contains("1/3"));

        colored::control::unset_override();
    }

    // =========================================
    // Simple message format tests
    // =========================================

    #[test]
    fn test_format_ctrl_c() {
        assert_eq!(format_ctrl_c(), "[ctrl-c received]");
    }

    #[test]
    fn test_format_cancelled() {
        colored::control::set_override(false);
        let msg = format_cancelled();
        assert!(msg.contains("ABORTED"));
        assert!(msg.contains("task cancelled by client"));
        colored::control::unset_override();
    }

    #[test]
    fn test_format_error_message() {
        colored::control::set_override(false);
        let msg = format_error_message("something went wrong");
        assert_eq!(msg, "something went wrong");
        colored::control::unset_override();
    }
}

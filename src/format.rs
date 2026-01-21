//! Pure formatting functions for UI output.
//!
//! All colored/styled output uses `format_*` helper functions defined here.
//! This keeps formatting testable, centralized, and out of business logic.
//!
//! # Categories
//!
//! ## Tool Output Formatters
//! - `format_tool_executing()` - Tool start line (`┌─ name args`)
//! - `format_tool_result()` - Tool completion line (`└─ name duration ~tokens tok`)
//! - `format_tool_args()` - Format tool arguments as key=value pairs
//! - `format_error_detail()` - Error detail line (indented)
//! - `format_result_block()` - Complete result block (result + optional error)
//!
//! ## Type-Aligned Formatters
//! These take genai-rs types directly for cleaner consumer API:
//! - `format_call()` - OwnedFunctionCallInfo → String
//! - `format_result()` - FunctionExecutionResult → String
//!
//! ## Other Formatters
//! - `format_context_warning()` - Context window warnings
//! - `format_retry()` - API retry messages
//! - `format_interaction_complete()` - Session interaction ID

use std::time::Duration;

use colored::Colorize;
use genai_rs::{FunctionExecutionResult, OwnedFunctionCallInfo};
use serde_json::Value;

// ============================================================================
// Constants
// ============================================================================

/// Maximum argument display length before truncation.
const MAX_ARG_DISPLAY_LEN: usize = 80;

/// Approximate characters per token for estimation.
const CHARS_PER_TOKEN: usize = 4;

// ============================================================================
// Tool Argument Formatting
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
/// Includes trailing newline for use with emit_line.
pub fn format_tool_executing(name: &str, args: &Value) -> String {
    let args_str = format_tool_args(name, args);
    format!("┌─ {} {}\n", name.cyan(), args_str)
}

/// Rough token estimate based on `CHARS_PER_TOKEN`.
pub fn estimate_tokens(value: &Value) -> u32 {
    (value.to_string().len() / CHARS_PER_TOKEN) as u32
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

/// Format error detail line for display (shown below tool result on error).
pub fn format_error_detail(error_message: &str) -> String {
    format!("  └─ error: {}", error_message.dimmed())
}

// ============================================================================
// Type-Aligned Formatters (take genai-rs types directly)
// ============================================================================

/// Pure: Format a function call for display.
/// Takes the genai-rs type directly for clean consumer API.
pub fn format_call(call: &OwnedFunctionCallInfo) -> String {
    format_tool_executing(&call.name, &call.args)
}

/// Compute token estimate for a function execution result (args + result).
pub fn compute_result_tokens(result: &FunctionExecutionResult) -> u32 {
    estimate_tokens(&result.args) + estimate_tokens(&result.result)
}

/// Pure: Format a function execution result for display.
/// Takes the genai-rs type directly, computing tokens internally.
pub fn format_result(result: &FunctionExecutionResult) -> String {
    let tokens = compute_result_tokens(result);
    let has_error = result.is_error();
    format_tool_result(&result.name, result.duration, tokens, has_error)
}

/// Pure: Format tool result block (result line + optional error).
/// Spacing between blocks is handled by write_to_log_file.
pub fn format_result_block(result: &FunctionExecutionResult) -> String {
    let mut output = format_result(result);
    if let Some(err_msg) = result.error_message() {
        output.push('\n');
        output.push_str(&format_error_detail(err_msg));
    }
    output
}

// ============================================================================
// Other Formatters
// ============================================================================

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

/// Format API retry message.
pub fn format_retry(attempt: u32, max_attempts: u32, delay: Duration, error: &str) -> String {
    format!(
        "[{}: retrying in {}s (attempt {}/{})]",
        error.bright_yellow(),
        delay.as_secs(),
        attempt,
        max_attempts
    )
}

/// Format interaction complete message with ID and model for session continuity.
pub fn format_interaction_complete(id: &str, model: &str) -> String {
    format!(
        "{} {} {} {}",
        "--interaction".dimmed(),
        id.yellow(),
        "--model".dimmed(),
        model.cyan()
    )
}

// ============================================================================
// Startup and Status Messages
// ============================================================================

/// Format the CLI startup banner.
pub fn format_startup_banner(version: &str, model: &str, cwd: &str) -> String {
    format!(
        "{} v{} | {} | {}",
        "clemini".bold(),
        version.cyan(),
        model.green(),
        cwd.yellow()
    )
}

/// Format the startup tip message.
pub fn format_startup_tip() -> String {
    format!(
        "{} Remember to take breaks during development!",
        "💡".yellow()
    )
}

/// Format ctrl-c received message.
pub fn format_ctrl_c() -> &'static str {
    "[ctrl-c received]"
}

/// Format task cancelled/aborted message.
pub fn format_cancelled() -> String {
    format!("{} task cancelled by client", "ABORTED".red())
}

/// Format an error message (red).
pub fn format_error_message(msg: &str) -> String {
    format!("{}", msg.red())
}

/// Format MCP server startup message.
pub fn format_mcp_startup() -> String {
    format!(
        "MCP server starting ({} enable multi-turn conversations)",
        "interaction IDs".cyan()
    )
}

// ============================================================================
// Builtin Command Formatters
// ============================================================================

/// Format /model command output (dimmed).
pub fn format_builtin_model(model: &str) -> String {
    format!("\n{}\n", model.dimmed())
}

/// Format /pwd command output (dimmed).
pub fn format_builtin_pwd(path: &str) -> String {
    format!("\n{}\n", path.dimmed())
}

/// Format /help command output (dimmed, extra trailing newline).
pub fn format_builtin_help(help_text: &str) -> String {
    format!("\n{}\n\n", help_text.dimmed())
}

/// Format /clear command output (dimmed, extra trailing newline).
pub fn format_builtin_cleared() -> String {
    format!("\n{}\n\n", "Conversation cleared.".dimmed())
}

/// Format shell escape (!cmd) output (dimmed).
pub fn format_builtin_shell(output: &str) -> String {
    format!("\n{}\n", output.dimmed())
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

    #[test]
    fn test_format_tool_args_truncation_indicator() {
        colored::control::set_override(false);

        let long_value = "x".repeat(100);
        let args = serde_json::json!({"content": long_value});
        let formatted = format_tool_args("test", &args);
        assert!(
            formatted.contains("...\""),
            "Truncated string should end with ...\", got: {}",
            formatted
        );
        assert!(
            formatted.len() < 100,
            "Should be truncated to reasonable length"
        );

        colored::control::unset_override();
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

    #[test]
    fn test_format_tool_executing_ends_with_newline() {
        colored::control::set_override(false);
        let formatted = format_tool_executing("test_tool", &serde_json::json!({"arg": "val"}));
        assert!(
            formatted.ends_with('\n'),
            "format_tool_executing must end with \\n for emit_line, got: {:?}",
            formatted
        );
        assert!(
            !formatted.ends_with("\n\n"),
            "Should not have double newline"
        );
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

    #[test]
    fn test_format_error_detail_structure() {
        colored::control::set_override(false);
        let formatted = format_error_detail("test error message");

        assert!(
            formatted.starts_with("  └─ error:"),
            "Error detail must start with '  └─ error:', got: {:?}",
            formatted
        );
        assert!(formatted.contains("test error message"));

        colored::control::unset_override();
    }

    // =========================================
    // Call and result format tests
    // =========================================

    #[test]
    fn test_format_call() {
        colored::control::set_override(false);

        let call = genai_rs::OwnedFunctionCallInfo {
            name: "read_file".to_string(),
            args: serde_json::json!({"file_path": "/tmp/test.txt"}),
            id: Some("call-1".to_string()),
        };
        let formatted = format_call(&call);
        assert!(formatted.starts_with("┌─"));
        assert!(formatted.contains("read_file"));
        assert!(formatted.ends_with('\n'));

        colored::control::unset_override();
    }

    #[test]
    fn test_format_result() {
        colored::control::set_override(false);

        let result = FunctionExecutionResult::new(
            "bash".to_string(),
            "call-1".to_string(),
            serde_json::json!({"command": "ls"}),
            serde_json::json!({"output": "file1.txt\nfile2.txt"}),
            Duration::from_millis(250),
        );
        let formatted = format_result(&result);
        assert!(formatted.starts_with("└─"));
        assert!(formatted.contains("bash"));
        assert!(formatted.contains("0.25s"));

        colored::control::unset_override();
    }

    // =========================================
    // Result block format tests
    // =========================================

    #[test]
    fn test_format_result_block_structure() {
        colored::control::set_override(false);

        let result = FunctionExecutionResult::new(
            "test_tool".to_string(),
            "call-1".to_string(),
            serde_json::json!({}),
            serde_json::json!({"ok": true}),
            Duration::from_millis(100),
        );
        let formatted = format_result_block(&result);
        assert!(formatted.starts_with("└─"), "Result should start with └─");
        assert!(formatted.contains("test_tool"));
        assert!(formatted.contains("0.10s"));
        assert!(
            !formatted.ends_with('\n'),
            "format_result_block should not end with newline (emit adds it)"
        );

        colored::control::unset_override();
    }

    #[test]
    fn test_format_result_block_with_error() {
        colored::control::set_override(false);

        let result = FunctionExecutionResult::new(
            "failing_tool".to_string(),
            "call-1".to_string(),
            serde_json::json!({}),
            serde_json::json!({"error": "something went wrong"}),
            Duration::from_millis(50),
        );
        let formatted = format_result_block(&result);

        assert!(formatted.contains("└─"), "Should have result line");
        assert!(formatted.contains("ERROR"), "Should show ERROR suffix");
        assert!(formatted.contains("failing_tool"));

        let lines: Vec<&str> = formatted.lines().collect();
        assert_eq!(lines.len(), 2, "Should have 2 lines: result + error detail");
        assert!(
            lines[1].starts_with("  └─ error:"),
            "Error detail should have 2-space indent and └─ prefix, got: {:?}",
            lines[1]
        );

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

    #[test]
    fn test_format_context_warning_boundary() {
        let msg = format_context_warning(95.0);
        assert!(!msg.contains("/clear"));

        let msg = format_context_warning(95.1);
        assert!(msg.contains("/clear"));
    }

    #[test]
    fn test_format_context_warning_structure() {
        let warning_80 = format_context_warning(80.5);
        assert!(warning_80.starts_with("WARNING:"));
        assert!(warning_80.contains("80.5%"));
        assert!(!warning_80.contains("/clear"));

        let warning_96 = format_context_warning(96.0);
        assert!(warning_96.starts_with("WARNING:"));
        assert!(warning_96.contains("96.0%"));
        assert!(warning_96.contains("/clear"));
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

        let msg = format_retry(2, 5, Duration::from_secs(10), "connection reset");
        assert!(msg.contains("connection reset"));
        assert!(msg.contains("10s"));
        assert!(msg.contains("2/5"));

        colored::control::unset_override();
    }

    // =========================================
    // Interaction complete format tests
    // =========================================

    #[test]
    fn test_format_interaction_complete() {
        colored::control::set_override(false);

        let msg = format_interaction_complete("v1_abc123", "gemini-2.5-flash");
        assert_eq!(msg, "--interaction v1_abc123 --model gemini-2.5-flash");

        let long_id = "v1_ChdTQjl3YWFIRk9lN3Ruc0VQaU1HRm9RNBIXVVI5d2FhbXlCN3ZkbnNFUDVfT09xQXc";
        let msg = format_interaction_complete(long_id, "gemini-2.5-pro");
        assert!(msg.starts_with("--interaction "));
        assert!(msg.contains(long_id));
        assert!(msg.contains("--model gemini-2.5-pro"));

        colored::control::unset_override();
    }

    // =========================================
    // Startup and status message tests
    // =========================================

    #[test]
    fn test_format_startup_banner() {
        colored::control::set_override(false);

        let banner = format_startup_banner("0.2.0", "gemini-2.5-flash", "/home/user/project");
        assert!(banner.contains("clemini"));
        assert!(banner.contains("v0.2.0"));
        assert!(banner.contains("gemini-2.5-flash"));
        assert!(banner.contains("/home/user/project"));

        colored::control::unset_override();
    }

    #[test]
    fn test_format_startup_tip() {
        let tip = format_startup_tip();
        assert!(tip.contains("Remember to take breaks"));
    }

    #[test]
    fn test_format_ctrl_c() {
        let msg = format_ctrl_c();
        assert_eq!(msg, "[ctrl-c received]");
    }

    #[test]
    fn test_format_cancelled() {
        colored::control::set_override(false);

        let msg = format_cancelled();
        assert!(msg.contains("ABORTED"));
        assert!(msg.contains("cancelled"));

        colored::control::unset_override();
    }

    #[test]
    fn test_format_error_message() {
        colored::control::set_override(false);

        let msg = format_error_message("Something went wrong");
        assert_eq!(msg, "Something went wrong");

        colored::control::unset_override();
    }

    #[test]
    fn test_format_mcp_startup() {
        colored::control::set_override(false);

        let msg = format_mcp_startup();
        assert!(msg.contains("MCP server starting"));
        assert!(msg.contains("interaction IDs"));

        colored::control::unset_override();
    }

    // =========================================
    // Builtin command format tests
    // =========================================

    #[test]
    fn test_format_builtin_model() {
        colored::control::set_override(false);

        let output = format_builtin_model("gemini-2.5-flash");
        assert!(output.starts_with('\n'), "must start with newline");
        assert!(output.ends_with('\n'), "must end with newline");
        assert!(output.contains("gemini-2.5-flash"));
        assert_eq!(output, "\ngemini-2.5-flash\n");

        colored::control::unset_override();
    }

    #[test]
    fn test_format_builtin_pwd() {
        colored::control::set_override(false);

        let output = format_builtin_pwd("/home/user/project");
        assert!(output.starts_with('\n'), "must start with newline");
        assert!(output.ends_with('\n'), "must end with newline");
        assert!(output.contains("/home/user/project"));
        assert_eq!(output, "\n/home/user/project\n");

        colored::control::unset_override();
    }

    #[test]
    fn test_format_builtin_help() {
        colored::control::set_override(false);

        let help_text = "Commands:\n  /help  Show help";
        let output = format_builtin_help(help_text);
        assert!(output.starts_with('\n'), "must start with newline");
        assert!(output.ends_with("\n\n"), "must end with double newline");
        assert!(output.contains("Commands:"));
        assert_eq!(output, "\nCommands:\n  /help  Show help\n\n");

        colored::control::unset_override();
    }

    #[test]
    fn test_format_builtin_cleared() {
        colored::control::set_override(false);

        let output = format_builtin_cleared();
        assert!(output.starts_with('\n'), "must start with newline");
        assert!(output.ends_with("\n\n"), "must end with double newline");
        assert!(output.contains("Conversation cleared."));
        assert_eq!(output, "\nConversation cleared.\n\n");

        colored::control::unset_override();
    }

    #[test]
    fn test_format_builtin_shell() {
        colored::control::set_override(false);

        let output = format_builtin_shell("file1.txt\nfile2.txt");
        assert!(output.starts_with('\n'), "must start with newline");
        assert!(output.ends_with('\n'), "must end with newline");
        assert!(output.contains("file1.txt"));
        assert_eq!(output, "\nfile1.txt\nfile2.txt\n");

        colored::control::unset_override();
    }

    #[test]
    fn test_format_builtin_shell_empty() {
        colored::control::set_override(false);

        let output = format_builtin_shell("");
        assert_eq!(output, "\n\n", "empty output still gets newline padding");

        colored::control::unset_override();
    }
}

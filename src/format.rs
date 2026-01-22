//! Pure formatting functions for UI output.
//!
//! This module re-exports primitive formatting functions from `clemitui` and
//! provides genai-rs-specific wrappers for use within clemini.
//!
//! # Categories
//!
//! ## Primitive Formatters (from clemitui)
//! - `format_tool_executing()` - Tool start line (`┌─ name args`)
//! - `format_tool_result()` - Tool completion line (`└─ name duration ~tokens tok`)
//! - `format_tool_args()` - Format tool arguments as key=value pairs
//! - `format_error_detail()` - Error detail line (indented)
//! - `format_context_warning()` - Context window warnings
//! - `format_retry()` - API retry messages
//!
//! ## Type-Aligned Formatters (clemini-specific)
//! These take genai-rs types directly for cleaner consumer API:
//! - `format_call()` - OwnedFunctionCallInfo → String
//! - `format_result()` - FunctionExecutionResult → String
//! - `format_result_block()` - Complete result block (result + optional error)

use colored::Colorize;
use genai_rs::{FunctionExecutionResult, OwnedFunctionCallInfo};

// Re-export primitive formatters from clemitui
pub use clemitui::format::{
    estimate_tokens, format_cancelled, format_context_warning, format_ctrl_c, format_error_detail,
    format_error_message, format_retry, format_tool_args, format_tool_executing,
    format_tool_result,
};

// Re-export TextBuffer from clemitui
pub use clemitui::TextBuffer;

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
/// Spacing between blocks is handled by the OutputSink's emit method.
pub fn format_result_block(result: &FunctionExecutionResult) -> String {
    let mut output = format_result(result);
    if let Some(err_msg) = result.error_message() {
        output.push('\n');
        output.push_str(&format_error_detail(err_msg));
    }
    output
}

// ============================================================================
// Clemini-Specific Formatters
// ============================================================================

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
    use std::time::Duration;

    // =========================================
    // Type-aligned formatter tests (genai-rs types)
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
    // Clemini-specific formatter tests
    // =========================================

    #[test]
    fn test_format_interaction_complete() {
        colored::control::set_override(false);

        let msg = format_interaction_complete("v1_abc123", "gemini-2.5-flash");
        assert_eq!(msg, "--interaction v1_abc123 --model gemini-2.5-flash");

        colored::control::unset_override();
    }

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

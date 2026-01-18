//! Diff formatting utilities for visualizing text changes.

use colored::Colorize;
use similar::{ChangeTag, TextDiff};

/// Find the minimum common leading whitespace across all non-empty lines.
fn common_indent(old: &str, new: &str) -> usize {
    old.lines()
        .chain(new.lines())
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0)
}

/// Strip `n` characters of leading whitespace from a line.
fn strip_indent(line: &str, n: usize) -> &str {
    if n == 0 || line.is_empty() {
        line
    } else {
        let chars_to_skip: usize = line.chars().take(n).map(|c| c.len_utf8()).sum();
        &line[chars_to_skip.min(line.len())..]
    }
}

/// Format a diff between old and new strings.
/// Returns ANSI-colored string suitable for terminal/logs.
///
/// Uses a hybrid approach:
/// - Single-line changes: simple `- old` / `+ new` format
/// - Multi-line changes: unified diff with context lines
pub fn format_diff(old: &str, new: &str, context_lines: usize) -> String {
    // No diff if strings are identical
    if old == new {
        return String::new();
    }

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Simple format for single-line changes
    if old_lines.len() <= 1 && new_lines.len() <= 1 {
        return format_simple_diff(old, new);
    }

    // Unified diff for multi-line changes
    format_unified_diff(old, new, context_lines)
}

/// Simple diff format for single-line changes:
/// ```text
///   - old content
///   + new content
/// ```
fn format_simple_diff(old: &str, new: &str) -> String {
    let indent = common_indent(old, new);
    let mut output = String::new();

    for line in old.lines() {
        let stripped = strip_indent(line, indent);
        output.push_str(&format!("  {} {}\n", "-".red(), stripped.red()));
    }
    // Handle empty old string (pure addition)
    if old.is_empty() && !new.is_empty() {
        // No deletion line needed
    } else if old.lines().count() == 0 && !old.is_empty() {
        // Single line without newline
        let stripped = strip_indent(old, indent);
        output.push_str(&format!("  {} {}\n", "-".red(), stripped.red()));
    }

    for line in new.lines() {
        let stripped = strip_indent(line, indent);
        output.push_str(&format!("  {} {}\n", "+".green(), stripped.green()));
    }
    // Handle empty new string (pure deletion)
    if new.is_empty() && !old.is_empty() {
        // No addition line needed
    } else if new.lines().count() == 0 && !new.is_empty() {
        // Single line without newline
        let stripped = strip_indent(new, indent);
        output.push_str(&format!("  {} {}\n", "+".green(), stripped.green()));
    }

    output.trim_end().to_string()
}

/// Unified diff format for multi-line changes with context:
/// ```text
///     context line before
///   - removed line
///   + added line
///     context line after
/// ```
fn format_unified_diff(old: &str, new: &str, context_lines: usize) -> String {
    let indent = common_indent(old, new);
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();

    for hunk in diff
        .unified_diff()
        .context_radius(context_lines)
        .iter_hunks()
    {
        for change in hunk.iter_changes() {
            let line = change.value().trim_end_matches('\n');
            let stripped = strip_indent(line, indent);
            match change.tag() {
                ChangeTag::Delete => {
                    output.push_str(&format!("  {} {}\n", "-".red(), stripped.red()));
                }
                ChangeTag::Insert => {
                    output.push_str(&format!("  {} {}\n", "+".green(), stripped.green()));
                }
                ChangeTag::Equal => {
                    output.push_str(&format!("    {}\n", stripped.dimmed()));
                }
            }
        }
    }

    output.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to strip ANSI codes for easier assertion
    fn strip_ansi(s: &str) -> String {
        let re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
        re.replace_all(s, "").to_string()
    }

    #[test]
    fn test_simple_single_line_change() {
        let old = "let x = 5;";
        let new = "let x = 10;";

        let diff = format_diff(old, new, 2);
        let plain = strip_ansi(&diff);

        assert!(plain.contains("- let x = 5;"));
        assert!(plain.contains("+ let x = 10;"));
    }

    #[test]
    fn test_simple_empty_to_content() {
        let old = "";
        let new = "new line";

        let diff = format_diff(old, new, 2);
        let plain = strip_ansi(&diff);

        assert!(plain.contains("+ new line"));
        assert!(!plain.contains("-")); // No deletion
    }

    #[test]
    fn test_simple_content_to_empty() {
        let old = "old line";
        let new = "";

        let diff = format_diff(old, new, 2);
        let plain = strip_ansi(&diff);

        assert!(plain.contains("- old line"));
        assert!(!plain.contains("+")); // No addition
    }

    #[test]
    fn test_multi_line_unified_diff() {
        let old = "fn test() {\n    let x = 5;\n    let y = 6;\n    return x + y;\n}";
        let new = "fn test() {\n    let x = 10;\n    let y = 12;\n    return x + y;\n}";

        let diff = format_diff(old, new, 2);
        let plain = strip_ansi(&diff);

        // Should show deletions and insertions
        assert!(plain.contains("- let x = 5;") || plain.contains("-     let x = 5;"));
        assert!(plain.contains("+ let x = 10;") || plain.contains("+     let x = 10;"));

        // Should include context
        assert!(plain.contains("fn test()") || plain.contains("return x + y"));
    }

    #[test]
    fn test_multi_line_with_context() {
        let old = "line1\nline2\nline3\nline4\nline5";
        let new = "line1\nline2\nCHANGED\nline4\nline5";

        let diff = format_diff(old, new, 1);
        let plain = strip_ansi(&diff);

        // Context lines should appear
        assert!(plain.contains("line2"));
        assert!(plain.contains("line4"));

        // Changed lines
        assert!(plain.contains("- line3"));
        assert!(plain.contains("+ CHANGED"));
    }

    #[test]
    fn test_identical_strings() {
        let text = "no change";
        let diff = format_diff(text, text, 2);

        // Should be empty or minimal when nothing changed
        assert!(diff.is_empty() || strip_ansi(&diff).trim().is_empty());
    }

    #[test]
    fn test_diff_structure() {
        // Test that the diff has the expected structure (- and + markers)
        let old = "old";
        let new = "new";

        let diff = format_diff(old, new, 2);
        let plain = strip_ansi(&diff);

        // Should have deletion and addition markers
        assert!(plain.contains("-"));
        assert!(plain.contains("+"));
        assert!(plain.contains("old"));
        assert!(plain.contains("new"));
    }

    #[test]
    fn test_common_indent_stripped() {
        // Both lines have 8 spaces of indentation - should be stripped
        let old = "        let x = 5;";
        let new = "        let x = 10;";

        let diff = format_diff(old, new, 2);
        let plain = strip_ansi(&diff);

        // Should NOT have the 8 leading spaces
        assert!(plain.contains("- let x = 5;"));
        assert!(plain.contains("+ let x = 10;"));
        // Should NOT contain the heavily indented version
        assert!(!plain.contains("-         let x"));
    }

    #[test]
    fn test_partial_indent_stripped() {
        // Lines have different indentation - only common part stripped
        let old = "    fn foo() {\n        let x = 5;\n    }";
        let new = "    fn foo() {\n        let x = 10;\n    }";

        let diff = format_diff(old, new, 2);
        let plain = strip_ansi(&diff);

        // Common indent is 4, so "    let x" becomes "let x" (still has 4 relative)
        // The diff shows the change, context lines keep relative indent
        assert!(plain.contains("let x = 5") || plain.contains("let x = 10"));
    }
}

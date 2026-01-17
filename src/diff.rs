//! Diff formatting utilities for visualizing text changes.

use colored::Colorize;
use similar::{ChangeTag, TextDiff};

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
    let mut output = String::new();

    for line in old.lines() {
        output.push_str(&format!("  {} {}\n", "-".red(), line.red()));
    }
    // Handle empty old string (pure addition)
    if old.is_empty() && !new.is_empty() {
        // No deletion line needed
    } else if old.lines().count() == 0 && !old.is_empty() {
        // Single line without newline
        output.push_str(&format!("  {} {}\n", "-".red(), old.red()));
    }

    for line in new.lines() {
        output.push_str(&format!("  {} {}\n", "+".green(), line.green()));
    }
    // Handle empty new string (pure deletion)
    if new.is_empty() && !old.is_empty() {
        // No addition line needed
    } else if new.lines().count() == 0 && !new.is_empty() {
        // Single line without newline
        output.push_str(&format!("  {} {}\n", "+".green(), new.green()));
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
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();

    for hunk in diff
        .unified_diff()
        .context_radius(context_lines)
        .iter_hunks()
    {
        for change in hunk.iter_changes() {
            let line = change.value().trim_end_matches('\n');
            match change.tag() {
                ChangeTag::Delete => {
                    output.push_str(&format!("  {} {}\n", "-".red(), line.red()));
                }
                ChangeTag::Insert => {
                    output.push_str(&format!("  {} {}\n", "+".green(), line.green()));
                }
                ChangeTag::Equal => {
                    output.push_str(&format!("    {}\n", line.dimmed()));
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
}

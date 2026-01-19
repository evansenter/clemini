//! Diff formatting utilities for visualizing text changes.

use colored::Colorize;
use similar::{ChangeTag, TextDiff};
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Syntax set for language detection and parsing.
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Catppuccin Mocha theme (bundled from catppuccin/bat).
static CATPPUCCIN_MOCHA: LazyLock<Theme> = LazyLock::new(|| {
    let theme_bytes = include_bytes!("../themes/catppuccin-mocha.tmTheme");
    let mut cursor = std::io::Cursor::new(theme_bytes);
    ThemeSet::load_from_reader(&mut cursor).expect("bundled theme should be valid")
});

/// Background colors for diff lines.
/// Slightly brighter backgrounds to distinguish from terminal background while
/// keeping syntax colors readable.
const DELETE_BG: (u8, u8, u8) = (80, 40, 40); // Dark red
const INSERT_BG: (u8, u8, u8) = (40, 80, 40); // Dark green

/// Apply syntax highlighting to a line with a background color.
/// Each token gets its foreground color from syntect and the specified background.
fn highlight_line_with_bg(
    line: &str,
    highlighter: &mut HighlightLines,
    bg: Option<(u8, u8, u8)>,
) -> String {
    // Get syntax-highlighted ranges
    let ranges = match highlighter.highlight_line(line, &SYNTAX_SET) {
        Ok(ranges) => ranges,
        Err(_) => return fallback_color(line, bg),
    };

    let mut output = String::new();
    for (style, text) in ranges {
        let colored_text = apply_style(text, style, bg);
        output.push_str(&colored_text);
    }
    output
}

/// Apply syntect style and optional background to text.
fn apply_style(text: &str, style: Style, bg: Option<(u8, u8, u8)>) -> String {
    let fg = style.foreground;
    // Build ANSI escape sequence manually to ensure truecolor is used
    let mut result = format!("\x1b[38;2;{};{};{}m", fg.r, fg.g, fg.b);
    if let Some((r, g, b)) = bg {
        result.push_str(&format!("\x1b[48;2;{};{};{}m", r, g, b));
    }
    result.push_str(text);
    result.push_str("\x1b[0m");
    result
}

/// Fallback coloring when syntax highlighting fails.
fn fallback_color(line: &str, bg: Option<(u8, u8, u8)>) -> String {
    match bg {
        Some((r, g, b)) => line.on_truecolor(r, g, b).to_string(),
        None => line.dimmed().to_string(),
    }
}

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
///
/// If `file_path` is provided, applies syntax highlighting based on file extension.
pub fn format_diff(old: &str, new: &str, context_lines: usize, file_path: Option<&str>) -> String {
    // No diff if strings are identical
    if old == new {
        return String::new();
    }

    // Try to get a syntax highlighter based on file extension
    let highlighter = file_path.and_then(|path| {
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())?;
        let syntax = SYNTAX_SET.find_syntax_by_extension(extension)?;
        Some(HighlightLines::new(syntax, &CATPPUCCIN_MOCHA))
    });

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Simple format for single-line changes
    if old_lines.len() <= 1 && new_lines.len() <= 1 {
        return format_simple_diff(old, new, highlighter);
    }

    // Unified diff for multi-line changes
    format_unified_diff(old, new, context_lines, highlighter)
}

/// Simple diff format for single-line changes:
/// ```text
///   - old content
///   + new content
/// ```
fn format_simple_diff(old: &str, new: &str, mut highlighter: Option<HighlightLines>) -> String {
    let indent = common_indent(old, new);
    let mut output = String::new();

    for line in old.lines() {
        let stripped = strip_indent(line, indent);
        let content = format_line_content(stripped, &mut highlighter, Some(DELETE_BG));
        output.push_str(&format!("  {} {}\n", "-".red(), content));
    }
    // Handle empty old string (pure addition)
    if old.is_empty() && !new.is_empty() {
        // No deletion line needed
    } else if old.lines().count() == 0 && !old.is_empty() {
        // Single line without newline
        let stripped = strip_indent(old, indent);
        let content = format_line_content(stripped, &mut highlighter, Some(DELETE_BG));
        output.push_str(&format!("  {} {}\n", "-".red(), content));
    }

    for line in new.lines() {
        let stripped = strip_indent(line, indent);
        let content = format_line_content(stripped, &mut highlighter, Some(INSERT_BG));
        output.push_str(&format!("  {} {}\n", "+".green(), content));
    }
    // Handle empty new string (pure deletion)
    if new.is_empty() && !old.is_empty() {
        // No addition line needed
    } else if new.lines().count() == 0 && !new.is_empty() {
        // Single line without newline
        let stripped = strip_indent(new, indent);
        let content = format_line_content(stripped, &mut highlighter, Some(INSERT_BG));
        output.push_str(&format!("  {} {}\n", "+".green(), content));
    }

    output.trim_end().to_string()
}

/// Format line content with optional syntax highlighting.
fn format_line_content(
    line: &str,
    highlighter: &mut Option<HighlightLines>,
    bg: Option<(u8, u8, u8)>,
) -> String {
    match highlighter {
        Some(h) => highlight_line_with_bg(line, h, bg),
        None => fallback_color(line, bg),
    }
}

/// Unified diff format for multi-line changes with context:
/// ```text
///     context line before
///   - removed line
///   + added line
///     context line after
/// ```
fn format_unified_diff(
    old: &str,
    new: &str,
    context_lines: usize,
    mut highlighter: Option<HighlightLines>,
) -> String {
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
                    let content = format_line_content(stripped, &mut highlighter, Some(DELETE_BG));
                    output.push_str(&format!("  {} {}\n", "-".red(), content));
                }
                ChangeTag::Insert => {
                    let content = format_line_content(stripped, &mut highlighter, Some(INSERT_BG));
                    output.push_str(&format!("  {} {}\n", "+".green(), content));
                }
                ChangeTag::Equal => {
                    let content = format_line_content(stripped, &mut highlighter, None);
                    output.push_str(&format!("    {}\n", content));
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

        let diff = format_diff(old, new, 2, None);
        let plain = strip_ansi(&diff);

        assert!(plain.contains("- let x = 5;"));
        assert!(plain.contains("+ let x = 10;"));
    }

    #[test]
    fn test_simple_empty_to_content() {
        let old = "";
        let new = "new line";

        let diff = format_diff(old, new, 2, None);
        let plain = strip_ansi(&diff);

        assert!(plain.contains("+ new line"));
        assert!(!plain.contains("-")); // No deletion
    }

    #[test]
    fn test_simple_content_to_empty() {
        let old = "old line";
        let new = "";

        let diff = format_diff(old, new, 2, None);
        let plain = strip_ansi(&diff);

        assert!(plain.contains("- old line"));
        assert!(!plain.contains("+")); // No addition
    }

    #[test]
    fn test_multi_line_unified_diff() {
        let old = "fn test() {\n    let x = 5;\n    let y = 6;\n    return x + y;\n}";
        let new = "fn test() {\n    let x = 10;\n    let y = 12;\n    return x + y;\n}";

        let diff = format_diff(old, new, 2, None);
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

        let diff = format_diff(old, new, 1, None);
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
        let diff = format_diff(text, text, 2, None);

        // Should be empty or minimal when nothing changed
        assert!(diff.is_empty() || strip_ansi(&diff).trim().is_empty());
    }

    #[test]
    fn test_diff_structure() {
        // Test that the diff has the expected structure (- and + markers)
        let old = "old";
        let new = "new";

        let diff = format_diff(old, new, 2, None);
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

        let diff = format_diff(old, new, 2, None);
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

        let diff = format_diff(old, new, 2, None);
        let plain = strip_ansi(&diff);

        // Common indent is 4, so "    let x" becomes "let x" (still has 4 relative)
        // The diff shows the change, context lines keep relative indent
        assert!(plain.contains("let x = 5") || plain.contains("let x = 10"));
    }

    #[test]
    fn test_syntax_highlighting_with_rs_extension() {
        // Force colors on (colored crate disables when no TTY)
        colored::control::set_override(true);

        let old = "let x = 5;";
        let new = "let x = 10;";

        // With .rs extension, should apply syntax highlighting (has ANSI codes)
        let diff = format_diff(old, new, 2, Some("test.rs"));
        eprintln!("Syntax-highlighted diff: {:?}", diff);

        // Still has the basic structure
        let plain = strip_ansi(&diff);
        assert!(plain.contains("- let x = 5;"));
        assert!(plain.contains("+ let x = 10;"));

        // Should have truecolor ANSI codes from syntect (38;2; for foreground, 48;2; for background)
        // These indicate actual syntax highlighting, not just red/green from fallback
        assert!(
            diff.contains("\x1b[38;2;") || diff.contains("\x1b[48;2;"),
            "expected truecolor ANSI codes from syntect, got: {:?}",
            diff
        );
    }

    #[test]
    fn test_syntax_highlighting_fallback_no_file() {
        let old = "let x = 5;";
        let new = "let x = 10;";

        // Without file path, should use simple coloring
        let diff = format_diff(old, new, 2, None);
        let plain = strip_ansi(&diff);

        assert!(plain.contains("- let x = 5;"));
        assert!(plain.contains("+ let x = 10;"));
    }

    #[test]
    fn test_syntax_highlighting_fallback_unknown_ext() {
        let old = "some content";
        let new = "other content";

        // Unknown extension should fallback gracefully
        let diff = format_diff(old, new, 2, Some("file.unknownext"));
        let plain = strip_ansi(&diff);

        assert!(plain.contains("- some content"));
        assert!(plain.contains("+ other content"));
    }

    #[test]
    fn test_syntax_highlighting_multiline() {
        let old = "fn test() {\n    let x = 5;\n}";
        let new = "fn test() {\n    let x = 10;\n}";

        // Multi-line diff with syntax highlighting
        let diff = format_diff(old, new, 2, Some("code.rs"));
        let plain = strip_ansi(&diff);

        // Should have deletion and insertion
        assert!(plain.contains("- let x = 5;") || plain.contains("-     let x = 5;"));
        assert!(plain.contains("+ let x = 10;") || plain.contains("+     let x = 10;"));
    }

    #[test]
    fn test_catppuccin_theme_loads() {
        // Verify the bundled Catppuccin theme loads without panic
        let _ = &*CATPPUCCIN_MOCHA;
        // If we get here, the theme loaded successfully
    }

    #[test]
    fn test_background_colors_differ_for_delete_and_insert() {
        colored::control::set_override(true);

        let old = "delete me";
        let new = "add me";

        let diff = format_diff(old, new, 2, Some("test.rs"));

        // Deletion should have DELETE_BG (80, 40, 40)
        assert!(
            diff.contains("\x1b[48;2;80;40;40m"),
            "deletion should have red background"
        );

        // Addition should have INSERT_BG (40, 80, 40)
        assert!(
            diff.contains("\x1b[48;2;40;80;40m"),
            "addition should have green background"
        );
    }

    #[test]
    fn test_context_lines_no_background() {
        colored::control::set_override(true);

        let old = "line1\nchange me\nline3";
        let new = "line1\nchanged\nline3";

        let diff = format_diff(old, new, 1, Some("test.rs"));

        // Context lines should NOT have our custom backgrounds
        // They should have syntax colors but no diff background
        let lines: Vec<&str> = diff.lines().collect();

        // Strip ANSI to find the actual content, then check original line
        for line in &lines {
            let plain = strip_ansi(line);
            let trimmed = plain.trim();
            // Context lines start with 4 spaces (no - or + marker)
            if plain.starts_with("    ") && !trimmed.is_empty() {
                // Context line - should not have DELETE_BG or INSERT_BG
                assert!(
                    !line.contains("\x1b[48;2;80;40;40m") && !line.contains("\x1b[48;2;40;80;40m"),
                    "context line should not have diff background: {:?}",
                    line
                );
            }
        }
    }

    #[test]
    fn test_python_syntax_highlighting() {
        colored::control::set_override(true);

        let old = "x = 5";
        let new = "x = 10";

        let diff = format_diff(old, new, 2, Some("test.py"));

        // Should have truecolor codes (syntax highlighting worked)
        assert!(
            diff.contains("\x1b[38;2;"),
            "Python should get syntax highlighting"
        );
    }

    #[test]
    fn test_javascript_syntax_highlighting() {
        colored::control::set_override(true);

        let old = "const x = 5;";
        let new = "const x = 10;";

        let diff = format_diff(old, new, 2, Some("test.js"));

        // Should have truecolor codes
        assert!(
            diff.contains("\x1b[38;2;"),
            "JavaScript should get syntax highlighting"
        );
    }
}

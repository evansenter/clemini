//! Text buffer for accumulating streaming text with markdown rendering.
//!
//! The [`TextBuffer`] collects text chunks from streaming responses and
//! renders them with markdown formatting when flushed.

use std::sync::LazyLock;
use termimad::MadSkin;

// ============================================================================
// Markdown Rendering
// ============================================================================

/// Termimad skin for markdown rendering. Left-aligns headers.
pub(crate) static SKIN: LazyLock<MadSkin> = LazyLock::new(|| {
    let mut skin = MadSkin::default();
    for h in &mut skin.headers {
        h.align = termimad::Alignment::Left;
    }
    skin
});

/// Render text with markdown formatting but without line wrapping.
/// Uses a very large width to effectively disable termimad's wrapping.
pub(crate) fn render_markdown_nowrap(text: &str) -> String {
    use termimad::FmtText;
    FmtText::from(&SKIN, text, Some(10000)).to_string()
}

// ============================================================================
// Text Buffer
// ============================================================================

/// Buffer for accumulating streaming text until event boundaries.
///
/// Text is buffered via `push()` during streaming, then flushed with markdown
/// rendering at logical boundaries (e.g., before tool execution, on completion).
/// The `flush()` method normalizes trailing newlines to exactly `\n\n`.
///
/// # Example
///
/// ```
/// use clemitui::TextBuffer;
///
/// let mut buffer = TextBuffer::new();
/// buffer.push("Hello ");
/// buffer.push("world!");
///
/// let rendered = buffer.flush();
/// assert!(rendered.is_some());
/// assert!(rendered.unwrap().contains("Hello world!"));
///
/// // Buffer is now empty
/// assert!(buffer.is_empty());
/// ```
#[derive(Debug, Default)]
pub struct TextBuffer(String);

impl TextBuffer {
    /// Create a new empty text buffer.
    pub fn new() -> Self {
        Self(String::new())
    }

    /// Append text to the buffer.
    pub fn push(&mut self, text: &str) {
        self.0.push_str(text);
    }

    /// Flush buffered text with markdown rendering, normalized to `\n\n`.
    /// Returns rendered text, or None if buffer was empty or whitespace-only.
    pub fn flush(&mut self) -> Option<String> {
        if self.0.is_empty() {
            return None;
        }

        let text = std::mem::take(&mut self.0);
        let rendered = render_markdown_nowrap(&text);

        // Normalize trailing newlines to exactly \n\n
        let trimmed = rendered.trim_end_matches('\n');
        if trimmed.is_empty() {
            None
        } else {
            Some(format!("{}\n\n", trimmed))
        }
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_buffer_accumulates() {
        let mut buffer = TextBuffer::new();

        // Buffer text chunks
        buffer.push("Hello ");
        buffer.push("world!");

        // Flush returns rendered content
        let out = buffer.flush();
        assert!(out.is_some());
        assert!(out.unwrap().contains("Hello world!"));

        // Buffer is now empty
        assert!(buffer.flush().is_none());
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_text_buffer_flush_empty() {
        let mut buffer = TextBuffer::new();
        assert!(buffer.is_empty());
        let out = buffer.flush();
        assert!(out.is_none());
    }

    #[test]
    fn test_text_buffer_flush_normalizes_to_double_newline() {
        // flush() should normalize output to end with exactly \n\n
        // This is critical for consistent spacing before tool calls

        // Case 1: Text with no trailing newline -> normalized to \n\n
        let mut buffer = TextBuffer::new();
        buffer.push("Hello world");
        let out = buffer.flush().unwrap();
        assert!(
            out.ends_with("\n\n"),
            "Should end with \\n\\n, got: {:?}",
            out
        );
        assert!(!out.ends_with("\n\n\n"), "Should not have triple newline");

        // Case 2: Text with single trailing newline -> normalized to \n\n
        let mut buffer = TextBuffer::new();
        buffer.push("Hello world\n");
        let out = buffer.flush().unwrap();
        assert!(
            out.ends_with("\n\n"),
            "Should end with \\n\\n, got: {:?}",
            out
        );

        // Case 3: Text with double trailing newline -> stays \n\n
        let mut buffer = TextBuffer::new();
        buffer.push("Hello world\n\n");
        let out = buffer.flush().unwrap();
        assert!(
            out.ends_with("\n\n"),
            "Should end with \\n\\n, got: {:?}",
            out
        );
        assert!(!out.ends_with("\n\n\n"), "Should not have triple newline");
    }

    #[test]
    fn test_text_buffer_flush_returns_none_for_whitespace_only() {
        // If buffer only contains whitespace/newlines, flush should return None
        let mut buffer = TextBuffer::new();
        buffer.push("\n\n");
        let out = buffer.flush();
        assert!(out.is_none(), "Whitespace-only buffer should return None");
    }

    #[test]
    fn test_text_buffer_default() {
        // TextBuffer implements Default
        let buffer = TextBuffer::default();
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_render_markdown_nowrap() {
        let rendered = render_markdown_nowrap("**bold** and *italic*");
        // Verify markdown is processed - should contain ANSI escape codes
        assert!(!rendered.is_empty());
        // ANSI escape codes start with \x1b[ (ESC[)
        assert!(
            rendered.contains("\x1b["),
            "Expected ANSI codes for bold/italic formatting, got: {:?}",
            rendered
        );
        // The text content should still be present
        assert!(rendered.contains("bold"), "Should contain 'bold' text");
        assert!(rendered.contains("italic"), "Should contain 'italic' text");
    }

    #[test]
    fn test_render_markdown_nowrap_plain_text() {
        // Plain text without markdown should pass through
        let rendered = render_markdown_nowrap("plain text");
        assert!(rendered.contains("plain text"));
    }

    #[test]
    fn test_render_markdown_nowrap_headers() {
        // Headers should be rendered (SKIN left-aligns them)
        let rendered = render_markdown_nowrap("# Header");
        assert!(!rendered.is_empty());
        assert!(rendered.contains("Header"));
    }
}

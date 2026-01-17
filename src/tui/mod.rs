//! TUI module for clemini's terminal interface.
//!
//! Uses ratatui for rendering and crossterm for input handling.

pub mod ui;

pub use ui::render;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Maximum lines to keep in chat history for scrollback
const MAX_CHAT_LINES: usize = 10_000;

/// Current activity state of the application
#[derive(Debug, Clone, Default)]
pub enum Activity {
    #[default]
    Idle,
    Streaming,
    Executing(String), // tool name
}

impl Activity {
    pub fn display(&self) -> &str {
        match self {
            Self::Idle => "ready",
            Self::Streaming => "streaming...",
            Self::Executing(tool) => tool,
        }
    }

    pub fn is_busy(&self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// Application state for the TUI
#[allow(dead_code)] // should_quit reserved for future keyboard shortcuts
pub struct App {
    // UI state
    chat_lines: VecDeque<String>,
    scroll_offset: u16,
    activity: Activity,

    // Session state
    pub model: String,
    pub estimated_tokens: u32,
    pub interaction_count: u32,

    // Control flags
    pub should_quit: bool,
    cancelled: Arc<AtomicBool>,
}

impl App {
    pub fn new(model: &str) -> Self {
        Self {
            chat_lines: VecDeque::with_capacity(MAX_CHAT_LINES),
            scroll_offset: 0,
            activity: Activity::default(),
            model: model.to_string(),
            estimated_tokens: 0,
            interaction_count: 0,
            should_quit: false,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Append text to the chat buffer (for complete lines/messages)
    /// Each call creates at least one new line (even for empty text)
    pub fn append_to_chat(&mut self, text: &str) {
        // Handle empty string as explicit blank line
        if text.is_empty() {
            if self.chat_lines.len() >= MAX_CHAT_LINES {
                self.chat_lines.pop_front();
            }
            self.chat_lines.push_back(String::new());
            return;
        }

        for line in text.lines() {
            if self.chat_lines.len() >= MAX_CHAT_LINES {
                self.chat_lines.pop_front();
            }
            self.chat_lines.push_back(line.to_string());
        }
        // If text ends with newline, add empty line for next content
        if text.ends_with('\n') {
            if self.chat_lines.len() >= MAX_CHAT_LINES {
                self.chat_lines.pop_front();
            }
            self.chat_lines.push_back(String::new());
        }
    }

    /// Append streaming text (may be partial lines, concatenates to current line)
    /// Preserves empty lines - won't fill them, creates new line instead.
    pub fn append_streaming(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        // Split into lines, handling partial lines properly
        let mut lines = text.split('\n').peekable();

        // First part appends to current line, or creates new if empty/blank
        if let Some(first) = lines.next() {
            let should_create_new =
                self.chat_lines.is_empty() || self.chat_lines.back().is_none_or(|l| l.is_empty());

            if should_create_new {
                // Don't fill empty lines - preserve them for spacing
                if self.chat_lines.len() >= MAX_CHAT_LINES {
                    self.chat_lines.pop_front();
                }
                self.chat_lines.push_back(first.to_string());
            } else {
                // Append to existing non-empty line
                if let Some(last_line) = self.chat_lines.back_mut() {
                    last_line.push_str(first);
                }
            }
        }

        // Remaining parts are new lines
        for line in lines {
            if self.chat_lines.len() >= MAX_CHAT_LINES {
                self.chat_lines.pop_front();
            }
            self.chat_lines.push_back(line.to_string());
        }
    }

    /// Clear the chat buffer
    pub fn clear_chat(&mut self) {
        self.chat_lines.clear();
        self.scroll_offset = 0;
    }

    /// Get chat lines for rendering
    pub fn chat_lines(&self) -> &VecDeque<String> {
        &self.chat_lines
    }

    /// Set the current activity
    pub fn set_activity(&mut self, activity: Activity) {
        self.activity = activity;
    }

    /// Get the current activity
    pub fn activity(&self) -> &Activity {
        &self.activity
    }

    /// Scroll up by n lines
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    /// Scroll down by n lines
    pub fn scroll_down(&mut self, n: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Get current scroll offset
    pub fn scroll_offset(&self) -> u16 {
        self.scroll_offset
    }

    /// Reset scroll to bottom (most recent)
    #[allow(dead_code)] // Reserved for future scroll behavior
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Request cancellation
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Check if cancelled
    #[allow(dead_code)] // Used by cancellation check loop
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Reset cancellation flag
    pub fn reset_cancellation(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }

    /// Get cancellation flag for sharing
    pub fn cancellation_flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    /// Update session stats after an interaction
    pub fn update_stats(&mut self, tokens: u32, tool_calls: usize) {
        self.estimated_tokens = tokens;
        self.interaction_count += 1;
        // Add tool call count if needed
        let _ = tool_calls;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===================
    // Activity enum tests
    // ===================

    #[test]
    fn test_activity_display() {
        assert_eq!(Activity::Idle.display(), "ready");
        assert_eq!(Activity::Streaming.display(), "streaming...");
        assert_eq!(
            Activity::Executing("read_file".to_string()).display(),
            "read_file"
        );
    }

    #[test]
    fn test_activity_is_busy() {
        assert!(!Activity::Idle.is_busy());
        assert!(Activity::Streaming.is_busy());
        assert!(Activity::Executing("bash".to_string()).is_busy());
    }

    #[test]
    fn test_activity_default() {
        let activity = Activity::default();
        assert!(matches!(activity, Activity::Idle));
    }

    // ===================
    // App::new tests
    // ===================

    #[test]
    fn test_app_new() {
        let app = App::new("gemini-flash");
        assert_eq!(app.model, "gemini-flash");
        assert_eq!(app.estimated_tokens, 0);
        assert_eq!(app.interaction_count, 0);
        assert!(!app.should_quit);
        assert!(app.chat_lines.is_empty());
        assert_eq!(app.scroll_offset, 0);
        assert!(matches!(app.activity, Activity::Idle));
    }

    // ===================
    // append_to_chat tests
    // ===================

    #[test]
    fn test_append_to_chat_single_line() {
        let mut app = App::new("test");
        app.append_to_chat("Hello world");
        assert_eq!(app.chat_lines.len(), 1);
        assert_eq!(app.chat_lines[0], "Hello world");
    }

    #[test]
    fn test_append_to_chat_multi_line() {
        let mut app = App::new("test");
        app.append_to_chat("Line 1\nLine 2\nLine 3");
        assert_eq!(app.chat_lines.len(), 3);
        assert_eq!(app.chat_lines[0], "Line 1");
        assert_eq!(app.chat_lines[1], "Line 2");
        assert_eq!(app.chat_lines[2], "Line 3");
    }

    #[test]
    fn test_append_to_chat_empty_string_adds_blank_line() {
        let mut app = App::new("test");
        app.append_to_chat("First");
        app.append_to_chat("");
        app.append_to_chat("Second");
        assert_eq!(app.chat_lines.len(), 3);
        assert_eq!(app.chat_lines[0], "First");
        assert_eq!(app.chat_lines[1], ""); // blank line
        assert_eq!(app.chat_lines[2], "Second");
    }

    #[test]
    fn test_append_to_chat_trailing_newline() {
        let mut app = App::new("test");
        app.append_to_chat("Line with newline\n");
        assert_eq!(app.chat_lines.len(), 2);
        assert_eq!(app.chat_lines[0], "Line with newline");
        assert_eq!(app.chat_lines[1], ""); // trailing empty line
    }

    #[test]
    fn test_append_to_chat_respects_max_lines() {
        // Use a smaller limit for testing by filling to capacity
        let mut app = App::new("test");

        // Fill to MAX_CHAT_LINES
        for i in 0..MAX_CHAT_LINES {
            app.append_to_chat(&format!("Line {i}"));
        }
        assert_eq!(app.chat_lines.len(), MAX_CHAT_LINES);
        assert_eq!(app.chat_lines[0], "Line 0");

        // Add one more - should evict the oldest
        app.append_to_chat("New line");
        assert_eq!(app.chat_lines.len(), MAX_CHAT_LINES);
        assert_eq!(app.chat_lines[0], "Line 1"); // Line 0 was evicted
        assert_eq!(app.chat_lines[MAX_CHAT_LINES - 1], "New line");
    }

    // ===================
    // append_streaming tests
    // ===================

    #[test]
    fn test_append_streaming_empty_does_nothing() {
        let mut app = App::new("test");
        app.append_streaming("");
        assert!(app.chat_lines.is_empty());
    }

    #[test]
    fn test_append_streaming_creates_line_if_empty() {
        let mut app = App::new("test");
        app.append_streaming("Hello");
        assert_eq!(app.chat_lines.len(), 1);
        assert_eq!(app.chat_lines[0], "Hello");
    }

    #[test]
    fn test_append_streaming_concatenates_to_existing() {
        let mut app = App::new("test");
        app.append_streaming("Hello");
        app.append_streaming(" world");
        assert_eq!(app.chat_lines.len(), 1);
        assert_eq!(app.chat_lines[0], "Hello world");
    }

    #[test]
    fn test_append_streaming_handles_newlines() {
        let mut app = App::new("test");
        app.append_streaming("First");
        app.append_streaming(" part\nSecond line");
        assert_eq!(app.chat_lines.len(), 2);
        assert_eq!(app.chat_lines[0], "First part");
        assert_eq!(app.chat_lines[1], "Second line");
    }

    #[test]
    fn test_append_streaming_multiple_newlines() {
        let mut app = App::new("test");
        app.append_streaming("A\nB\nC");
        assert_eq!(app.chat_lines.len(), 3);
        assert_eq!(app.chat_lines[0], "A");
        assert_eq!(app.chat_lines[1], "B");
        assert_eq!(app.chat_lines[2], "C");
    }

    #[test]
    fn test_append_streaming_respects_max_lines() {
        let mut app = App::new("test");

        // Fill to MAX_CHAT_LINES
        for i in 0..MAX_CHAT_LINES {
            app.append_to_chat(&format!("Line {i}"));
        }

        // Stream text with newline - should evict oldest
        app.append_streaming("\nNew streamed line");
        assert_eq!(app.chat_lines.len(), MAX_CHAT_LINES);
        assert_eq!(app.chat_lines[0], "Line 1"); // Line 0 was evicted
    }

    // ===================
    // clear_chat tests
    // ===================

    #[test]
    fn test_clear_chat() {
        let mut app = App::new("test");
        app.append_to_chat("Line 1");
        app.append_to_chat("Line 2");
        app.scroll_up(5);

        app.clear_chat();

        assert!(app.chat_lines.is_empty());
        assert_eq!(app.scroll_offset, 0);
    }

    // ===================
    // scroll tests
    // ===================

    #[test]
    fn test_scroll_up() {
        let mut app = App::new("test");
        assert_eq!(app.scroll_offset(), 0);

        app.scroll_up(5);
        assert_eq!(app.scroll_offset(), 5);

        app.scroll_up(10);
        assert_eq!(app.scroll_offset(), 15);
    }

    #[test]
    fn test_scroll_down() {
        let mut app = App::new("test");
        app.scroll_up(20);

        app.scroll_down(5);
        assert_eq!(app.scroll_offset(), 15);

        app.scroll_down(10);
        assert_eq!(app.scroll_offset(), 5);
    }

    #[test]
    fn test_scroll_down_saturates_at_zero() {
        let mut app = App::new("test");
        app.scroll_up(5);
        app.scroll_down(10); // Try to go below zero
        assert_eq!(app.scroll_offset(), 0);
    }

    #[test]
    fn test_scroll_to_bottom() {
        let mut app = App::new("test");
        app.scroll_up(100);
        app.scroll_to_bottom();
        assert_eq!(app.scroll_offset(), 0);
    }

    // ===================
    // activity tests
    // ===================

    #[test]
    fn test_set_and_get_activity() {
        let mut app = App::new("test");
        assert!(matches!(app.activity(), Activity::Idle));

        app.set_activity(Activity::Streaming);
        assert!(matches!(app.activity(), Activity::Streaming));

        app.set_activity(Activity::Executing("bash".to_string()));
        assert!(matches!(app.activity(), Activity::Executing(_)));
    }

    // ===================
    // cancellation tests
    // ===================

    #[test]
    fn test_cancellation_flag() {
        let app = App::new("test");
        assert!(!app.is_cancelled());

        app.cancel();
        assert!(app.is_cancelled());

        app.reset_cancellation();
        assert!(!app.is_cancelled());
    }

    #[test]
    fn test_cancellation_flag_sharing() {
        let app = App::new("test");
        let flag = app.cancellation_flag();

        assert!(!flag.load(Ordering::SeqCst));

        app.cancel();
        assert!(flag.load(Ordering::SeqCst));
    }

    // ===================
    // update_stats tests
    // ===================

    #[test]
    fn test_update_stats() {
        let mut app = App::new("test");
        assert_eq!(app.estimated_tokens, 0);
        assert_eq!(app.interaction_count, 0);

        app.update_stats(1500, 3);
        assert_eq!(app.estimated_tokens, 1500);
        assert_eq!(app.interaction_count, 1);

        app.update_stats(3000, 5);
        assert_eq!(app.estimated_tokens, 3000);
        assert_eq!(app.interaction_count, 2);
    }

    // =========================================
    // Regression tests for bugs we fixed
    // =========================================

    /// Test that streaming chunks concatenate properly, not stair-step.
    /// Bug: Each streaming chunk was creating a new line, causing:
    ///   "Hel"
    ///   "lo "
    ///   "wor"
    ///   "ld"
    /// Instead of: "Hello world"
    #[test]
    fn test_no_stairstepping_on_streaming() {
        let mut app = App::new("test");

        // Simulate streaming chunks arriving
        app.append_streaming("Hel");
        app.append_streaming("lo ");
        app.append_streaming("wor");
        app.append_streaming("ld!");

        // Should be ONE line, not four
        assert_eq!(app.chat_lines.len(), 1);
        assert_eq!(app.chat_lines[0], "Hello world!");
    }

    /// Test that streaming with embedded newlines works correctly.
    /// Bug: Newlines in streaming chunks caused stair-stepping.
    #[test]
    fn test_streaming_with_embedded_newlines() {
        let mut app = App::new("test");

        app.append_streaming("First line\nSecond ");
        app.append_streaming("line continues\nThird line");

        assert_eq!(app.chat_lines.len(), 3);
        assert_eq!(app.chat_lines[0], "First line");
        assert_eq!(app.chat_lines[1], "Second line continues");
        assert_eq!(app.chat_lines[2], "Third line");
    }

    /// Test that empty string creates blank line for spacing.
    /// Bug: `append_to_chat("")` did nothing because "".lines() yields nothing.
    /// Fix: Explicit check for empty string to add blank line.
    #[test]
    fn test_empty_string_creates_blank_line_for_spacing() {
        let mut app = App::new("test");

        // Simulate: user message, blank line, then tool result
        app.append_to_chat("> what can you do?");
        app.append_to_chat(""); // This MUST create a blank line
        app.append_to_chat("[read_file] 0.02s, ~100 tok");

        assert_eq!(app.chat_lines.len(), 3);
        assert_eq!(app.chat_lines[0], "> what can you do?");
        assert_eq!(app.chat_lines[1], ""); // blank line for spacing
        assert_eq!(app.chat_lines[2], "[read_file] 0.02s, ~100 tok");
    }

    /// Test newline after user messages pattern.
    /// Bug: "> what can you do?I am clemini" - no separation.
    /// The blank line is preserved for visual spacing.
    #[test]
    fn test_newline_after_user_message() {
        let mut app = App::new("test");

        // Pattern used in TUI: user message + blank line + streaming response
        app.append_to_chat("> user prompt");
        app.append_to_chat(""); // Creates blank line for visual spacing
        app.append_streaming("I am ");
        app.append_streaming("clemini");

        // Blank line is preserved - streaming creates new line after it
        assert_eq!(app.chat_lines.len(), 3);
        assert_eq!(app.chat_lines[0], "> user prompt");
        assert_eq!(app.chat_lines[1], ""); // Visible blank line
        assert_eq!(app.chat_lines[2], "I am clemini");
    }

    /// Test newline after tool results pattern.
    /// Bug: "[read_file] 0.000s, ~110 tokThe first" - no separation.
    /// The blank line is preserved for visual spacing.
    #[test]
    fn test_newline_after_tool_result() {
        let mut app = App::new("test");

        // Pattern: tool result (Line) + blank line + streaming continues
        app.append_to_chat("[read_file] 0.02s, ~110 tok");
        app.append_to_chat(""); // Creates blank line for visual spacing
        app.append_streaming("The file contains...");

        // Blank line is preserved
        assert_eq!(app.chat_lines.len(), 3);
        assert_eq!(app.chat_lines[0], "[read_file] 0.02s, ~110 tok");
        assert_eq!(app.chat_lines[1], ""); // Visible blank line
        assert_eq!(app.chat_lines[2], "The file contains...");
    }

    /// Test that streaming after Line message starts on new line.
    /// This simulates the TuiMessage::Line followed by TuiMessage::Streaming pattern.
    #[test]
    fn test_line_then_streaming_separation() {
        let mut app = App::new("test");

        // TuiMessage::Line handler adds blank line after
        app.append_to_chat("CALL bash command=\"ls\"");
        app.append_to_chat(""); // blank line added by Line handler

        // Then streaming continues - blank line preserved
        app.append_streaming("Output: ");
        app.append_streaming("file1.txt file2.txt");

        // Blank line is preserved for visual spacing
        assert_eq!(app.chat_lines.len(), 3);
        assert_eq!(app.chat_lines[0], "CALL bash command=\"ls\"");
        assert_eq!(app.chat_lines[1], ""); // Visible blank line
        assert_eq!(app.chat_lines[2], "Output: file1.txt file2.txt");
    }

    /// Test multiple blank lines don't collapse.
    #[test]
    fn test_multiple_blank_lines_preserved() {
        let mut app = App::new("test");

        app.append_to_chat("Line 1");
        app.append_to_chat("");
        app.append_to_chat("");
        app.append_to_chat("Line 2");

        assert_eq!(app.chat_lines.len(), 4);
        assert_eq!(app.chat_lines[1], "");
        assert_eq!(app.chat_lines[2], "");
    }

    /// Demonstrate the bug WITHOUT blank line - streaming appends to previous content.
    /// This is what caused "> what can you do?I am clemini" on one line.
    #[test]
    fn test_without_blank_line_streaming_appends_to_previous() {
        let mut app = App::new("test");

        app.append_to_chat("> user prompt");
        // NO blank line added!
        app.append_streaming("Response text");

        // Bug: response appends directly to user prompt line
        assert_eq!(app.chat_lines.len(), 1);
        assert_eq!(app.chat_lines[0], "> user promptResponse text"); // Wrong!
    }

    /// Show that blank line is the fix for the above bug.
    #[test]
    fn test_with_blank_line_streaming_is_separate() {
        let mut app = App::new("test");

        app.append_to_chat("> user prompt");
        app.append_to_chat(""); // The fix!
        app.append_streaming("Response text");

        // Correct: blank line preserved, response on its own line
        assert_eq!(app.chat_lines.len(), 3);
        assert_eq!(app.chat_lines[0], "> user prompt");
        assert_eq!(app.chat_lines[1], ""); // Visible blank line
        assert_eq!(app.chat_lines[2], "Response text");
    }

    // =========================================
    // Blank line preservation tests
    // =========================================

    /// Streaming into empty chat creates a line (no blank line to preserve).
    #[test]
    fn test_streaming_into_empty_chat() {
        let mut app = App::new("test");
        app.append_streaming("First chunk");
        app.append_streaming(" second chunk");

        assert_eq!(app.chat_lines.len(), 1);
        assert_eq!(app.chat_lines[0], "First chunk second chunk");
    }

    /// Streaming preserves blank lines - they stay visible.
    #[test]
    fn test_streaming_preserves_blank_lines() {
        let mut app = App::new("test");

        app.append_to_chat("Line 1");
        app.append_to_chat(""); // blank
        app.append_to_chat(""); // another blank
        app.append_streaming("Streamed");

        assert_eq!(app.chat_lines.len(), 4);
        assert_eq!(app.chat_lines[0], "Line 1");
        assert_eq!(app.chat_lines[1], "");
        assert_eq!(app.chat_lines[2], "");
        assert_eq!(app.chat_lines[3], "Streamed");
    }

    /// Streaming after non-empty line appends to it.
    #[test]
    fn test_streaming_appends_to_nonempty_line() {
        let mut app = App::new("test");

        app.append_to_chat("Start");
        app.append_streaming(" continued");

        assert_eq!(app.chat_lines.len(), 1);
        assert_eq!(app.chat_lines[0], "Start continued");
    }

    /// Full conversation flow with proper spacing.
    #[test]
    fn test_full_conversation_flow_with_spacing() {
        let mut app = App::new("test");

        // User message
        app.append_to_chat("> what can you do?");
        app.append_to_chat("");

        // Model response (streamed)
        app.append_streaming("I can help with ");
        app.append_streaming("coding tasks.");
        app.append_to_chat(""); // blank after response

        // Tool call
        app.append_to_chat("CALL read_file path=\"src/main.rs\"");
        app.append_to_chat("");

        // Tool result
        app.append_to_chat("[read_file] 0.02s, ~100 tok");
        app.append_to_chat("");

        // Continued response
        app.append_streaming("The file contains...");

        assert_eq!(app.chat_lines.len(), 9);
        assert_eq!(app.chat_lines[0], "> what can you do?");
        assert_eq!(app.chat_lines[1], "");
        assert_eq!(app.chat_lines[2], "I can help with coding tasks.");
        assert_eq!(app.chat_lines[3], "");
        assert_eq!(app.chat_lines[4], "CALL read_file path=\"src/main.rs\"");
        assert_eq!(app.chat_lines[5], "");
        assert_eq!(app.chat_lines[6], "[read_file] 0.02s, ~100 tok");
        assert_eq!(app.chat_lines[7], "");
        assert_eq!(app.chat_lines[8], "The file contains...");
    }

    // =========================================
    // Tool call interspersed with streaming tests
    // Regression tests for the bug where stream chunks used append_to_chat
    // instead of append_streaming, causing broken lines mid-sentence.
    // =========================================

    /// Bug: Using append_to_chat for stream chunks breaks sentences.
    /// Each chunk becomes its own line, breaking mid-word:
    ///   "I'll start by search"
    ///   "ing for the function"
    /// Instead of: "I'll start by searching for the function"
    #[test]
    fn test_stream_chunks_must_concatenate_not_create_lines() {
        let mut app = App::new("test");

        // Simulate what happens when AppEvent::StreamChunk uses append_to_chat (WRONG)
        app.append_to_chat("I'll start by search");
        app.append_to_chat("ing for the function");

        // Bug: creates 2 lines
        assert_eq!(app.chat_lines.len(), 2);
        // This is the broken behavior we DON'T want
    }

    /// Fix: Using append_streaming for stream chunks keeps sentences intact.
    #[test]
    fn test_stream_chunks_use_append_streaming() {
        let mut app = App::new("test");

        // Correct: AppEvent::StreamChunk should use append_streaming
        app.append_streaming("I'll start by search");
        app.append_streaming("ing for the function");

        // Correct: 1 line with complete sentence
        assert_eq!(app.chat_lines.len(), 1);
        assert_eq!(
            app.chat_lines[0],
            "I'll start by searching for the function"
        );
    }

    /// Tool calls must use append_to_chat to appear on their own line.
    /// This ensures tools don't get appended to streaming text.
    #[test]
    fn test_tool_calls_use_append_to_chat() {
        let mut app = App::new("test");

        // Streaming text
        app.append_streaming("Let me search");

        // Tool call (append_to_chat creates NEW line)
        app.append_to_chat("🔧 grep pattern=\"fn run_interaction\"");

        // More streaming (appends to last line since it's non-empty)
        app.append_streaming("Found it!");

        assert_eq!(app.chat_lines.len(), 2);
        assert_eq!(app.chat_lines[0], "Let me search");
        // Note: append_streaming appends to the tool line since it's non-empty
        assert_eq!(
            app.chat_lines[1],
            "🔧 grep pattern=\"fn run_interaction\"Found it!"
        );
    }

    /// Correct pattern: blank line after streaming before tool call.
    /// This is how TUI should display streaming → tool → streaming.
    #[test]
    fn test_streaming_tool_streaming_flow() {
        let mut app = App::new("test");

        // Model starts responding (streaming concatenates)
        app.append_streaming("I'll search for the ");
        app.append_streaming("function.\n\n"); // Model ends with newlines

        // Tool call (append_to_chat creates new lines)
        app.append_to_chat("🔧 grep pattern=\"fn run_interaction\"");
        app.append_to_chat("  └─ 20ms");
        app.append_to_chat(""); // blank line after tool completes

        // Model continues after tool (streaming after blank line creates new line)
        app.append_streaming("Found it in ");
        app.append_streaming("src/agent.rs");

        assert_eq!(app.chat_lines.len(), 7);
        assert_eq!(app.chat_lines[0], "I'll search for the function.");
        assert_eq!(app.chat_lines[1], ""); // from first \n in streaming
        assert_eq!(app.chat_lines[2], ""); // from second \n in streaming
        assert_eq!(app.chat_lines[3], "🔧 grep pattern=\"fn run_interaction\"");
        assert_eq!(app.chat_lines[4], "  └─ 20ms");
        assert_eq!(app.chat_lines[5], ""); // blank after tool
        assert_eq!(app.chat_lines[6], "Found it in src/agent.rs");
    }

    /// Multiple tool calls in sequence should each be on their own line.
    #[test]
    fn test_multiple_tool_calls_each_on_own_line() {
        let mut app = App::new("test");

        app.append_streaming("I'll read both files.\n\n");

        // First tool
        app.append_to_chat("🔧 read_file file_path=\"src/main.rs\"");
        app.append_to_chat("  └─ 5ms");
        app.append_to_chat("");

        // Second tool
        app.append_to_chat("🔧 read_file file_path=\"src/agent.rs\"");
        app.append_to_chat("  └─ 3ms");
        app.append_to_chat("");

        app.append_streaming("Both files loaded.");

        assert_eq!(app.chat_lines.len(), 10);
        assert_eq!(app.chat_lines[0], "I'll read both files.");
        assert_eq!(app.chat_lines[1], ""); // first \n
        assert_eq!(app.chat_lines[2], ""); // second \n
        assert_eq!(app.chat_lines[3], "🔧 read_file file_path=\"src/main.rs\"");
        assert_eq!(app.chat_lines[4], "  └─ 5ms");
        assert_eq!(app.chat_lines[5], "");
        assert_eq!(app.chat_lines[6], "🔧 read_file file_path=\"src/agent.rs\"");
        assert_eq!(app.chat_lines[7], "  └─ 3ms");
        assert_eq!(app.chat_lines[8], "");
        assert_eq!(app.chat_lines[9], "Both files loaded.");
    }
}

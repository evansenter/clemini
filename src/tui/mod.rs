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
    pub fn append_streaming(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        // Split into lines, handling partial lines properly
        let mut lines = text.split('\n').peekable();

        // First part appends to current line (or creates one if empty)
        if let Some(first) = lines.next() {
            if let Some(last_line) = self.chat_lines.back_mut() {
                last_line.push_str(first);
            } else {
                self.chat_lines.push_back(first.to_string());
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

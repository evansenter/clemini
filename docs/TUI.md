# TUI Architecture

This document describes clemini's terminal user interface built with ratatui.

## Overview

The TUI provides a full-screen terminal interface with:
- Status bar showing model, token count, interaction number, and activity
- Scrollable chat area for conversation history
- Input area with multi-line editing and history navigation

## Mode Selection

| Condition | Mode | Output Sink |
|-----------|------|-------------|
| `--no-tui` flag | Plain REPL | `TerminalSink` |
| Not a TTY (piped) | Plain REPL | `TerminalSink` |
| Default TTY | TUI | `TuiSink` |
| `--mcp-server` | MCP Server | `FileSink` |
| `-p "prompt"` | Non-interactive | `TerminalSink` |

## Layout

```
┌─────────────────────────────────────────────────────────┐
│ [clemini] model | ~Nk tokens | #N | activity            │  <- Status bar (1 line)
├─ Chat ──────────────────────────────────────────────────┤
│                                                         │
│ > user message                                          │
│ model response text...                                  │
│                                                         │
│                                                    ↑↓   │  <- Scrollbar (if needed)
├─ Input (Enter to send, Ctrl-D to quit) ─────────────────┤
│ user input here                                         │  <- tui-textarea widget
└─────────────────────────────────────────────────────────┘
```

## Key Bindings

| Key | Action |
|-----|--------|
| `Enter` | Submit input |
| `Ctrl-D` | Quit |
| `Escape` | Cancel current operation (during streaming/tool execution) |
| `Up/Down` | Navigate command history |
| `PageUp/PageDown` | Scroll chat area |

## Architecture

### Event Loop

The TUI uses `tokio::select!` to handle multiple event sources concurrently:

```rust
tokio::select! {
    // Keyboard input from crossterm EventStream
    Some(Ok(event)) = event_stream.next() => { ... }

    // Task completion events (tool progress, interaction complete)
    Some(event) = rx.recv() => { ... }

    // Output from TuiSink (streaming text, log messages)
    Some(message) = tui_rx.recv() => { ... }
}
```

### Output Channel

`TuiSink` sends output through an unbounded channel with tagged message types:

```rust
enum TuiMessage {
    Line(String),      // Complete line (uses append_to_chat)
    Streaming(String), // Partial chunk (uses append_streaming)
}
```

- `Line`: Used by `log_event()` for tool calls, errors, etc.
- `Streaming`: Used by `emit_streaming()` for model response chunks

### Text Accumulation

The `App` struct maintains a `VecDeque<String>` of chat lines:

- `append_to_chat()`: Splits text by newlines, adds each as a separate line
- `append_streaming()`: Appends to the current line, handles embedded newlines

This distinction prevents streaming chunks from creating stair-step patterns.

### Cancellation

Cancellation is implemented via `Arc<AtomicBool>`:

1. User presses Escape
2. `app.cancel()` sets the flag
3. A background task polls the flag and triggers `CancellationToken`
4. `run_interaction` checks cancellation between tool calls

## Rendering

### Status Bar

```
[clemini] gemini-3-flash-preview | ~2k tokens | #3 | streaming...
```

- Model name (green)
- Estimated token count (yellow)
- Interaction count (cyan)
- Activity: "ready" (green), "streaming..." (yellow), or tool name (yellow)

### Chat Area

- Uses `ansi-to-tui` to convert ANSI color codes to ratatui styles
- Word wrapping enabled with `Wrap { trim: true }`
- Scroll position tracked as offset from bottom (0 = latest)
- Scrollbar appears when content exceeds visible height

### Input Area

Uses `tui-textarea` widget with:
- Multi-line input support
- Basic editing (cursor movement, delete, etc.)
- History navigation via Up/Down arrows

## Differences from Plain REPL

| Aspect | TUI | Plain REPL |
|--------|-----|------------|
| Markdown rendering | Raw text (`###`, `**`) | termimad rendering |
| Streaming | Via channel to chat area | Direct print to stdout |
| Tool output | Via channel to chat area | Direct eprint to stderr |
| Cancellation | Escape key | Ctrl-C only |

## Files

| File | Purpose |
|------|---------|
| `src/tui/mod.rs` | `App` struct, `Activity` enum, state management |
| `src/tui/ui.rs` | Layout and rendering functions |
| `src/main.rs` | `TuiSink`, `TuiMessage`, event loop |

## Future Enhancements

- Markdown rendering in TUI (convert `**bold**` to styled text)
- Syntax highlighting in code blocks
- Split panes for tool output
- Settings panel (`/settings`)

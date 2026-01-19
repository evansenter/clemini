# Text Rendering Guidelines

This document defines the visual output standards for clemini across all output modes.

## Output Architecture

### OutputSink Trait

All user-facing output flows through the `OutputSink` trait, which has three implementations:

| Sink | Mode | Behavior |
|------|------|----------|
| `TerminalSink` | Plain REPL (`--no-tui`) | Writes to stderr + log files, uses termimad for markdown |
| `TuiSink` | TUI REPL (default) | Sends to TUI via channel + log files, plain text (no termimad) |
| `FileSink` | MCP Server | Writes to log files only |

The trait also includes `emit_streaming()` for real-time streaming output:
- `TerminalSink`: Uses `print!()` to stdout
- `TuiSink`: Sends `TuiMessage::Streaming` through channel
- `FileSink`: No-op (no terminal to stream to)

### Logging Functions

| Function | Purpose |
|----------|---------|
| `log_event(msg)` | Standard output through OutputSink (markdown-rendered) |
| `log_event_raw(msg)` | Output without markdown rendering (for protocol messages) |
| `log_to_file(msg)` | Write to log file only, bypassing terminal |

### Log Files

- Location: `~/.clemini/logs/`
- Naming: `clemini.log.YYYY-MM-DD`
- ANSI colors are preserved in human-readable logs

### EventHandler Trait

All three UI modes (Terminal, TUI, MCP) implement the `EventHandler` trait in `events.rs`. Handlers use shared formatting functions to ensure consistent output:

| Function | Output |
|----------|--------|
| `format_tool_executing()` | `┌─ tool_name args...` |
| `format_tool_result()` | `└─ tool_name 0.02s ~18 tok` |
| `format_error_detail()` | `  └─ error: message` |
| `format_tool_args()` | `key=value key2=value2` |
| `format_context_warning()` | Context window warnings |
| `render_streaming_chunk()` | Streaming text with markdown |

See [CLAUDE.md](../CLAUDE.md) for the full architecture.

## Color Palette

Uses the `colored` crate for ANSI terminal colors:

| Element | Color | Method |
|---------|-------|--------|
| Tool names | Cyan | `.cyan()` |
| Duration | Yellow | `.yellow()` |
| Error labels | Bright red + bold | `.bright_red().bold()` |
| Tool bracket (┌─) | Dimmed grey | `.dimmed()` |
| Tool arguments | Dimmed grey | `.dimmed()` |
| Bash command/output | Dimmed grey + italic | `.dimmed().italic()` |
| Diff deletions | Red | `.red()` |
| Diff additions | Green | `.green()` |
| Diff context | Dimmed grey | `.dimmed()` |
| Pending todos | Dimmed icon + text | `.dimmed()` |
| In-progress todos | Yellow icon | `.yellow()` |
| Completed todos | Green icon | `.green()` |

## Tool Call Format

### Executing Line (Before Execution)

```
┌─ <tool_name> <formatted_args>
```

- `┌─`: Dimmed
- `<tool_name>`: Cyan
- `<formatted_args>`: Dimmed grey, key=value pairs

Example:
```
┌─ read_file file_path="/src/main.rs"
```

### Result Line (After Execution)

```
└─ <tool_name> <duration> ~<tokens> tok<error_suffix>
```

- `└─` and tool name: Cyan
- Duration: Yellow, always in seconds (e.g., `0.02s`)
- Token estimate: Rough estimate (~4 chars per token of result JSON)
- Error suffix: ` ERROR` in bright red bold (only if error occurred)

Examples:
```
└─ read_file 0.02s ~18 tok
└─ bash 1.45s ~256 tok ERROR
```

### Error Detail Line

When a tool returns an error, show details:
```
  └─ error: <message>
```

- Prefix: Two spaces + tree character
- `error:` label
- Message: Dimmed grey

## Duration Formatting

All durations are displayed in seconds with 2-3 decimal places:

| Elapsed Time | Format | Example |
|--------------|--------|---------|
| < 1ms | 3 decimal places | `0.001s` |
| ≥ 1ms | 2 decimal places | `0.02s`, `1.45s` |

## Argument Display

### Truncation Rules

| Type | Max Length | Truncation |
|------|------------|------------|
| Strings | 80 chars | `"first 77 chars..."` |
| Arrays/Objects | N/A | Show as `...` |
| Numbers/Booleans | N/A | Show full value |
| Null | N/A | Show as `null` |

### Newline Handling

Newlines in string arguments are replaced with spaces before display.

### Format

Arguments displayed as space-separated key=value pairs:
```
path="/src/main.rs" line=42
command="echo hello world"
```

## Bash Tool Output

### Command Line

Before execution, show the command dimmed and italic:
```
[bash] running: "echo hello"
```

### Streaming Output

- Lines are shown dimmed and italic as they arrive
- Maximum 10 lines of stdout displayed
- Maximum 10 lines of stderr displayed
- After limit: `[...more stdout...]` or `[...more stderr...]`

Example:
```
[bash] running: "cargo build"
   Compiling clemini v0.1.0
   Compiling serde v1.0.0
[...more stdout...]
```

### Truncation in Results

Output returned to the model is truncated at 50,000 characters with message:
```
...
[truncated, N bytes total]
```

## Edit Tool Diff Output

When the edit tool successfully modifies a file, it displays a colored diff with syntax highlighting.

### Syntax Highlighting

Diffs use `syntect` for language-aware syntax highlighting based on file extension:
- Theme: Catppuccin Mocha (bundled at `themes/catppuccin-mocha.tmTheme`)
- Foreground colors: Language-specific token colors (keywords, strings, etc.)
- Background colors distinguish line types:
  - Deletions: Dark red background `rgb(80, 40, 40)`
  - Additions: Dark green background `rgb(40, 80, 40)`
  - Context: No background (dimmed)

Falls back to simple red/green coloring for unknown file types.

### Single-line changes (simple format)
```
  - old content here
  + new content here
```

### Multi-line changes (unified diff with context)
```
    context line before
  - removed line
  + added line
    context line after
```

- `-` marker: Red
- `+` marker: Green
- Line content: Syntax-highlighted with background color
- Context lines: Syntax-highlighted, dimmed (no background)
- Two-space indent before markers

## Todo List Display

### Icons

| Status | Icon | Color |
|--------|------|-------|
| Pending | `○` | Dimmed |
| In Progress | `→` | Yellow |
| Completed | `✓` | Green |

### Format

```
  ○ Pending task text (dimmed)
  → In progress task text
  ✓ Completed task text
```

Two-space indent before icon.

## Ask User Display

### Question

Question text with leading newline:
```

What is your preferred option?
```

### Options (if provided)

Numbered list:
```
1. Option one
2. Option two
```

### Prompt

Input prompt on same line:
```
>
```

## Response Text

### Markdown Rendering

Response text from the model is rendered using `termimad`:
- Headers left-aligned
- Code blocks preserved
- Paragraph spacing applied

### Logging

Response text is logged via `events::render_streaming_chunk()` which:
- Buffers text until complete lines are available
- Renders complete lines with termimad markdown styling
- Flushes remaining text when streaming completes via `events::flush_streaming_buffer()`

This ensures `tail -f` shows streaming text naturally while still applying markdown formatting.

## Spacing Guidelines

1. **After streaming text (before tool execution or OUT)**: Exactly one blank line.
   - If `flush_streaming_buffer()` returns `Some` → content normalized to `\n\n` → no extra blank needed
   - If `flush_streaming_buffer()` returns `None` → handler adds blank line manually
   - This handles both cases: model sends text with trailing `\n` (rendered immediately) or without (buffered)
2. **After tool result**: Single blank line (added by `on_tool_result`)
3. **After user input**: Single blank line
4. **Todo list**: Leading newline before list, no trailing newline

## Implementation Notes

### Markdown Rendering and Newlines

`termimad`'s `term_text()` includes trailing newlines. When using `TerminalSink`:
- Use `eprint!` (not `eprintln!`) when `render_markdown=true` to avoid double newlines
- Use `eprintln!` when `render_markdown=false` for protocol/structured messages

### Avoiding Duplicate Output

Tools should use `log_event()` only, not both `log_event()` and `eprintln!`. The `TerminalSink` handles terminal output automatically.

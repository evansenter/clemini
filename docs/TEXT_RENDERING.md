# Text Rendering Guidelines

This document defines the visual output standards for clemini across all output modes.

## Output Architecture

### OutputSink Trait

All user-facing output flows through the `OutputSink` trait, which has two implementations:

| Sink | Mode | Behavior |
|------|------|----------|
| `TerminalSink` | REPL | Writes to stderr + log files |
| `FileSink` | MCP Server | Writes to log files only |

### Logging Functions

| Function | Purpose |
|----------|---------|
| `log_event(msg)` | Standard output through OutputSink (markdown-rendered) |
| `log_event_raw(msg)` | Output without markdown rendering (for protocol messages) |
| `log_to_file(msg)` | Write to log file only, bypassing terminal |

### Log Files

- Location: `~/.clemini/logs/`
- Naming: `clemini.log.YYYY-MM-DD` (human-readable), `clemini.json.YYYY-MM-DD` (structured)
- ANSI colors are preserved in human-readable logs

## Color Palette

Uses the `colored` crate for ANSI terminal colors:

| Element | Color | Method |
|---------|-------|--------|
| Tool names | Cyan | `.cyan()` |
| Duration | Yellow | `.yellow()` |
| Error labels | Bright red + bold | `.bright_red().bold()` |
| CALL label | Magenta + bold | `.magenta().bold()` |
| CALL tool name | Purple | `.purple()` |
| CALL arguments | Dimmed grey | `.dimmed()` |
| Bash command/output | Dimmed grey | `.dimmed()` |
| Pending todos | Dimmed icon + text | `.dimmed()` |
| In-progress todos | Yellow icon | `.yellow()` |
| Completed todos | Green icon | `.green()` |

## Tool Call Format

### CALL Line (Before Execution)

```
CALL <tool_name> <formatted_args>
```

- `CALL`: Magenta, bold
- `<tool_name>`: Purple
- `<formatted_args>`: Dimmed grey, key=value pairs

Example:
```
CALL read_file path="/src/main.rs"
```

### Result Line (After Execution)

```
[<tool_name>] <duration><error_suffix>
```

- Brackets and tool name: Cyan
- Duration: Yellow, format varies by elapsed time
- Error suffix: ` ERROR` in bright red bold (only if error occurred)

Examples:
```
[read_file] 0.02s
[bash] 1.45s ERROR
```

### Error Detail Line

When a tool returns an error, show details:
```
  └─ error: <message>
```

- Prefix: Two spaces + tree character
- `error`: Red
- Message: Dimmed grey

## Duration Formatting

| Elapsed Time | Format | Example |
|--------------|--------|---------|
| < 1ms | `{:.3}s` | `0.001s` |
| ≥ 1ms | `{:.2}s` | `0.02s`, `1.45s` |

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

Before execution, show the command dimmed:
```
[bash] running: "echo hello"
```

### Streaming Output

- Lines are shown dimmed as they arrive
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

Response text is logged to file only (not duplicated to terminal since it's already streamed):
```rust
log_to_file(&format!("> {}", response_text.trim()));
```

## Spacing Guidelines

1. **Before CALL**: No extra blank line
2. **After tool result**: Standard line break
3. **Between response and tools**: Natural flow (no forced spacing)
4. **Todo list**: Leading newline before list, no trailing newline

## Implementation Notes

### Markdown Rendering and Newlines

`termimad`'s `term_text()` includes trailing newlines. When using `TerminalSink`:
- Use `eprint!` (not `eprintln!`) when `render_markdown=true` to avoid double newlines
- Use `eprintln!` when `render_markdown=false` for protocol/structured messages

### Avoiding Duplicate Output

Tools should use `log_event()` only, not both `log_event()` and `eprintln!`. The `TerminalSink` handles terminal output automatically.

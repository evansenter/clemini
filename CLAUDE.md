# CLAUDE.md

Project-specific instructions for Claude Code when working on clemini.

## Project Overview

Clemini is a Gemini-powered coding CLI built with genai-rs. It's designed to be self-improving - we use clemini to build clemini.

## Build & Test

```bash
cargo check          # Fast type checking
cargo build          # Debug build
cargo build --release  # Release build
cargo test           # Run tests
cargo clippy -- -D warnings  # Lint
cargo fmt            # Format
```

## Architecture

```
src/
├── main.rs          # CLI entry, REPL loop, streaming handler
└── tools/
    ├── mod.rs       # ToolService impl, path validation
    ├── read.rs      # Read file tool
    ├── write.rs     # Write file tool
    └── bash.rs      # Bash execution with safety checks
```

## Key Design Decisions

### Safety / Sandboxing
- All file operations restricted to cwd (no access outside working directory)
- Bash tool has blocklist for dangerous patterns (rm -rf /, fork bombs, etc.)
- Caution logging for sensitive commands (sudo, rm, mv, etc.)

### genai-rs Integration
- Uses `create_stream_with_auto_functions()` for streaming + auto tool execution
- Server-side storage for multi-turn conversations via `with_previous_interaction()`
- `ToolService` trait for stateful tools that share cwd context

## genai-rs Sharp Edges

When encountering pain points or API issues with genai-rs, file issues at:
https://github.com/evansenter/genai-rs/issues

Known issues to file:
- [ ] `InteractionBuilder` typestate makes conditional chaining awkward (can't reassign after `with_previous_interaction()`)

## Environment

- `GEMINI_API_KEY` - Required for API access
- Model: `gemini-3.0-flash-preview`

## Conventions

- Use Rust 2024 edition features (let chains, etc.)
- All tools return JSON with either success data or `{"error": "..."}`
- Log tool invocations to stderr: `[read: path]`, `[write: path (N bytes)]`, `[bash: cmd]`
- Token usage logged after each interaction: `[tokens: N in, M out]`

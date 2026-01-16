# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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

The CLI has two modes: single-prompt (`-p "prompt"`) and interactive REPL.

**Streaming with auto-functions**: Uses `create_stream_with_auto_functions()` which handles the tool execution loop automatically. The stream emits `ExecutingFunctions` before tool calls and `FunctionResults` after, with timing/token info.

**Tool sandboxing**: All tools share a `cwd` via `CleminiToolService`. Path validation (`validate_path`) ensures operations stay within the working directory. Bash has regex blocklists for dangerous patterns.

**Multi-turn conversations**: Server-side storage via `with_previous_interaction(interaction_id)`. Note: `system_instruction` is NOT inherited - must send on every turn.

## genai-rs Integration Notes

When encountering API issues, file at: https://github.com/evansenter/genai-rs/issues

Known issues:
- [#367](https://github.com/evansenter/genai-rs/issues/367) - `InteractionBuilder` typestate makes conditional chaining awkward
- [#368](https://github.com/evansenter/genai-rs/issues/368) - `FunctionExecutionResult` missing args field for logging

Debugging: `LOUD_WIRE=1` logs all HTTP requests/responses.

## Environment

- `GEMINI_API_KEY` - Required
- Model: `gemini-3-flash-preview`

## Conventions

- Rust 2024 edition (let chains, etc.)
- Tools return JSON: success data or `{"error": "..."}`
- Logging format: `[tool_name] 0.5s, 1.2k tokens (+50)` per call, `[total: N.Nk tokens (X in + Y out)]` at end

## Development Process

**Test features yourself before considering them done** - Run clemini and verify the feature works before reporting completion.

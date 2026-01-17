# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Important Distinction

- **This file (CLAUDE.md)** guides Claude Code when working on clemini's codebase
- **SYSTEM_PROMPT in src/main.rs** guides clemini itself (what Gemini sees)

When updating clemini's behavior, modify `SYSTEM_PROMPT` in main.rs. This file is for codebase conventions and development process.

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

The CLI has three modes: single-prompt (`-p "prompt"`), interactive REPL, and MCP server (`--mcp-server`).

**Streaming with auto-functions**: Uses `create_stream_with_auto_functions()` which handles the tool execution loop automatically. The stream emits `ExecutingFunctions` before tool calls and `FunctionResults` after, with timing/token info.

**Tool sandboxing**: All tools share a `cwd` via `CleminiToolService`. Path validation (`validate_path`) ensures operations stay within the working directory. Bash has regex blocklists for dangerous patterns.

**Multi-turn conversations**: Server-side storage via `with_previous_interaction(interaction_id)`. Note: `system_instruction` is NOT inherited - must send on every turn.

## genai-rs Integration Notes

When encountering API issues, file at: https://github.com/evansenter/genai-rs/issues

Known issues:
- [#367](https://github.com/evansenter/genai-rs/issues/367) - `InteractionBuilder` typestate makes conditional chaining awkward (PR #369 open)

Debugging: `LOUD_WIRE=1` logs all HTTP requests/responses.

## Environment

- `GEMINI_API_KEY` - Required
- Model: `gemini-3-flash-preview`

## Conventions

- Rust 2024 edition (let chains, etc.)
- Tools return JSON: success data or `{"error": "..."}`
- Logging format: `[tool_name] 0.5s, 1.2k tokens (+50)` per call, `[X→Y tok]` at end

## Development Process

**Test features yourself before considering them done** - Run clemini and verify the feature works before reporting completion.

**Always verify compilation** - After making changes, run `cargo check` or `cargo clippy -- -D warnings` before reporting completion. Never leave code in a non-compiling state.

**Always rebuild before testing** - After making ANY changes to clemini code, run `clemini_rebuild` and wait for completion BEFORE using `clemini_chat`. The rebuild replaces the running process, so calling `clemini_chat` too early will fail with AbortError.

**Minimal scope** - Only implement what was asked. Don't add "nice to have" features beyond the request. For example, if asked for a stdio server, don't also add HTTP support.

**Complete dependency management** - When using a new crate, ensure it's added to Cargo.toml with the proper features before writing code that depends on it. Never reference crates that aren't in dependencies.

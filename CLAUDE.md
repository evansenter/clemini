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
make check               # Fast type checking
make build               # Debug build
make release             # Release build
make test                # Run tests
make clippy              # Lint with warnings as errors
make fmt                 # Format code
make logs                # Tail human-readable logs
make json-logs           # Tail JSON logs with jq formatting
```

Logs are stored in `~/.clemini/logs/` with daily rotation.

## Architecture

The CLI has three modes: single-prompt (`-p "prompt"`), interactive REPL, and MCP server (`--mcp-server`). The interactive REPL uses a full-screen TUI by default; use `--no-tui` for plain terminal output.

### Module Structure

```
src/
├── main.rs          # CLI entry, UI loops (TUI/REPL), MCP server startup
├── agent.rs         # Core interaction logic, AgentEvent enum
├── diff.rs          # Diff formatting for edit tool output
├── events.rs        # EventHandler trait, TerminalEventHandler
├── mcp.rs           # MCP server implementation
├── tui/             # TUI mode (ratatui)
└── tools/           # Tool implementations (bash, read_file, etc.)
```

### Event-Driven Architecture

The agent (`src/agent.rs`) is decoupled from UI via channel-based events:

```
run_interaction()                    UI Layer
      │                                 │
      ├─► AgentEvent::TextDelta ───────►│ print/append to chat
      ├─► AgentEvent::ToolExecuting ───►│ log tool start
      ├─► AgentEvent::ToolResult ──────►│ log tool completion
      ├─► AgentEvent::ContextWarning ──►│ show warning
      └─► AgentEvent::Complete ────────►│ finalize
```

**`AgentEvent` enum** (`src/agent.rs`): Events emitted during interaction.
- `TextDelta(String)` - Streaming text chunk
- `ToolExecuting(Vec<OwnedFunctionCallInfo>)` - Tools about to run
- `ToolResult(FunctionExecutionResult)` - Tool completed (uses genai-rs type)
- `Complete { interaction_id, response }` - Interaction finished
- `ContextWarning { used, limit, percentage }` - Context window >80%
- `Cancelled` - User cancelled

**`EventHandler` trait** (`src/events.rs`): UI modes implement this to handle events.
- `TerminalEventHandler` - For plain REPL and non-interactive modes
- TUI mode handles events directly via `AppEvent` enum

### Core Functions

**`run_interaction()`** (`src/agent.rs`): Main interaction loop.
- Takes `events_tx: mpsc::Sender<AgentEvent>` channel
- Streams response, accumulates function calls from Delta chunks
- Executes tools via `execute_tools()`, sends results back to Gemini
- Loops until no more function calls

**Manual function calling**: Uses `create_stream()` instead of auto-function API. This enables ctrl-c cancellation between tool calls - the auto-function API executes tools internally, losing fine-grained cancellation control.

### Tool Sandboxing

All tools share a `cwd` via `CleminiToolService`. Path validation (`validate_path`) ensures operations stay within the working directory. Bash has regex blocklists for dangerous patterns.

### Multi-turn Conversations

Stateless via `with_previous_interaction(interaction_id)`. The MCP server passes `interaction_id` through (no server-side session storage). Note: `system_instruction` is NOT inherited - must send on every turn.

**When to reuse interaction_id**: Pass the previous interaction_id when iterating on the same task (e.g., sending feedback after reviewing clemini's changes, fixing errors it made). Start fresh (no interaction_id) for unrelated tasks. The ID encodes the full conversation history, so clemini remembers what files it modified and why.

**IMPORTANT**: Failing to reuse interaction_id is expensive - clemini loses all context and starts from scratch, re-reading files and rebuilding understanding. When delegating multi-step work via `clemini_chat`, ALWAYS capture the returned interaction_id and pass it to subsequent calls for the same task. Check MCP response or logs at `~/.clemini/logs/` if the ID isn't visible.

## genai-rs Integration Notes

When encountering API issues, file at: https://github.com/evansenter/genai-rs/issues

Debugging: `LOUD_WIRE=1` logs all HTTP requests/responses.

## Environment

- `GEMINI_API_KEY` - Required
- Model: `gemini-3-flash-preview`
- Config: `~/.clemini/config.toml` (optional)

## Documentation

- [docs/TUI.md](docs/TUI.md) - TUI architecture (ratatui, event loop, output channels)
- [docs/TEXT_RENDERING.md](docs/TEXT_RENDERING.md) - Output formatting guidelines (colors, truncation, spacing)

## Conventions

- Rust 2024 edition (let chains, etc.)
- Tools return JSON: success data or `{"error": "..."}`
- Tool errors return as JSON (not propagated) so Gemini can see them and retry

## Development Process

**Test features yourself before considering them done** - Run clemini and verify the feature works before reporting completion.

**Always verify compilation** - After making changes, run `cargo check` or `cargo clippy -- -D warnings` before reporting completion. Never leave code in a non-compiling state.

**Always rebuild before testing** - After making ANY changes to clemini code, run `clemini_rebuild` and wait for completion BEFORE using `clemini_chat`. The rebuild replaces the running process, so calling `clemini_chat` too early will fail with AbortError.

**Minimal scope** - Only implement what was asked. Don't add "nice to have" features beyond the request. For example, if asked for a stdio server, don't also add HTTP support.

**Complete dependency management** - When using a new crate, ensure it's added to Cargo.toml with the proper features before writing code that depends on it. Never reference crates that aren't in dependencies.

**Quality gates before pushing** - All of these must pass:
- `make clippy` (no warnings)
- `make fmt` then check for changes (formatted)
- `make test` (tests pass)

Don't skip tests. If a test is flaky or legitimately broken by your change, fix the test as part of the PR.

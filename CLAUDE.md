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
make test                # Unit tests only (fast, no API key)
make test-all            # Full suite including integration tests (requires GEMINI_API_KEY)
make clippy              # Lint with warnings as errors
make fmt                 # Format code
make logs                # Tail human-readable logs
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

**`EventHandler` trait** (`src/events.rs`): All UI modes implement this trait:
- `TerminalEventHandler` (`events.rs`) - Plain REPL and non-interactive modes
- `TuiEventHandler` (`main.rs`) - TUI mode, sends AppEvents via channel
- `McpEventHandler` (`mcp.rs`) - MCP server mode

All handlers use shared formatting functions:
- `format_tool_executing()` - Format tool executing line (`┌─ name args`)
- `format_tool_result()` - Format tool completion line (`└─ name duration ~tokens tok`)
- `format_tool_args()` - Format tool arguments as key=value pairs (used by format_tool_executing)
- `format_context_warning()` - Format context window warnings

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

- [docs/TOOLS.md](docs/TOOLS.md) - Tool reference, design philosophy, implementation guide
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

**Integration tests** - Tests in `tests/` that require `GEMINI_API_KEY` use semantic validation:
- `confirmation_tests.rs` - Confirmation flow for destructive commands
- `tool_output_tests.rs` - Tool output events and model interpretation
- `semantic_integration_tests.rs` - Multi-turn state, error recovery, code analysis

Run locally with: `cargo test --test <name> -- --include-ignored --nocapture`

These use `validate_response_semantically()` from `tests/common/mod.rs` - a second Gemini call with structured output that judges whether responses are appropriate. This provides a middle ground between brittle string assertions and purely structural checks.

**Visual output changes** - Tool output formatting is centralized in `src/events.rs`:

| Change | Location |
|--------|----------|
| Tool executing format (`┌─ name...`) | `format_tool_executing()` in `events.rs` |
| Tool result format (`└─ name...`) | `format_tool_result()` in `events.rs` |
| Tool error detail (`└─ error:...`) | `format_error_detail()` in `events.rs` |
| Tool args format (`key=value`) | `format_tool_args()` in `events.rs` |
| Context warnings | `format_context_warning()` in `events.rs` |
| Streaming text (markdown) | `render_streaming_chunk()` + `flush_streaming_buffer()` in `events.rs` |

All three EventHandler implementations (`TerminalEventHandler`, `TuiEventHandler`, `McpEventHandler`) use these shared functions, so changes apply everywhere automatically.

Test visual changes by running clemini in each mode and verifying the output looks correct.

## Design Principles

### Core Principles

| Principle | Meaning |
|-----------|---------|
| **Explicit over implicit** | No magical defaults. Clear code beats hidden behavior. If spacing/formatting varies by mode, that's a bug. |
| **Formatting owns visual output** | Format functions return complete visual blocks including spacing. Output layer just emits—no newline decisions. |
| **Pure rendering** | Format/render functions are pure: no side effects, no global state. Color control, file I/O, and logging happen in callers, not formatters. |
| **Graceful unknowns** | Unknown/unexpected data is logged and handled, not crashed on. Tool errors return JSON so the model can retry. |
| **Breaking changes over shims** | Clean breaks preferred. No deprecated wrappers, re-exports for compatibility, or `// legacy` code paths. |

### Architecture Principles

**Agent isolation** - The agent (`agent.rs`) emits structured events via channel. No formatting, colors, or UI logic. This keeps the agent testable and UI implementations independent.

**Unified implementations** - UI logic appearing in multiple modes (Terminal, TUI, MCP) belongs in shared functions, not duplicated per-handler. Examples: `format_tool_executing()`, `render_streaming_chunk()`.

**Handlers near dependencies** - EventHandler implementations live where their protocol-specific types are:
- `TerminalEventHandler` in `events.rs` (generic)
- `TuiEventHandler` in `main.rs` (needs `AppEvent`)
- `McpEventHandler` in `mcp.rs` (needs MCP notification channel)

**Tool output via events** - Tools emit `AgentEvent::ToolOutput` for visual output, never call `log_event()` directly. This ensures correct ordering (all output flows through the event channel) and keeps tools decoupled from the UI layer. The standard `emit()` helper pattern:
```rust
fn emit(&self, output: &str) {
    if let Some(tx) = &self.events_tx {
        let _ = tx.try_send(AgentEvent::ToolOutput(output.to_string()));
    } else {
        crate::logging::log_event(output);
    }
}
```
Uses `try_send` (non-blocking) to avoid stalling tools on slow consumers. The fallback to `log_event()` allows tools to work in contexts where events aren't available (e.g., direct tool tests).

### Module Responsibilities

| Module | Responsibility |
|--------|----------------|
| `agent.rs` | Core interaction logic, `AgentEvent` enum, `run_interaction()` |
| `events.rs` | `EventHandler` trait, formatting functions, streaming text rendering |
| `main.rs` | CLI entry, UI loops, OutputSink implementations, `TuiEventHandler` |
| `mcp.rs` | MCP server protocol, `McpEventHandler` |
| `tui/` | TUI components, ratatui widgets, `AppEvent` |

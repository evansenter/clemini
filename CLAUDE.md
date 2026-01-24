# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Important Distinction

- **This file (CLAUDE.md)** guides Claude Code when working on clemini's codebase
- **System prompt in src/system_prompt.md** guides clemini itself (what Gemini sees)

When updating clemini's behavior, modify `src/system_prompt.md`. This file is for codebase conventions and development process.

## Project Overview

Clemini is a Gemini-powered coding CLI built with genai-rs. It's designed to be self-improving - we use clemini to build clemini.

## Build & Test

```bash
make check               # Fast type checking
make build               # Debug build
make release             # Release build
make run                 # Run the CLI
make test                # Unit tests only (fast, no API key)
make test-all            # Full suite including integration tests (requires GEMINI_API_KEY)
make clippy              # Lint with warnings as errors
make fmt                 # Format code
make logs                # Tail human-readable logs
```

Logs are stored in `~/.clemini/logs/` with daily rotation.

## Architecture

The CLI has three modes: single-prompt (`-p "prompt"`), interactive REPL, and MCP server (`--mcp-server`).

### Workspace Structure

This project is a Cargo workspace with two crates:

```
.
├── Cargo.toml       # Workspace root
├── src/             # clemini crate (AI agent)
└── crates/
    └── clemitui/    # TUI library crate (reusable by any ACP agent)
```

#### clemini (AI Agent)

```
src/
├── main.rs          # CLI entry, REPL loop, MCP server startup
├── lib.rs           # Library crate exposing core types for integration tests
├── acp.rs           # ACP server implementation
├── acp_client.rs    # ACP client for spawning subagents
├── agent.rs         # Core interaction logic, AgentEvent enum
├── diff.rs          # Diff formatting for edit tool output
├── event_bus.rs     # Cross-session event bus (SQLite-backed)
├── events.rs        # EventHandler trait, TerminalEventHandler
├── format.rs        # Re-exports clemitui + genai-rs-specific formatters
├── logging.rs       # Re-exports clemitui::logging
├── mcp.rs           # MCP server implementation
├── plan.rs          # Plan mode manager
├── system_prompt.md # System prompt for Gemini (included at compile time)
└── tools/           # Tool implementations
    ├── mod.rs       # CleminiToolService, ToolEmitter trait, EventsGuard
    ├── tasks.rs     # Unified task registry (Task enum, namespaced IDs)
    ├── bash/        # BashTool (mod.rs) + safety patterns (safety.rs)
    └── ...          # Individual tool modules (edit, read, grep, etc.)
```

#### clemitui (TUI Library)

Standalone crate for terminal UI, usable by any ACP-compatible agent:

```
crates/clemitui/
├── Cargo.toml
├── src/
│   ├── lib.rs       # Re-exports
│   ├── format.rs    # Primitive formatting functions (tool output, warnings)
│   ├── logging.rs   # OutputSink trait, log_event functions
│   └── text_buffer.rs # TextBuffer for streaming markdown
└── tests/
    ├── common/mod.rs        # Shared test helpers (strip_ansi, RAII guards, CaptureSink)
    ├── acp_simulation_tests.rs  # 29 tests simulating ACP agent patterns
    └── e2e_tests.rs         # 19 PTY-based tests for actual terminal output
```

**Design**: clemitui takes primitive types (strings, durations, token counts), not genai-rs types. This allows it to work with any ACP agent. clemini's format.rs re-exports these and adds genai-rs-specific wrappers.

Run clemitui tests: `cargo test -p clemitui`

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
- `ToolOutput(String)` - Tool output to display (emitted by tools via `ToolEmitter` trait)
- `Complete { interaction_id, response }` - Interaction finished
- `ContextWarning(ContextWarning)` - Context window >80%
- `Cancelled` - User cancelled
- `Retry { attempt, max_attempts, delay, error }` - API retry in progress

**`EventHandler` trait** (`src/events.rs`): All UI modes implement this trait:
- `TerminalEventHandler` (`events.rs`) - REPL and non-interactive modes
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

- [CHANGELOG.md](CHANGELOG.md) - Version history and notable changes
- [docs/TOOLS.md](docs/TOOLS.md) - Tool reference, design philosophy, implementation guide
- [docs/TEXT_RENDERING.md](docs/TEXT_RENDERING.md) - Output formatting guidelines (colors, truncation, spacing)

**Changelog updates required**: Any user-facing changes (new features, behavior changes, bug fixes) must be documented in CHANGELOG.md before merging.

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
- `make fmt` (run formatter, then commit any changes it makes)
- `make test` (tests pass)

Don't skip tests. If a test is flaky or legitimately broken by your change, fix the test as part of the PR.

**Comprehensive test coverage** - New code requires tests. When adding or modifying functionality:
- New modules need unit tests in the same file or a `tests` submodule
- New tools need tests covering success cases, error cases, and edge cases
- Refactors that change behavior need tests proving the new behavior
- Bug fixes need regression tests that would have caught the bug

If you're unsure whether coverage is sufficient, add more tests. Undertesting causes regressions; overtesting just means slightly longer CI.

**Integration tests** - Tests in `tests/` that require `GEMINI_API_KEY` use semantic validation:
- `confirmation_tests.rs` - Confirmation flow for destructive commands
- `tool_output_tests.rs` - Tool output events and model interpretation
- `semantic_integration_tests.rs` - Multi-turn state, error recovery, code analysis
- `acp_integration_tests.rs` - ACP subagent spawning and communication
- `background_tasks_tests.rs` - Background task execution and output retrieval
- `comprehensive_agent_tests.rs` - Agent interaction patterns, tool chaining, error recovery
- `plan_mode_tests.rs` - Plan mode entry/exit, tool restrictions, state management
- `terminal_tests.rs` - PTY-based REPL tests (history, ctrl-c, shell escape, builtins)
- `event_ordering_tests.rs` - Tool output event ordering (no API key required)

Run locally with: `cargo test --test <name> -- --include-ignored --nocapture`

These use `validate_response_semantically()` from `tests/common/mod.rs` - a second Gemini call with structured output that judges whether responses are appropriate. This provides a middle ground between brittle string assertions and purely structural checks.

**Shared test helpers** - Common patterns for test utilities:
- Put shared helpers in `tests/common/mod.rs` (for clemini) or `crates/clemitui/tests/common/mod.rs` (for clemitui)
- Use `#![allow(dead_code)]` in shared test modules since not all test files use all helpers
- RAII guards for cleanup: `DisableColors` (reset color override on drop), `LoggingGuard` (disable logging on drop)
- Pattern: `let _guard = DisableColors::new();` at test start ensures cleanup even on panic

**Flaky test handling** - Tests using LLM calls can be non-deterministic:
- Use `temperature: 0` in test API calls for more determinism (not always sufficient)
- Semantic validation is preferred over exact string matching
- If a test is inherently flaky due to LLM non-determinism, track it in an issue and consider:
  - Retry logic with max attempts
  - Mocking the LLM call for unit tests
  - Moving to integration test suite (run with `--include-ignored`)
- Never skip flaky tests silently - fix or track them

**Visual output changes** - Tool output formatting is centralized in `src/format.rs`:

| Change | Location |
|--------|----------|
| Tool executing format (`┌─ name...`) | `format_tool_executing()` in `format.rs` |
| Tool result format (`└─ name...`) | `format_tool_result()` in `format.rs` |
| Tool error detail (`└─ error:...`) | `format_error_detail()` in `format.rs` |
| Tool args format (`key=value`) | `format_tool_args()` in `format.rs` |
| Context warnings | `format_context_warning()` in `format.rs` |
| Streaming text (markdown) | `TextBuffer::push()` + `TextBuffer::flush()` in `format.rs` |

Both EventHandler implementations (`TerminalEventHandler`, `McpEventHandler`) use these shared functions, so changes apply everywhere automatically.

Test visual changes by running clemini in each mode and verifying the output looks correct.

**Output formatting tests are critical** - The output formatting has strict contracts that were hard to get right. Keep the test coverage comprehensive:

- `src/format.rs` tests: Format function contracts (newlines, indentation, structure)
- `src/main.rs` output_tests: Log file spacing, complete tool blocks, edge cases
- `tests/event_ordering_tests.rs`: End-to-end event ordering and output

When modifying output code, ensure all these tests pass. Add new tests for any new format patterns. Regressions in output spacing are subtle and hard to catch without tests.

## Design Principles

### Core Principles

| Principle | Meaning |
|-----------|---------|
| **Explicit over implicit** | No magical defaults. Clear code beats hidden behavior. If spacing/formatting varies by mode, that's a bug. |
| **Graceful unknowns** | Unknown/unexpected data is logged and handled, not crashed on. Tool errors return JSON so the model can retry. |
| **Formatting owns visual output** | Format functions return complete visual blocks including spacing. Output layer just emits—no newline decisions. |
| **Pure rendering** | Format/render functions are pure: no side effects, no global state. Color control, file I/O, and logging happen in callers, not formatters. |
| **Format helpers for all output** | All colored/styled output uses `format_*` helper functions. No inline `.cyan()`, `.bold()`, etc. in handlers or business logic. Keeps formatting testable and centralized. |
| **Breaking changes over shims** | Clean breaks preferred. No deprecated wrappers, re-exports for compatibility, or `// legacy` code paths. |

### Architecture Principles

**Agent isolation** - The agent (`agent.rs`) emits structured events via channel. No formatting, colors, or UI logic. This keeps the agent testable and UI implementations independent.

**Unified implementations** - UI logic appearing in multiple modes (Terminal, MCP) belongs in shared functions, not duplicated per-handler. Examples: `format_tool_executing()`, `format_tool_result()`, `TextBuffer`.

**Handlers near dependencies** - EventHandler implementations live where their protocol-specific types are:
- `TerminalEventHandler` in `events.rs` (generic terminal output)
- `McpEventHandler` in `mcp.rs` (needs MCP notification channel)

**Tool output via events** - Tools emit `AgentEvent::ToolOutput` for visual output, never call `log_event()` directly. This ensures correct ordering (all output flows through the event channel) and keeps tools decoupled from the UI layer. Tools implement the `ToolEmitter` trait (`src/tools/mod.rs`):
```rust
pub trait ToolEmitter {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>>;

    fn emit(&self, output: &str) {
        if let Some(tx) = self.events_tx() {
            let _ = tx.try_send(AgentEvent::ToolOutput(output.to_string()));
        } else {
            crate::logging::log_event(output);
        }
    }
}
```
Uses `try_send` (non-blocking) to avoid stalling tools on slow consumers. The fallback to `log_event()` allows tools to work in contexts where events aren't available (e.g., direct tool tests).

### Module Responsibilities

| Module | Responsibility |
|--------|----------------|
| `agent.rs` | Core interaction logic, `AgentEvent` enum, `run_interaction()` |
| `events.rs` | `EventHandler` trait, `TerminalEventHandler`, event dispatch |
| `format.rs` | Pure formatting functions, `TextBuffer`, markdown rendering |
| `main.rs` | CLI entry, REPL loop, OutputSink implementations |
| `mcp.rs` | MCP server protocol, `McpEventHandler` |

### Output Streams (stdout vs stderr)

**stdout** - The AI conversation (what you'd pipe to a file to save the chat):
- Model text responses
- Tool output

**stderr** - Session status and diagnostics:
- Startup banner and tip
- User input echo (visual feedback)
- Builtin command responses (`/model`, `/pwd`, etc.)
- Status messages (`[conversation cleared]`)
- ctrl-c message
- Error messages

This separation allows `clemini -p "prompt" > output.txt` to capture just the conversation.

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-01-24

### Added
- **Reedline REPL**: Line editing, persistent history (10,000 entries), Ctrl-R search
- **clemitui crate**: Standalone TUI library for ACP-compatible agents
- **Event bus**: Cross-session coordination via SQLite-backed pub/sub
- **Plan mode**: Structured planning with tool restrictions and user approval
- **ACP server**: Agent Client Protocol implementation for subagent spawning
- **TaskOutput tool**: Retrieve results from background tasks
- **Comprehensive test suite**: PTY-based terminal tests, plan mode tests, ACP simulation tests

### Changed
- Improved Ctrl-C handling via `tokio::select!` with biased polling
- Extracted formatting functions to clemitui for reuse by other agents
- Consolidated tool formatting into pure functions

### Fixed
- Made `test_command_safety_classification` more deterministic
- Fixed newlines in tool output formatting

## [0.3.0] - 2026-01-15

### Added
- **Task tool**: Spawn subagents for complex operations
- **Diff highlighting**: Syntax highlighting with Catppuccin Mocha theme
- **Auto-retry**: Automatic retry on transient API failures

### Changed
- Simplified streaming architecture to full buffering
- Consolidated event handling

### Fixed
- Prevent self-confirmation of dangerous commands
- Normalize spacing before tools and OUT lines

## [0.2.0] - 2026-01-10

### Added
- MCP server mode (`--mcp-server`)
- Tool confirmation flow for destructive commands
- Background task execution

### Changed
- Refactored to event-driven architecture
- Improved tool output formatting

## [0.1.0] - 2026-01-01

### Added
- Initial release
- Interactive REPL with streaming output
- Single prompt mode (`-p "prompt"`)
- Built-in tools: bash, read, write, edit, glob, grep
- Tool sandboxing with path validation

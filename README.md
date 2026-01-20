# Clemini

Clemini is a Gemini-powered coding CLI built with [genai-rs](https://github.com/evansenter/genai-rs). It's designed to be self-improving - we use clemini to build clemini.

## Features

- **Interactive REPL**: Terminal-based conversation with streaming output
- **Single Prompt Mode**: Run one-off commands with `-p "your prompt"`
- **MCP Server**: Expose clemini as an MCP tool for Claude Code (`--mcp-server`)
- **Tool Integration**: Built-in tools for file operations, bash execution, searching, and more
- **Self-Improving**: Optimized for working on its own codebase

## Prerequisites

- Rust toolchain (2024 edition, requires Rust 1.88+)
- `GEMINI_API_KEY` environment variable set with a valid Google Gemini API key

## Installation

```bash
cargo install --path .
```

## Usage

Start the interactive REPL:
```bash
clemini
```

Run a single prompt:
```bash
clemini -p "summarize the current directory"
```

Start as MCP server (for Claude Code integration):
```bash
clemini --mcp-server
```

### REPL Commands

- `/h`, `/help` - Show available commands
- `/c`, `/clear` - Clear conversation history
- `/q`, `/quit`, `/exit` - Exit the REPL
- `/v`, `/version` - Show version and model
- `/m`, `/model` - Show model name
- `/pwd`, `/cwd` - Show current working directory
- `/d`, `/diff` - Show git diff
- `/s`, `/status` - Show git status
- `/l`, `/log` - Show recent git log
- `/b`, `/branch` - Show git branches
- `! <command>` - Run shell command directly

## Development

### Build & Test

```bash
make check               # Fast type checking
make build               # Debug build
make release             # Release build
make test                # Run tests
make clippy              # Lint with warnings as errors
make fmt                 # Format code
make logs                # Tail human-readable logs
```

### Environment Variables

- `GEMINI_API_KEY`: Required for API access
- `LOUD_WIRE=1`: Log all HTTP requests and responses for debugging

### Configuration

Optional config file at `~/.clemini/config.toml`:
```toml
model = "gemini-3-flash-preview"
bash_timeout = 120
allowed_paths = ["~/Documents/projects", "/tmp"]
```

- `model`: Gemini model to use (default: `gemini-3-flash-preview`)
- `bash_timeout`: Timeout in seconds for bash commands (default: 120)
- `allowed_paths`: Additional paths tools can access beyond cwd (default: none)

Logs are stored in `~/.clemini/logs/` with daily rotation.

## License

[MIT](LICENSE)

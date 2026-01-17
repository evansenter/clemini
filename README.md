# Clemini

Clemini is a Gemini-powered coding CLI built with [genai-rs](https://github.com/evansenter/genai-rs). It's designed to be self-improving - we use clemini to build clemini.

## Features

- **Interactive REPL**: A conversational interface for coding tasks.
- **Single Prompt Mode**: Run one-off commands with `-p "your prompt"`.
- **Tool Integration**: Built-in tools for file operations, bash execution, searching, and more.
- **Self-Improving**: Optimized for working on its own codebase.

## Prerequisites

- Rust toolchain (2024 edition)
- `GEMINI_API_KEY` environment variable set with a valid Google Gemini API key.

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

## Development

### Build & Test

```bash
cargo check          # Fast type checking
cargo build          # Debug build
cargo test           # Run tests
cargo clippy -- -D warnings  # Lint
cargo fmt            # Format
```

### Environment Variables

- `GEMINI_API_KEY`: Required for API access.
- `LOUD_WIRE=1`: Log all HTTP requests and responses for debugging.

## License

[MIT](LICENSE) (or specify your license)

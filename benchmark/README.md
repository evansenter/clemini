# clemini Polyglot Benchmark

This directory contains a multi-language benchmark for `clemini`. It uses exercises from the [Aider Polyglot Benchmark](https://github.com/Aider-AI/polyglot-benchmark), which are based on Exercism exercises.

## Supported Languages

- **Python**: uses `pytest`
- **Rust**: uses `cargo test`
- **Go**: uses `go test`
- **JavaScript**: uses `npm test` or `node`
- **Java**: uses `gradle`
- **C++**: uses `cmake` and `make`

## Prerequisites

Ensure you have the necessary toolchains installed for the languages you want to benchmark:
- Python 3.9+, `pytest`
- Rust, `cargo`
- Go
- Node.js, `npm`
- Java, `gradle`
- CMake, `make`, C++ compiler
- Gemini API key set in `GEMINI_API_KEY` environment variable.

## Setup

Before running the benchmark for the first time, you need to fetch the exercises:

```bash
python3 benchmark/setup.py
```

This script clones the `polyglot-benchmark` repository and populates the `benchmark/exercises/` directory with exercises for all supported languages. Each exercise is prefixed with its language (e.g., `rust-affine-cipher`).

## Running the Benchmark

From the root of the repository, run:

```bash
python3 benchmark/run.py
```

### Options

- `--parallel N`: Run `N` exercises in parallel (default: 2).
- `--time-limit M`: Run for `M` minutes (default: 5). Shuffles exercise order and stops starting new exercises when time is up.

### Examples

Run a quick 5-minute test with 3 exercises in parallel:
```bash
python3 benchmark/run.py --time-limit 5 --parallel 3
```

Run all exercises sequentially with no time limit:
```bash
python3 benchmark/run.py --time-limit 0 --parallel 1
```

## How it Works

1. **Discovery**: The script finds all exercise directories in `benchmark/exercises/`.
2. **Implementation**: For each exercise, `clemini` is provided with the instructions and the stub files.
3. **Verification**: It runs the language-specific test suite.
4. **Retry**: If the tests fail, it provides the error output back to `clemini` for a single retry attempt.
5. **Reporting**: Reports the final pass/fail status, language, and time taken.

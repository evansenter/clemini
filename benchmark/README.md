# clemini Benchmark

This directory contains an aider-style benchmark for `clemini`. It uses Python exercises from the [Aider Polyglot Benchmark](https://github.com/Aider-AI/polyglot-benchmark), which are based on Exercism exercises.

## Prerequisites

- Python 3.9+
- `pytest` installed (`pip install pytest`)
- Rust and `cargo` (to run `clemini`)
- Gemini API key set in `GEMINI_API_KEY` environment variable.

## Setup

Before running the benchmark for the first time, you need to fetch the exercises:

```bash
python3 benchmark/setup.py
```

This script clones the `polyglot-benchmark` repository and populates the `benchmark/exercises/` directory with Python exercises.

## Running the Benchmark

From the root of the repository, run:

```bash
python3 benchmark/run.py
```

### Options

- `--parallel N`: Run `N` exercises in parallel.
- `--time-limit M`: Run for `M` minutes. Shuffles exercise order and stops starting new exercises when time is up.

### Examples

Run a quick 5-minute test with 3 exercises in parallel:
```bash
python3 benchmark/run.py --time-limit 5 --parallel 3
```

Run all exercises sequentially:
```bash
python3 benchmark/run.py
```

## How it Works

1. For each exercise, the script provides the instructions and the stub file to `clemini`.
2. It runs the tests using `pytest`.
3. If the tests fail, it provides the error output to `clemini` for a single retry.
4. Reports the final pass/fail status and time taken.

## Structure

- `exercises/`: Contains subdirectories for each exercise with:
  - `instructions.md`: The problem description.
  - `<exercise>.py`: The stub file to be implemented.
  - `<exercise>_test.py`: The test suite.
- `run.py`: The benchmark harness script.
- `setup.py`: Script to fetch and update exercises.

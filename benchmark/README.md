# clemini Benchmark

This directory contains an aider-style benchmark for `clemini`. It uses a subset of Python exercises from the [Aider Polyglot Benchmark](https://github.com/Aider-AI/polyglot-benchmark), which are based on Exercism exercises.

## Prerequisites

- Python 3.10+
- `pytest` installed (`pip install pytest`)
- Rust and `cargo` (to run `clemini`)
- Gemini API key set in `GEMINI_API_KEY` environment variable.

## Exercise Subset

The benchmark currently runs on the following 5 exercises:
- `proverb`
- `grade-school`
- `phone-number`
- `bowling`
- `simple-linked-list`

## Running the Benchmark

From the root of the repository, run:

```bash
python benchmark/run.py
```

The script will:
1. For each exercise, provide the instructions and the stub file to `clemini`.
2. Run the tests using `pytest`.
3. If the tests fail, it will provide the error output to `clemini` for a single retry.
4. Report the final pass/fail status for each exercise.

## Structure

- `exercises/`: Contains subdirectories for each exercise with:
  - `instructions.md`: The problem description.
  - `<exercise>.py`: The stub file to be implemented.
  - `<exercise>_test.py`: The test suite.
- `run.py`: The benchmark harness script.

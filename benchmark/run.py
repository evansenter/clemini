import os
import subprocess
import sys
import argparse
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor, as_completed

# List of exercises to run
EXERCISES = ["proverb", "grade-school", "phone-number", "bowling", "simple-linked-list"]

def run_clemini(prompt, cwd):
    """Call clemini via subprocess with the given prompt."""
    # Assume we are running from the repo root
    cmd = [
        "cargo", "run", "--quiet", "--",
        "--prompt", prompt,
        "--cwd", str(cwd)
    ]
    # Set CLEMINI_LOG to a file in the exercise directory to avoid cluttering or for debugging
    env = os.environ.copy()
    env["CLEMINI_LOG"] = str(Path(cwd) / "clemini.log")
    
    process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
    stdout, stderr = process.communicate()
    return stdout, stderr, process.returncode

def run_tests(exercise_path, test_file):
    """Run pytest on the test file and return success status and output."""
    # We use pytest to run the unittest-based tests
    cmd = [sys.executable, "-m", "pytest", "-v", str(test_file)]
    process = subprocess.Popen(cmd, cwd=exercise_path, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    stdout, stderr = process.communicate()
    output = stdout + stderr
    return process.returncode == 0, output

def run_exercise(ex, base_dir):
    """Logic for a single exercise run, suitable for parallel execution."""
    ex_dir = base_dir / ex
    code_file_name = ex.replace("-", "_") + ".py"
    test_file_name = ex.replace("-", "_") + "_test.py"
    
    instr_file = ex_dir / "instructions.md"
    if not instr_file.exists():
        return ex, "SKIPPED", f"instructions.md not found"
        
    with open(instr_file, "r") as f:
        instructions = f.read()
        
    prompt = f"Implement the code in {code_file_name} to pass the tests in {test_file_name}. Here are the instructions: {instructions}"
    
    # Initial attempt
    run_clemini(prompt, ex_dir)
    passed, output = run_tests(ex_dir, test_file_name)
    
    if not passed:
        # Retry once
        retry_prompt = f"The tests failed with the following output:\n\n{output}\n\nPlease fix the implementation in {code_file_name} to pass the tests."
        run_clemini(retry_prompt, ex_dir)
        passed, output = run_tests(ex_dir, test_file_name)
        
    return ex, "PASSED" if passed else "FAILED", None

def main():
    parser = argparse.ArgumentParser(description="Run clemini benchmark on exercises.")
    parser.add_argument("--parallel", type=int, default=len(EXERCISES), help="Number of exercises to run in parallel.")
    args = parser.parse_args()

    repo_root = Path(__file__).parent.parent.absolute()
    os.chdir(repo_root)
    
    base_dir = Path("benchmark/exercises")
    
    print("Starting clemini benchmark...")
    print(f"Exercises: {', '.join(EXERCISES)}")
    print(f"Parallelism: {args.parallel}")
    print("-" * 40)
    
    results = {}
    
    with ThreadPoolExecutor(max_workers=args.parallel) as executor:
        future_to_ex = {executor.submit(run_exercise, ex, base_dir): ex for ex in EXERCISES}
        
        for future in as_completed(future_to_ex):
            ex, status, error = future.result()
            results[ex] = status
            status_color = "\033[92mPASSED\033[0m" if status == "PASSED" else "\033[91mFAILED\033[0m"
            if status == "SKIPPED":
                status_color = f"\033[93mSKIPPED\033[0m ({error})"
            print(f"Completed {ex:20}: {status_color}")
        
    print("-" * 40)
    print("Benchmark Summary:")
    passed_count = sum(1 for res in results.values() if res == "PASSED")
    for ex in EXERCISES:
        res = results.get(ex, "SKIPPED")
        color = "\033[92m" if res == "PASSED" else ("\033[91m" if res == "FAILED" else "\033[93m")
        print(f"{ex:20}: {color}{res}\033[0m")
    
    print(f"\nTotal: {passed_count}/{len(EXERCISES)} passed.")

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nBenchmark interrupted by user.")
        sys.exit(1)

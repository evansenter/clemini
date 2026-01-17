import os
import subprocess
import sys
import argparse
import time
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
    start_time = time.time()
    ex_dir = base_dir / ex
    code_file_name = ex.replace("-", "_") + ".py"
    test_file_name = ex.replace("-", "_") + "_test.py"
    
    instr_file = ex_dir / "instructions.md"
    if not instr_file.exists():
        duration = time.time() - start_time
        return ex, {"status": "SKIPPED", "error": f"instructions.md not found", "duration": duration, "attempts": 0}
        
    with open(instr_file, "r") as f:
        instructions = f.read()
        
    prompt = f"Implement the code in {code_file_name} to pass the tests in {test_file_name}. Here are the instructions: {instructions}"
    
    # Initial attempt
    attempts = 1
    run_clemini(prompt, ex_dir)
    passed, output = run_tests(ex_dir, test_file_name)
    
    if not passed:
        # Retry once
        attempts = 2
        retry_prompt = f"The tests failed with the following output:\n\n{output}\n\nPlease fix the implementation in {code_file_name} to pass the tests."
        run_clemini(retry_prompt, ex_dir)
        passed, output = run_tests(ex_dir, test_file_name)
        
    duration = time.time() - start_time
    status = "PASSED" if passed else "FAILED"
    return ex, {"status": status, "duration": duration, "attempts": attempts if passed else 0}

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
    start_time = time.time()
    
    with ThreadPoolExecutor(max_workers=args.parallel) as executor:
        future_to_ex = {executor.submit(run_exercise, ex, base_dir): ex for ex in EXERCISES}
        
        for future in as_completed(future_to_ex):
            ex, res = future.result()
            results[ex] = res
            status = res["status"]
            duration = res["duration"]
            attempts = res["attempts"]
            
            status_color = "\033[92mPASSED\033[0m" if status == "PASSED" else "\033[91mFAILED\033[0m"
            if status == "SKIPPED":
                status_color = f"\033[93mSKIPPED\033[0m ({res['error']})"
            
            attempt_info = ""
            if status == "PASSED":
                suffix = "st" if attempts == 1 else "nd"
                attempt_info = f" ({attempts}{suffix} attempt)"
            
            print(f"Completed {ex:20}: {status_color}{attempt_info} [{duration:.2f}s]")
        
    total_duration = time.time() - start_time
    print("-" * 40)
    print("Benchmark Summary:")
    passed_count = sum(1 for res in results.values() if res["status"] == "PASSED")
    for ex in EXERCISES:
        res = results.get(ex, {"status": "SKIPPED", "duration": 0, "attempts": 0})
        status = res["status"]
        duration = res["duration"]
        attempts = res["attempts"]
        
        color = "\033[92m" if status == "PASSED" else ("\033[91m" if status == "FAILED" else "\033[93m")
        
        attempt_info = ""
        if status == "PASSED":
            suffix = "st" if attempts == 1 else "nd"
            attempt_info = f" ({attempts}{suffix} attempt)"
            
        print(f"{ex:20}: {color}{status}\033[0m{attempt_info:15} [{duration:.2f}s]")
    
    print(f"\nTotal: {passed_count}/{len(EXERCISES)} passed.")
    print(f"Total time elapsed: {total_duration:.2f}s")

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nBenchmark interrupted by user.")
        sys.exit(1)

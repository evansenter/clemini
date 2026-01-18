import os
import subprocess
import sys
import argparse
import time
import random
from pathlib import Path
from concurrent.futures import ThreadPoolExecutor, as_completed

def run_clemini(prompt, cwd):
    """Call clemini via subprocess with the given prompt."""
    cmd = [
        "cargo", "run", "--quiet", "--",
        "--prompt", prompt,
        "--cwd", str(cwd)
    ]
    env = os.environ.copy()
    env["CLEMINI_LOG"] = str(Path(cwd) / "clemini.log")
    
    process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
    stdout, stderr = process.communicate()
    return stdout, stderr, process.returncode

def run_tests(exercise_path, test_file):
    """Run pytest on the test file and return success status and output."""
    cmd = [sys.executable, "-m", "pytest", "-v", str(test_file)]
    process = subprocess.Popen(cmd, cwd=exercise_path, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    stdout, stderr = process.communicate()
    output = stdout + stderr
    return process.returncode == 0, output

def run_exercise(ex, base_dir):
    """Logic for a single exercise run."""
    start_time = time.time()
    ex_dir = base_dir / ex
    
    # Discovery of code and test files
    # Usually they match the directory name with underscores
    code_file_name = ex.replace("-", "_") + ".py"
    test_file_name = ex.replace("-", "_") + "_test.py"
    
    # Fallback: find any .py and _test.py if the above don't exist
    if not (ex_dir / code_file_name).exists():
        py_files = list(ex_dir.glob("*.py"))
        py_files = [f for f in py_files if not f.name.endswith("_test.py")]
        if py_files:
            code_file_name = py_files[0].name
            
    if not (ex_dir / test_file_name).exists():
        test_files = list(ex_dir.glob("*_test.py"))
        if test_files:
            test_file_name = test_files[0].name

    instr_file = ex_dir / "instructions.md"
    if not instr_file.exists():
        duration = time.time() - start_time
        return ex, {"status": "SKIPPED", "error": "instructions.md not found", "duration": duration, "attempts": 0}
        
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

def format_duration(seconds):
    mins = int(seconds // 60)
    secs = int(seconds % 60)
    if mins > 0:
        return f"{mins}m {secs}s"
    return f"{secs}s"

def main():
    parser = argparse.ArgumentParser(description="Run clemini benchmark on exercises.")
    parser.add_argument("--parallel", type=int, default=1, help="Number of exercises to run in parallel.")
    parser.add_argument("--time-limit", type=int, help="Time limit in minutes.")
    args = parser.parse_args()

    repo_root = Path(__file__).parent.parent.absolute()
    os.chdir(repo_root)
    
    base_dir = Path("benchmark/exercises")
    if not base_dir.exists():
        print(f"Error: {base_dir} not found. Run setup.py first.")
        sys.exit(1)
        
    exercises = sorted([d.name for d in base_dir.iterdir() if d.is_dir()])
    random.shuffle(exercises)
    
    limit_str = f" ({args.time_limit}m limit)" if args.time_limit else ""
    print(f"Clemini Benchmark{limit_str}")
    print("=" * (17 + len(limit_str)))
    
    results = {}
    start_time = time.time()
    time_limit_reached = False
    completed_count = 0
    total_to_run = len(exercises)
    
    # Default buffer: don't start new exercise if less than 2 minutes left
    # (assuming average exercise takes ~1-2 mins)
    time_buffer = 120 
    
    with ThreadPoolExecutor(max_workers=args.parallel) as executor:
        futures = {}
        
        # Initial submission
        ex_iter = iter(exercises)
        for _ in range(args.parallel):
            try:
                ex = next(ex_iter)
                futures[executor.submit(run_exercise, ex, base_dir)] = ex
            except StopIteration:
                break
        
        from concurrent.futures import wait, FIRST_COMPLETED
        
        while futures:
            # Wait for at least one to complete or timeout to check time limit
            done, not_done = wait(futures.keys(), timeout=1.0, return_when=FIRST_COMPLETED)
            
            for f in done:
                ex = futures.pop(f)
                ex_name, res = f.result()
                results[ex_name] = res
                completed_count += 1
                
                status = res["status"]
                duration = int(res["duration"])
                attempts = res["attempts"]
                
                if status == "PASSED":
                    mark = "\033[92m✓ pass\033[0m"
                    suffix = "st" if attempts == 1 else "nd"
                    attempt_info = f"({attempts}{suffix})"
                elif status == "FAILED":
                    mark = "\033[91m✗ fail\033[0m"
                    attempt_info = "     "
                else:
                    mark = f"\033[93m? {status.lower()}\033[0m"
                    attempt_info = "     "
                
                ex_display = f"{ex_name}:"
                print(f"[{completed_count}/{total_to_run}]  {ex_display:20} {mark} {attempt_info} [{duration}s]")

            if args.time_limit:
                elapsed = time.time() - start_time
                if elapsed > (args.time_limit * 60):
                    time_limit_reached = True
                    for f in futures.keys():
                        f.cancel()
                    break
                
                if elapsed > (args.time_limit * 60) - time_buffer:
                    # Don't start new ones
                    pass
                else:
                    # Submit more if capacity available
                    while len(futures) < args.parallel:
                        try:
                            ex = next(ex_iter)
                            futures[executor.submit(run_exercise, ex, base_dir)] = ex
                        except StopIteration:
                            break
            else:
                # No time limit, just keep submitting
                while len(futures) < args.parallel:
                    try:
                        ex = next(ex_iter)
                        futures[executor.submit(run_exercise, ex, base_dir)] = ex
                    except StopIteration:
                        break

    if time_limit_reached:
        print("\n[TIME LIMIT REACHED]")

    total_duration = time.time() - start_time
    passed_count = sum(1 for res in results.values() if res["status"] == "PASSED")
    
    print(f"\nResults: {passed_count}/{completed_count} passed ({int(passed_count/completed_count*100) if completed_count > 0 else 0}%)")
    print(f"Time: {format_duration(total_duration)}")

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nBenchmark interrupted by user.")
        sys.exit(1)

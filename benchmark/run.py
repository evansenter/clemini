import os
import subprocess
import sys
import argparse
import time
import random
import shutil
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

def run_tests(exercise_path, lang):
    """Run tests for the given language and return success status and output."""
    cmd = []
    if lang == "python":
        cmd = [sys.executable, "-m", "pytest", "--no-header", "-v"]
    elif lang == "rust":
        cmd = ["cargo", "test", "--offline"]
    elif lang == "go":
        cmd = ["go", "test", "-v", "."]
    elif lang == "javascript":
        if (exercise_path / "package.json").exists():
            cmd = ["npm", "test"]
        else:
            # Fallback to finding .spec.js or .test.js
            test_files = list(exercise_path.glob("*.spec.js")) + list(exercise_path.glob("*.test.js"))
            if test_files:
                cmd = ["node", str(test_files[0])]
    elif lang == "java":
        if (exercise_path / "gradlew").exists():
            cmd = ["./gradlew", "test"]
        else:
            cmd = ["gradle", "test"]
    elif lang == "cpp":
        # C++ exercises usually require cmake build
        build_dir = exercise_path / "build"
        build_dir.mkdir(exist_ok=True)
        try:
            subprocess.run(["cmake", ".."], cwd=build_dir, check=True, capture_output=True)
            subprocess.run(["make"], cwd=build_dir, check=True, capture_output=True)
            # Run the test executable (usually matches exercise name or 'tests')
            test_exe = next(build_dir.glob("*test*"), None)
            if test_exe:
                cmd = [str(test_exe)]
            else:
                return False, "Could not find test executable in build directory"
        except subprocess.CalledProcessError as e:
            return False, f"Build failed:\n{e.stdout.decode() if e.stdout else ''}\n{e.stderr.decode() if e.stderr else ''}"

    if not cmd:
        return False, f"No test runner configured for {lang}"

    process = subprocess.Popen(cmd, cwd=exercise_path, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    stdout, stderr = process.communicate()
    output = stdout + stderr
    return process.returncode == 0, output

def run_exercise(ex, base_dir):
    """Logic for a single exercise run."""
    start_time = time.time()
    ex_dir = base_dir / ex
    
    lang_file = ex_dir / ".lang"
    if not lang_file.exists():
        return ex, {"status": "SKIPPED", "error": ".lang file not found", "duration": 0, "attempts": 0}
    
    with open(lang_file, "r") as f:
        lang = f.read().strip()

    instr_file = ex_dir / "instructions.md"
    if not instr_file.exists():
        duration = time.time() - start_time
        return ex, {"status": "SKIPPED", "error": "instructions.md not found", "duration": duration, "attempts": 0}
        
    with open(instr_file, "r") as f:
        instructions = f.read()
        
    prompt = f"Implement the solution in the appropriate source files to pass the tests. Here are the instructions: {instructions}"
    
    # Initial attempt
    attempts = 1
    run_clemini(prompt, ex_dir)
    passed, output = run_tests(ex_dir, lang)
    
    if not passed:
        # Retry once
        attempts = 2
        retry_prompt = f"The tests failed with the following output:\n\n{output}\n\nPlease fix the implementation to pass the tests."
        run_clemini(retry_prompt, ex_dir)
        passed, output = run_tests(ex_dir, lang)
        
    duration = time.time() - start_time
    status = "PASSED" if passed else "FAILED"
    return ex, {"status": status, "duration": duration, "attempts": attempts if passed else 0, "lang": lang}

def format_duration(seconds):
    mins = int(seconds // 60)
    secs = int(seconds % 60)
    if mins > 0:
        return f"{mins}m {secs}s"
    return f"{secs}s"

def main():
    parser = argparse.ArgumentParser(description="Run clemini benchmark on exercises.")
    parser.add_argument("--parallel", type=int, default=2, help="Number of exercises to run in parallel.")
    parser.add_argument("--time-limit", type=int, default=5, help="Time limit in minutes.")
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
    print(f"Clemini Polyglot Benchmark{limit_str}")
    print("=" * (26 + len(limit_str)))
    
    results = {}
    start_time = time.time()
    time_limit_reached = False
    completed_count = 0
    total_to_run = len(exercises)
    
    # Default buffer: don't start new exercise if less than 2 minutes left
    time_buffer = 120 
    
    with ThreadPoolExecutor(max_workers=args.parallel) as executor:
        futures = {}
        
        # Initial submission
        ex_iter = iter(exercises)
        while len(futures) < args.parallel:
            try:
                ex = next(ex_iter)
                futures[executor.submit(run_exercise, ex, base_dir)] = ex
            except StopIteration:
                break
        
        from concurrent.futures import wait, FIRST_COMPLETED
        
        while futures:
            done, not_done = wait(futures.keys(), timeout=1.0, return_when=FIRST_COMPLETED)
            
            for f in done:
                ex = futures.pop(f)
                ex_name, res = f.result()
                results[ex_name] = res
                completed_count += 1
                
                status = res.get("status", "ERROR")
                duration = int(res.get("duration", 0))
                attempts = res.get("attempts", 0)
                lang = res.get("lang", "???")
                
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
                print(f"[{completed_count}/{total_to_run}] {lang:10} {ex_display:30} {mark} {attempt_info} [{duration}s]")

            elapsed = time.time() - start_time
            if args.time_limit and elapsed > (args.time_limit * 60):
                time_limit_reached = True
                for f in not_done:
                    f.cancel()
                break
            
            # Submit more if capacity available and not approaching limit
            while len(futures) < args.parallel:
                if args.time_limit and elapsed > (args.time_limit * 60) - time_buffer:
                    break
                try:
                    ex = next(ex_iter)
                    futures[executor.submit(run_exercise, ex, base_dir)] = ex
                except StopIteration:
                    break

    if time_limit_reached:
        print("\n[TIME LIMIT REACHED]")

    total_duration = time.time() - start_time
    passed_count = sum(1 for res in results.values() if res.get("status") == "PASSED")
    
    print(f"\nResults: {passed_count}/{completed_count} passed ({int(passed_count/completed_count*100) if completed_count > 0 else 0}%)")
    print(f"Time: {format_duration(total_duration)}")

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nBenchmark interrupted by user.")
        sys.exit(1)

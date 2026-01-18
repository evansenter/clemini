import os
import shutil
import subprocess
import tempfile
from pathlib import Path

def run_command(cmd, cwd=None):
    print(f"Running: {' '.join(cmd)}")
    subprocess.run(cmd, cwd=cwd, check=True)

def setup_benchmark():
    repo_url = "https://github.com/Aider-AI/polyglot-benchmark"
    benchmark_dir = Path(__file__).parent.absolute()
    exercises_dest = benchmark_dir / "exercises"
    
    # Create exercises directory if it doesn't exist
    exercises_dest.mkdir(parents=True, exist_ok=True)
    
    with tempfile.TemporaryDirectory() as temp_dir:
        temp_dir_path = Path(temp_dir)
        print(f"Cloning {repo_url} into {temp_dir}...")
        run_command(["git", "clone", "--depth", "1", repo_url, "."], cwd=temp_dir_path)
        
        src_exercises_dir = temp_dir_path / "python" / "exercises" / "practice"
        
        if not src_exercises_dir.exists():
            print(f"Error: {src_exercises_dir} does not exist.")
            return

        python_exercises = [d for d in src_exercises_dir.iterdir() if d.is_dir()]
        
        print(f"Found {len(python_exercises)} Python exercises.")
        
        for ex_src in python_exercises:
            ex_name = ex_src.name
            ex_dest = exercises_dest / ex_name
            ex_dest.mkdir(parents=True, exist_ok=True)
            
            # Copy instructions
            instr_src = ex_src / ".docs" / "instructions.md"
            if instr_src.exists():
                shutil.copy(instr_src, ex_dest / "instructions.md")
            else:
                print(f"Warning: instructions.md not found for {ex_name}")
            
            # Copy all .py files
            # The files might be named differently (e.g. affine_cipher.py in affine-cipher dir)
            for py_file in ex_src.glob("*.py"):
                shutil.copy(py_file, ex_dest / py_file.name)
            
            print(f"Copied {ex_name}")

if __name__ == "__main__":
    setup_benchmark()

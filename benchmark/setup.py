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
    if exercises_dest.exists():
        shutil.rmtree(exercises_dest)
    exercises_dest.mkdir(parents=True, exist_ok=True)
    
    languages = ["python", "rust", "go", "javascript", "java", "cpp"]
    
    with tempfile.TemporaryDirectory() as temp_dir:
        temp_dir_path = Path(temp_dir)
        print(f"Cloning {repo_url} into {temp_dir}...")
        run_command(["git", "clone", "--depth", "1", repo_url, "."], cwd=temp_dir_path)
        
        for lang in languages:
            src_exercises_dir = temp_dir_path / lang / "exercises" / "practice"
            
            if not src_exercises_dir.exists():
                print(f"Warning: {src_exercises_dir} does not exist. Skipping {lang}.")
                continue

            lang_exercises = [d for d in src_exercises_dir.iterdir() if d.is_dir()]
            print(f"Found {len(lang_exercises)} {lang} exercises.")
            
            for ex_src in lang_exercises:
                ex_name = ex_src.name
                # Use lang-prefix to avoid name collisions across languages
                ex_dest = exercises_dest / f"{lang}-{ex_name}"
                
                # Copy the entire exercise directory
                shutil.copytree(ex_src, ex_dest, dirs_exist_ok=True)
                
                # Copy instructions to root of ex_dest for easy discovery
                instr_src = ex_src / ".docs" / "instructions.md"
                if instr_src.exists():
                    shutil.copy(instr_src, ex_dest / "instructions.md")
                
                # Create a .lang file to help run.py identify the language
                with open(ex_dest / ".lang", "w") as f:
                    f.write(lang)
                
                print(f"Copied {lang}-{ex_name}")

if __name__ == "__main__":
    setup_benchmark()

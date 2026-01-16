use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use glob::glob;
use regex::Regex;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct GrepTool {
    cwd: PathBuf,
}

impl GrepTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl CallableFunction for GrepTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "grep".to_string(),
            "Search for a pattern in files. Returns matching lines with file paths and line numbers. Supports regex patterns.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for"
                    },
                    "file_pattern": {
                        "type": "string",
                        "description": "Glob pattern for files to search (e.g., '**/*.rs', 'src/*.ts'). Defaults to '**/*' if not specified."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of matches to return (default: 100)"
                    }
                }),
                vec!["pattern".to_string()],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing pattern".to_string()))?;

        let file_pattern = args
            .get("file_pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("**/*");

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;

        // Compile regex
        let regex = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Invalid regex pattern: {}", e)
                }));
            }
        };

        // Find files matching the glob pattern
        let full_pattern = self.cwd.join(file_pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let file_paths: Vec<PathBuf> = match glob(&pattern_str) {
            Ok(paths) => paths.filter_map(|p| p.ok()).filter(|p| p.is_file()).collect(),
            Err(e) => {
                return Ok(json!({
                    "error": format!("Invalid glob pattern: {}", e)
                }));
            }
        };

        let mut matches: Vec<Value> = Vec::new();
        let mut files_searched = 0;
        let mut files_with_matches = 0;

        for path in file_paths {
            // Skip binary files by checking if we can read as text
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => continue, // Skip files we can't read as text
            };

            files_searched += 1;
            let mut file_has_match = false;

            for (line_num, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    if !file_has_match {
                        file_has_match = true;
                        files_with_matches += 1;
                    }

                    let relative_path = path
                        .strip_prefix(&self.cwd)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();

                    matches.push(json!({
                        "file": relative_path,
                        "line": line_num + 1,
                        "content": line.trim()
                    }));

                    if matches.len() >= max_results {
                        return Ok(json!({
                            "pattern": pattern,
                            "file_pattern": file_pattern,
                            "matches": matches,
                            "count": matches.len(),
                            "files_searched": files_searched,
                            "files_with_matches": files_with_matches,
                            "truncated": true
                        }));
                    }
                }
            }
        }

        Ok(json!({
            "pattern": pattern,
            "file_pattern": file_pattern,
            "matches": matches,
            "count": matches.len(),
            "files_searched": files_searched,
            "files_with_matches": files_with_matches,
            "truncated": false
        }))
    }
}

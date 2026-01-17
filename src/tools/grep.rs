use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use glob::glob;
use regex::Regex;
use serde_json::{Value, json};
use std::path::PathBuf;

use super::{make_relative, validate_path};

pub struct GrepTool {
    cwd: PathBuf,
}

const DEFAULT_EXCLUDES: &[&str] = &[".git", "node_modules", "target", "__pycache__", ".venv"];

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
            "Search for a pattern in files. Returns matching lines with file paths and line numbers. Supports regex patterns, case-insensitive search, and context lines.".to_string(),
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
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "If true, perform case-insensitive matching (default: false)"
                    },
                    "context": {
                        "type": "integer",
                        "description": "Number of lines to show before and after each match (default: 0)"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of matches to return (default: 100)"
                    },
                    "include_large": {
                        "type": "boolean",
                        "description": "If true, include files larger than 1MB in the search (default: false)"
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

        let case_insensitive = args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let context = args
            .get("context")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let max_results = usize::try_from(
            args.get("max_results")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(100),
        )
        .unwrap_or(usize::MAX);

        let include_large = args
            .get("include_large")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Compile regex with optional case-insensitivity
        let pattern_str = if case_insensitive {
            format!("(?i){}", pattern)
        } else {
            pattern.to_string()
        };

        let regex = match Regex::new(&pattern_str) {
            Ok(r) => r,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Invalid regex pattern '{}': {}. Ensure you are using valid Rust regex syntax. Suggestions: check for unclosed parentheses, invalid escape sequences, or other regex syntax errors. Note: use '(?i)' for case-insensitive search.", pattern, e)
                }));
            }
        };

        // Find files matching the glob pattern
        let full_pattern = self.cwd.join(file_pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let file_paths: Vec<PathBuf> = match glob(&pattern_str) {
            Ok(paths) => {
                let paths: Vec<PathBuf> = paths
                    .filter_map(std::result::Result::ok)
                    .filter_map(|p| {
                        // Security check - only include files within cwd
                        let validated_path = validate_path(&p, &self.cwd).ok()?;

                        if !validated_path.is_file() {
                            return None;
                        }
                        // Skip excluded directories
                        if validated_path.components().any(|c| {
                            if let std::path::Component::Normal(s) = c {
                                DEFAULT_EXCLUDES.contains(&s.to_string_lossy().as_ref())
                            } else {
                                false
                            }
                        }) {
                            return None;
                        }
                        Some(validated_path)
                    })
                    .collect();

                if paths.is_empty() {
                    return Ok(json!({
                        "error": format!("No files matched the pattern '{}'. Suggestions: ensure the pattern is correct, check that the files exist, and that they are not in excluded directories (e.g., .git, node_modules).", file_pattern)
                    }));
                }
                paths
            }
            Err(e) => {
                return Ok(json!({
                    "error": format!("Invalid glob pattern: {}. Ensure you are using valid glob syntax (e.g., '**/*.rs', 'src/*.ts'). Suggestions: check for invalid characters or incorrectly nested patterns.", e)
                }));
            }
        };

        let mut matches: Vec<Value> = Vec::new();
        let mut files_searched = 0;
        let mut files_with_matches = 0;
        let mut skipped_large_files = Vec::new();

        for path in file_paths {
            // Skip large files (> 1MB) unless include_large is true
            if let Ok(metadata) = tokio::fs::metadata(&path).await {
                if metadata.len() > 1_000_000 && !include_large {
                    skipped_large_files.push(make_relative(&path, &self.cwd));
                    continue;
                }
            }

            // Skip binary files by checking if we can read as text
            let Ok(content) = tokio::fs::read_to_string(&path).await else {
                continue; // Skip files we can't read as text
            };

            files_searched += 1;
            let mut file_has_match = false;
            let lines: Vec<&str> = content.lines().collect();

            for (line_num, line) in lines.iter().enumerate() {
                if regex.is_match(line) {
                    if !file_has_match {
                        file_has_match = true;
                        files_with_matches += 1;
                    }

                    let relative_path = make_relative(&path, &self.cwd);

                    // Collect context lines if requested
                    let truncate_line = |l: &str| {
                        if l.len() > 1000 {
                            let truncated: String = l.chars().take(1000).collect();
                            format!("{}... [truncated]", truncated)
                        } else {
                            l.to_string()
                        }
                    };

                    let match_content = if context > 0 {
                        let start = line_num.saturating_sub(context);
                        let end = (line_num + context + 1).min(lines.len());
                        let context_lines: Vec<String> = (start..end)
                            .map(|i| {
                                let prefix = if i == line_num { ">" } else { " " };
                                format!("{}{:>4}:{}", prefix, i + 1, truncate_line(lines[i]))
                            })
                            .collect();
                        context_lines.join("\n")
                    } else {
                        truncate_line(line.trim())
                    };

                    matches.push(json!({
                        "file": relative_path,
                        "line": line_num + 1,
                        "content": match_content
                    }));

                    if matches.len() >= max_results {
                        let mut res = json!({
                            "pattern": pattern,
                            "file_pattern": file_pattern,
                            "matches": matches,
                            "count": matches.len(),
                            "files_searched": files_searched,
                            "files_with_matches": files_with_matches,
                            "truncated": true
                        });

                        if !skipped_large_files.is_empty() {
                            res["warning"] = json!(format!(
                                "Skipped {} files over 1MB: {}. Use 'include_large: true' to search them.",
                                skipped_large_files.len(),
                                skipped_large_files.join(", ")
                            ));
                        }

                        return Ok(res);
                    }
                }
            }
        }

        if files_searched == 0 {
            let mut error_msg = format!("No searchable text files were found matching '{}'. Suggestions: check file permissions and ensure files are not binary.", file_pattern);
            if !skipped_large_files.is_empty() {
                error_msg.push_str(&format!(
                    " Note: {} files were skipped because they are over 1MB: {}. Use 'include_large: true' to search them.",
                    skipped_large_files.len(),
                    skipped_large_files.join(", ")
                ));
            }
            return Ok(json!({
                "error": error_msg
            }));
        }

        if matches.is_empty() {
            let mut error_msg = format!("No matches found for pattern '{}' in files matching '{}'. Suggestions: check the pattern for typos, ensure the correct case is used, or try a simpler search pattern to find the relevant section.", pattern, file_pattern);
            if !skipped_large_files.is_empty() {
                error_msg.push_str(&format!(
                    " Note: {} files were skipped because they are over 1MB: {}. Use 'include_large: true' to search them.",
                    skipped_large_files.len(),
                    skipped_large_files.join(", ")
                ));
            }
            return Ok(json!({
                "error": error_msg,
                "pattern": pattern,
                "file_pattern": file_pattern,
                "files_searched": files_searched
            }));
        }

        let mut res = json!({
            "pattern": pattern,
            "file_pattern": file_pattern,
            "matches": matches,
            "count": matches.len(),
            "files_searched": files_searched,
            "files_with_matches": files_with_matches,
            "truncated": false
        });

        if !skipped_large_files.is_empty() {
            res["warning"] = json!(format!(
                "Skipped {} files over 1MB: {}. Use 'include_large: true' to search them.",
                skipped_large_files.len(),
                skipped_large_files.join(", ")
            ));
        }

        Ok(res)
    }
}

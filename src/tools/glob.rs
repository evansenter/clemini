use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use glob::glob;
use serde_json::{Value, json};
use std::path::PathBuf;
use tracing::instrument;

use super::{DEFAULT_EXCLUDES, make_relative, validate_path};

pub struct GlobTool {
    cwd: PathBuf,
    allowed_paths: Vec<PathBuf>,
}

impl GlobTool {
    pub fn new(cwd: PathBuf, allowed_paths: Vec<PathBuf>) -> Self {
        Self { cwd, allowed_paths }
    }
}

#[async_trait]
impl CallableFunction for GlobTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "glob".to_string(),
            "Find files matching a glob pattern. Returns list of matching file paths relative to cwd. Use patterns like '**/*.rs' for recursive search or 'src/*.rs' for single directory.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match (e.g., '**/*.rs', 'src/**/*.ts', '*.json')"
                    }
                }),
                vec!["pattern".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing pattern".to_string()))?;

        // Construct full pattern from cwd
        let full_pattern = self.cwd.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        // Logging handled by main.rs event loop

        match glob(&pattern_str) {
            Ok(paths) => {
                let mut matches: Vec<String> = Vec::new();
                let mut errors: Vec<String> = Vec::new();

                for entry in paths {
                    match entry {
                        Ok(path) => {
                            // Security check - only include files within allowed paths
                            let path = match validate_path(&path, &self.allowed_paths) {
                                Ok(p) => p,
                                Err(_) => continue, // Skip files outside allowed paths
                            };

                            // Only include files, skip directories
                            if !path.is_file() {
                                continue;
                            }

                            // Skip excluded directories
                            if path.components().any(|c| {
                                if let std::path::Component::Normal(s) = c {
                                    DEFAULT_EXCLUDES.contains(&s.to_string_lossy().as_ref())
                                } else {
                                    false
                                }
                            }) {
                                continue;
                            }

                            // Convert to relative path from cwd
                            let relative = make_relative(&path, &self.cwd);
                            matches.push(relative);
                        }
                        Err(e) => {
                            errors.push(e.to_string());
                        }
                    }
                }

                if matches.is_empty() {
                    // Check if the pattern matches a directory
                    let full_path = self.cwd.join(pattern);
                    if validate_path(&full_path, &self.allowed_paths).is_ok_and(|p| p.is_dir()) {
                        return Ok(json!({
                            "error": format!("The pattern '{}' matches a directory, but this tool is for finding files. Suggestion: use '{}/*' to find files within this directory or '{}/**/*' for recursive search.", pattern, pattern, pattern)
                        }));
                    }

                    return Ok(json!({
                        "error": format!("No files matched the pattern '{}'. Suggestions: ensure the pattern is correct, check that the files exist, and that they are not in excluded directories (e.g., .git, node_modules).", pattern)
                    }));
                }

                Ok(json!({
                    "pattern": pattern,
                    "matches": matches,
                    "count": matches.len(),
                    "errors": if errors.is_empty() { Value::Null } else { json!(errors) }
                }))
            }
            Err(e) => Ok(json!({
                "error": format!("Invalid glob pattern: {}. Ensure you are using valid glob syntax (e.g., '**/*.rs', 'src/*.ts'). Suggestions: check for invalid characters or incorrectly nested patterns.", e)
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_glob_tool_success() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::create_dir(cwd.join("src")).unwrap();
        fs::write(cwd.join("src/main.rs"), "").unwrap();
        fs::write(cwd.join("src/lib.rs"), "").unwrap();
        fs::write(cwd.join("README.md"), "").unwrap();

        let tool = GlobTool::new(cwd.clone(), vec![cwd.clone()]);
        let args = json!({ "pattern": "src/*.rs" });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().any(|m| m.as_str().unwrap() == "src/main.rs"));
        assert!(matches.iter().any(|m| m.as_str().unwrap() == "src/lib.rs"));
    }

    #[tokio::test]
    async fn test_glob_tool_excludes() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::create_dir(cwd.join(".git")).unwrap();
        fs::write(cwd.join(".git/config"), "").unwrap();
        fs::write(cwd.join("file.txt"), "").unwrap();

        let tool = GlobTool::new(cwd.clone(), vec![cwd.clone()]);
        let args = json!({ "pattern": "**/*" });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].as_str().unwrap(), "file.txt");
    }

    #[tokio::test]
    async fn test_glob_tool_no_matches() {
        let dir = tempdir().unwrap();
        let tool = GlobTool::new(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let args = json!({ "pattern": "*.nonexistent" });

        let result = tool.call(args).await.unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("No files matched")
        );
    }
}

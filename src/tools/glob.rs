use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use glob::glob;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct GlobTool {
    cwd: PathBuf,
}

impl GlobTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
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
                            // Convert to relative path from cwd
                            let relative = path
                                .strip_prefix(&self.cwd)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .to_string();
                            matches.push(relative);
                        }
                        Err(e) => {
                            errors.push(e.to_string());
                        }
                    }
                }

                Ok(json!({
                    "pattern": pattern,
                    "matches": matches,
                    "count": matches.len(),
                    "errors": errors
                }))
            }
            Err(e) => Ok(json!({
                "error": format!("Invalid glob pattern: {}", e)
            })),
        }
    }
}

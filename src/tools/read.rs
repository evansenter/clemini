use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;

use super::resolve_and_validate_path;

pub struct ReadTool {
    cwd: PathBuf,
}

impl ReadTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl CallableFunction for ReadTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "read_file".to_string(),
            "Read the contents of a file. Returns the file contents as text with line numbers. Supports pagination via offset and limit.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "file_path": {
                        "type": "string",
                        "description": "The path to the file to read (absolute or relative to cwd)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "1-indexed line number to start from (default: 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max lines to read (default: 500)"
                    }
                }),
                vec!["file_path".to_string()],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing file_path".to_string()))?;

        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(500) as usize;

        // Resolve and validate path
        let path = match resolve_and_validate_path(file_path, &self.cwd) {
            Ok(p) => p,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Access denied: {}. Only files within the current working directory can be accessed.", e)
                }));
            }
        };

        match tokio::fs::read_to_string(&path).await {
            Ok(contents) => {
                let lines: Vec<&str> = contents.lines().collect();
                let total_lines = lines.len();
                
                let start = offset.saturating_sub(1);
                let end = (start + limit).min(total_lines);
                
                if start >= total_lines && total_lines > 0 {
                    return Ok(json!({
                        "path": path.display().to_string(),
                        "total_lines": total_lines,
                        "error": format!("Offset {} is out of bounds (total lines: {})", offset, total_lines)
                    }));
                }

                let mut formatted_contents = String::new();
                for (i, line) in lines.iter().enumerate().take(end).skip(start) {
                    let line_num = i + 1;
                    formatted_contents.push_str(&format!("{:>4}→{line}\n", line_num));
                }

                let mut response = json!({
                    "path": path.display().to_string(),
                    "contents": formatted_contents,
                    "total_lines": total_lines,
                });

                if end < total_lines {
                    response["truncated"] = json!(format!(
                        "Showing lines {}-{} of {}. Use offset to read more.",
                        start + 1,
                        end,
                        total_lines
                    ));
                }

                Ok(response)
            }
            Err(e) => Ok(json!({
                "error": format!("Failed to read {}: {}. Ensure the file exists and is not a directory.", path.display(), e)
            })),
        }
    }
}

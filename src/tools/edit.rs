use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;
use tracing::instrument;

use super::resolve_and_validate_path;

pub struct EditTool {
    cwd: PathBuf,
}

impl EditTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl CallableFunction for EditTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "edit".to_string(),
            "Replace a specific string in a file with new content. If 'replace_all' is true, all occurrences are replaced. Otherwise, 'old_string' must match exactly and uniquely in the file.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file to edit"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact string to find and replace"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The string to replace it with"
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "If true, replace all occurrences of 'old_string'. If false (default), 'old_string' must be unique."
                    }
                }),
                vec!["file_path".to_string(), "old_string".to_string(), "new_string".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing file_path".to_string()))?;

        let old_string = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing old_string".to_string()))?;

        let new_string = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing new_string".to_string()))?;

        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if old_string == new_string {
            return Ok(json!({
                "error": "The 'old_string' and 'new_string' are the same. No replacement needed."
            }));
        }

        // Resolve and validate path
        let path = match resolve_and_validate_path(file_path, &self.cwd) {
            Ok(p) => p,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Access denied: {}. Only files within the current working directory can be accessed.", e)
                }));
            }
        };

        // Read the file
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Failed to read {}: {}. Ensure the file exists and is not a directory.", path.display(), e)
                }));
            }
        };

        // Check that old_string exists and is unique
        let matches: Vec<_> = content.match_indices(old_string).collect();

        if matches.is_empty() {
            return Ok(json!({
                "error": format!("The 'old_string' was not found in {}. Ensure the string matches exactly, including whitespace and indentation. Use 'read_file' to confirm the file's current content.", file_path),
                "file_path": file_path
            }));
        }

        if !replace_all && matches.len() > 1 {
            return Ok(json!({
                "error": format!("The 'old_string' was found {} times in {}. It must be unique to ensure the correct replacement. Provide more surrounding context to make it unique, or set 'replace_all' to true.", matches.len(), file_path),
                "file_path": file_path,
                "occurrences": matches.len()
            }));
        }

        // Perform the replacement
        let (new_content, count) = if replace_all {
            (content.replace(old_string, new_string), matches.len())
        } else {
            (content.replacen(old_string, new_string, 1), 1)
        };

        // Write the file
        match tokio::fs::write(&path, &new_content).await {
            Ok(()) => {
                // Log the diff
                let diff_output = crate::diff::format_diff(old_string, new_string, 2);
                if !diff_output.is_empty() {
                    crate::log_event_raw(&diff_output);
                }

                Ok(json!({
                    "file_path": file_path,
                    "success": true,
                    "old_length": old_string.len(),
                    "new_length": new_string.len(),
                    "file_size": new_content.len(),
                    "replacements": count
                }))
            }
            Err(e) => Ok(json!({
                "error": format!("Failed to write {}: {}. Check file permissions.", path.display(), e)
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
    async fn test_edit_tool_success() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "original content").unwrap();

        let tool = EditTool::new(cwd.clone());
        let args = json!({
            "file_path": "test.txt",
            "old_string": "original",
            "new_string": "updated"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["replacements"], 1);

        let saved_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(saved_content, "updated content");
    }

    #[tokio::test]
    async fn test_edit_tool_not_found() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "content").unwrap();

        let tool = EditTool::new(cwd.clone());
        let args = json!({
            "file_path": "test.txt",
            "old_string": "missing",
            "new_string": "whatever"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("was not found"));
    }

    #[tokio::test]
    async fn test_edit_tool_not_unique() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "repeat repeat").unwrap();

        let tool = EditTool::new(cwd.clone());
        let args = json!({
            "file_path": "test.txt",
            "old_string": "repeat",
            "new_string": "once"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("must be unique"));
    }

    #[tokio::test]
    async fn test_edit_tool_replace_all() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "repeat repeat").unwrap();

        let tool = EditTool::new(cwd.clone());
        let args = json!({
            "file_path": "test.txt",
            "old_string": "repeat",
            "new_string": "replaced",
            "replace_all": true
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert_eq!(result["replacements"], 2);

        let saved_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(saved_content, "replaced replaced");
    }
}

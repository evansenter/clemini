use crate::agent::AgentEvent;
use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;
use strsim::normalized_levenshtein;
use tokio::sync::mpsc;
use tracing::instrument;

use super::{ToolEmitter, error_codes, error_response, resolve_and_validate_path};

pub struct EditTool {
    cwd: PathBuf,
    allowed_paths: Vec<PathBuf>,
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl EditTool {
    pub fn new(
        cwd: PathBuf,
        allowed_paths: Vec<PathBuf>,
        events_tx: Option<mpsc::Sender<AgentEvent>>,
    ) -> Self {
        Self {
            cwd,
            allowed_paths,
            events_tx,
        }
    }
}

impl ToolEmitter for EditTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

fn offset_to_line(content: &str, offset: usize) -> usize {
    content[..offset].lines().count() + 1
}

/// Find strings in content similar to the target.
/// Returns up to `max_suggestions` matches with similarity >= `threshold`.
fn find_similar_strings(
    content: &str,
    target: &str,
    max_suggestions: usize,
    threshold: f64,
) -> Vec<(String, usize, f64)> {
    let target_lines: Vec<&str> = target.lines().collect();
    let target_line_count = target_lines.len();
    let content_lines: Vec<&str> = content.lines().collect();

    let mut candidates: Vec<(String, usize, f64)> = Vec::new();

    // For single-line targets, compare against individual lines
    if target_line_count == 1 {
        for (i, line) in content_lines.iter().enumerate() {
            let similarity = normalized_levenshtein(target, line);
            if similarity >= threshold && similarity < 1.0 {
                candidates.push((line.to_string(), i + 1, similarity));
            }
        }
    } else {
        // For multi-line targets, use sliding window
        for i in 0..=content_lines.len().saturating_sub(target_line_count) {
            let window = content_lines[i..i + target_line_count].join("\n");
            let similarity = normalized_levenshtein(target, &window);
            if similarity >= threshold && similarity < 1.0 {
                candidates.push((window, i + 1, similarity));
            }
        }
    }

    // Sort by similarity descending
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(max_suggestions);
    candidates
}

#[async_trait]
impl CallableFunction for EditTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "edit".to_string(),
            "Replace a specific string in a file with new content. If 'replace_all' is true, all occurrences are replaced. Otherwise, 'old_string' must match exactly and uniquely in the file. Returns: {success, replacements, file_size} or {error, suggestions?}".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "file_path": {
                        "type": "string",
                        "description": "Path to the file to edit"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact string to find and replace. Optional if create_if_not_exists is true."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The string to replace it with"
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "If true, replace all occurrences of 'old_string'. If false, 'old_string' must be unique. (default: false)"
                    },
                    "create_if_not_exists": {
                        "type": "boolean",
                        "description": "If true, create the file if it does not exist. In this case, 'old_string' is ignored and the file is created with 'new_string' as its content. (default: false)"
                    }
                }),
                vec!["file_path".to_string(), "new_string".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing file_path".to_string()))?;

        let old_string = args.get("old_string").and_then(|v| v.as_str());

        let new_string = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing new_string".to_string()))?;

        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let create_if_not_exists = args
            .get("create_if_not_exists")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if let Some(old) = old_string
            && old == new_string
        {
            return Ok(error_response(
                "The 'old_string' and 'new_string' are the same. No replacement needed.",
                error_codes::INVALID_ARGUMENT,
                json!({}),
            ));
        }

        // Resolve and validate path
        let path = match resolve_and_validate_path(file_path, &self.cwd, &self.allowed_paths) {
            Ok(p) => p,
            Err(e) => {
                return Ok(error_response(
                    &format!("Access denied: {}. Path must be within allowed paths.", e),
                    error_codes::ACCESS_DENIED,
                    json!({"path": file_path}),
                ));
            }
        };

        // Read the file
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => Some(c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Ok(error_response(
                    &format!(
                        "Failed to read {}: {}. Ensure the file exists and is not a directory.",
                        path.display(),
                        e
                    ),
                    error_codes::IO_ERROR,
                    json!({"path": file_path}),
                ));
            }
        };

        let content = match content {
            Some(c) => c,
            None if create_if_not_exists => {
                // Create new file
                match tokio::fs::write(&path, new_string).await {
                    Ok(()) => {
                        return Ok(json!({
                            "file_path": file_path,
                            "success": true,
                            "created": true,
                            "file_size": new_string.len()
                        }));
                    }
                    Err(e) => {
                        return Ok(error_response(
                            &format!("Failed to create {}: {}", path.display(), e),
                            error_codes::IO_ERROR,
                            json!({"path": file_path}),
                        ));
                    }
                }
            }
            None => {
                return Ok(error_response(
                    &format!(
                        "File not found: {}. Set 'create_if_not_exists' to true to create it.",
                        file_path
                    ),
                    error_codes::NOT_FOUND,
                    json!({"path": file_path}),
                ));
            }
        };

        let old_string = match old_string {
            Some(s) => s,
            None => {
                return Ok(error_response(
                    "Missing 'old_string' for existing file. 'old_string' is only optional when 'create_if_not_exists' is true and the file does not exist.",
                    error_codes::INVALID_ARGUMENT,
                    json!({"path": file_path}),
                ));
            }
        };

        // Check that old_string exists and is unique
        let matches: Vec<_> = content.match_indices(old_string).collect();

        if matches.is_empty() {
            let suggestions = find_similar_strings(&content, old_string, 3, 0.6);

            let mut context = json!({
                "path": file_path
            });

            if !suggestions.is_empty() {
                let suggestion_details: Vec<Value> = suggestions
                    .iter()
                    .map(|(text, line, similarity)| {
                        json!({
                            "line": line,
                            "similarity": format!("{:.0}%", similarity * 100.0),
                            "text": if text.len() > 100 {
                                format!("{}...", &text[..100])
                            } else {
                                text.clone()
                            }
                        })
                    })
                    .collect();

                context["suggestions"] = json!(suggestion_details);
                context["hint"] = json!(
                    "Similar content found. Check for whitespace differences or use read_file to verify current content."
                );
            }

            return Ok(error_response(
                &format!(
                    "The 'old_string' was not found in {}. Ensure the string matches exactly, including whitespace and indentation.",
                    file_path
                ),
                error_codes::NOT_FOUND,
                context,
            ));
        }

        if !replace_all && matches.len() > 1 {
            let lines: Vec<_> = matches
                .iter()
                .map(|(offset, _)| offset_to_line(&content, *offset))
                .collect();

            let lines_str = lines
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join(", ");

            return Ok(error_response(
                &format!(
                    "The 'old_string' was found {} times in {} at lines {}. It must be unique to ensure the correct replacement. Provide more surrounding context to make it unique, or set 'replace_all' to true.",
                    matches.len(),
                    file_path,
                    lines_str
                ),
                error_codes::NOT_UNIQUE,
                json!({
                    "path": file_path,
                    "occurrences": matches.len(),
                    "lines": lines
                }),
            ));
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
                let diff_output =
                    crate::diff::format_diff(old_string, new_string, 2, Some(file_path));
                if !diff_output.is_empty() {
                    self.emit(&diff_output);
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
            Err(e) => Ok(error_response(
                &format!(
                    "Failed to write {}: {}. Check file permissions.",
                    path.display(),
                    e
                ),
                error_codes::IO_ERROR,
                json!({"path": file_path}),
            )),
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

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
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

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "old_string": "missing",
            "new_string": "whatever"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("was not found"));
        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
        assert_eq!(result["context"]["path"], "test.txt");
    }

    #[tokio::test]
    async fn test_edit_tool_not_unique() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "repeat\nrepeat").unwrap();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "old_string": "repeat",
            "new_string": "once"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("must be unique"));
        assert!(result["error"].as_str().unwrap().contains("at lines 1, 2"));
        assert_eq!(result["error_code"], error_codes::NOT_UNIQUE);
        assert_eq!(result["context"]["occurrences"], 2);
        assert_eq!(result["context"]["lines"], json!([1, 2]));
    }

    #[tokio::test]
    async fn test_edit_tool_replace_all() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "repeat repeat").unwrap();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
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

    #[tokio::test]
    async fn test_edit_tool_empty_file() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("empty.txt");
        fs::write(&file_path, "").unwrap();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "empty.txt",
            "old_string": "something",
            "new_string": "nothing"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("was not found"));
        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_edit_tool_multiline() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "line 1\nline 2\nline 3").unwrap();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "old_string": "line 1\nline 2",
            "new_string": "new line 1\nnew line 2"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());

        let saved_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(saved_content, "new line 1\nnew line 2\nline 3");
    }

    #[tokio::test]
    async fn test_edit_tool_unicode() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "old_string": "world",
            "new_string": "🦀"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());

        let saved_content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(saved_content, "hello 🦀");
    }

    #[tokio::test]
    async fn test_edit_tool_fuzzy_suggestions() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "fn hello_world() {\n    println!(\"hi\");\n}").unwrap();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "old_string": "fn hello_wrold() {",  // typo
            "new_string": "fn greet() {"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("not found"));
        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
        assert!(result["context"]["suggestions"].is_array());
        assert!(
            !result["context"]["suggestions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_edit_tool_no_suggestions_when_nothing_similar() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "completely different content").unwrap();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "old_string": "xyz123abc",
            "new_string": "replacement"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("not found"));
        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
        assert!(
            result["context"].get("suggestions").is_none()
                || result["context"]["suggestions"]
                    .as_array()
                    .unwrap()
                    .is_empty()
        );
    }

    #[tokio::test]
    async fn test_edit_tool_file_not_exists_default() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "nonexistent.txt",
            "old_string": "old",
            "new_string": "new"
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("File not found"));
        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_edit_tool_create_if_not_exists() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "new_file.txt",
            "new_string": "initial content",
            "create_if_not_exists": true
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert!(result["created"].as_bool().unwrap());

        let saved_content = fs::read_to_string(cwd.join("new_file.txt")).unwrap();
        assert_eq!(saved_content, "initial content");
    }

    #[tokio::test]
    async fn test_edit_tool_create_if_not_exists_with_old_string_ignored() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "new_file.txt",
            "old_string": "something",
            "new_string": "initial content",
            "create_if_not_exists": true
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["success"].as_bool().unwrap());
        assert!(result["created"].as_bool().unwrap());

        let saved_content = fs::read_to_string(cwd.join("new_file.txt")).unwrap();
        assert_eq!(saved_content, "initial content");
    }

    #[tokio::test]
    async fn test_edit_tool_existing_file_missing_old_string_error() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "content").unwrap();

        let tool = EditTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "new_string": "new content",
            "create_if_not_exists": true
        });

        let result = tool.call(args).await.unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("Missing 'old_string' for existing file")
        );
        assert_eq!(result["error_code"], error_codes::INVALID_ARGUMENT);
    }
}

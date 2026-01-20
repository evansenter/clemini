use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tracing::instrument;

use super::{ToolEmitter, error_codes, error_response, resolve_and_validate_path};
use crate::agent::AgentEvent;

pub struct ReadTool {
    cwd: PathBuf,
    allowed_paths: Vec<PathBuf>,
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl ReadTool {
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

impl ToolEmitter for ReadTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for ReadTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "read_file".to_string(),
            "Read the contents of a file. Returns the file contents as text with line numbers. Use this to examine source code, configuration files, or other text documents. For large files, use offset and limit to read in chunks. Returns: {contents, total_lines, truncated?}".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "file_path": {
                        "type": "string",
                        "description": "The path to the file to read (absolute or relative to current directory)."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "The 1-indexed line number to start reading from. For example, to start from the beginning of the file, use 1. If the offset is beyond the end of the file, an error is returned. (default: 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "The maximum number of lines to read. Use this to avoid hitting context limits with very large files. If set to 0, no lines will be returned (only metadata like total_lines). (default: 2000)"
                    }
                }),
                vec!["file_path".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing file_path".to_string()))?;

        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;

        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

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

        // Check if binary
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(e) => {
                return Ok(error_response(
                    &format!(
                        "Failed to read {}: {}. Ensure the file exists and is not a directory.",
                        path.display(),
                        e
                    ),
                    error_codes::IO_ERROR,
                    json!({"path": path.display().to_string()}),
                ));
            }
        };

        let mut buffer = vec![0; 8192];
        let bytes_read = match file.read(&mut buffer).await {
            Ok(n) => n,
            Err(e) => {
                return Ok(error_response(
                    &format!("Failed to read {}: {}", path.display(), e),
                    error_codes::IO_ERROR,
                    json!({"path": path.display().to_string()}),
                ));
            }
        };
        buffer.truncate(bytes_read);

        if is_binary(&buffer) {
            return Ok(error_response(
                "File appears to be binary. This tool is for reading text files.",
                error_codes::BINARY_FILE,
                json!({"path": path.display().to_string()}),
            ));
        }

        match tokio::fs::read_to_string(&path).await {
            Ok(contents) => {
                let lines: Vec<&str> = contents.lines().collect();
                let total_lines = lines.len();

                let start = offset.saturating_sub(1);
                let end = (start + limit).min(total_lines);

                if start >= total_lines && total_lines > 0 {
                    return Ok(error_response(
                        &format!(
                            "Offset {} is out of bounds (total lines: {})",
                            offset, total_lines
                        ),
                        error_codes::INVALID_ARGUMENT,
                        json!({"path": path.display().to_string(), "offset": offset, "total_lines": total_lines}),
                    ));
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

                // Emit visual output
                let lines_shown = end.saturating_sub(start);
                let msg = if end < total_lines {
                    format!(
                        "  {} lines ({}-{} of {})",
                        lines_shown,
                        start + 1,
                        end,
                        total_lines
                    )
                    .dimmed()
                    .to_string()
                } else {
                    format!("  {} lines", total_lines).dimmed().to_string()
                };
                self.emit(&msg);

                Ok(response)
            }
            Err(e) => Ok(error_response(
                &format!(
                    "Failed to read {}: {}. Ensure the file exists and is not a directory.",
                    path.display(),
                    e
                ),
                error_codes::IO_ERROR,
                json!({"path": path.display().to_string()}),
            )),
        }
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }

    // Check for null bytes
    if bytes.contains(&0) {
        return true;
    }

    // Check for high proportion of non-printable characters
    let non_printable = bytes
        .iter()
        .filter(|&&b| !b.is_ascii_graphic() && !b.is_ascii_whitespace())
        .count();

    // If more than 30% are non-printable, consider it binary
    non_printable as f64 / bytes.len() as f64 > 0.3
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_read_tool_success() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "line 1\nline 2\nline 3").unwrap();

        let tool = ReadTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "offset": 1,
            "limit": 2
        });

        let result = tool.call(args).await.unwrap();
        assert_eq!(result["total_lines"], 3);
        assert!(result["contents"].as_str().unwrap().contains("line 1"));
        assert!(result["contents"].as_str().unwrap().contains("line 2"));
        assert!(!result["contents"].as_str().unwrap().contains("line 3"));
        assert!(result["truncated"].is_string());
    }

    #[tokio::test]
    async fn test_read_tool_missing_file() {
        let dir = tempdir().unwrap();
        let tool = ReadTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            None,
        );
        let args = json!({ "file_path": "missing.txt" });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("Failed to read"));
        assert_eq!(result["error_code"], error_codes::IO_ERROR);
        assert!(result["context"]["path"].is_string());
    }

    #[tokio::test]
    async fn test_read_tool_outside_cwd() {
        let dir = tempdir().unwrap();
        let tool = ReadTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            None,
        );
        let args = json!({ "file_path": "../outside.txt" });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("Access denied"));
        assert_eq!(result["error_code"], error_codes::ACCESS_DENIED);
        assert!(result["context"]["path"].is_string());
    }

    #[tokio::test]
    async fn test_read_tool_offset_out_of_bounds() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "line 1\nline 2").unwrap();

        let tool = ReadTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "offset": 5
        });

        let result = tool.call(args).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("out of bounds"));
        assert_eq!(result["error_code"], error_codes::INVALID_ARGUMENT);
        assert_eq!(result["context"]["offset"], 5);
    }

    #[tokio::test]
    async fn test_read_tool_empty_file() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("empty.txt");
        fs::write(&file_path, "").unwrap();

        let tool = ReadTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({ "file_path": "empty.txt" });

        let result = tool.call(args).await.unwrap();
        assert_eq!(result["total_lines"], 0);
        assert_eq!(result["contents"], "");
    }

    #[tokio::test]
    async fn test_read_tool_default_limit() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("large.txt");
        let content = (1..=2100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&file_path, content).unwrap();

        let tool = ReadTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({ "file_path": "large.txt" });

        let result = tool.call(args).await.unwrap();
        assert_eq!(result["total_lines"], 2100);
        assert!(
            result["truncated"]
                .as_str()
                .unwrap()
                .contains("Showing lines 1-2000")
        );
    }

    #[tokio::test]
    async fn test_read_tool_limit_zero() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("test.txt");
        fs::write(&file_path, "line 1\nline 2\nline 3").unwrap();

        let tool = ReadTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "file_path": "test.txt",
            "limit": 0
        });

        let result = tool.call(args).await.unwrap();
        assert_eq!(result["total_lines"], 3);
        assert_eq!(result["contents"], "");
        assert!(result["truncated"].is_string());
        assert!(
            result["truncated"]
                .as_str()
                .unwrap()
                .contains("Showing lines 1-0 of 3")
        );
    }

    #[tokio::test]
    async fn test_read_tool_binary_file() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let file_path = cwd.join("binary.bin");
        // PNG header + some nulls
        fs::write(&file_path, b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0DIHDR").unwrap();

        let tool = ReadTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({ "file_path": "binary.bin" });

        let result = tool.call(args).await.unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("File appears to be binary")
        );
        assert_eq!(result["error_code"], error_codes::BINARY_FILE);
        assert!(result["context"]["path"].is_string());
    }
}

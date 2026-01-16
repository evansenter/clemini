use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

use super::{make_relative, resolve_and_validate_path};

pub struct ListDirectoryTool {
    cwd: PathBuf,
}

impl ListDirectoryTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl CallableFunction for ListDirectoryTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "list_directory".to_string(),
            "List entries in a directory. Returns name, type (file/directory), and size in bytes.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "path": {
                        "type": "string",
                        "description": "The path to list entries for (defaults to current directory)"
                    }
                }),
                vec![],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");

        let path = match resolve_and_validate_path(path_str, &self.cwd) {
            Ok(p) => p,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Access denied: {}. Only directories within the current working directory can be accessed.", e)
                }));
            }
        };

        if !path.is_dir() {
            return Ok(json!({
                "error": format!("Path '{}' is not a directory", path_str)
            }));
        }

        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(e) => {
                return Ok(json!({
                    "error": format!("Failed to read directory: {}", e)
                }));
            }
        };

        let mut result = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    return Ok(json!({
                        "error": format!("Error reading entry: {}", e)
                    }));
                }
            };
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    return Ok(json!({
                        "error": format!("Error reading metadata: {}", e)
                    }));
                }
            };
            
            let name = entry.file_name().to_string_lossy().to_string();
            let entry_type = if metadata.is_dir() {
                "directory"
            } else if metadata.is_file() {
                "file"
            } else if metadata.is_symlink() {
                "symlink"
            } else {
                "unknown"
            };
            let size = metadata.len();

            result.push(json!({
                "name": name,
                "type": entry_type,
                "size": size
            }));
        }

        Ok(json!({
            "path": make_relative(&path, &self.cwd),
            "entries": result
        }))
    }
}

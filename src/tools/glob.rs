use crate::agent::AgentEvent;
use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use glob::glob;
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::instrument;

use super::{
    DEFAULT_EXCLUDES, ToolEmitter, error_codes, error_response, make_relative,
    resolve_and_validate_path, validate_path,
};

pub struct GlobTool {
    cwd: PathBuf,
    allowed_paths: Vec<PathBuf>,
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl GlobTool {
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

impl ToolEmitter for GlobTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for GlobTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "glob".to_string(),
            "Find files matching a glob pattern. Use patterns like '**/*.rs' for recursive search or 'src/*.rs' for single directory. Returns: {matches[], count, total_found, truncated}".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match (e.g., '**/*.rs', 'src/**/*.ts', '*.json')"
                    },
                    "directory": {
                        "type": "string",
                        "description": "Directory to search in (relative to cwd or absolute). Defaults to current working directory."
                    },
                    "sort": {
                        "type": "string",
                        "description": "How to sort results: 'name' (alphabetical), 'modified' (newest first), 'size' (largest first). (default: 'name')",
                        "enum": ["name", "modified", "size"]
                    },
                    "head_limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return from the final list (applied after sorting). (default: no limit)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Number of results to skip from the beginning of the final list (for pagination). (default: 0)"
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

        let search_path = args.get("directory").and_then(|v| v.as_str());
        let sort_by = args.get("sort").and_then(|v| v.as_str()).unwrap_or("name");
        let head_limit = args.get("head_limit").and_then(|v| v.as_u64());
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        // Resolve and validate the search path
        let base_dir = if let Some(p) = search_path {
            match resolve_and_validate_path(p, &self.cwd, &self.allowed_paths) {
                Ok(p) => p,
                Err(e) => {
                    return Ok(error_response(
                        &format!(
                            "Access denied for path '{}': {}. Path must be within allowed paths.",
                            p, e
                        ),
                        error_codes::ACCESS_DENIED,
                        json!({"path": p}),
                    ));
                }
            }
        } else {
            self.cwd.clone()
        };

        // Construct full pattern from base_dir
        let full_pattern = base_dir.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        // Logging handled by main.rs event loop

        match glob(&pattern_str) {
            Ok(paths) => {
                let mut matches: Vec<(String, std::time::SystemTime, u64)> = Vec::new();
                let mut errors: Vec<String> = Vec::new();

                for entry in paths {
                    match entry {
                        Ok(path) => {
                            // Security check - only include files within allowed paths
                            let path = match validate_path(&path, &self.allowed_paths) {
                                Ok(p) => p,
                                Err(_) => continue, // Skip files outside allowed paths
                            };

                            let metadata = match path.metadata() {
                                Ok(m) => m,
                                Err(_) => continue,
                            };

                            // Only include files, skip directories
                            if !metadata.is_file() {
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
                            let modified = metadata
                                .modified()
                                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                            let size = metadata.len();
                            matches.push((relative, modified, size));
                        }
                        Err(e) => {
                            errors.push(e.to_string());
                        }
                    }
                }

                match sort_by {
                    "modified" => matches.sort_by(|a, b| b.1.cmp(&a.1)), // Newest first
                    "size" => matches.sort_by(|a, b| b.2.cmp(&a.2)),     // Largest first
                    _ => matches.sort_by(|a, b| a.0.cmp(&b.0)),          // Name alphabetical
                }

                let mut matches: Vec<String> = matches.into_iter().map(|m| m.0).collect();
                let total_found = matches.len();

                if offset > 0 {
                    if offset >= matches.len() {
                        matches = Vec::new();
                    } else {
                        matches.drain(0..offset);
                    }
                }

                if let Some(limit) = head_limit {
                    matches.truncate(limit as usize);
                }

                if matches.is_empty() && offset == 0 {
                    // Check if the pattern matches a directory
                    let full_path = base_dir.join(pattern);
                    if validate_path(&full_path, &self.allowed_paths).is_ok_and(|p| p.is_dir()) {
                        return Ok(error_response(
                            &format!(
                                "Pattern '{}' matches a directory. Use '{}/*' for files.",
                                pattern, pattern
                            ),
                            error_codes::INVALID_ARGUMENT,
                            json!({"pattern": pattern}),
                        ));
                    }

                    return Ok(error_response(
                        &format!("No files matched pattern '{}'", pattern),
                        error_codes::NOT_FOUND,
                        json!({"pattern": pattern}),
                    ));
                }

                let count = matches.len();
                self.emit(&format!("  {}", format!("{} files", count).dimmed()));

                Ok(json!({
                    "pattern": pattern,
                    "matches": matches,
                    "count": count,
                    "total_found": total_found,
                    "truncated": head_limit.is_some_and(|l| total_found > offset + l as usize),
                    "errors": if errors.is_empty() { Value::Null } else { json!(errors) }
                }))
            }
            Err(e) => Ok(error_response(
                &format!("Invalid glob pattern: {}", e),
                error_codes::INVALID_ARGUMENT,
                json!({"pattern": pattern}),
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
    async fn test_glob_tool_success() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::create_dir(cwd.join("src")).unwrap();
        fs::write(cwd.join("src/main.rs"), "").unwrap();
        fs::write(cwd.join("src/lib.rs"), "").unwrap();
        fs::write(cwd.join("README.md"), "").unwrap();

        let tool = GlobTool::new(cwd.clone(), vec![cwd.clone()], None);
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

        let tool = GlobTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({ "pattern": "**/*" });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].as_str().unwrap(), "file.txt");
    }

    #[tokio::test]
    async fn test_glob_tool_no_matches() {
        let dir = tempdir().unwrap();
        let tool = GlobTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            None,
        );
        let args = json!({ "pattern": "*.nonexistent" });

        let result = tool.call(args).await.unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("No files matched")
        );
        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
        assert_eq!(result["context"]["pattern"], "*.nonexistent");
    }

    #[tokio::test]
    async fn test_glob_tool_with_path() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let subdir = cwd.join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("test.txt"), "hello").unwrap();
        fs::write(cwd.join("root.txt"), "world").unwrap();

        let tool = GlobTool::new(cwd.clone(), vec![cwd.clone()], None);

        // Search in subdir
        let args = json!({
            "pattern": "*.txt",
            "directory": "subdir"
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].as_str().unwrap(), "subdir/test.txt");

        // Search in root (default)
        let args = json!({
            "pattern": "*.txt"
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].as_str().unwrap(), "root.txt");
    }

    #[tokio::test]
    async fn test_glob_tool_sorting() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();

        // Create files with different names, sizes, and modification times
        fs::write(cwd.join("a.txt"), "small").unwrap(); // 5 bytes
        std::thread::sleep(std::time::Duration::from_millis(100));
        fs::write(cwd.join("c.txt"), "medium content").unwrap(); // 14 bytes
        std::thread::sleep(std::time::Duration::from_millis(100));
        fs::write(cwd.join("b.txt"), "very large content indeed").unwrap(); // 25 bytes

        let tool = GlobTool::new(cwd.clone(), vec![cwd.clone()], None);

        // Sort by name (alphabetical)
        let args = json!({
            "pattern": "*.txt",
            "sort": "name"
        });
        let result = tool.call(args).await.unwrap();
        let matches: Vec<String> = result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(matches, vec!["a.txt", "b.txt", "c.txt"]);

        // Sort by size (largest first)
        let args = json!({
            "pattern": "*.txt",
            "sort": "size"
        });
        let result = tool.call(args).await.unwrap();
        let matches: Vec<String> = result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(matches, vec!["b.txt", "c.txt", "a.txt"]);

        // Sort by modified (newest first)
        let args = json!({
            "pattern": "*.txt",
            "sort": "modified"
        });
        let result = tool.call(args).await.unwrap();
        let matches: Vec<String> = result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        // c.txt was written last (newest), then b.txt, then a.txt
        // Wait, I wrote a, then c, then b.
        // So b is newest, then c, then a.
        assert_eq!(matches, vec!["b.txt", "c.txt", "a.txt"]);
    }

    #[tokio::test]
    async fn test_glob_tool_pagination() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();

        fs::write(cwd.join("a.txt"), "").unwrap();
        fs::write(cwd.join("b.txt"), "").unwrap();
        fs::write(cwd.join("c.txt"), "").unwrap();
        fs::write(cwd.join("d.txt"), "").unwrap();

        let tool = GlobTool::new(cwd.clone(), vec![cwd.clone()], None);

        // Test offset
        let args = json!({
            "pattern": "*.txt",
            "offset": 2
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].as_str().unwrap(), "c.txt");
        assert_eq!(matches[1].as_str().unwrap(), "d.txt");

        // Test head_limit
        let args = json!({
            "pattern": "*.txt",
            "head_limit": 2
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].as_str().unwrap(), "a.txt");
        assert_eq!(matches[1].as_str().unwrap(), "b.txt");
        assert!(result["truncated"].as_bool().unwrap());

        // Test offset + head_limit
        let args = json!({
            "pattern": "*.txt",
            "offset": 1,
            "head_limit": 2
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].as_str().unwrap(), "b.txt");
        assert_eq!(matches[1].as_str().unwrap(), "c.txt");
        assert!(result["truncated"].as_bool().unwrap());
    }
}

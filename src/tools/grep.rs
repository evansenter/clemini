use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use globset::{Glob, GlobSetBuilder};
use grep::regex::RegexMatcherBuilder;
use grep::searcher::{BinaryDetection, SearcherBuilder, Sink, SinkContext, SinkFinish, SinkMatch};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::instrument;

pub struct GrepTool {
    cwd: PathBuf,
    _allowed_paths: Vec<PathBuf>,
}

use crate::tools::DEFAULT_EXCLUDES;

const MAX_LINE_LENGTH: usize = 1000;

impl GrepTool {
    pub fn new(cwd: PathBuf, allowed_paths: Vec<PathBuf>) -> Self {
        Self {
            cwd,
            _allowed_paths: allowed_paths,
        }
    }
}

fn truncate_line(l: &str) -> String {
    if l.len() > MAX_LINE_LENGTH {
        let truncated: String = l.chars().take(MAX_LINE_LENGTH).collect();
        format!("{}... [truncated]", truncated)
    } else {
        l.to_string()
    }
}

struct GrepSink<'a> {
    path: &'a Path,
    matches: Arc<Mutex<Vec<Value>>>,
    max_results: usize,
    context: u64,
    current_block: Option<MatchBlock>,
}

struct MatchBlock {
    file: String,
    line: u64,
    lines: Vec<(u64, String, bool)>, // (line_number, content, is_match)
}

impl<'a> GrepSink<'a> {
    fn flush_block(&mut self) {
        if let Some(block) = self.current_block.take() {
            let mut matches = self.matches.lock().unwrap();
            if matches.len() >= self.max_results {
                return;
            }

            let formatted_content = block
                .lines
                .iter()
                .map(|(num, text, is_match)| {
                    let prefix = if *is_match { ">" } else { " " };
                    format!("{}{:>4}:{}", prefix, num, truncate_line(text))
                })
                .collect::<Vec<_>>()
                .join("\n");

            matches.push(json!({
                "file": block.file,
                "line": block.line,
                "content": formatted_content
            }));
        }
    }
}

impl<'a> Sink for GrepSink<'a> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &grep::searcher::Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        let line_number = mat.line_number().unwrap_or(0);
        let content = std::str::from_utf8(mat.bytes())
            .unwrap_or("")
            .trim_end()
            .to_string();
        let path_str = self.path.to_string_lossy().to_string();

        if self.context == 0 {
            let mut matches = self.matches.lock().unwrap();
            matches.push(json!({
                "file": path_str,
                "line": line_number,
                "content": truncate_line(content.trim())
            }));
            if matches.len() >= self.max_results {
                return Ok(false);
            }
        } else if let Some(block) = self.current_block.as_mut() {
            if line_number > block.lines.last().unwrap().0 + 1 {
                self.flush_block();
                self.current_block = Some(MatchBlock {
                    file: path_str,
                    line: line_number,
                    lines: vec![(line_number, content, true)],
                });
            } else {
                block.lines.push((line_number, content, true));
            }
        } else {
            self.current_block = Some(MatchBlock {
                file: path_str,
                line: line_number,
                lines: vec![(line_number, content, true)],
            });
        }

        let matches_len = self.matches.lock().unwrap().len();
        Ok(matches_len < self.max_results)
    }

    fn context(
        &mut self,
        _searcher: &grep::searcher::Searcher,
        mat: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        if self.context == 0 {
            return Ok(true);
        }

        let line_number = mat.line_number().unwrap_or(0);
        let content = std::str::from_utf8(mat.bytes())
            .unwrap_or("")
            .trim_end()
            .to_string();
        let path_str = self.path.to_string_lossy().to_string();

        if let Some(block) = self.current_block.as_mut() {
            if line_number > block.lines.last().unwrap().0 + 1 {
                self.flush_block();
                self.current_block = Some(MatchBlock {
                    file: path_str,
                    line: line_number,
                    lines: vec![(line_number, content, false)],
                });
            } else {
                block.lines.push((line_number, content, false));
            }
        } else {
            self.current_block = Some(MatchBlock {
                file: path_str,
                line: line_number,
                lines: vec![(line_number, content, false)],
            });
        }

        let matches_len = self.matches.lock().unwrap().len();
        Ok(matches_len < self.max_results)
    }

    fn finish(
        &mut self,
        _searcher: &grep::searcher::Searcher,
        _finish: &SinkFinish,
    ) -> Result<(), Self::Error> {
        self.flush_block();
        Ok(())
    }
}

#[async_trait]
impl CallableFunction for GrepTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "grep".to_string(),
            "Search for a pattern in files using ripgrep. Returns matching lines with file paths and line numbers. Supports regex patterns, case-insensitive search, and context lines.".to_string(),
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

        let file_pattern = args
            .get("file_pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("**/*");

        let case_insensitive = args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let context = args.get("context").and_then(|v| v.as_u64()).unwrap_or(0);

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;

        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(case_insensitive)
            .build(pattern)
            .map_err(|e| FunctionError::ExecutionError(format!("Invalid regex: {}", e).into()))?;

        let mut searcher = SearcherBuilder::new()
            .before_context(context as usize)
            .after_context(context as usize)
            .line_number(true)
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .build();

        let matches = Arc::new(Mutex::new(Vec::<Value>::new()));
        let mut files_searched = 0;
        let mut files_with_matches = 0;

        let mut glob_builder = GlobSetBuilder::new();
        glob_builder.add(Glob::new(file_pattern).map_err(|e| {
            FunctionError::ExecutionError(format!("Invalid file pattern: {}", e).into())
        })?);
        let glob_set = glob_builder.build().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to build glob set: {}", e).into())
        })?;

        let mut override_builder = OverrideBuilder::new(&self.cwd);
        for exclude in DEFAULT_EXCLUDES {
            override_builder
                .add(&format!("!{}", exclude))
                .map_err(|e| {
                    FunctionError::ExecutionError(format!("Invalid exclude: {}", e).into())
                })?;
        }
        let overrides = override_builder.build().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to build overrides: {}", e).into())
        })?;

        let mut walker = WalkBuilder::new(&self.cwd);
        walker.overrides(overrides);
        let walk = walker.build();
        for result in walk {
            let entry = match result {
                Ok(entry) => entry,
                Err(_) => continue,
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }

            let path = entry.path();
            let relative_path = path.strip_prefix(&self.cwd).unwrap_or(path);

            if !glob_set.is_match(relative_path) {
                continue;
            }

            files_searched += 1;
            let mut sink = GrepSink {
                path: relative_path,
                matches: Arc::clone(&matches),
                max_results,
                context,
                current_block: None,
            };

            let prev_count = matches.lock().unwrap().len();
            if searcher.search_path(&matcher, path, &mut sink).is_err() {
                continue;
            }
            if matches.lock().unwrap().len() > prev_count {
                files_with_matches += 1;
            }

            if matches.lock().unwrap().len() >= max_results {
                break;
            }
        }

        let final_matches = Arc::try_unwrap(matches).unwrap().into_inner().unwrap();

        if final_matches.is_empty() {
            return Ok(json!({
                "error": format!("No matches found for pattern '{}' in files matching '{}'.", pattern, file_pattern),
                "pattern": pattern,
                "file_pattern": file_pattern
            }));
        }

        Ok(json!({
            "pattern": pattern,
            "file_pattern": file_pattern,
            "matches": final_matches,
            "count": final_matches.len(),
            "files_searched": files_searched,
            "files_with_matches": files_with_matches,
            "truncated": final_matches.len() >= max_results
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_grep_tool_success() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(cwd.join("test.txt"), "hello world\ngoodbye world").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()]);
        let args = json!({
            "pattern": "hello"
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["file"], "test.txt");
        assert_eq!(matches[0]["line"], 1);
        assert!(
            matches[0]["content"]
                .as_str()
                .unwrap()
                .contains("hello world")
        );
    }

    #[tokio::test]
    async fn test_grep_tool_with_context() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(cwd.join("test.txt"), "line 1\nline 2 (match)\nline 3").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()]);
        let args = json!({
            "pattern": "match",
            "context": 1
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        let content = matches[0]["content"].as_str().unwrap();
        assert!(content.contains("line 1"));
        assert!(content.contains("line 2 (match)"));
        assert!(content.contains("line 3"));
    }

    #[tokio::test]
    async fn test_grep_tool_no_matches() {
        let dir = tempdir().unwrap();
        let tool = GrepTool::new(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let args = json!({ "pattern": "nonexistent" });

        let result = tool.call(args).await.unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("No matches found")
        );
    }
}

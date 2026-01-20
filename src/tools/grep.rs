use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use globset::{Glob, GlobSetBuilder};
use grep::regex::RegexMatcherBuilder;
use grep::searcher::{BinaryDetection, SearcherBuilder, Sink, SinkContext, SinkFinish, SinkMatch};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::instrument;

use crate::agent::AgentEvent;
use crate::tools::{
    DEFAULT_EXCLUDES, ToolEmitter, error_codes, error_response, make_relative,
    resolve_and_validate_path, validate_path,
};

const MAX_LINE_LENGTH: usize = 1000;

pub struct GrepTool {
    cwd: PathBuf,
    allowed_paths: Vec<PathBuf>,
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl GrepTool {
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

impl ToolEmitter for GrepTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum OutputMode {
    Content,
    FilesWithMatches,
    Count,
}

struct GrepSink {
    path: String,
    matches: Arc<Mutex<Vec<Value>>>,
    max_results: usize,
    before_context: u64,
    after_context: u64,
    multiline: bool,
    current_block: Option<MatchBlock>,
    output_mode: OutputMode,
    match_count: usize,
}

struct MatchBlock {
    file: String,
    line: u64,
    lines: Vec<(u64, String, bool)>, // (line_number, content, is_match)
}

impl GrepSink {
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

impl Sink for GrepSink {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &grep::searcher::Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        self.match_count += 1;

        match self.output_mode {
            OutputMode::FilesWithMatches => {
                let mut matches = self.matches.lock().unwrap();
                matches.push(json!({
                    "file": self.path.clone()
                }));
                return Ok(false); // Stop searching this file
            }
            OutputMode::Count => {
                return Ok(true); // Continue searching to count matches
            }
            OutputMode::Content => {
                let start_line_number = mat.line_number().unwrap_or(0);
                let content = std::str::from_utf8(mat.bytes())
                    .unwrap_or("")
                    .trim_end_matches(['\n', '\r'])
                    .to_string();
                let path_str = self.path.clone();

                // Split into lines to handle multiline matches correctly
                let lines: Vec<&str> = content.split('\n').collect();

                // If it's a single line match and no context, keep existing simple format
                if !self.multiline && self.before_context == 0 && self.after_context == 0 {
                    let mut matches = self.matches.lock().unwrap();
                    matches.push(json!({
                        "file": path_str,
                        "line": start_line_number,
                        "content": truncate_line(content.trim())
                    }));
                    if matches.len() >= self.max_results {
                        return Ok(false);
                    }
                } else {
                    // Use block formatting for multiline or when context is requested
                    let mut current_line_number = start_line_number;
                    for line in lines {
                        let line_content = line.trim_end_matches('\r').to_string();

                        if let Some(block) = self.current_block.as_mut() {
                            if current_line_number > block.lines.last().unwrap().0 + 1 {
                                self.flush_block();
                                self.current_block = Some(MatchBlock {
                                    file: path_str.clone(),
                                    line: current_line_number,
                                    lines: vec![(current_line_number, line_content, true)],
                                });
                            } else {
                                block.lines.push((current_line_number, line_content, true));
                            }
                        } else {
                            self.current_block = Some(MatchBlock {
                                file: path_str.clone(),
                                line: current_line_number,
                                lines: vec![(current_line_number, line_content, true)],
                            });
                        }
                        current_line_number += 1;
                    }
                }
            }
        }

        let matches_len = self.matches.lock().unwrap().len();
        Ok(matches_len < self.max_results)
    }

    fn context(
        &mut self,
        _searcher: &grep::searcher::Searcher,
        mat: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        if self.output_mode != OutputMode::Content
            || (self.before_context == 0 && self.after_context == 0)
        {
            return Ok(true);
        }

        let line_number = mat.line_number().unwrap_or(0);
        let content = std::str::from_utf8(mat.bytes())
            .unwrap_or("")
            .trim_end_matches(['\r', '\n'])
            .to_string();
        let path_str = self.path.clone();

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
        match self.output_mode {
            OutputMode::Content => {
                self.flush_block();
            }
            OutputMode::Count => {
                if self.match_count > 0 {
                    let mut matches = self.matches.lock().unwrap();
                    matches.push(json!({
                        "file": self.path.clone(),
                        "count": self.match_count
                    }));
                }
            }
            OutputMode::FilesWithMatches => {}
        }
        Ok(())
    }
}

#[async_trait]
impl CallableFunction for GrepTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "grep".to_string(),
            "Search for a pattern in files using ripgrep. Supports regex, case-insensitive search, and different output modes (content, files_with_matches, count). Returns: {matches[], count, total_found, truncated?}".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for (e.g., 'fn\\s+\\w+', 'TODO|FIXME', 'impl.*for')"
                    },
                    "file_pattern": {
                        "type": "string",
                        "description": "Glob pattern for files to search (e.g., '**/*.rs', 'src/*.ts'). (default: '**/*')"
                    },
                    "type": {
                        "type": "string",
                        "description": "Filter by file type (e.g., 'rust', 'js', 'ts', 'py', 'go', 'json', 'md', 'toml', 'yaml')."
                    },
                    "directory": {
                        "type": "string",
                        "description": "Directory to search in (relative to cwd or absolute). Defaults to current working directory."
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "If true, perform case-insensitive matching (default: false)"
                    },
                    "context": {
                        "type": "integer",
                        "description": "Number of lines to show before and after each match (default: 0)"
                    },
                    "before_context": {
                        "type": "integer",
                        "description": "Number of lines to show before each match. Takes precedence over 'context' (default: 0)"
                    },
                    "after_context": {
                        "type": "integer",
                        "description": "Number of lines to show after each match. Takes precedence over 'context' (default: 0)"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of matches to find during search (default: 100). Use this to limit search effort."
                    },
                    "head_limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return from the final list (applied after search). (default: no limit)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Number of results to skip from the beginning of the final list (for pagination). (default: 0)"
                    },
                    "multiline": {
                        "type": "boolean",
                        "description": "If true, allow patterns to match across multiple lines (default: false). Use [\\s\\S] to match any character including newline."
                    },
                    "output_mode": {
                        "type": "string",
                        "enum": ["content", "files_with_matches", "count"],
                        "description": "Output format: 'content' for matching lines, 'files_with_matches' for just file paths, 'count' for number of matches per file (default: 'content')"
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
        let before_context = args
            .get("before_context")
            .and_then(|v| v.as_u64())
            .unwrap_or(context);
        let after_context = args
            .get("after_context")
            .and_then(|v| v.as_u64())
            .unwrap_or(context);

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;

        let head_limit = args.get("head_limit").and_then(|v| v.as_u64());
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        let multiline = args
            .get("multiline")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let search_path = args.get("directory").and_then(|v| v.as_str());

        let type_arg = args.get("type").and_then(|v| v.as_str());

        let output_mode = match args.get("output_mode").and_then(|v| v.as_str()) {
            Some("files_with_matches") => OutputMode::FilesWithMatches,
            Some("count") => OutputMode::Count,
            _ => OutputMode::Content,
        };

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

        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(case_insensitive)
            .multi_line(true)
            .build(pattern)
            .map_err(|e| FunctionError::ExecutionError(format!("Invalid regex: {}", e).into()))?;

        let mut searcher_builder = SearcherBuilder::new();
        searcher_builder
            .before_context(before_context as usize)
            .after_context(after_context as usize)
            .line_number(true)
            .multi_line(multiline)
            .binary_detection(BinaryDetection::quit(b'\x00'));

        let mut searcher = searcher_builder.build();

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

        let mut type_glob_set = None;
        if let Some(t) = type_arg {
            let mut type_glob_builder = GlobSetBuilder::new();
            let globs = match t {
                "rust" => vec!["**/*.rs"],
                "js" => vec!["**/*.js", "**/*.mjs", "**/*.cjs"],
                "py" => vec!["**/*.py"],
                "ts" => vec!["**/*.ts", "**/*.tsx"],
                "go" => vec!["**/*.go"],
                "json" => vec!["**/*.json"],
                "md" => vec!["**/*.md"],
                "toml" => vec!["**/*.toml"],
                "yaml" => vec!["**/*.yaml", "**/*.yml"],
                _ => vec![],
            };

            if globs.is_empty() {
                return Ok(error_response(
                    &format!("Unsupported file type: {}", t),
                    error_codes::INVALID_ARGUMENT,
                    json!({"type": t}),
                ));
            }

            for g in globs {
                type_glob_builder.add(Glob::new(g).map_err(|e| {
                    FunctionError::ExecutionError(format!("Invalid type glob: {}", e).into())
                })?);
            }
            type_glob_set = Some(type_glob_builder.build().map_err(|e| {
                FunctionError::ExecutionError(
                    format!("Failed to build type glob set: {}", e).into(),
                )
            })?);
        }

        let mut override_builder = OverrideBuilder::new(&base_dir);
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

        let mut walker = WalkBuilder::new(&base_dir);
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

            // Validate path is within allowed paths
            if validate_path(path, &self.allowed_paths).is_err() {
                continue;
            }

            let relative_path_str = make_relative(path, &self.cwd);

            if !glob_set.is_match(&relative_path_str) {
                continue;
            }

            if let Some(ref tgs) = type_glob_set
                && !tgs.is_match(&relative_path_str)
            {
                continue;
            }

            files_searched += 1;
            let mut sink = GrepSink {
                path: relative_path_str,
                matches: Arc::clone(&matches),
                max_results,
                before_context,
                after_context,
                multiline,
                current_block: None,
                output_mode,
                match_count: 0,
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

        let mut final_matches = Arc::try_unwrap(matches).unwrap().into_inner().unwrap();

        let total_found = final_matches.len();
        if offset > 0 {
            if offset >= final_matches.len() {
                final_matches = Vec::new();
            } else {
                final_matches.drain(0..offset);
            }
        }

        if let Some(limit) = head_limit {
            final_matches.truncate(limit as usize);
        }

        if final_matches.is_empty() {
            let mut error_msg = format!(
                "No matches found for pattern '{}' in files matching '{}'",
                pattern, file_pattern
            );
            if let Some(t) = type_arg {
                error_msg.push_str(&format!(" and type '{}'", t));
            }

            self.emit(&format!("  {}", "no matches".dimmed()));

            return Ok(error_response(
                &error_msg,
                error_codes::NOT_FOUND,
                json!({"pattern": pattern, "file_pattern": file_pattern, "type": type_arg}),
            ));
        }

        // Emit visual output
        let msg = format!("  {} matches in {} files", total_found, files_with_matches)
            .dimmed()
            .to_string();
        self.emit(&msg);

        Ok(json!({
            "pattern": pattern,
            "file_pattern": file_pattern,
            "type": type_arg,
            "matches": final_matches,
            "count": final_matches.len(),
            "total_found": total_found,
            "files_searched": files_searched,
            "files_with_matches": files_with_matches,
            "truncated": total_found >= max_results || head_limit.is_some_and(|l| total_found > offset + l as usize)
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

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);
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

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);
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
        let tool = GrepTool::new(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            None,
        );
        let args = json!({ "pattern": "nonexistent" });

        let result = tool.call(args).await.unwrap();
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("No matches found")
        );
        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
        assert_eq!(result["context"]["pattern"], "nonexistent");
    }

    #[tokio::test]
    async fn test_grep_special_characters() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(cwd.join("test.txt"), "some [special] (chars) here.").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "pattern": r"\[special\] \(chars\)"
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert!(
            matches[0]["content"]
                .as_str()
                .unwrap()
                .contains("[special] (chars)")
        );
    }

    #[tokio::test]
    async fn test_grep_case_insensitive() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(cwd.join("test.txt"), "HELLO world").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "pattern": "hello",
            "case_insensitive": true
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert!(
            matches[0]["content"]
                .as_str()
                .unwrap()
                .contains("HELLO world")
        );
    }

    #[tokio::test]
    async fn test_grep_subdirectory() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let sub = cwd.join("subdir");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("test.txt"), "match in subdir").unwrap();
        fs::write(cwd.join("test.txt"), "match in root").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "pattern": "match",
            "file_pattern": "subdir/*"
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["file"], "subdir/test.txt");
    }

    #[tokio::test]
    async fn test_grep_with_path() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let sub = cwd.join("subdir");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("test.txt"), "match in subdir").unwrap();
        fs::write(cwd.join("test.txt"), "match in root").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);

        // Search ONLY in subdir using the directory parameter
        let args = json!({
            "pattern": "match",
            "directory": "subdir"
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();

        // Should ONLY find the one in subdir
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["file"], "subdir/test.txt");
    }

    #[tokio::test]
    async fn test_grep_security_boundary() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let allowed_dir = cwd.join("allowed");
        let restricted_dir = cwd.join("restricted");
        fs::create_dir(&allowed_dir).unwrap();
        fs::create_dir(&restricted_dir).unwrap();

        fs::write(allowed_dir.join("test.txt"), "secret in allowed").unwrap();
        fs::write(restricted_dir.join("test.txt"), "secret in restricted").unwrap();

        // Tool is configured with cwd, but ONLY allowed_dir is in allowed_paths
        let tool = GrepTool::new(cwd.clone(), vec![allowed_dir.clone()], None);

        let args = json!({
            "pattern": "secret"
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();

        // Should only find the one in allowed_dir
        assert_eq!(matches.len(), 1);
        let file_path = matches[0]["file"].as_str().unwrap();
        assert!(file_path.contains("allowed"));
        assert!(!file_path.contains("restricted"));
    }

    #[tokio::test]
    async fn test_grep_output_mode_files_with_matches() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(cwd.join("test1.txt"), "match").unwrap();
        fs::write(cwd.join("test2.txt"), "match").unwrap();
        fs::write(cwd.join("test3.txt"), "other").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "pattern": "match",
            "output_mode": "files_with_matches"
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();

        assert_eq!(matches.len(), 2);
        let files: Vec<_> = matches
            .iter()
            .map(|m| m["file"].as_str().unwrap())
            .collect();
        assert!(files.contains(&"test1.txt"));
        assert!(files.contains(&"test2.txt"));
        assert!(!files.contains(&"test3.txt"));
        // Ensure no 'content' or 'line' in results
        assert!(matches[0].get("content").is_none());
        assert!(matches[0].get("line").is_none());
    }

    #[tokio::test]
    async fn test_grep_output_mode_count() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(cwd.join("test1.txt"), "match\nmatch\nother").unwrap();
        fs::write(cwd.join("test2.txt"), "match\nother").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);
        let args = json!({
            "pattern": "match",
            "output_mode": "count"
        });

        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();

        assert_eq!(matches.len(), 2);
        for m in matches {
            if m["file"] == "test1.txt" {
                assert_eq!(m["count"], 2);
            } else if m["file"] == "test2.txt" {
                assert_eq!(m["count"], 1);
            } else {
                panic!("Unexpected file: {}", m["file"]);
            }
        }
    }

    #[tokio::test]
    async fn test_grep_asymmetric_context() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(
            cwd.join("test.txt"),
            "line 1\nline 2\nline 3 (match)\nline 4\nline 5",
        )
        .unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);

        // Test -B (before_context) only
        let args = json!({
            "pattern": "match",
            "before_context": 2,
            "after_context": 0
        });
        let result = tool.call(args).await.unwrap();
        let content = result["matches"][0]["content"].as_str().unwrap();
        assert!(content.contains("line 1"));
        assert!(content.contains("line 2"));
        assert!(content.contains("line 3 (match)"));
        assert!(!content.contains("line 4"));

        // Test -A (after_context) only
        let args = json!({
            "pattern": "match",
            "before_context": 0,
            "after_context": 2
        });
        let result = tool.call(args).await.unwrap();
        let content = result["matches"][0]["content"].as_str().unwrap();
        assert!(!content.contains("line 1"));
        assert!(content.contains("line 3 (match)"));
        assert!(content.contains("line 4"));
        assert!(content.contains("line 5"));

        // Test -A and -B together
        let args = json!({
            "pattern": "match",
            "before_context": 1,
            "after_context": 1
        });
        let result = tool.call(args).await.unwrap();
        let content = result["matches"][0]["content"].as_str().unwrap();
        assert!(!content.contains("line 1"));
        assert!(content.contains("line 2"));
        assert!(content.contains("line 3 (match)"));
        assert!(content.contains("line 4"));
        assert!(!content.contains("line 5"));

        // Test precedence: -A/-B should override context
        let args = json!({
            "pattern": "match",
            "context": 10,
            "before_context": 1,
            "after_context": 1
        });
        let result = tool.call(args).await.unwrap();
        let content = result["matches"][0]["content"].as_str().unwrap();
        assert!(!content.contains("line 1")); // Would be present if context=10 was used
        assert!(content.contains("line 2"));
        assert!(content.contains("line 3 (match)"));
        assert!(content.contains("line 4"));
        assert!(!content.contains("line 5")); // Would be present if context=10 was used
    }

    #[tokio::test]
    async fn test_grep_with_type() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(cwd.join("test.rs"), "fn main() { println!(\"hello\"); }").unwrap();
        fs::write(cwd.join("test.js"), "console.log(\"hello\");").unwrap();
        fs::write(cwd.join("test.mjs"), "export const hello = \"hello\";").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);

        // Test type: "rust"
        let args = json!({
            "pattern": "hello",
            "type": "rust"
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["file"], "test.rs");

        // Test type: "js" (should find .js and .mjs)
        let args = json!({
            "pattern": "hello",
            "type": "js"
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        let files: Vec<_> = matches
            .iter()
            .map(|m| m["file"].as_str().unwrap())
            .collect();
        assert!(files.contains(&"test.js"));
        assert!(files.contains(&"test.mjs"));
    }

    #[tokio::test]
    async fn test_grep_with_type_and_pattern() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::create_dir(cwd.join("src")).unwrap();
        fs::write(cwd.join("src/main.rs"), "hello").unwrap();
        fs::write(cwd.join("main.rs"), "hello").unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);

        let args = json!({
            "pattern": "hello",
            "type": "rust",
            "file_pattern": "src/*"
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["file"], "src/main.rs");
    }

    #[tokio::test]
    async fn test_grep_pagination() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(
            cwd.join("test.txt"),
            "match 1\nmatch 2\nmatch 3\nmatch 4\nmatch 5",
        )
        .unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);

        // Test offset
        let args = json!({
            "pattern": "match",
            "offset": 2
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 3);
        assert!(matches[0]["content"].as_str().unwrap().contains("match 3"));

        // Test head_limit
        let args = json!({
            "pattern": "match",
            "head_limit": 2
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert!(matches[0]["content"].as_str().unwrap().contains("match 1"));
        assert!(matches[1]["content"].as_str().unwrap().contains("match 2"));
        assert!(result["truncated"].as_bool().unwrap());

        // Test offset + head_limit
        let args = json!({
            "pattern": "match",
            "offset": 1,
            "head_limit": 2
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert!(matches[0]["content"].as_str().unwrap().contains("match 2"));
        assert!(matches[1]["content"].as_str().unwrap().contains("match 3"));
        assert!(result["truncated"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_grep_multiline() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        fs::write(
            cwd.join("test.rs"),
            "struct Test {\n    field: i32,\n}\n\nfn main() {}",
        )
        .unwrap();

        let tool = GrepTool::new(cwd.clone(), vec![cwd.clone()], None);

        // Test with multiline: true
        let args = json!({
            "pattern": r"struct.*\{[\s\S]*?\}",
            "multiline": true
        });
        let result = tool.call(args).await.unwrap();
        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        let content = matches[0]["content"].as_str().unwrap();
        assert!(content.contains("struct Test {"));
        assert!(content.contains("field: i32,"));
        assert!(content.contains("}"));
        assert_eq!(matches[0]["line"], 1);

        // Test with multiline: false (should NOT match)
        let args = json!({
            "pattern": r"struct.*\{[\s\S]*?\}",
            "multiline": false
        });
        let result = tool.call(args).await.unwrap();
        assert_eq!(result["error_code"], error_codes::NOT_FOUND);
    }
}

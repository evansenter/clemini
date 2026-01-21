# Tools

This document describes clemini's tool system: the philosophy behind tool design, conventions for implementation, and a complete reference of available tools.

## Philosophy

### Single Responsibility
Each tool should do one thing well. Prefer focused tools over Swiss Army knives. If a tool is doing multiple distinct things, consider splitting it.

### Consistent Response Format
All tools return JSON. Success responses include relevant data fields. Error responses follow a consistent structure:

```json
{
  "error": "Human-readable error message",
  "error_code": "MACHINE_READABLE_CODE",
  "context": { "additional": "debugging info" }
}
```

Error codes are defined in `src/tools/mod.rs`:
- `ACCESS_DENIED` - Path outside allowed directories
- `NOT_FOUND` - File/resource doesn't exist
- `IO_ERROR` - Filesystem or network error
- `INVALID_ARGUMENT` - Bad parameter value
- `NOT_UNIQUE` - String matches multiple locations (edit tool)
- `BINARY_FILE` - Attempted to read binary as text
- `BLOCKED` - Command blocked for safety (bash tool)
- `TIMEOUT` - Command exceeded time limit (bash tool)

### Actionable Errors
Error messages should tell the user what to do next. Instead of "File not found", say "File not found: foo.txt. Check that the file exists or use glob to search." Include context like file paths, line numbers, and suggestions.

### Security First
- All file operations validate paths against `allowed_paths`
- Bash commands are filtered through blocklist patterns
- Destructive commands require explicit confirmation
- No arbitrary code execution or shell injection

### Return Schema Documentation
Every tool description ends with a `Returns:` clause documenting the response structure. Use `?` suffix for optional fields:

```
Returns: {success, bytes_written, created?, overwritten?}
```

## Conventions for Implementation

### Parameter Naming
- `file_path` - Single file path
- `directory` - Directory to search in (not `path`)
- `pattern` - Search/glob pattern
- `content` - File content to write

### Default Documentation
Document defaults at the end of parameter descriptions using `(default: X)` format:

```json
{
  "offset": {
    "type": "integer",
    "description": "Line to start from. (default: 1)"
  }
}
```

### Error Handling
Tools should not propagate Rust errors directly. Convert errors to JSON responses so the LLM can see them and retry:

```rust
match operation() {
    Ok(result) => Ok(json!({ "success": true, ... })),
    Err(e) => Ok(error_response(
        &format!("Operation failed: {}. Try X instead.", e),
        error_codes::IO_ERROR,
        json!({"context": "value"}),
    )),
}
```

### Testing
Every tool needs tests covering:
1. Happy path (success case)
2. Error cases (missing args, invalid paths)
3. Edge cases (empty files, unicode, large inputs)
4. Security boundaries (path validation)

## Adding a New Tool

1. **Create the tool file** in `src/tools/`:
   ```rust
   pub struct MyTool { ... }

   impl CallableFunction for MyTool {
       fn declaration(&self) -> FunctionDeclaration { ... }
       async fn call(&self, args: Value) -> Result<Value, FunctionError> { ... }
   }
   ```

2. **Follow naming conventions** for parameters and use `(default: X)` format

3. **Include Returns schema** in the description

4. **Add tests** for success, error, and edge cases

5. **Register in `CleminiToolService`** in `src/tools/mod.rs`

6. **Update the System Instruction** in `src/main.rs` SYSTEM_PROMPT

7. **Run quality gates**: `make clippy && make fmt && make test`

## Tool Reference

### File Operations

#### read_file
Read the contents of a file with line numbers.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| file_path | string | yes | Path to file (absolute or relative) |
| offset | integer | no | Line to start from. (default: 1) |
| limit | integer | no | Max lines to read. (default: 2000) |

**Returns:** `{contents, total_lines, truncated?}`

**Examples:**

```json
// Read entire file
{"file_path": "src/main.rs"}
// → {"contents": "1: fn main() {\n2:     println!(\"Hello\");\n3: }", "total_lines": 3}

// Read with offset and limit
{"file_path": "src/lib.rs", "offset": 50, "limit": 10}
// → {"contents": "50: impl Foo {\n51:     ...", "total_lines": 200, "truncated": true}

// File not found
{"file_path": "nonexistent.rs"}
// → {"error": "File not found: nonexistent.rs", "error_code": "NOT_FOUND"}
```

---

#### write_file
Create or overwrite a file.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| file_path | string | yes | Path to file |
| content | string | yes | Content to write |
| backup | boolean | no | Create .bak before overwrite. (default: false) |

**Returns:** `{success, bytes_written, created?, overwritten?, backup_created?}`

**Examples:**

```json
// Create new file
{"file_path": "new_file.txt", "content": "Hello, world!"}
// → {"success": true, "bytes_written": 13, "created": true}

// Overwrite existing file with backup
{"file_path": "config.toml", "content": "[settings]\nkey = \"value\"", "backup": true}
// → {"success": true, "bytes_written": 25, "overwritten": true, "backup_created": true}

// Path outside allowed directories
{"file_path": "/etc/passwd", "content": "malicious"}
// → {"error": "Access denied: /etc/passwd is outside allowed paths", "error_code": "ACCESS_DENIED"}
```

---

#### edit
Replace a string in an existing file.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| file_path | string | yes | Path to file |
| old_string | string | no* | Exact string to find and replace |
| new_string | string | yes | Replacement string |
| replace_all | boolean | no | Replace all occurrences. (default: false) |
| create_if_not_exists | boolean | no | Create file if missing. (default: false) |

*`old_string` is only optional when `create_if_not_exists=true` and file doesn't exist.

**Returns:** `{success, replacements, file_size}` or `{error, suggestions?}`

**Examples:**

```json
// Simple replacement
{"file_path": "src/lib.rs", "old_string": "let x = 5;", "new_string": "let x = 10;"}
// → {"success": true, "replacements": 1, "file_size": 1024}

// Replace all occurrences
{"file_path": "src/main.rs", "old_string": "println!", "new_string": "eprintln!", "replace_all": true}
// → {"success": true, "replacements": 5, "file_size": 2048}

// Create file if missing
{"file_path": "new_module.rs", "new_string": "pub fn init() {}", "create_if_not_exists": true}
// → {"success": true, "replacements": 0, "file_size": 17, "created": true}

// String not unique (ambiguous match)
{"file_path": "src/lib.rs", "old_string": "let x", "new_string": "let y"}
// → {"error": "String matches 3 locations. Provide more context.", "error_code": "NOT_UNIQUE", "suggestions": ["let x = 5;", "let x = foo();", "let x: i32"]}

// String not found
{"file_path": "src/lib.rs", "old_string": "nonexistent code", "new_string": "replacement"}
// → {"error": "String not found in file", "error_code": "NOT_FOUND"}
```

---

### Search

#### glob
Find files matching a pattern.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| pattern | string | yes | Glob pattern (e.g., `**/*.rs`, `src/*.ts`) |
| directory | string | no | Search directory. (default: cwd) |
| sort | string | no | `name`, `modified`, or `size`. (default: name) |
| head_limit | integer | no | Max results. (default: no limit) |
| offset | integer | no | Skip first N results. (default: 0) |

**Returns:** `{matches[], count, total_found, truncated}`

**Examples:**

```json
// Find all Rust files
{"pattern": "**/*.rs"}
// → {"matches": ["src/main.rs", "src/lib.rs", "src/tools/mod.rs"], "count": 3, "total_found": 3, "truncated": false}

// Find in specific directory, sorted by modification time
{"pattern": "*.md", "directory": "docs", "sort": "modified"}
// → {"matches": ["docs/TOOLS.md", "docs/TEXT_RENDERING.md"], "count": 2, "total_found": 2, "truncated": false}

// Paginated results
{"pattern": "**/*.ts", "head_limit": 10, "offset": 20}
// → {"matches": ["src/components/Button.ts", ...], "count": 10, "total_found": 150, "truncated": true}

// No matches
{"pattern": "**/*.xyz"}
// → {"matches": [], "count": 0, "total_found": 0, "truncated": false}
```

---

#### grep
Search file contents with regex.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| pattern | string | yes | Regex pattern (e.g., `fn\s+\w+`, `TODO\|FIXME`) |
| directory | string | no | Search directory. (default: cwd) |
| file_pattern | string | no | File glob filter. (default: `**/*`) |
| type | string | no | File type filter (`rs`, `ts`, `py`, etc.) |
| output_mode | string | no | `content`, `files_with_matches`, `count`. (default: content) |
| case_insensitive | boolean | no | Ignore case. (default: false) |
| context | integer | no | Lines of context around matches. (default: 0) |
| before_context | integer | no | Lines before match. (default: context) |
| after_context | integer | no | Lines after match. (default: context) |
| head_limit | integer | no | Max results. (default: no limit) |
| offset | integer | no | Skip first N results. (default: 0) |

**Returns:** `{matches[], count, total_found, truncated?}`

**Examples:**

```json
// Search for function definitions in Rust files
{"pattern": "fn\\s+\\w+", "type": "rs", "output_mode": "content"}
// → {"matches": [{"file": "src/main.rs", "line": 10, "content": "fn main() {"}], "count": 1, "total_found": 1}

// Find files containing "TODO" (file list only)
{"pattern": "TODO", "output_mode": "files_with_matches"}
// → {"matches": ["src/lib.rs", "src/tools/bash.rs"], "count": 2, "total_found": 2}

// Case-insensitive search with context
{"pattern": "error", "case_insensitive": true, "context": 2}
// → {"matches": [{"file": "src/main.rs", "line": 42, "content": "...", "before": ["line 40", "line 41"], "after": ["line 43", "line 44"]}], ...}

// Count matches per file
{"pattern": "unwrap\\(\\)", "type": "rs", "output_mode": "count"}
// → {"matches": [{"file": "src/main.rs", "count": 5}, {"file": "src/lib.rs", "count": 2}], "count": 2, "total_found": 7}

// Search in specific directory with file pattern
{"pattern": "import", "directory": "src/components", "file_pattern": "*.tsx"}
// → {"matches": [...], "count": 15, "total_found": 15}
```

---

### Execution

#### bash
Execute shell commands.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| command | string | yes | Command to run (e.g., `cargo test`, `gh issue view 42`) |
| description | string | no | What this command does (for logging) |
| working_directory | string | no | Directory to run in. (default: cwd) |
| timeout_seconds | integer | no | Maximum time to wait for the command. (default: 120) |
| confirmed | boolean | no | Skip confirmation for destructive commands. (default: false) |
| run_in_background | boolean | no | Return immediately with task_id. (default: false) |

**Returns:** `{stdout, stderr, exit_code}` or `{task_id, status}` when `run_in_background=true`

**Blocked patterns:** Fork bombs, recursive rm on root, destructive writes to /etc, /boot, etc.

**Caution patterns (require confirmation):** `sudo`, `rm`, `chmod`, `kill`, `git push --force`, `docker rm`, etc.

**Examples:**

```json
// Run a simple command
{"command": "cargo build"}
// → {"stdout": "   Compiling clemini v0.1.0\n    Finished dev [unoptimized + debuginfo]", "stderr": "", "exit_code": 0}

// Command with description (for logging)
{"command": "cargo test --lib", "description": "Run library tests"}
// → {"stdout": "running 42 tests\ntest result: ok. 42 passed", "stderr": "", "exit_code": 0}

// Run in different directory
{"command": "npm install", "working_directory": "/home/user/frontend"}
// → {"stdout": "added 150 packages", "stderr": "", "exit_code": 0}

// Background execution
{"command": "cargo build --release", "run_in_background": true}
// → {"task_id": "abc123", "status": "running"}

// Destructive command (requires confirmation)
{"command": "rm -rf target/"}
// → {"error": "Command requires confirmation. Re-run with confirmed=true", "error_code": "NEEDS_CONFIRMATION", "needs_confirmation": true}

// Destructive command with confirmation
{"command": "rm -rf target/", "confirmed": true}
// → {"stdout": "", "stderr": "", "exit_code": 0}

// Blocked command (dangerous pattern)
{"command": "rm -rf /"}
// → {"error": "Command blocked: recursive delete on root", "error_code": "BLOCKED"}

// Command failure
{"command": "cargo build"}
// → {"stdout": "", "stderr": "error[E0308]: mismatched types", "exit_code": 101}
```

---

#### kill_shell
Kill a background task (bash or subagent).

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| task_id | string | yes | Task ID from bash or task tool |

**Returns:** `{task_id, status, success}`

**Examples:**

```json
// Kill a running background task
{"task_id": "abc123"}
// → {"task_id": "abc123", "status": "killed", "success": true}

// Task already completed
{"task_id": "xyz789"}
// → {"task_id": "xyz789", "status": "already_completed", "success": true}

// Task not found
{"task_id": "nonexistent"}
// → {"error": "Task not found: nonexistent", "error_code": "NOT_FOUND"}
```

---

#### task
Spawn a clemini subagent to handle delegated work.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| prompt | string | yes | The task/prompt for the subagent |
| background | boolean | no | Return immediately with task_id. (default: false) |

**Returns:** `{status, stdout, stderr, exit_code}` or `{task_id, status, prompt}` when `background=true`

**Limitations:**
- Subagent cannot use interactive tools (`ask_user`) - stdin is null
- Subagent gets its own sandbox based on cwd (does not inherit parent's `allowed_paths`)
- Use `task_output` to check status and retrieve output of background tasks

**Use cases:**
- Parallel work on independent subtasks
- Breaking down complex tasks for focused execution
- Long-running operations that don't need real-time output

**Examples:**

```json
// Delegate a task and wait for result
{"prompt": "Analyze the error handling in src/tools/bash.rs and suggest improvements"}
// → {"status": "success", "stdout": "Analysis complete. Found 3 areas...", "stderr": "", "exit_code": 0}

// Run task in background
{"prompt": "Run the full test suite and report failures", "background": true}
// → {"task_id": "task_abc123", "status": "running", "prompt": "Run the full test suite..."}

// Complex analysis task
{"prompt": "Review all TODO comments in the codebase and create a prioritized list"}
// → {"status": "success", "stdout": "Found 15 TODOs:\n1. [HIGH] src/main.rs:42...", "stderr": "", "exit_code": 0}

// Subagent failure (e.g., API error)
{"prompt": "Analyze this codebase"}
// → {"status": "failed", "stdout": "", "stderr": "Error: API rate limit exceeded", "exit_code": 1}
```

---

#### task_output
Get the output and status of a background task.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| task_id | string | yes | The task ID to check |
| wait | boolean | no | Wait for completion (up to timeout). (default: false) |
| timeout | integer | no | Max wait time in seconds if wait=true. (default: 30) |

**Returns:** `{task_id, status, exit_code, stdout, stderr}`

**Examples:**

```json
// Check status of a running task
{"task_id": "abc123"}
// → {"task_id": "abc123", "status": "running", "stdout": "Building...", "stderr": ""}

// Wait for completion
{"task_id": "abc123", "wait": true}
// → {"task_id": "abc123", "status": "completed", "exit_code": 0, "stdout": "Build successful", "stderr": ""}

// Wait with custom timeout
{"task_id": "abc123", "wait": true, "timeout": 5}
// → {"task_id": "abc123", "status": "running", "stdout": "Still building...", "stderr": ""}
```

---

### Interaction

#### ask_user
Ask the user a question.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| question | string | yes | The question to ask |
| options | array | no | Multiple choice options |

**Returns:** `{answer}`

**Examples:**

```json
// Open-ended question
{"question": "What testing framework do you prefer?"}
// → {"answer": "pytest"}

// Multiple choice question
{"question": "Which database should we use?", "options": ["PostgreSQL", "MySQL", "SQLite"]}
// → {"answer": "PostgreSQL"}

// Yes/no question
{"question": "Should I proceed with the refactoring?", "options": ["Yes", "No"]}
// → {"answer": "Yes"}

// User provides custom answer (not in options)
{"question": "Which port?", "options": ["3000", "8080"]}
// → {"answer": "9000"}
```

---

#### todo_write
Track progress on multi-step tasks.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| todos | array | yes | Array of todo items |

Each todo item:
| Field | Type | Required | Description |
|-------|------|----------|-------------|
| content | string | yes | Task in imperative form ("Run tests") |
| activeForm | string | yes | Task in continuous form ("Running tests") |
| status | string | yes | `pending`, `in_progress`, or `completed` |

**Returns:** `{success, count}`

**Examples:**

```json
// Create initial todo list
{"todos": [
  {"content": "Read the existing code", "activeForm": "Reading the existing code", "status": "in_progress"},
  {"content": "Write unit tests", "activeForm": "Writing unit tests", "status": "pending"},
  {"content": "Implement the feature", "activeForm": "Implementing the feature", "status": "pending"}
]}
// → {"success": true, "count": 3}

// Update progress (mark first complete, start second)
{"todos": [
  {"content": "Read the existing code", "activeForm": "Reading the existing code", "status": "completed"},
  {"content": "Write unit tests", "activeForm": "Writing unit tests", "status": "in_progress"},
  {"content": "Implement the feature", "activeForm": "Implementing the feature", "status": "pending"}
]}
// → {"success": true, "count": 3}

// All tasks complete
{"todos": [
  {"content": "Read the existing code", "activeForm": "Reading the existing code", "status": "completed"},
  {"content": "Write unit tests", "activeForm": "Writing unit tests", "status": "completed"},
  {"content": "Implement the feature", "activeForm": "Implementing the feature", "status": "completed"}
]}
// → {"success": true, "count": 3}
```

---

### Web

#### web_search
Search the web via DuckDuckGo.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| query | string | yes | Search query |
| allowed_domains | array | no | Only include results from these domains |
| blocked_domains | array | no | Exclude results from these domains |

**Returns:** `{results[], query}`

**Examples:**

```json
// Basic search
{"query": "rust async programming tutorial"}
// → {"results": [{"title": "Async Rust Book", "url": "https://...", "snippet": "Learn async..."}, ...], "query": "rust async programming tutorial"}

// Search with domain filter
{"query": "tokio runtime", "allowed_domains": ["docs.rs", "github.com"]}
// → {"results": [{"title": "tokio - Rust", "url": "https://docs.rs/tokio/...", "snippet": "..."}], "query": "tokio runtime"}

// Search excluding certain sites
{"query": "rust error handling", "blocked_domains": ["reddit.com", "stackoverflow.com"]}
// → {"results": [...], "query": "rust error handling"}

// No results found
{"query": "xyzzy123nonexistent"}
// → {"results": [], "query": "xyzzy123nonexistent"}
```

---

#### web_fetch
Fetch and optionally process a web page.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| url | string | yes | URL to fetch (e.g., `https://docs.rs/tokio/latest/tokio`) |
| prompt | string | no | Process content with this prompt |

**Returns:** `{content}` or `{processed_content}` if prompt provided

**Examples:**

```json
// Fetch raw content
{"url": "https://docs.rs/tokio/latest/tokio/"}
// → {"content": "# tokio\n\nA runtime for writing reliable network applications..."}

// Fetch and process with prompt
{"url": "https://docs.rs/serde/latest/serde/", "prompt": "List the main derive macros and their purposes"}
// → {"processed_content": "Main derive macros:\n1. Serialize - converts structs to...\n2. Deserialize - parses data into..."}

// Extract specific information
{"url": "https://github.com/rust-lang/rust/releases", "prompt": "What is the latest stable Rust version?"}
// → {"processed_content": "The latest stable Rust version is 1.75.0, released on December 28, 2023."}

// URL not reachable
{"url": "https://nonexistent.example.com/page"}
// → {"error": "Failed to fetch URL: connection refused", "error_code": "IO_ERROR"}
```

---

## When to Use Which Tool

| Task | Preferred Tool | Why |
|------|---------------|-----|
| Find files by name | `glob` | Pattern matching without reading content |
| Search file contents | `grep` | Always prefer over `bash grep` |
| Modify existing code | `edit` | Precise string replacement with validation |
| Create new files | `write_file` | Only for new files or complete rewrites |
| Run builds/tests | `bash` | Shell commands with output capture |
| Long-running commands | `bash` + `run_in_background` | Don't block on slow operations |
| Delegate complex work | `task` | Spawn focused subagent for subtasks |
| Parallel subtasks | `task` + `background=true` | Multiple subagents working concurrently |
| Need user input | `ask_user` | Rather than guessing |
| Multi-step tasks | `todo_write` | Create todos FIRST, then work through them |

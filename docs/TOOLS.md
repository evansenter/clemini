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
- `NEEDS_CONFIRMATION` - Destructive command needs user approval

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
| confirmed | boolean | no | Skip confirmation for destructive commands. (default: false) |
| run_in_background | boolean | no | Return immediately with task_id. (default: false) |

**Returns:** `{stdout, stderr, exit_code}` or `{task_id, status}` when `run_in_background=true`

**Blocked patterns:** Fork bombs, recursive rm on root, destructive writes to /etc, /boot, etc.

**Caution patterns (require confirmation):** `sudo`, `rm`, `chmod`, `kill`, `git push --force`, `docker rm`, etc.

---

#### kill_shell
Kill a background bash task.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| task_id | string | yes | Task ID from bash with `run_in_background=true` |

**Returns:** `{task_id, status, success}`

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

---

#### web_fetch
Fetch and optionally process a web page.

**Parameters:**
| Name | Type | Required | Description |
|------|------|----------|-------------|
| url | string | yes | URL to fetch (e.g., `https://docs.rs/tokio/latest/tokio`) |
| prompt | string | no | Process content with this prompt |

**Returns:** `{content}` or `{processed_content}` if prompt provided

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
| Need user input | `ask_user` | Rather than guessing |
| Multi-step tasks | `todo_write` | Create todos FIRST, then work through them |

//! ACP Agent Simulation Tests for clemitui.
//!
//! These tests simulate the patterns of formatting calls that an ACP-compatible
//! agent would make during complex operations, verifying clemitui handles
//! realistic workloads correctly.
//!
//! Run with: `cargo test -p clemitui --test acp_simulation_tests`

use clemitui::{
    OutputSink, TextBuffer, disable_logging, enable_logging, format_cancelled,
    format_context_warning, format_ctrl_c, format_error_detail, format_retry, format_tool_args,
    format_tool_executing, format_tool_result, log_event, log_event_line, set_output_sink,
};
use serde_json::json;
use std::time::Duration;

/// Strip ANSI escape codes for content verification
fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

// =============================================================================
// Rapid Tool Execution Sequences
// =============================================================================

/// Simulates an agent performing multiple glob searches in rapid succession
/// (e.g., exploring a codebase).
#[test]
fn test_rapid_glob_sequence() {
    colored::control::set_override(false);

    let patterns = [
        "**/*.rs",
        "**/*.toml",
        "**/*.md",
        "**/test*.rs",
        "**/mod.rs",
    ];

    let mut all_output = String::new();

    for (i, pattern) in patterns.iter().enumerate() {
        let args = json!({"pattern": pattern});
        let executing = format_tool_executing("glob", &args);
        let result =
            format_tool_result("glob", Duration::from_millis(15 + i as u64 * 5), 50, false);

        all_output.push_str(&executing);
        all_output.push_str(&result);
        all_output.push('\n');
    }

    let stripped = strip_ansi(&all_output);

    // Should have 5 tool execution blocks
    assert_eq!(
        stripped.matches("┌─").count(),
        5,
        "Should have 5 opening brackets"
    );
    assert_eq!(
        stripped.matches("└─").count(),
        5,
        "Should have 5 closing brackets"
    );

    // Each pattern should appear
    for pattern in patterns {
        assert!(
            stripped.contains(pattern),
            "Should contain pattern: {}",
            pattern
        );
    }

    colored::control::unset_override();
}

/// Simulates an agent chaining grep -> read -> edit tools
/// (typical find-and-fix workflow).
#[test]
fn test_grep_read_edit_chain() {
    colored::control::set_override(false);

    let mut output = String::new();

    // Step 1: grep to find occurrences
    let grep_args = json!({"pattern": "TODO", "path": "src/"});
    output.push_str(&format_tool_executing("grep", &grep_args));
    output.push_str(&format_tool_result(
        "grep",
        Duration::from_millis(45),
        120,
        false,
    ));
    output.push('\n');

    // Step 2: read the file
    let read_args = json!({"file_path": "/project/src/main.rs", "offset": 100, "limit": 50});
    output.push_str(&format_tool_executing("read", &read_args));
    output.push_str(&format_tool_result(
        "read",
        Duration::from_millis(12),
        250,
        false,
    ));
    output.push('\n');

    // Step 3: edit the file
    let edit_args = json!({
        "file_path": "/project/src/main.rs",
        "old_string": "TODO: implement",
        "new_string": "DONE: implemented"
    });
    output.push_str(&format_tool_executing("edit", &edit_args));
    output.push_str(&format_tool_result(
        "edit",
        Duration::from_millis(8),
        30,
        false,
    ));
    output.push('\n');

    let stripped = strip_ansi(&output);

    // Verify tool chain structure
    assert!(stripped.contains("grep"), "Should contain grep");
    assert!(stripped.contains("read"), "Should contain read");
    assert!(stripped.contains("edit"), "Should contain edit");

    // Edit tool should filter old_string/new_string
    assert!(
        !stripped.contains("TODO: implement"),
        "Should NOT show old_string content"
    );
    assert!(
        !stripped.contains("DONE: implemented"),
        "Should NOT show new_string content"
    );
    assert!(
        stripped.contains("file_path="),
        "Should show file_path for edit"
    );

    colored::control::unset_override();
}

/// Simulates rapid bash commands (e.g., running tests, build steps).
#[test]
fn test_rapid_bash_sequence() {
    colored::control::set_override(false);

    let commands = [
        ("cargo check", 1200, 80, false),
        ("cargo clippy", 2500, 150, false),
        ("cargo test", 3000, 500, false),
        ("cargo build --release", 8000, 200, false),
    ];

    let mut output = String::new();

    for (cmd, duration_ms, tokens, has_error) in commands {
        let args = json!({"command": cmd});
        output.push_str(&format_tool_executing("bash", &args));
        output.push_str(&format_tool_result(
            "bash",
            Duration::from_millis(duration_ms),
            tokens,
            has_error,
        ));
        output.push('\n');
    }

    let stripped = strip_ansi(&output);

    // All commands should appear
    for (cmd, _, _, _) in commands {
        assert!(stripped.contains(cmd), "Should contain command: {}", cmd);
    }

    // Duration formatting should be correct
    assert!(stripped.contains("1.20s"), "Should show 1.20s");
    assert!(stripped.contains("2.50s"), "Should show 2.50s");
    assert!(stripped.contains("3.00s"), "Should show 3.00s");
    assert!(stripped.contains("8.00s"), "Should show 8.00s");

    colored::control::unset_override();
}

// =============================================================================
// Long Streaming Markdown Scenarios
// =============================================================================

/// Simulates streaming a long markdown response with multiple sections.
#[test]
fn test_streaming_long_markdown() {
    let mut buffer = TextBuffer::new();

    // Simulate streaming chunks as they would arrive from an LLM
    let chunks = [
        "# Project Analysis\n\n",
        "Here's my analysis of the codebase:\n\n",
        "## Overview\n\n",
        "The project is structured as follows:\n\n",
        "- `src/` - Main source code\n",
        "- `tests/` - Test files\n",
        "- `docs/` - Documentation\n\n",
        "## Key Findings\n\n",
        "1. **Architecture** - The code follows ",
        "a clean event-driven pattern.\n",
        "2. **Testing** - Good coverage ",
        "with both unit and integration tests.\n",
        "3. **Documentation** - Inline docs ",
        "are comprehensive.\n\n",
        "## Code Example\n\n",
        "```rust\n",
        "fn main() {\n",
        "    println!(\"Hello, world!\");\n",
        "}\n",
        "```\n\n",
        "## Recommendations\n\n",
        "Consider adding more integration tests.\n",
    ];

    for chunk in chunks {
        buffer.push(chunk);
    }

    let rendered = buffer.flush();
    assert!(rendered.is_some(), "Should have rendered content");

    let content = strip_ansi(&rendered.unwrap());

    // Verify all sections are present
    assert!(content.contains("Project Analysis"), "Should have title");
    assert!(content.contains("Overview"), "Should have Overview section");
    assert!(content.contains("Key Findings"), "Should have Key Findings");
    assert!(content.contains("Code Example"), "Should have Code Example");
    assert!(
        content.contains("Recommendations"),
        "Should have Recommendations"
    );

    // Verify list items
    assert!(
        content.contains("Main source code"),
        "Should have list content"
    );

    // Verify code block content is present
    assert!(
        content.contains("Hello, world!"),
        "Should have code block content"
    );
}

/// Simulates streaming with incomplete markdown that gets completed.
#[test]
fn test_streaming_incomplete_markdown_completion() {
    let mut buffer = TextBuffer::new();

    // Start with incomplete bold
    buffer.push("This is **important");

    // Continue streaming
    buffer.push(" text** that spans chunks.\n\n");

    // Add more content
    buffer.push("And here's more content.");

    let rendered = buffer.flush();
    assert!(rendered.is_some(), "Should have rendered content");

    let content = strip_ansi(&rendered.unwrap());
    assert!(content.contains("important"), "Should contain 'important'");
    assert!(
        content.contains("spans chunks"),
        "Should contain 'spans chunks'"
    );
}

/// Simulates streaming a response with multiple code blocks.
#[test]
fn test_streaming_multiple_code_blocks() {
    let mut buffer = TextBuffer::new();

    buffer.push("Here's the Rust version:\n\n");
    buffer.push("```rust\nfn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n```\n\n");
    buffer.push("And the Python version:\n\n");
    buffer.push("```python\ndef add(a, b):\n    return a + b\n```\n\n");
    buffer.push("Both achieve the same result.");

    let rendered = buffer.flush();
    assert!(rendered.is_some(), "Should have rendered content");

    let content = strip_ansi(&rendered.unwrap());
    assert!(content.contains("Rust version"), "Should mention Rust");
    assert!(content.contains("Python version"), "Should mention Python");
    assert!(content.contains("a + b"), "Should have code content");
}

// =============================================================================
// Interleaved Tool and Text Output
// =============================================================================

/// Simulates an agent explaining what it's doing while executing tools.
#[test]
fn test_interleaved_explanation_and_tools() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Agent explains what it will do
    buffer.push("I'll search for TODO comments in the codebase.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
        output.push('\n');
    }

    // Execute grep tool
    let grep_args = json!({"pattern": "TODO"});
    output.push_str(&strip_ansi(&format_tool_executing("grep", &grep_args)));
    output.push_str(&strip_ansi(&format_tool_result(
        "grep",
        Duration::from_millis(35),
        80,
        false,
    )));
    output.push('\n');

    // Agent comments on results
    buffer.push("Found 3 TODO items. Let me read the first file.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
        output.push('\n');
    }

    // Execute read tool
    let read_args = json!({"file_path": "src/main.rs"});
    output.push_str(&strip_ansi(&format_tool_executing("read", &read_args)));
    output.push_str(&strip_ansi(&format_tool_result(
        "read",
        Duration::from_millis(12),
        150,
        false,
    )));
    output.push('\n');

    // Final explanation
    buffer.push("Here's what I found:\n\n- Line 42: TODO: add error handling\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify interleaving
    assert!(
        output.contains("search for TODO"),
        "Should have initial explanation"
    );
    assert!(output.contains("┌─"), "Should have tool markers");
    assert!(
        output.contains("Found 3 TODO"),
        "Should have middle explanation"
    );
    assert!(output.contains("Line 42"), "Should have final explanation");

    colored::control::unset_override();
}

/// Simulates an agent providing step-by-step progress updates.
#[test]
fn test_step_by_step_progress() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    let steps = [
        (
            "Step 1: Checking project structure...",
            "glob",
            json!({"pattern": "**/*"}),
            25,
        ),
        (
            "Step 2: Reading configuration...",
            "read",
            json!({"file_path": "config.toml"}),
            10,
        ),
        (
            "Step 3: Running tests...",
            "bash",
            json!({"command": "cargo test"}),
            2500,
        ),
        (
            "Step 4: Building release...",
            "bash",
            json!({"command": "cargo build --release"}),
            5000,
        ),
    ];

    for (explanation, tool, args, duration_ms) in steps {
        // Add explanation
        buffer.push(explanation);
        buffer.push("\n\n");
        if let Some(text) = buffer.flush() {
            output.push_str(&strip_ansi(&text));
        }

        // Execute tool
        output.push_str(&strip_ansi(&format_tool_executing(tool, &args)));
        output.push_str(&strip_ansi(&format_tool_result(
            tool,
            Duration::from_millis(duration_ms),
            100,
            false,
        )));
        output.push('\n');
    }

    // Verify all steps present
    assert!(output.contains("Step 1"), "Should have Step 1");
    assert!(output.contains("Step 2"), "Should have Step 2");
    assert!(output.contains("Step 3"), "Should have Step 3");
    assert!(output.contains("Step 4"), "Should have Step 4");

    // Verify tools executed
    assert!(output.contains("glob"), "Should have glob");
    assert!(output.contains("config.toml"), "Should have config.toml");
    assert!(output.contains("cargo test"), "Should have cargo test");
    assert!(output.contains("cargo build"), "Should have cargo build");

    colored::control::unset_override();
}

// =============================================================================
// Error Recovery Visualization
// =============================================================================

/// Simulates a tool failing and agent recovering.
#[test]
fn test_tool_error_and_recovery() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // First attempt - fails
    let args = json!({"file_path": "nonexistent.rs"});
    output.push_str(&strip_ansi(&format_tool_executing("read", &args)));
    output.push_str(&strip_ansi(&format_tool_result(
        "read",
        Duration::from_millis(5),
        20,
        true, // has_error = true
    )));
    output.push_str(&strip_ansi(&format_error_detail(
        "File not found: nonexistent.rs",
    )));
    output.push('\n');

    // Agent explains the error
    buffer.push("\nThe file doesn't exist. Let me search for similar files.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Recovery attempt - glob search
    let glob_args = json!({"pattern": "**/*.rs"});
    output.push_str(&strip_ansi(&format_tool_executing("glob", &glob_args)));
    output.push_str(&strip_ansi(&format_tool_result(
        "glob",
        Duration::from_millis(20),
        50,
        false,
    )));
    output.push('\n');

    // Found the right file
    buffer.push("Found it! The file is `src/main.rs`.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Successful read
    let correct_args = json!({"file_path": "src/main.rs"});
    output.push_str(&strip_ansi(&format_tool_executing("read", &correct_args)));
    output.push_str(&strip_ansi(&format_tool_result(
        "read",
        Duration::from_millis(10),
        200,
        false,
    )));

    // Verify error indicator
    assert!(output.contains("ERROR"), "Should show ERROR marker");
    assert!(
        output.contains("File not found"),
        "Should show error detail"
    );

    // Verify recovery narrative
    assert!(output.contains("doesn't exist"), "Should explain the error");
    assert!(output.contains("Found it"), "Should show recovery");

    colored::control::unset_override();
}

/// Simulates API retry scenario with backoff.
#[test]
fn test_api_retry_sequence() {
    colored::control::set_override(false);

    let mut output = String::new();

    // First retry
    output.push_str(&strip_ansi(&format_retry(
        1,
        3,
        Duration::from_secs(2),
        "rate limit exceeded",
    )));
    output.push('\n');

    // Second retry
    output.push_str(&strip_ansi(&format_retry(
        2,
        3,
        Duration::from_secs(4),
        "rate limit exceeded",
    )));
    output.push('\n');

    // Verify retry formatting
    assert!(output.contains("1/3"), "Should show attempt 1/3");
    assert!(output.contains("2/3"), "Should show attempt 2/3");
    assert!(output.contains("2s"), "Should show 2s delay");
    assert!(output.contains("4s"), "Should show 4s delay");
    assert!(
        output.contains("rate limit"),
        "Should show rate limit reason"
    );

    colored::control::unset_override();
}

/// Simulates multiple consecutive errors (agent struggling).
#[test]
fn test_multiple_consecutive_errors() {
    colored::control::set_override(false);

    let mut output = String::new();

    let error_sequence = [
        (
            "edit",
            json!({"file_path": "test.rs", "old_string": "foo", "new_string": "bar"}),
            "old_string not found in file",
        ),
        (
            "edit",
            json!({"file_path": "test.rs", "old_string": "FOO", "new_string": "bar"}),
            "old_string not found in file",
        ),
        ("read", json!({"file_path": "test.rs"}), ""), // Success to read file
    ];

    for (i, (tool, args, error)) in error_sequence.iter().enumerate() {
        output.push_str(&strip_ansi(&format_tool_executing(tool, args)));

        let has_error = !error.is_empty();
        output.push_str(&strip_ansi(&format_tool_result(
            tool,
            Duration::from_millis(10),
            30,
            has_error,
        )));

        if has_error {
            output.push_str(&strip_ansi(&format_error_detail(error)));
        }
        output.push_str(&format!(" // attempt {}\n", i + 1));
    }

    // Should have 2 errors and 1 success
    assert_eq!(
        output.matches("ERROR").count(),
        2,
        "Should have 2 ERROR markers"
    );
    assert_eq!(
        output.matches("old_string not found").count(),
        2,
        "Should have 2 error details"
    );

    colored::control::unset_override();
}

// =============================================================================
// Context Window Warning Scenarios
// =============================================================================

/// Simulates context window filling up during a session.
#[test]
fn test_context_warning_progression() {
    // format_context_warning only adds /clear when percentage > 95.0 (strict greater than)
    let warnings = [
        (80.0, false), // First warning at 80%
        (85.0, false), // Getting higher
        (90.0, false), // Still below critical
        (95.0, false), // At 95% - still no /clear (requires > 95.0)
        (95.1, true),  // Just above 95% - now suggests /clear
        (98.0, true),  // Very critical
    ];

    for (percentage, should_suggest_clear) in warnings {
        let warning = format_context_warning(percentage);

        assert!(
            warning.contains(&format!("{:.1}%", percentage)),
            "Should contain percentage {:.1}%",
            percentage
        );

        if should_suggest_clear {
            assert!(
                warning.contains("/clear"),
                "Should suggest /clear at {:.1}%",
                percentage
            );
        } else {
            assert!(
                !warning.contains("/clear"),
                "Should NOT suggest /clear at {:.1}%",
                percentage
            );
        }
    }
}

// =============================================================================
// Large Output Handling
// =============================================================================

/// Simulates formatting very large tool arguments (should be truncated).
#[test]
fn test_large_argument_truncation() {
    colored::control::set_override(false);

    // Create a very long string argument
    let long_content = "x".repeat(500);
    let args = json!({
        "content": long_content,
        "file_path": "output.txt"
    });

    let formatted = format_tool_args("write", &args);

    // Should be truncated (MAX_ARG_DISPLAY_LEN is 80)
    assert!(
        formatted.len() < 300,
        "Should truncate very long arguments: len={}",
        formatted.len()
    );
    assert!(
        formatted.contains("..."),
        "Should have truncation indicator"
    );
    assert!(
        formatted.contains("file_path="),
        "Should still show file_path"
    );

    colored::control::unset_override();
}

/// Simulates a complex multi-tool operation with mixed output sizes.
#[test]
fn test_mixed_output_sizes() {
    colored::control::set_override(false);

    let operations = [
        ("glob", json!({"pattern": "*.rs"}), 10, 25), // Small output
        ("read", json!({"file_path": "main.rs"}), 50, 2000), // Large output
        ("grep", json!({"pattern": "fn "}), 30, 500), // Medium output
        ("bash", json!({"command": "wc -l **/*.rs"}), 100, 50), // Small output
    ];

    let mut total_output = String::new();

    for (tool, args, duration_ms, tokens) in operations {
        let executing = format_tool_executing(tool, &args);
        let result = format_tool_result(tool, Duration::from_millis(duration_ms), tokens, false);

        total_output.push_str(&executing);
        total_output.push_str(&result);
        total_output.push('\n');
    }

    let stripped = strip_ansi(&total_output);

    // All tools should be present
    assert!(stripped.contains("glob"), "Should have glob");
    assert!(stripped.contains("read"), "Should have read");
    assert!(stripped.contains("grep"), "Should have grep");
    assert!(stripped.contains("bash"), "Should have bash");

    // Token counts should vary
    assert!(stripped.contains("~25 tok"), "Should show 25 tokens");
    assert!(stripped.contains("~2000 tok"), "Should show 2000 tokens");
    assert!(stripped.contains("~500 tok"), "Should show 500 tokens");
    assert!(stripped.contains("~50 tok"), "Should show 50 tokens");

    colored::control::unset_override();
}

// =============================================================================
// Complex Multi-Turn Simulation
// =============================================================================

// =============================================================================
// Background Task Scenarios
// =============================================================================

/// Simulates spawning a background task and later checking its output.
#[test]
fn test_background_task_lifecycle() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Agent explains
    buffer.push("I'll run the test suite in the background while we continue working.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Spawn background task
    let bash_args = json!({"command": "cargo test", "background": true});
    output.push_str(&strip_ansi(&format_tool_executing("bash", &bash_args)));
    output.push_str(&strip_ansi(&format_tool_result(
        "bash",
        Duration::from_millis(50),
        30,
        false,
    )));
    output.push('\n');

    // Simulate tool output showing task ID
    output.push_str("  task bg-1 running in background\n\n");

    // Agent continues with other work
    buffer.push("Tests are running. Meanwhile, let me check the code coverage setup.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Do other work
    output.push_str(&strip_ansi(&format_tool_executing(
        "read",
        &json!({"file_path": "codecov.yml"}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "read",
        Duration::from_millis(10),
        50,
        false,
    )));
    output.push('\n');

    // Later: check task output
    buffer.push("Let me check if the tests completed.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    output.push_str(&strip_ansi(&format_tool_executing(
        "task_output",
        &json!({"task_id": "bg-1"}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "task_output",
        Duration::from_millis(5),
        500,
        false,
    )));
    output.push('\n');

    // Agent reports results
    buffer.push("Tests completed successfully: 223 passed, 0 failed.\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify background task flow
    assert!(output.contains("background"), "Should mention background");
    assert!(output.contains("bg-1"), "Should show task ID");
    assert!(output.contains("task_output"), "Should check task output");
    assert!(output.contains("223 passed"), "Should report results");

    colored::control::unset_override();
}

/// Simulates multiple concurrent background tasks.
#[test]
fn test_multiple_background_tasks() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Spawn multiple background tasks
    buffer.push("Running build and tests in parallel.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Task 1: Build
    output.push_str(&strip_ansi(&format_tool_executing(
        "bash",
        &json!({"command": "cargo build --release", "background": true}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "bash",
        Duration::from_millis(20),
        25,
        false,
    )));
    output.push_str("  task bg-1 running in background\n");

    // Task 2: Tests
    output.push_str(&strip_ansi(&format_tool_executing(
        "bash",
        &json!({"command": "cargo test", "background": true}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "bash",
        Duration::from_millis(20),
        25,
        false,
    )));
    output.push_str("  task bg-2 running in background\n");

    // Task 3: Clippy
    output.push_str(&strip_ansi(&format_tool_executing(
        "bash",
        &json!({"command": "cargo clippy", "background": true}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "bash",
        Duration::from_millis(20),
        25,
        false,
    )));
    output.push_str("  task bg-3 running in background\n\n");

    // Later: check all tasks
    for task_id in ["bg-1", "bg-2", "bg-3"] {
        output.push_str(&strip_ansi(&format_tool_executing(
            "task_output",
            &json!({"task_id": task_id}),
        )));
        output.push_str(&strip_ansi(&format_tool_result(
            "task_output",
            Duration::from_millis(5),
            100,
            false,
        )));
        output.push('\n');
    }

    // Verify all tasks
    assert!(output.contains("bg-1"), "Should have task bg-1");
    assert!(output.contains("bg-2"), "Should have task bg-2");
    assert!(output.contains("bg-3"), "Should have task bg-3");
    // Each task_output call produces 2 occurrences: executing line + result line
    assert_eq!(
        output.matches("task_output").count(),
        6, // 3 tasks × 2 (executing + result)
        "Should check all 3 tasks (6 total: 3 executing + 3 result lines)"
    );

    colored::control::unset_override();
}

/// Simulates a background task that fails.
#[test]
fn test_background_task_failure() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Spawn background task
    output.push_str(&strip_ansi(&format_tool_executing(
        "bash",
        &json!({"command": "cargo test --features broken", "background": true}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "bash",
        Duration::from_millis(20),
        25,
        false,
    )));
    output.push_str("  task bg-1 running in background\n\n");

    // Check output - task failed
    output.push_str(&strip_ansi(&format_tool_executing(
        "task_output",
        &json!({"task_id": "bg-1"}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "task_output",
        Duration::from_millis(5),
        200,
        true, // Error - task failed
    )));
    output.push_str(&strip_ansi(&format_error_detail("Task exited with code 1")));
    output.push('\n');

    // Agent handles the failure
    buffer.push("The tests failed. Let me investigate the error.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify error handling
    assert!(output.contains("ERROR"), "Should show error marker");
    assert!(output.contains("exited with code"), "Should show exit code");
    assert!(output.contains("investigate"), "Should explain recovery");

    colored::control::unset_override();
}

// =============================================================================
// Subagent / Task Delegation Scenarios
// =============================================================================

/// Simulates spawning a subagent for a delegated task.
#[test]
fn test_subagent_delegation() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Main agent explains delegation
    buffer.push(
        "This is a complex refactoring task. I'll delegate the test updates to a subagent.\n\n",
    );
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Spawn subagent task
    let task_args = json!({
        "prompt": "Update all tests in tests/ to use the new API. The function signatures changed from foo(x) to foo(x, y).",
        "background": true
    });
    output.push_str(&strip_ansi(&format_tool_executing("task", &task_args)));
    output.push_str(&strip_ansi(&format_tool_result(
        "task",
        Duration::from_millis(100),
        50,
        false,
    )));
    output.push_str("  task acp-1 running in background\n\n");

    // Main agent continues
    buffer.push("Subagent is working on tests. I'll update the main source files.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Main agent does its work
    output.push_str(&strip_ansi(&format_tool_executing(
        "edit",
        &json!({
            "file_path": "src/lib.rs",
            "old_string": "fn foo(x: i32)",
            "new_string": "fn foo(x: i32, y: i32)"
        }),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "edit",
        Duration::from_millis(10),
        30,
        false,
    )));
    output.push('\n');

    // Check subagent result
    buffer.push("Let me check if the subagent finished.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    output.push_str(&strip_ansi(&format_tool_executing(
        "task_output",
        &json!({"task_id": "acp-1"}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "task_output",
        Duration::from_millis(5),
        300,
        false,
    )));
    output.push('\n');

    // Final summary
    buffer.push("Subagent completed: updated 8 test files. All changes are ready for review.\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify subagent flow
    assert!(output.contains("task"), "Should use task tool");
    assert!(output.contains("acp-1"), "Should show ACP task ID");
    assert!(output.contains("Subagent"), "Should mention subagent");
    assert!(output.contains("8 test files"), "Should report results");

    colored::control::unset_override();
}

/// Simulates multiple subagents working in parallel.
#[test]
fn test_parallel_subagents() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Explain parallel approach
    buffer.push("I'll use parallel subagents to update different parts of the codebase:\n");
    buffer.push("1. Frontend updates\n");
    buffer.push("2. Backend updates\n");
    buffer.push("3. Documentation updates\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Spawn subagents
    let tasks = [
        ("Update frontend components to use new theme", "acp-1"),
        ("Update backend API handlers", "acp-2"),
        ("Update documentation for new features", "acp-3"),
    ];

    for (prompt, task_id) in tasks {
        output.push_str(&strip_ansi(&format_tool_executing(
            "task",
            &json!({"prompt": prompt, "background": true}),
        )));
        output.push_str(&strip_ansi(&format_tool_result(
            "task",
            Duration::from_millis(80),
            40,
            false,
        )));
        output.push_str(&format!("  task {} running in background\n", task_id));
    }
    output.push('\n');

    // Wait and collect results
    buffer.push("All subagents dispatched. Waiting for completion...\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Check each result
    for (_, task_id) in tasks {
        output.push_str(&strip_ansi(&format_tool_executing(
            "task_output",
            &json!({"task_id": task_id}),
        )));
        output.push_str(&strip_ansi(&format_tool_result(
            "task_output",
            Duration::from_millis(5),
            150,
            false,
        )));
        output.push('\n');
    }

    // Verify parallel execution
    assert!(output.contains("acp-1"), "Should have task acp-1");
    assert!(output.contains("acp-2"), "Should have task acp-2");
    assert!(output.contains("acp-3"), "Should have task acp-3");
    // "task " appears in: 3 spawns × 2 (executing + result) + 3 "task acp-X running" messages = 9
    assert_eq!(
        output.matches("task ").count(),
        9,
        "Should have 9 task references (3 spawns × 2 lines + 3 status messages)"
    );

    colored::control::unset_override();
}

// =============================================================================
// Kill Shell Scenarios
// =============================================================================

/// Simulates killing a long-running background task.
#[test]
fn test_kill_background_task() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Start a long-running task
    output.push_str(&strip_ansi(&format_tool_executing(
        "bash",
        &json!({"command": "cargo build --all-targets", "background": true}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "bash",
        Duration::from_millis(30),
        25,
        false,
    )));
    output.push_str("  task bg-1 running in background\n\n");

    // Decide to kill it
    buffer.push("Actually, I need to stop the build. The configuration is wrong.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Kill the task
    output.push_str(&strip_ansi(&format_tool_executing(
        "kill_shell",
        &json!({"task_id": "bg-1"}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "kill_shell",
        Duration::from_millis(10),
        15,
        false,
    )));
    output.push('\n');

    // Confirm
    buffer.push("Task killed. Let me fix the configuration first.\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify kill flow
    assert!(output.contains("bg-1"), "Should reference task ID");
    assert!(output.contains("kill_shell"), "Should use kill_shell tool");
    assert!(output.contains("Task killed"), "Should confirm kill");

    colored::control::unset_override();
}

// =============================================================================
// Complete Multi-Turn Simulation
// =============================================================================

/// Simulates a complete multi-turn agent interaction for a refactoring task.
#[test]
fn test_complete_refactoring_session() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut session_output = String::new();

    // Turn 1: Agent explores the codebase
    buffer
        .push("I'll help you refactor the error handling. First, let me explore the codebase.\n\n");
    if let Some(text) = buffer.flush() {
        session_output.push_str(&strip_ansi(&text));
    }

    // Glob search
    session_output.push_str(&strip_ansi(&format_tool_executing(
        "glob",
        &json!({"pattern": "**/*.rs"}),
    )));
    session_output.push_str(&strip_ansi(&format_tool_result(
        "glob",
        Duration::from_millis(20),
        150,
        false,
    )));
    session_output.push('\n');

    // Grep for error handling
    session_output.push_str(&strip_ansi(&format_tool_executing(
        "grep",
        &json!({"pattern": "unwrap\\(\\)", "path": "src/"}),
    )));
    session_output.push_str(&strip_ansi(&format_tool_result(
        "grep",
        Duration::from_millis(45),
        300,
        false,
    )));
    session_output.push('\n');

    // Agent summarizes findings
    buffer.push(
        "Found 12 uses of `unwrap()` that should be replaced with proper error handling:\n\n",
    );
    buffer.push("- `src/main.rs`: 4 occurrences\n");
    buffer.push("- `src/lib.rs`: 3 occurrences\n");
    buffer.push("- `src/utils.rs`: 5 occurrences\n\n");
    if let Some(text) = buffer.flush() {
        session_output.push_str(&strip_ansi(&text));
    }

    // Turn 2: Agent starts fixing
    buffer.push("Let me start with `src/main.rs`.\n\n");
    if let Some(text) = buffer.flush() {
        session_output.push_str(&strip_ansi(&text));
    }

    // Read file
    session_output.push_str(&strip_ansi(&format_tool_executing(
        "read",
        &json!({"file_path": "src/main.rs"}),
    )));
    session_output.push_str(&strip_ansi(&format_tool_result(
        "read",
        Duration::from_millis(15),
        500,
        false,
    )));
    session_output.push('\n');

    // Edit file (multiple edits)
    for i in 1..=4 {
        session_output.push_str(&strip_ansi(&format_tool_executing(
            "edit",
            &json!({
                "file_path": "src/main.rs",
                "old_string": format!("unwrap() // {}", i),
                "new_string": "?"
            }),
        )));
        session_output.push_str(&strip_ansi(&format_tool_result(
            "edit",
            Duration::from_millis(8),
            30,
            false,
        )));
        session_output.push('\n');
    }

    // Agent confirms completion
    buffer.push("Fixed all 4 occurrences in `src/main.rs`. ");
    buffer.push("The function now returns `Result<(), Error>` instead of panicking.\n\n");
    buffer.push("Would you like me to continue with the other files?");
    if let Some(text) = buffer.flush() {
        session_output.push_str(&strip_ansi(&text));
    }

    // Verify the complete session
    assert!(
        session_output.contains("refactor"),
        "Should have initial explanation"
    );
    assert!(session_output.contains("glob"), "Should have glob search");
    assert!(session_output.contains("grep"), "Should have grep search");
    assert!(
        session_output.contains("12 uses"),
        "Should summarize findings"
    );
    // Each edit operation produces 2 occurrences: executing line + result line
    assert_eq!(
        session_output.matches("edit").count(),
        8, // 4 edits × 2 (executing + result)
        "Should have 4 edit operations (8 total: 4 executing + 4 result lines)"
    );
    assert!(
        session_output.contains("Fixed all 4"),
        "Should confirm completion"
    );
    assert!(
        session_output.contains("continue with"),
        "Should offer next steps"
    );

    colored::control::unset_override();
}

// =============================================================================
// Cancellation and Interrupt Handling
// =============================================================================

/// Simulates user pressing ctrl-c during a long operation.
#[test]
fn test_ctrl_c_during_tool_execution() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Start a long-running tool
    output.push_str(&strip_ansi(&format_tool_executing(
        "bash",
        &json!({"command": "cargo build --release"}),
    )));

    // User presses ctrl-c
    output.push_str(format_ctrl_c());
    output.push('\n');

    // Agent acknowledges
    buffer.push("Build interrupted. Would you like me to continue or try something else?\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify
    assert!(output.contains("bash"), "Should show tool that was running");
    assert!(
        output.to_lowercase().contains("ctrl") || output.contains("interrupted"),
        "Should indicate interruption"
    );

    colored::control::unset_override();
}

/// Simulates operation being cancelled by user.
#[test]
fn test_operation_cancelled() {
    colored::control::set_override(false);

    let mut output = String::new();

    // Start some work
    output.push_str(&strip_ansi(&format_tool_executing(
        "task",
        &json!({"prompt": "Implement authentication system"}),
    )));

    // Operation cancelled
    output.push_str(&strip_ansi(&format_cancelled()));
    output.push('\n');

    // Verify
    assert!(output.contains("task"), "Should show what was cancelled");
    assert!(
        output.to_lowercase().contains("cancel"),
        "Should indicate cancellation"
    );

    colored::control::unset_override();
}

// =============================================================================
// Multi-Turn Conversation Simulation
// =============================================================================

/// Simulates a multi-turn debugging session with context accumulation.
#[test]
fn test_multi_turn_debugging_session() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Turn 1: User reports bug
    buffer.push("I see there's a panic in `parse_config`. Let me investigate.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Read the file
    output.push_str(&strip_ansi(&format_tool_executing(
        "read",
        &json!({"file_path": "src/config.rs"}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "read",
        Duration::from_millis(5),
        1200,
        false,
    )));
    output.push('\n');

    // Analysis
    buffer.push(
        "I found the issue. The `unwrap()` on line 42 panics when the config file is missing.\n\n",
    );
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Turn 2: User asks for fix
    // Agent proposes fix
    output.push_str(&strip_ansi(&format_tool_executing(
        "edit",
        &json!({
            "file_path": "src/config.rs",
            "old_string": "config.unwrap()",
            "new_string": "config.unwrap_or_default()"
        }),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "edit",
        Duration::from_millis(8),
        30,
        false,
    )));
    output.push('\n');

    // Turn 3: Verify fix
    buffer.push("Applied the fix. Let me verify it compiles.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    output.push_str(&strip_ansi(&format_tool_executing(
        "bash",
        &json!({"command": "cargo check"}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "bash",
        Duration::from_millis(2500),
        150,
        false,
    )));
    output.push('\n');

    buffer.push("Compiles successfully. The panic should now be fixed.\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify multi-turn flow
    assert!(
        output.contains("parse_config"),
        "Should reference original bug"
    );
    assert!(
        output.contains("unwrap"),
        "Should identify the problematic code"
    );
    assert!(output.contains("edit"), "Should propose edit");
    assert!(output.contains("cargo check"), "Should verify fix");
    assert!(
        output.contains("successfully"),
        "Should confirm successful fix"
    );

    colored::control::unset_override();
}

/// Simulates a planning session before implementation.
#[test]
fn test_planning_before_implementation() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Agent starts planning
    buffer.push("Let me plan the implementation for the authentication feature.\n\n");
    buffer.push("## Implementation Plan\n\n");
    buffer.push("1. Create auth middleware\n");
    buffer.push("2. Add JWT validation\n");
    buffer.push("3. Implement login/logout routes\n");
    buffer.push("4. Add session management\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Research existing patterns
    output.push_str(&strip_ansi(&format_tool_executing(
        "grep",
        &json!({"pattern": "middleware", "path": "src/"}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "grep",
        Duration::from_millis(20),
        150,
        false,
    )));
    output.push('\n');

    output.push_str(&strip_ansi(&format_tool_executing(
        "read",
        &json!({"file_path": "src/middleware/mod.rs"}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "read",
        Duration::from_millis(5),
        800,
        false,
    )));
    output.push('\n');

    // Refine plan
    buffer.push("Based on existing patterns, I'll use the same middleware structure.\n\n");
    buffer.push("Ready to implement. Should I proceed?\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify planning flow (markdown header rendered, so check for text without ##)
    assert!(
        output.contains("Implementation Plan"),
        "Should have plan header"
    );
    assert!(output.contains("grep"), "Should research patterns");
    assert!(output.contains("read"), "Should read existing code");
    assert!(output.contains("proceed"), "Should ask for confirmation");

    colored::control::unset_override();
}

// =============================================================================
// Concurrent Task Coordination
// =============================================================================

/// Simulates coordinating multiple long-running tasks with status checks.
#[test]
fn test_task_coordination_with_polling() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Spawn multiple tasks
    let tasks = [
        ("cargo test --lib", "bg-1"),
        ("cargo test --doc", "bg-2"),
        ("cargo clippy", "bg-3"),
    ];

    buffer.push("Running quality checks in parallel...\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    for (cmd, task_id) in tasks {
        output.push_str(&strip_ansi(&format_tool_executing(
            "bash",
            &json!({"command": cmd, "background": true}),
        )));
        output.push_str(&strip_ansi(&format_tool_result(
            "bash",
            Duration::from_millis(30),
            20,
            false,
        )));
        output.push_str(&format!("  {} started as {}\n", cmd, task_id));
    }
    output.push('\n');

    // First poll - some still running
    buffer.push("Checking status...\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // bg-1 completed
    output.push_str(&strip_ansi(&format_tool_executing(
        "task_output",
        &json!({"task_id": "bg-1", "block": false}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "task_output",
        Duration::from_millis(5),
        100,
        false,
    )));
    output.push_str("  ✓ bg-1 completed successfully\n");

    // bg-2 still running
    output.push_str(&strip_ansi(&format_tool_executing(
        "task_output",
        &json!({"task_id": "bg-2", "block": false}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "task_output",
        Duration::from_millis(5),
        20,
        false,
    )));
    output.push_str("  ⏳ bg-2 still running...\n");

    // bg-3 failed
    output.push_str(&strip_ansi(&format_tool_executing(
        "task_output",
        &json!({"task_id": "bg-3", "block": false}),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "task_output",
        Duration::from_millis(5),
        50,
        true,
    )));
    output.push_str(&strip_ansi(&format_error_detail("clippy found 3 warnings")));
    output.push('\n');

    buffer.push("1 passed, 1 still running, 1 has warnings. Investigating clippy issues...\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify coordination
    assert!(
        output.contains("parallel"),
        "Should mention parallel execution"
    );
    assert!(output.contains("bg-1") && output.contains("bg-2") && output.contains("bg-3"));
    assert!(
        output.contains("completed") || output.contains("✓"),
        "Should show completion"
    );
    assert!(
        output.contains("running") || output.contains("⏳"),
        "Should show running status"
    );
    assert!(
        output.contains("ERROR"),
        "Should show error from failed task"
    );

    colored::control::unset_override();
}

// =============================================================================
// Deep Nesting and Complex Workflows
// =============================================================================

/// Simulates a complex nested workflow: spawn subagent that spawns more tasks.
#[test]
fn test_nested_subagent_workflow() {
    colored::control::set_override(false);

    let mut buffer = TextBuffer::new();
    let mut output = String::new();

    // Main agent delegates to subagent
    buffer.push("This is a large refactoring task. I'll delegate to specialized subagents.\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Spawn architecture subagent
    output.push_str(&strip_ansi(&format_tool_executing(
        "task",
        &json!({
            "prompt": "Analyze and improve the architecture of the auth module",
            "background": true
        }),
    )));
    output.push_str(&strip_ansi(&format_tool_result(
        "task",
        Duration::from_millis(100),
        50,
        false,
    )));
    output.push_str("  acp-1 (architecture analyzer) running\n\n");

    // Subagent reports back (simulated output)
    buffer.push("acp-1 completed. Analysis:\n");
    buffer.push("- Found 3 circular dependencies\n");
    buffer.push("- Recommends extracting `TokenValidator` trait\n");
    buffer.push("- Suggests moving session logic to separate module\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Main agent spawns implementation subagents based on analysis
    let implementation_tasks = [
        (
            "Break circular dependency between auth and user modules",
            "acp-2",
        ),
        ("Extract TokenValidator trait to separate crate", "acp-3"),
        ("Move session logic to new session module", "acp-4"),
    ];

    buffer.push("Spawning implementation subagents based on analysis:\n\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    for (prompt, task_id) in implementation_tasks {
        output.push_str(&strip_ansi(&format_tool_executing(
            "task",
            &json!({"prompt": prompt, "background": true}),
        )));
        output.push_str(&strip_ansi(&format_tool_result(
            "task",
            Duration::from_millis(80),
            40,
            false,
        )));
        output.push_str(&format!("  {} running\n", task_id));
    }
    output.push('\n');

    // Final summary
    buffer.push("Refactoring in progress. 1 analysis + 3 implementation subagents spawned.\n");
    if let Some(text) = buffer.flush() {
        output.push_str(&strip_ansi(&text));
    }

    // Verify nested workflow
    assert!(
        output.contains("acp-1"),
        "Should have architecture subagent"
    );
    assert!(
        output.contains("acp-2") && output.contains("acp-3") && output.contains("acp-4"),
        "Should spawn implementation subagents"
    );
    assert!(
        output.contains("circular"),
        "Should report analysis findings"
    );
    assert!(
        output.contains("Refactoring in progress"),
        "Should summarize overall status"
    );

    colored::control::unset_override();
}

// =============================================================================
// Logging Infrastructure Tests
// =============================================================================

/// Simulates logging multiple events through the OutputSink.
#[test]
fn test_output_sink_logging() {
    use std::sync::{Arc, Mutex};

    // Create a custom sink that captures output
    struct CaptureSink {
        captured: Arc<Mutex<Vec<String>>>,
    }

    impl OutputSink for CaptureSink {
        fn emit(&self, message: &str) {
            self.captured.lock().unwrap().push(message.to_string());
        }
        fn emit_line(&self, message: &str) {
            self.captured.lock().unwrap().push(message.to_string());
        }
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(CaptureSink {
        captured: captured.clone(),
    });

    // Install sink and enable logging
    set_output_sink(sink);
    enable_logging();

    // Log some events
    log_event("Starting operation...");
    log_event_line("Tool bash completed");
    log_event("Operation finished.");

    // Verify
    let logs = captured.lock().unwrap();
    assert!(logs.len() >= 3, "Should have logged 3 events");
    assert!(
        logs.iter().any(|l| l.contains("Starting")),
        "Should contain start event"
    );
    assert!(
        logs.iter().any(|l| l.contains("bash")),
        "Should contain tool event"
    );
    assert!(
        logs.iter().any(|l| l.contains("finished")),
        "Should contain end event"
    );

    // Cleanup
    disable_logging();
}

/// Test that logging can be disabled.
#[test]
fn test_logging_disabled() {
    use std::sync::{Arc, Mutex};

    struct CaptureSink {
        captured: Arc<Mutex<Vec<String>>>,
    }

    impl OutputSink for CaptureSink {
        fn emit(&self, message: &str) {
            self.captured.lock().unwrap().push(message.to_string());
        }
        fn emit_line(&self, message: &str) {
            self.captured.lock().unwrap().push(message.to_string());
        }
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(CaptureSink {
        captured: captured.clone(),
    });

    set_output_sink(sink);
    disable_logging();

    // These should not be captured
    log_event("Should not appear");
    log_event_line("Also should not appear");

    let logs = captured.lock().unwrap();
    assert!(logs.is_empty(), "Should not log when disabled: {:?}", logs);
}

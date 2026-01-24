//! Demo binary for clemitui E2E testing.
//!
//! This binary exercises clemitui's public API for PTY-based integration tests.
//! Each subcommand demonstrates a specific feature.

use clemitui::{
    OutputSink, TextBuffer, format_cancelled, format_context_warning, format_ctrl_c,
    format_error_detail, format_error_message, format_retry, format_tool_args,
    format_tool_executing, format_tool_result, log_event, log_event_line, set_output_sink,
};
use serde_json::json;
use std::env;
use std::sync::Arc;
use std::time::Duration;

/// Simple stdout sink for demo purposes.
struct StdoutSink;

impl OutputSink for StdoutSink {
    fn emit(&self, message: &str) {
        print!("{}", message);
    }

    fn emit_line(&self, message: &str) {
        println!("{}", message);
    }
}

fn main() {
    // Force color output even in non-TTY (for test capture)
    colored::control::set_override(true);

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: clemitui-demo <command> [args...]");
        eprintln!("Commands:");
        eprintln!("  tool-executing <name> [args_json]");
        eprintln!("  tool-result <name> <duration_ms> <tokens> [error]");
        eprintln!("  text-buffer <markdown>");
        eprintln!("  context-warning <used> <limit>");
        eprintln!("  error-detail <message>");
        eprintln!("  error-message <message>");
        eprintln!("  retry <attempt> <max> <reason>");
        eprintln!("  ctrl-c");
        eprintln!("  cancelled");
        eprintln!("  logging");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "tool-executing" => {
            let name = args.get(2).map(|s| s.as_str()).unwrap_or("test_tool");
            let args_json = args.get(3).map(|s| s.as_str()).unwrap_or("{}");
            let args_value: serde_json::Value =
                serde_json::from_str(args_json).unwrap_or(json!({}));
            let output = format_tool_executing(name, &args_value);
            print!("{}", output); // output already has newline
        }

        "tool-result" => {
            let name = args.get(2).map(|s| s.as_str()).unwrap_or("test_tool");
            let duration_ms: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
            let tokens: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(50);
            let has_error = args.get(5).is_some();
            let duration = Duration::from_millis(duration_ms);
            let output = format_tool_result(name, duration, tokens, has_error);
            println!("{}", output);
        }

        "text-buffer" => {
            let markdown = args
                .get(2)
                .map(|s| s.as_str())
                .unwrap_or("**Hello** world!");
            let mut buffer = TextBuffer::new();
            buffer.push(markdown);
            if let Some(rendered) = buffer.flush() {
                println!("{}", rendered);
            }
        }

        "context-warning" => {
            let used: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(900000);
            let limit: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1000000);
            let percentage = (used as f64 / limit as f64) * 100.0;
            let output = format_context_warning(percentage);
            println!("{}", output);
        }

        "error-detail" => {
            let message = args
                .get(2)
                .map(|s| s.as_str())
                .unwrap_or("Something went wrong");
            let output = format_error_detail(message);
            println!("{}", output);
        }

        "error-message" => {
            let message = args.get(2).map(|s| s.as_str()).unwrap_or("Error occurred");
            let output = format_error_message(message);
            println!("{}", output);
        }

        "retry" => {
            let attempt: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
            let max: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
            let reason = args.get(4).map(|s| s.as_str()).unwrap_or("rate limit");
            let output = format_retry(attempt, max, Duration::from_secs(2), reason);
            println!("{}", output);
        }

        "ctrl-c" => {
            let output = format_ctrl_c();
            println!("{}", output);
        }

        "cancelled" => {
            let output = format_cancelled();
            println!("{}", output);
        }

        "logging" => {
            // Test the logging infrastructure
            set_output_sink(Arc::new(StdoutSink));
            log_event("This is a log event");
            log_event_line("This is a log line");
            log_event("Another event");
        }

        "tool-args-complex" => {
            // Test complex tool args formatting
            let args_json = json!({
                "command": "echo hello",
                "timeout": 30,
                "background": false,
                "very_long_value": "This is a very long string that should be truncated when it exceeds the maximum length allowed for display"
            });
            let formatted = format_tool_args("bash", &args_json);
            println!("{}", formatted);
        }

        "tool-args-edit" => {
            // Test edit tool args filtering (should hide old_string/new_string)
            let args_json = json!({
                "file_path": "/path/to/file.rs",
                "old_string": "original content here",
                "new_string": "replacement content here"
            });
            let formatted = format_tool_args("edit", &args_json);
            println!("{}", formatted);
        }

        "text-buffer-multiline" => {
            // Test multiline markdown rendering
            let markdown = r#"# Header

This is a paragraph with **bold** and *italic* text.

- Item 1
- Item 2
- Item 3

```rust
fn main() {
    println!("Hello!");
}
```
"#;
            let mut buffer = TextBuffer::new();
            buffer.push(markdown);
            if let Some(rendered) = buffer.flush() {
                println!("{}", rendered);
            }
        }

        "text-buffer-streaming" => {
            // Simulate streaming text accumulation
            let mut buffer = TextBuffer::new();
            buffer.push("Hello ");
            buffer.push("**world**");
            buffer.push("! This is ");
            buffer.push("streaming text.");
            if let Some(rendered) = buffer.flush() {
                println!("{}", rendered);
            }
        }

        _ => {
            eprintln!("Unknown command: {}", args[1]);
            std::process::exit(1);
        }
    }
}

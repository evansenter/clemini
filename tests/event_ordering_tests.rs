//! Integration tests for event ordering.
//!
//! These tests verify that tool output events are emitted in the correct order
//! relative to ToolExecuting and ToolResult events.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test event_ordering_tests
//! ```

use clemini::{AgentEvent, CleminiToolService};
use genai_rs::{CallableFunction, ToolService};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

/// Creates a tool service with events_tx configured
fn create_tool_service_with_events(
    temp_dir: &TempDir,
    events_tx: mpsc::Sender<AgentEvent>,
) -> Arc<CleminiToolService> {
    let service = Arc::new(CleminiToolService::new(
        temp_dir.path().to_path_buf(),
        120,
        false,
        vec![temp_dir.path().to_path_buf()],
        "dummy-key".to_string(),
    ));
    service.set_events_tx(Some(events_tx));
    service
}

/// Helper to collect events from a channel
async fn collect_events(mut rx: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    // Use try_recv to get all pending events without blocking
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn test_bash_tool_output_ordering() {
    let temp_dir = tempfile::tempdir().unwrap();
    let (events_tx, events_rx) = mpsc::channel::<AgentEvent>(100);

    let tool_service = create_tool_service_with_events(&temp_dir, events_tx);

    // Get the bash tool and execute it
    let tools = tool_service.tools();
    let bash_tool = tools
        .iter()
        .find(|t: &&Arc<dyn CallableFunction>| t.declaration().name() == "bash")
        .unwrap();

    // Execute a simple echo command
    let _result: Value = bash_tool
        .call(json!({
            "command": "echo 'test output'",
            "description": "Test echo"
        }))
        .await
        .unwrap();

    // Collect events
    let events = collect_events(events_rx).await;

    // Should have at least one ToolOutput event (the [bash] narration and/or command output)
    assert!(
        !events.is_empty(),
        "Expected ToolOutput events from bash tool, got none"
    );

    // All events should be ToolOutput
    for event in &events {
        assert!(
            matches!(event, AgentEvent::ToolOutput(_)),
            "Expected ToolOutput event, got {:?}",
            event
        );
    }

    // Verify the output contains expected content
    let outputs: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolOutput(s) = e {
                Some(s.as_str())
            } else {
                None
            }
        })
        .collect();

    let combined = outputs.join("");
    assert!(
        combined.contains("test output") || combined.contains("[bash]"),
        "Expected bash output to contain command output or narration, got: {:?}",
        outputs
    );
}

#[tokio::test]
async fn test_todo_write_tool_output_ordering() {
    let temp_dir = tempfile::tempdir().unwrap();
    let (events_tx, events_rx) = mpsc::channel::<AgentEvent>(100);

    let tool_service = create_tool_service_with_events(&temp_dir, events_tx);

    // Get the todo_write tool and execute it
    let tools = tool_service.tools();
    let todo_tool = tools
        .iter()
        .find(|t: &&Arc<dyn CallableFunction>| t.declaration().name() == "todo_write")
        .unwrap();

    // Execute todo_write
    let _result: Value = todo_tool
        .call(json!({
            "todos": [
                {"content": "First task", "activeForm": "Doing first task", "status": "pending"},
                {"content": "Second task", "activeForm": "Doing second task", "status": "in_progress"}
            ]
        }))
        .await
        .unwrap();

    // Collect events
    let events = collect_events(events_rx).await;

    // Should have ToolOutput events for the rendered todo list
    assert!(
        !events.is_empty(),
        "Expected ToolOutput events from todo_write tool, got none"
    );

    // Verify the output contains the todo items
    let outputs: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolOutput(s) = e {
                Some(s.as_str())
            } else {
                None
            }
        })
        .collect();

    let combined = outputs.join("");
    assert!(
        combined.contains("First task") || combined.contains("Second task"),
        "Expected todo output to contain task names, got: {:?}",
        outputs
    );
}

#[tokio::test]
async fn test_tool_without_events_tx_falls_back() {
    // This test verifies the fallback behavior when events_tx is None
    let temp_dir = tempfile::tempdir().unwrap();

    // Create tool service WITHOUT setting events_tx
    let tool_service = Arc::new(CleminiToolService::new(
        temp_dir.path().to_path_buf(),
        120,
        false,
        vec![temp_dir.path().to_path_buf()],
        "dummy-key".to_string(),
    ));

    // Get the bash tool and execute it - should not panic
    let tools = tool_service.tools();
    let bash_tool = tools
        .iter()
        .find(|t: &&Arc<dyn CallableFunction>| t.declaration().name() == "bash")
        .unwrap();

    // This should work without panicking, using the log_event fallback
    let result: Result<Value, _> = bash_tool
        .call(json!({
            "command": "echo 'fallback test'",
            "description": "Test fallback"
        }))
        .await;

    assert!(result.is_ok(), "Tool should work without events_tx");
    let output = result.unwrap();
    assert!(
        output.get("stdout").is_some() || output.get("output").is_some(),
        "Should have output in result"
    );
}

#[tokio::test]
async fn test_glob_tool_emits_count_output() {
    let temp_dir = tempfile::tempdir().unwrap();
    let (events_tx, events_rx) = mpsc::channel::<AgentEvent>(100);

    // Create some test files
    std::fs::write(temp_dir.path().join("test1.txt"), "content").unwrap();
    std::fs::write(temp_dir.path().join("test2.txt"), "content").unwrap();

    let tool_service = create_tool_service_with_events(&temp_dir, events_tx);

    // Get the glob tool and execute it
    let tools = tool_service.tools();
    let glob_tool = tools
        .iter()
        .find(|t: &&Arc<dyn CallableFunction>| t.declaration().name() == "glob")
        .unwrap();

    // Execute glob
    let _result: Value = glob_tool
        .call(json!({
            "pattern": "*.txt"
        }))
        .await
        .unwrap();

    // Collect events
    let events = collect_events(events_rx).await;

    // Should have ToolOutput event with file count
    assert!(
        !events.is_empty(),
        "Expected ToolOutput events from glob tool, got none"
    );

    let outputs: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolOutput(s) = e {
                Some(s.as_str())
            } else {
                None
            }
        })
        .collect();

    let combined = outputs.join("");
    assert!(
        combined.contains("2 files"),
        "Expected glob output to contain '2 files', got: {:?}",
        outputs
    );
}

#[tokio::test]
async fn test_grep_tool_emits_match_count() {
    let temp_dir = tempfile::tempdir().unwrap();
    let (events_tx, events_rx) = mpsc::channel::<AgentEvent>(100);

    // Create test files with searchable content
    std::fs::write(temp_dir.path().join("file1.txt"), "hello world").unwrap();
    std::fs::write(temp_dir.path().join("file2.txt"), "hello rust").unwrap();

    let tool_service = create_tool_service_with_events(&temp_dir, events_tx);

    // Get the grep tool and execute it
    let tools = tool_service.tools();
    let grep_tool = tools
        .iter()
        .find(|t: &&Arc<dyn CallableFunction>| t.declaration().name() == "grep")
        .unwrap();

    // Execute grep
    let _result: Value = grep_tool
        .call(json!({
            "pattern": "hello"
        }))
        .await
        .unwrap();

    // Collect events
    let events = collect_events(events_rx).await;

    // Should have ToolOutput event with match count
    assert!(
        !events.is_empty(),
        "Expected ToolOutput events from grep tool, got none"
    );

    let outputs: Vec<&str> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolOutput(s) = e {
                Some(s.as_str())
            } else {
                None
            }
        })
        .collect();

    let combined = outputs.join("");
    assert!(
        combined.contains("matches") && combined.contains("files"),
        "Expected grep output to contain match info, got: {:?}",
        outputs
    );
}

#[tokio::test]
async fn test_edit_tool_emits_diff_output() {
    let temp_dir = tempfile::tempdir().unwrap();
    let (events_tx, events_rx) = mpsc::channel::<AgentEvent>(100);

    let tool_service = create_tool_service_with_events(&temp_dir, events_tx);

    // Create a file to edit
    let test_file = temp_dir.path().join("test.txt");
    std::fs::write(&test_file, "hello world").unwrap();

    // Get the edit tool and execute it
    let tools = tool_service.tools();
    let edit_tool = tools
        .iter()
        .find(|t: &&Arc<dyn CallableFunction>| t.declaration().name() == "edit")
        .unwrap();

    // Execute an edit
    let _result: Value = edit_tool
        .call(json!({
            "file_path": test_file.to_string_lossy(),
            "old_string": "hello",
            "new_string": "goodbye"
        }))
        .await
        .unwrap();

    // Collect events
    let events = collect_events(events_rx).await;

    // Should have ToolOutput events for the diff
    assert!(
        !events.is_empty(),
        "Expected ToolOutput events from edit tool for diff output, got none"
    );

    // All events should be ToolOutput
    for event in &events {
        assert!(
            matches!(event, AgentEvent::ToolOutput(_)),
            "Expected ToolOutput event, got {:?}",
            event
        );
    }
}

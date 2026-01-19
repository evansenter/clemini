//! Integration tests for tool output events.
//!
//! These tests verify that tools emit ToolOutput events correctly during
//! real interactions with the Gemini API, and that the model interprets
//! tool results appropriately.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test tool_output_tests -- --include-ignored --nocapture
//! ```

mod common;

use clemini::{AgentEvent, CleminiToolService, run_interaction};
use common::{
    DEFAULT_TIMEOUT, assert_response_semantic, create_temp_dir, get_api_key, get_client,
    init_test_logging, with_timeout,
};
use std::fs;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const TEST_MODEL: &str = "gemini-3-flash-preview";

const TEST_SYSTEM_PROMPT: &str = r#"You are a helpful coding assistant being tested.
Execute the requested commands and report results clearly.
Be concise in your responses."#;

/// Helper to create a tool service for testing
fn create_test_tool_service(
    temp_dir: &tempfile::TempDir,
    api_key: &str,
) -> Arc<CleminiToolService> {
    Arc::new(CleminiToolService::new(
        temp_dir.path().to_path_buf(),
        120,   // bash_timeout
        false, // mcp_mode = false for standard behavior
        vec![temp_dir.path().to_path_buf()],
        api_key.to_string(),
    ))
}

/// Helper to run an interaction and collect events
async fn run_test_interaction(
    client: &genai_rs::Client,
    tool_service: &Arc<CleminiToolService>,
    input: &str,
    previous_id: Option<&str>,
    events_tx: mpsc::Sender<AgentEvent>,
) -> clemini::InteractionResult {
    let cancellation = CancellationToken::new();

    // Set events_tx on tool service so tools can emit ToolOutput events
    tool_service.set_events_tx(Some(events_tx.clone()));

    let result = run_interaction(
        client,
        tool_service,
        input,
        previous_id,
        TEST_MODEL,
        TEST_SYSTEM_PROMPT,
        events_tx,
        cancellation,
    )
    .await
    .expect("Interaction failed");

    // Clear events_tx after interaction
    tool_service.set_events_tx(None);

    result
}

/// Collect ToolOutput events from a list of events
fn collect_tool_outputs(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolOutput(s) = e {
                Some(s.clone())
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_bash_tool_emits_output_events() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    let (events_tx, mut events_rx) = mpsc::channel(100);

    // Spawn a task to collect events
    let events_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = events_rx.recv().await {
            events.push(event);
        }
        events
    });

    let result = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Run: echo 'hello from integration test'",
            None,
            events_tx,
        ),
    )
    .await;

    let events = events_handle.await.expect("Event collection failed");
    let tool_outputs = collect_tool_outputs(&events);

    // Should have ToolOutput events from bash
    assert!(
        !tool_outputs.is_empty(),
        "Expected ToolOutput events from bash tool"
    );

    // Combined output should contain the echo result
    let combined = tool_outputs.join("");
    assert!(
        combined.contains("hello from integration test") || combined.contains("[bash]"),
        "Expected bash output to contain command result or narration, got: {:?}",
        tool_outputs
    );

    // Model should acknowledge the command ran
    assert_response_semantic(
        &client,
        "User asked to run 'echo hello from integration test'",
        &result.response,
        "Does the response indicate the echo command was executed successfully?",
    )
    .await;
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_glob_tool_emits_file_count() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();

    // Create test files
    fs::write(temp_dir.path().join("file1.txt"), "content1").unwrap();
    fs::write(temp_dir.path().join("file2.txt"), "content2").unwrap();
    fs::write(temp_dir.path().join("file3.txt"), "content3").unwrap();

    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    let (events_tx, mut events_rx) = mpsc::channel(100);

    let events_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = events_rx.recv().await {
            events.push(event);
        }
        events
    });

    let result = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "List all .txt files in the current directory",
            None,
            events_tx,
        ),
    )
    .await;

    let events = events_handle.await.expect("Event collection failed");
    let tool_outputs = collect_tool_outputs(&events);

    // Should have ToolOutput events with file count
    let combined = tool_outputs.join("");
    assert!(
        combined.contains("3 files") || combined.contains("files"),
        "Expected glob output to contain file count, got: {:?}",
        tool_outputs
    );

    // Model should report finding the files
    assert_response_semantic(
        &client,
        "User asked to list .txt files. There are 3 .txt files in the directory.",
        &result.response,
        "Does the response indicate that multiple .txt files were found?",
    )
    .await;
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_edit_tool_emits_diff_and_model_confirms() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();

    // Create a file to edit
    let test_file = temp_dir.path().join("greeting.txt");
    fs::write(&test_file, "Hello World").unwrap();

    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    let (events_tx, mut events_rx) = mpsc::channel(100);

    let events_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = events_rx.recv().await {
            events.push(event);
        }
        events
    });

    let result = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!(
                "In the file {}, change 'Hello' to 'Goodbye'",
                test_file.display()
            ),
            None,
            events_tx,
        ),
    )
    .await;

    let events = events_handle.await.expect("Event collection failed");
    let tool_outputs = collect_tool_outputs(&events);

    // Should have ToolOutput events (diff output)
    assert!(
        !tool_outputs.is_empty(),
        "Expected ToolOutput events from edit tool"
    );

    // Verify the file was actually changed
    let contents = fs::read_to_string(&test_file).unwrap();
    assert!(
        contents.contains("Goodbye"),
        "File should contain 'Goodbye' after edit"
    );

    // Model should confirm the edit
    assert_response_semantic(
        &client,
        "User asked to change 'Hello' to 'Goodbye' in a file",
        &result.response,
        "Does the response indicate the file was successfully edited or modified?",
    )
    .await;
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_grep_tool_emits_match_count() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();

    // Create files with searchable content
    fs::write(
        temp_dir.path().join("app.rs"),
        "fn main() { println!(\"hello\"); }",
    )
    .unwrap();
    fs::write(temp_dir.path().join("lib.rs"), "pub fn hello() { }").unwrap();
    fs::write(temp_dir.path().join("test.rs"), "fn test_hello() { }").unwrap();

    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    let (events_tx, mut events_rx) = mpsc::channel(100);

    let events_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = events_rx.recv().await {
            events.push(event);
        }
        events
    });

    let result = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Search for 'hello' in all .rs files",
            None,
            events_tx,
        ),
    )
    .await;

    let events = events_handle.await.expect("Event collection failed");
    let tool_outputs = collect_tool_outputs(&events);

    // Should have ToolOutput events with match info
    let combined = tool_outputs.join("");
    assert!(
        combined.contains("matches") || combined.contains("files"),
        "Expected grep output to contain match info, got: {:?}",
        tool_outputs
    );

    // Model should report finding matches
    assert_response_semantic(
        &client,
        "User searched for 'hello' in .rs files. There are 3 files containing 'hello'.",
        &result.response,
        "Does the response indicate that 'hello' was found in multiple files?",
    )
    .await;
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_read_tool_emits_line_count() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();

    // Create a file with known content
    let test_file = temp_dir.path().join("data.txt");
    fs::write(&test_file, "line 1\nline 2\nline 3\nline 4\nline 5").unwrap();

    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    let (events_tx, mut events_rx) = mpsc::channel(100);

    let events_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = events_rx.recv().await {
            events.push(event);
        }
        events
    });

    let result = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!("Read the file {}", test_file.display()),
            None,
            events_tx,
        ),
    )
    .await;

    let events = events_handle.await.expect("Event collection failed");
    let tool_outputs = collect_tool_outputs(&events);

    // Should have ToolOutput events with line count
    let combined = tool_outputs.join("");
    assert!(
        combined.contains("5 lines") || combined.contains("lines"),
        "Expected read output to contain line count, got: {:?}",
        tool_outputs
    );

    // Model should acknowledge the file contents
    assert_response_semantic(
        &client,
        "User asked to read a file with 5 lines",
        &result.response,
        "Does the response indicate the file was read and contains multiple lines?",
    )
    .await;
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_todo_write_emits_task_output() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    let (events_tx, mut events_rx) = mpsc::channel(100);

    let events_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = events_rx.recv().await {
            events.push(event);
        }
        events
    });

    let result = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Create a todo list with three tasks: 'Write tests', 'Run tests', 'Fix bugs'",
            None,
            events_tx,
        ),
    )
    .await;

    let events = events_handle.await.expect("Event collection failed");
    let tool_outputs = collect_tool_outputs(&events);

    // Should have ToolOutput events with todo items
    assert!(
        !tool_outputs.is_empty(),
        "Expected ToolOutput events from todo_write tool"
    );

    let combined = tool_outputs.join("");
    // At least one task should appear in output
    assert!(
        combined.contains("Write tests")
            || combined.contains("Run tests")
            || combined.contains("Fix bugs")
            || combined.contains("pending")
            || combined.contains("○"), // pending marker
        "Expected todo output to contain task names or status markers, got: {:?}",
        tool_outputs
    );

    // Model should confirm the todo list was created
    assert_response_semantic(
        &client,
        "User asked to create a todo list with three tasks",
        &result.response,
        "Does the response indicate that a todo list was created with multiple tasks?",
    )
    .await;
}

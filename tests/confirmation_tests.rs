//! Integration tests for the confirmation flow.
//!
//! These tests verify that destructive commands properly request confirmation
//! and that approval allows execution.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test confirmation_tests -- --include-ignored --nocapture
//! ```

mod common;

use clemini::{AgentEvent, CleminiToolService, run_interaction};
use common::{
    DEFAULT_TIMEOUT, EXTENDED_TIMEOUT, assert_response_semantic, create_temp_dir, get_api_key,
    get_client, init_test_logging, with_timeout,
};
use std::fs;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const TEST_MODEL: &str = "gemini-3-flash-preview";

const TEST_SYSTEM_PROMPT: &str = r#"You are a helpful coding assistant being tested.
When asked to delete files, use the bash tool with rm command.
Do not ask for confirmation yourself - just execute the command.
If a command returns needs_confirmation, explain what needs approval and stop."#;

/// Helper to create a tool service for testing
fn create_test_tool_service(
    temp_dir: &tempfile::TempDir,
    api_key: &str,
) -> Arc<CleminiToolService> {
    Arc::new(CleminiToolService::new(
        temp_dir.path().to_path_buf(),
        120,  // bash_timeout
        true, // mcp_mode = true for confirmation testing
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
) -> (clemini::InteractionResult, Vec<AgentEvent>) {
    let (events_tx, mut events_rx) = mpsc::channel(100);
    let cancellation = CancellationToken::new();

    // Spawn a task to collect events
    let events_handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = events_rx.recv().await {
            events.push(event);
        }
        events
    });

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

    let events = events_handle.await.expect("Event collection failed");

    (result, events)
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_destructive_command_requests_confirmation() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a test file
    let test_file = temp_dir.path().join("test-delete-me.txt");
    fs::write(&test_file, "test content").expect("Failed to create test file");
    assert!(test_file.exists(), "Test file should exist before test");

    let (result, _events) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!("Delete the file at {}", test_file.display()),
            None,
        ),
    )
    .await;

    // The result should have needs_confirmation set
    assert!(
        result.needs_confirmation.is_some(),
        "Expected needs_confirmation to be set, got response: {}",
        result.response
    );

    // The file should still exist (not deleted yet)
    assert!(
        test_file.exists(),
        "File should NOT be deleted before confirmation"
    );

    println!("Confirmation requested: {:?}", result.needs_confirmation);
    println!("Response: {}", result.response);
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_confirmation_approval_executes_command() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let temp_dir = create_temp_dir();
    let api_key = get_api_key().expect("API key required");
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a test file
    let test_file = temp_dir.path().join("test-approved-delete.txt");
    fs::write(&test_file, "test content").expect("Failed to create test file");

    // First interaction: request deletion (should get confirmation request)
    let (result1, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!("Delete the file at {}", test_file.display()),
            None,
        ),
    )
    .await;

    assert!(
        result1.needs_confirmation.is_some(),
        "First interaction should request confirmation"
    );
    assert!(
        test_file.exists(),
        "File should exist after first interaction"
    );

    let interaction_id = result1.id.as_ref().expect("Should have interaction ID");

    // Second interaction: approve the deletion
    let (result2, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Yes, please proceed with deleting that file. I approve.",
            Some(interaction_id),
        ),
    )
    .await;

    // Second interaction should NOT need confirmation
    assert!(
        result2.needs_confirmation.is_none(),
        "Second interaction should not need confirmation, got: {:?}",
        result2.needs_confirmation
    );

    // File should be deleted after approval
    assert!(
        !test_file.exists(),
        "File should be deleted after approval. Response: {}",
        result2.response
    );

    println!("Successfully deleted file after approval");
    println!("Final response: {}", result2.response);
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_safe_command_no_confirmation() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let temp_dir = create_temp_dir();
    let api_key = get_api_key().expect("API key required");
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a test file
    let test_file = temp_dir.path().join("safe-read-test.txt");
    fs::write(&test_file, "hello world").expect("Failed to create test file");

    // Request to read the file (should NOT need confirmation)
    let (result, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!("Read the contents of {}", test_file.display()),
            None,
        ),
    )
    .await;

    // Should NOT need confirmation for safe commands
    assert!(
        result.needs_confirmation.is_none(),
        "Safe command should not request confirmation, got: {:?}",
        result.needs_confirmation
    );

    // Response should contain the file contents
    assert!(
        result.response.contains("hello world"),
        "Response should contain file contents: {}",
        result.response
    );

    println!("Safe command executed without confirmation");
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_confirmation_response_is_semantic() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let temp_dir = create_temp_dir();
    let api_key = get_api_key().expect("API key required");
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a test file
    let test_file = temp_dir.path().join("semantic-test.txt");
    fs::write(&test_file, "important data").expect("Failed to create test file");

    let (result, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!("Delete {}", test_file.display()),
            None,
        ),
    )
    .await;

    // When needs_confirmation is set, the response text is empty because the agent
    // breaks out early. We validate the confirmation payload itself.
    let confirmation = result
        .needs_confirmation
        .as_ref()
        .expect("Expected needs_confirmation to be set");

    // Use semantic validation to check the confirmation message
    let confirmation_str = serde_json::to_string_pretty(confirmation).unwrap();
    assert_response_semantic(
        &client,
        &format!(
            "User asked to delete file {}. The system returned a confirmation request.",
            test_file.display()
        ),
        &confirmation_str,
        "Does this JSON payload indicate that a destructive command (file deletion) requires user confirmation?",
    )
    .await;
}

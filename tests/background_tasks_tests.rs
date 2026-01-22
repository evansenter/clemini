//! Integration tests for background task execution and monitoring.
//!
//! These tests verify that background tasks (via bash or task tools) are correctly
//! spawned, executed, and their outputs can be retrieved using the task_output tool.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test background_tasks_tests -- --include-ignored --nocapture
//! ```

mod common;

use clemini::{AgentEvent, CleminiToolService, run_interaction};
use common::{
    assert_response_semantic, create_temp_dir, get_api_key, get_client, init_test_logging,
    with_timeout,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const TEST_MODEL: &str = "gemini-3-flash-preview";

const TEST_SYSTEM_PROMPT: &str = r#"You are a helpful coding assistant being tested.
Execute the requested commands and report results clearly.
Be concise in your responses."#;

fn create_test_tool_service(
    temp_dir: &tempfile::TempDir,
    api_key: &str,
) -> Arc<CleminiToolService> {
    Arc::new(CleminiToolService::new(
        temp_dir.path().to_path_buf(),
        120,   // bash_timeout
        false, // mcp_mode = false
        vec![temp_dir.path().to_path_buf()],
        api_key.to_string(),
    ))
}

async fn run_test_interaction(
    client: &genai_rs::Client,
    tool_service: &Arc<CleminiToolService>,
    input: &str,
    previous_id: Option<&str>,
    events_tx: mpsc::Sender<AgentEvent>,
) -> clemini::InteractionResult {
    let cancellation = CancellationToken::new();

    // Guard clears events_tx when dropped
    let _events_guard = tool_service.with_events_tx(events_tx.clone());

    run_interaction(
        client,
        tool_service,
        input,
        previous_id,
        TEST_MODEL,
        TEST_SYSTEM_PROMPT,
        events_tx,
        cancellation,
        clemini::RetryConfig::default(),
    )
    .await
    .expect("Interaction failed")
}

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_bash_background_task_lifecycle() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };
    let api_key = get_api_key().unwrap();
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    let (events_tx, mut events_rx) = mpsc::channel(100);

    // Drain events in background
    tokio::spawn(async move { while events_rx.recv().await.is_some() {} });

    // Step 1: Start a background task
    let prompt1 = "Run 'echo hello_world && sleep 2 && echo goodbye_world >&2' in the background using bash. Give me the task_id.";
    let result1 = with_timeout(
        common::DEFAULT_TIMEOUT,
        run_test_interaction(&client, &tool_service, prompt1, None, events_tx.clone()),
    )
    .await;

    // Verify it started
    assert_response_semantic(
        &client,
        prompt1,
        &result1.response,
        r#"{
            "action": "verify_task_started",
            "criteria": "The response should confirm a background task was started and provide a task ID."
        }"#
    ).await;

    // Step 2: Use task_output to wait for it and get results
    // We pass the conversation history so it knows the task_id
    let prompt2 = "Wait for that task to complete and show me the stdout and stderr output.";
    let result2 = with_timeout(
        common::DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            prompt2,
            result1.id.as_deref(),
            events_tx.clone(),
        ),
    )
    .await;

    // Verify output
    assert_response_semantic(
        &client,
        prompt2,
        &result2.response,
        r#"{
            "action": "verify_output",
            "criteria": "The response should show 'hello_world' in stdout and 'goodbye_world' in stderr."
        }"#
    ).await;
}

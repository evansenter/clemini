//! Plan mode integration tests.
//!
//! These tests verify that plan mode correctly restricts tool access
//! and manages the planning workflow.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test plan_mode_tests -- --include-ignored --nocapture
//! ```

mod common;

use clemini::{AgentEvent, CleminiToolService, plan::PLAN_MANAGER, run_interaction};
use common::{
    DEFAULT_TIMEOUT, create_temp_dir, get_api_key, get_client, init_test_logging, with_timeout,
};
use std::fs;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const TEST_MODEL: &str = "gemini-3-flash-preview";

const TEST_SYSTEM_PROMPT: &str = r#"You are clemini, a helpful coding assistant.
Execute requested tasks using the available tools.
Be concise in your responses.
When asked to enter plan mode, use the enter_plan_mode tool.
When asked to exit plan mode, use the exit_plan_mode tool.
When asked to read a file, use the read tool.
When asked to run a bash command, use the bash tool.
When you encounter errors, explain what went wrong."#;

/// Helper to create a tool service for testing
fn create_test_tool_service(
    temp_dir: &tempfile::TempDir,
    api_key: &str,
) -> Arc<CleminiToolService> {
    Arc::new(CleminiToolService::new(
        temp_dir.path().to_path_buf(),
        120,
        false, // mcp_mode = false for standard behavior
        vec![temp_dir.path().to_path_buf()],
        api_key.to_string(),
    ))
}

/// Helper to run an interaction and return result + events
async fn run_test_interaction(
    client: &genai_rs::Client,
    tool_service: &Arc<CleminiToolService>,
    input: &str,
    previous_id: Option<&str>,
) -> (clemini::InteractionResult, Vec<AgentEvent>) {
    let (events_tx, mut events_rx) = mpsc::channel(100);
    let cancellation = CancellationToken::new();

    // Set events_tx on tool service
    tool_service.set_events_tx(Some(events_tx.clone()));

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
        clemini::RetryConfig::default(),
    )
    .await
    .expect("Interaction failed");

    tool_service.set_events_tx(None);

    let events = events_handle.await.expect("Event collection failed");
    (result, events)
}

/// Reset plan manager state between tests
fn reset_plan_manager() {
    if let Ok(mut manager) = PLAN_MANAGER.write() {
        while manager.is_in_plan_mode() {
            manager.exit_plan_mode();
        }
    }
}

// =============================================================================
// Unit Tests (No API key required)
// =============================================================================

/// Test that plan manager enter/exit works correctly.
#[test]
fn test_plan_manager_state_transitions() {
    use clemini::plan::{PlanEntryInput, PlanEntryPriority, PlanEntryStatus, PlanManager};

    let mut manager = PlanManager::new();

    // Initially not in plan mode
    assert!(!manager.is_in_plan_mode());

    // Enter plan mode
    manager
        .enter_plan_mode(None)
        .expect("Should enter plan mode");
    assert!(manager.is_in_plan_mode());
    assert!(manager.plan_file_path().is_some());

    // Cannot enter again
    assert!(manager.enter_plan_mode(None).is_err());

    // Create a plan
    let plan = manager.create_plan(vec![
        PlanEntryInput {
            content: "Step 1: Research".to_string(),
            priority: PlanEntryPriority::High,
        },
        PlanEntryInput {
            content: "Step 2: Implement".to_string(),
            priority: PlanEntryPriority::Medium,
        },
        PlanEntryInput {
            content: "Step 3: Test".to_string(),
            priority: PlanEntryPriority::Low,
        },
    ]);
    assert_eq!(plan.entries.len(), 3);

    // Update entry status
    manager
        .update_entry_status(0, PlanEntryStatus::InProgress)
        .expect("Should update status");
    let current = manager.current_plan().expect("Should have plan");
    assert_eq!(
        current.entries[0].status,
        agent_client_protocol::PlanEntryStatus::InProgress
    );

    // Exit plan mode
    assert!(manager.exit_plan_mode());
    assert!(!manager.is_in_plan_mode());

    // Can't exit again (returns false, not error)
    assert!(!manager.exit_plan_mode());
}

/// Test that tool_is_read_only correctly categorizes tools.
#[test]
fn test_tool_read_only_classification() {
    use clemini::plan::is_tool_allowed_in_plan_mode;

    // Read-only tools should be allowed
    let allowed = [
        "read",
        "glob",
        "grep",
        "web_fetch",
        "web_search",
        "ask_user",
        "todo_write",
        "task_output",
        "enter_plan_mode",
        "exit_plan_mode",
        "event_bus_list_sessions",
        "event_bus_list_channels",
        "event_bus_get_events",
    ];

    for tool in allowed {
        assert!(
            is_tool_allowed_in_plan_mode(tool),
            "{} should be allowed in plan mode",
            tool
        );
    }

    // Write tools should be blocked
    let blocked = [
        "bash",
        "edit",
        "write",
        "kill_shell",
        "task",
        "event_bus_register",
        "event_bus_publish",
        "event_bus_unregister",
        "notify",
    ];

    for tool in blocked {
        assert!(
            !is_tool_allowed_in_plan_mode(tool),
            "{} should be blocked in plan mode",
            tool
        );
    }
}

/// Test plan entry status transitions.
#[test]
fn test_plan_entry_status_updates() {
    use clemini::plan::{PlanEntryInput, PlanEntryPriority, PlanEntryStatus, PlanManager};

    let mut manager = PlanManager::new();
    manager.enter_plan_mode(None).unwrap();

    manager.create_plan(vec![PlanEntryInput {
        content: "Task 1".to_string(),
        priority: PlanEntryPriority::High,
    }]);

    // Update to InProgress
    manager
        .update_entry_status(0, PlanEntryStatus::InProgress)
        .unwrap();
    assert_eq!(
        manager.current_plan().unwrap().entries[0].status,
        agent_client_protocol::PlanEntryStatus::InProgress
    );

    // Update to Completed
    manager
        .update_entry_status(0, PlanEntryStatus::Completed)
        .unwrap();
    assert_eq!(
        manager.current_plan().unwrap().entries[0].status,
        agent_client_protocol::PlanEntryStatus::Completed
    );

    // Out of range index should fail
    assert!(
        manager
            .update_entry_status(5, PlanEntryStatus::Completed)
            .is_err()
    );
}

/// Test that plan file path can be customized.
#[test]
fn test_plan_file_path_customization() {
    use clemini::plan::PlanManager;
    use std::path::PathBuf;

    let mut manager = PlanManager::new();
    let custom_path = PathBuf::from("/tmp/test-plan.md");

    manager.enter_plan_mode(Some(custom_path.clone())).unwrap();
    assert_eq!(manager.plan_file_path(), Some(&custom_path));
}

// =============================================================================
// Integration Tests (Require GEMINI_API_KEY)
// =============================================================================

/// Test that entering plan mode works via the tool.
#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_enter_plan_mode_via_tool() {
    init_test_logging();
    reset_plan_manager();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    let (result, events) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(&client, &tool_service, "Enter plan mode now.", None),
    )
    .await;

    // Should have used enter_plan_mode tool
    let used_enter_plan = events.iter().any(|e| {
        matches!(e,
            AgentEvent::ToolExecuting(calls) if calls.iter().any(|c| c.name == "enter_plan_mode")
        )
    });
    assert!(used_enter_plan, "Should have used enter_plan_mode tool");

    // Response should mention plan mode
    let response_lower = result.response.to_lowercase();
    assert!(
        response_lower.contains("plan") || response_lower.contains("mode"),
        "Response should mention plan mode: {}",
        result.response
    );

    reset_plan_manager();
}

/// Test that read-only tools work in plan mode.
#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_read_tools_work_in_plan_mode() {
    init_test_logging();
    reset_plan_manager();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a test file
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "Hello from test file!").unwrap();

    // Enter plan mode first
    {
        let mut manager = PLAN_MANAGER.write().unwrap();
        manager.enter_plan_mode(None).unwrap();
    }

    // Ask to read the file - should work in plan mode
    let (result, events) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!(
                "Read the file at {} and tell me what it contains.",
                test_file.display()
            ),
            None,
        ),
    )
    .await;

    // Should have used read tool successfully
    let used_read = events.iter().any(|e| {
        matches!(e,
            AgentEvent::ToolExecuting(calls) if calls.iter().any(|c| c.name == "read")
        )
    });
    assert!(used_read, "Should have used read tool");

    // Response should contain the file content
    assert!(
        result.response.contains("Hello") || result.response.contains("test file"),
        "Response should mention file content: {}",
        result.response
    );

    reset_plan_manager();
}

/// Test that write tools are blocked in plan mode.
#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_write_tools_blocked_in_plan_mode() {
    init_test_logging();
    reset_plan_manager();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Enter plan mode first
    {
        let mut manager = PLAN_MANAGER.write().unwrap();
        manager.enter_plan_mode(None).unwrap();
    }

    // Ask to run bash command - should be blocked
    let (result, events) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(&client, &tool_service, "Run 'echo hello' using bash.", None),
    )
    .await;

    // Check if bash was attempted and blocked
    let bash_attempted = events.iter().any(|e| {
        matches!(e,
            AgentEvent::ToolExecuting(calls) if calls.iter().any(|c| c.name == "bash")
        )
    });

    // Either bash wasn't attempted (model knows it's blocked), or it was blocked
    // The response should indicate the tool is not available in plan mode
    if bash_attempted {
        // Check for a ToolResult with an error about plan mode
        let got_plan_mode_error = events.iter().any(|e| {
            if let AgentEvent::ToolResult(result) = e {
                result
                    .result
                    .to_string()
                    .to_lowercase()
                    .contains("plan mode")
            } else {
                false
            }
        });
        assert!(
            got_plan_mode_error,
            "Bash should have been blocked with plan mode error"
        );
    }

    // Response should mention plan mode restriction or that bash can't be used
    let response_lower = result.response.to_lowercase();
    assert!(
        response_lower.contains("plan")
            || response_lower.contains("not available")
            || response_lower.contains("cannot")
            || response_lower.contains("blocked")
            || response_lower.contains("read-only"),
        "Response should mention plan mode restrictions: {}",
        result.response
    );

    reset_plan_manager();
}

/// Test that exiting plan mode works via the tool.
#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_exit_plan_mode_via_tool() {
    init_test_logging();
    reset_plan_manager();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Enter plan mode first
    {
        let mut manager = PLAN_MANAGER.write().unwrap();
        manager.enter_plan_mode(None).unwrap();
    }

    // Ask to exit plan mode
    let (result, events) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(&client, &tool_service, "Exit plan mode now.", None),
    )
    .await;

    // Should have used exit_plan_mode tool
    let used_exit_plan = events.iter().any(|e| {
        matches!(e,
            AgentEvent::ToolExecuting(calls) if calls.iter().any(|c| c.name == "exit_plan_mode")
        )
    });
    assert!(used_exit_plan, "Should have used exit_plan_mode tool");

    // Should no longer be in plan mode
    let in_plan_mode = PLAN_MANAGER
        .read()
        .map(|m| m.is_in_plan_mode())
        .unwrap_or(false);
    assert!(!in_plan_mode, "Should have exited plan mode");

    // Response should mention exiting plan mode or ready for review
    let response_lower = result.response.to_lowercase();
    assert!(
        response_lower.contains("exit")
            || response_lower.contains("plan")
            || response_lower.contains("review")
            || response_lower.contains("ready"),
        "Response should mention exiting plan mode: {}",
        result.response
    );

    reset_plan_manager();
}

/// Test that glob tool works in plan mode.
#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_glob_works_in_plan_mode() {
    init_test_logging();
    reset_plan_manager();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create some test files
    fs::write(temp_dir.path().join("file1.txt"), "content1").unwrap();
    fs::write(temp_dir.path().join("file2.txt"), "content2").unwrap();
    fs::write(temp_dir.path().join("other.md"), "markdown").unwrap();

    // Enter plan mode first
    {
        let mut manager = PLAN_MANAGER.write().unwrap();
        manager.enter_plan_mode(None).unwrap();
    }

    // Ask to find txt files using glob
    let (result, events) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!(
                "Use glob to find all .txt files in {}",
                temp_dir.path().display()
            ),
            None,
        ),
    )
    .await;

    // Should have used glob tool
    let used_glob = events.iter().any(|e| {
        matches!(e,
            AgentEvent::ToolExecuting(calls) if calls.iter().any(|c| c.name == "glob")
        )
    });
    assert!(used_glob, "Should have used glob tool");

    // Response should mention the txt files
    let response_lower = result.response.to_lowercase();
    assert!(
        response_lower.contains("file1")
            || response_lower.contains("file2")
            || response_lower.contains("txt")
            || response_lower.contains("2 file"),
        "Response should mention the txt files: {}",
        result.response
    );

    reset_plan_manager();
}

/// Test that exiting plan mode when not in plan mode returns error.
#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_exit_plan_mode_when_not_in_plan_mode() {
    init_test_logging();
    reset_plan_manager();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Make sure we're NOT in plan mode
    reset_plan_manager();

    // Ask to exit plan mode when not in it
    let (result, events) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(&client, &tool_service, "Exit plan mode now.", None),
    )
    .await;

    // Should have used exit_plan_mode tool
    let used_exit_plan = events.iter().any(|e| {
        matches!(e,
            AgentEvent::ToolExecuting(calls) if calls.iter().any(|c| c.name == "exit_plan_mode")
        )
    });

    if used_exit_plan {
        // Response should mention not being in plan mode
        let response_lower = result.response.to_lowercase();
        assert!(
            response_lower.contains("not")
                || response_lower.contains("error")
                || response_lower.contains("already")
                || response_lower.contains("cannot"),
            "Response should mention not being in plan mode: {}",
            result.response
        );
    }
    // If model didn't use the tool, it might have just explained that we're not in plan mode
}

/// Test full plan mode workflow: enter -> read -> exit.
#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_full_plan_mode_workflow() {
    init_test_logging();
    reset_plan_manager();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a test file
    let test_file = temp_dir.path().join("code.rs");
    fs::write(&test_file, "fn main() { println!(\"Hello\"); }").unwrap();

    // Turn 1: Enter plan mode
    let (result1, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Enter plan mode to explore this codebase.",
            None,
        ),
    )
    .await;

    // Verify we're in plan mode
    let in_plan_mode_1 = PLAN_MANAGER
        .read()
        .map(|m| m.is_in_plan_mode())
        .unwrap_or(false);
    assert!(in_plan_mode_1, "Should be in plan mode after turn 1");

    // Turn 2: Read a file (should work)
    let (result2, events2) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!("Read {} to understand what it does.", test_file.display()),
            result1.id.as_deref(),
        ),
    )
    .await;

    // Should have read the file
    let used_read = events2.iter().any(|e| {
        matches!(e,
            AgentEvent::ToolExecuting(calls) if calls.iter().any(|c| c.name == "read")
        )
    });
    assert!(used_read, "Should have used read tool in plan mode");

    // Response should mention the code
    assert!(
        result2.response.contains("main")
            || result2.response.contains("println")
            || result2.response.contains("Hello"),
        "Response should mention code content: {}",
        result2.response
    );

    // Turn 3: Exit plan mode
    let (_, events3) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "I've reviewed the code. Exit plan mode now.",
            result2.id.as_deref(),
        ),
    )
    .await;

    // Should have exited plan mode
    let used_exit = events3.iter().any(|e| {
        matches!(e,
            AgentEvent::ToolExecuting(calls) if calls.iter().any(|c| c.name == "exit_plan_mode")
        )
    });
    assert!(used_exit, "Should have used exit_plan_mode tool");

    // Verify no longer in plan mode
    let in_plan_mode_3 = PLAN_MANAGER
        .read()
        .map(|m| m.is_in_plan_mode())
        .unwrap_or(false);
    assert!(!in_plan_mode_3, "Should not be in plan mode after exit");

    reset_plan_manager();
}

/// Test that entering plan mode twice fails.
#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_enter_plan_mode_twice_fails() {
    init_test_logging();
    reset_plan_manager();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Enter plan mode manually
    {
        let mut manager = PLAN_MANAGER.write().unwrap();
        manager.enter_plan_mode(None).unwrap();
    }

    // Try to enter plan mode again via tool
    let (result, events) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(&client, &tool_service, "Enter plan mode.", None),
    )
    .await;

    // Should have attempted enter_plan_mode
    let used_enter = events.iter().any(|e| {
        matches!(e,
            AgentEvent::ToolExecuting(calls) if calls.iter().any(|c| c.name == "enter_plan_mode")
        )
    });

    if used_enter {
        // Response should mention already being in plan mode
        let response_lower = result.response.to_lowercase();
        assert!(
            response_lower.contains("already")
                || response_lower.contains("error")
                || response_lower.contains("cannot"),
            "Response should mention already in plan mode: {}",
            result.response
        );
    }

    reset_plan_manager();
}

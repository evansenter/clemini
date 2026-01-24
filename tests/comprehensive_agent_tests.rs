//! Comprehensive agent E2E tests for complex features.
//!
//! These tests cover advanced agent capabilities:
//! - Multi-turn conversation with state preservation
//! - Background task management (spawn, check, kill)
//! - Task output retrieval
//! - Complex multi-step workflows
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test comprehensive_agent_tests -- --include-ignored --nocapture
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
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

const TEST_MODEL: &str = "gemini-3-flash-preview";

const TEST_SYSTEM_PROMPT: &str = r#"You are clemini, a helpful coding assistant.
Execute requested tasks using the available tools.
Be concise but complete in your responses.
When you encounter errors, explain what went wrong and try alternative approaches.
Use `ls` via the `bash` tool or use the `glob` tool to list files.
Do not hallucinate tools that are not in your provided toolset."#;

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

// =============================================================================
// Test 1: Multi-turn conversation resumption
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_multiturn_conversation_resumption() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create initial project structure
    fs::create_dir_all(temp_dir.path().join("src")).unwrap();
    fs::write(
        temp_dir.path().join("src/main.rs"),
        "fn main() {\n    println!(\"Hello\");\n}\n",
    )
    .unwrap();

    // Turn 1: Ask to explore the codebase
    let (result1, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "What files are in this project? List them.",
            None,
        ),
    )
    .await;

    // Turn 2: Ask about context from turn 1
    let (result2, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "What was the main.rs file doing?",
            result1.id.as_deref(),
        ),
    )
    .await;

    // Turn 3: Continue the conversation
    let (result3, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Can you modify it to print 'Goodbye' instead?",
            result2.id.as_deref(),
        ),
    )
    .await;

    // Verify the file was modified based on conversation context
    let contents = fs::read_to_string(temp_dir.path().join("src/main.rs")).unwrap();

    assert!(
        contents.contains("Goodbye"),
        "Model should remember context and modify the file: {}",
        contents
    );

    assert_response_semantic(
        &client,
        "Three-turn conversation: T1=list files, T2=what does main.rs do, T3=change Hello to Goodbye",
        &result3.response,
        "Does the response indicate the model remembered the context from previous turns and made the change?",
    )
    .await;
}

// =============================================================================
// Test 2: Three-turn deep context preservation
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_deep_context_three_turns() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Turn 1: Set up a variable/fact
    let (result1, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Remember this: the secret code is ALPHA-7749. Confirm you've noted it.",
            None,
        ),
    )
    .await;

    // Turn 2: Do something unrelated
    let (result2, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "What is 15 + 27?",
            result1.id.as_deref(),
        ),
    )
    .await;

    // Turn 3: Ask for the remembered fact
    let (result3, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "What was the secret code I told you earlier?",
            result2.id.as_deref(),
        ),
    )
    .await;

    // Model should remember ALPHA-7749
    assert_response_semantic(
        &client,
        "Turn 1 established secret code ALPHA-7749, Turn 2 was unrelated math, Turn 3 asks for the code",
        &result3.response,
        "Does the response correctly recall the secret code ALPHA-7749?",
    )
    .await;
}

// =============================================================================
// Test 3: Background bash command and task output
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_background_bash_and_task_output() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a script that takes some time
    let script_path = temp_dir.path().join("slow_script.sh");
    fs::write(
        &script_path,
        "#!/bin/bash\necho 'Starting...'\nsleep 1\necho 'Done with result: SUCCESS-123'",
    )
    .unwrap();

    // Make script executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();
    }

    // Turn 1: Run bash command in background
    let (result1, events1) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!("Run {} in the background", script_path.display()),
            None,
        ),
    )
    .await;

    // Check that tool was used and task started
    let tool_used = events1
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolExecuting(_)));
    assert!(tool_used, "Should have used a tool to run command");

    // Give script time to complete
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Turn 2: Check task output
    let (result2, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Check if the background task finished and show me its output",
            result1.id.as_deref(),
        ),
    )
    .await;

    // Should show SUCCESS-123 in output
    assert_response_semantic(
        &client,
        "User ran slow_script.sh in background, then asked for output which should contain SUCCESS-123",
        &result2.response,
        "Does the response show the task completed and include SUCCESS-123 in the output?",
    )
    .await;
}

// =============================================================================
// Test 4: Complex multi-file creation and editing
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_multifile_project_creation() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Ask to create a simple multi-file project
    let (result, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Create a simple Python project with three files: \
             1. main.py - imports utils and prints 'Hello from main' \
             2. utils.py - has a function greet(name) that returns 'Hi {name}' \
             3. test_utils.py - tests the greet function",
            None,
        ),
    )
    .await;

    // Verify files were created
    assert!(
        temp_dir.path().join("main.py").exists(),
        "main.py should be created"
    );
    assert!(
        temp_dir.path().join("utils.py").exists(),
        "utils.py should be created"
    );
    assert!(
        temp_dir.path().join("test_utils.py").exists(),
        "test_utils.py should be created"
    );

    // Verify content is reasonable
    let utils_content = fs::read_to_string(temp_dir.path().join("utils.py")).unwrap();
    assert!(
        utils_content.contains("def greet") || utils_content.contains("greet("),
        "utils.py should have greet function: {}",
        utils_content
    );

    assert_response_semantic(
        &client,
        "User asked for three Python files: main.py, utils.py, test_utils.py",
        &result.response,
        "Does the response indicate all three files were successfully created?",
    )
    .await;
}

// =============================================================================
// Test 5: Tool chaining - grep, read, edit workflow
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_tool_chaining_grep_read_edit() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create multiple files with various content
    fs::write(temp_dir.path().join("file1.txt"), "Normal content here").unwrap();
    fs::write(
        temp_dir.path().join("file2.txt"),
        "This file has DEPRECATED code",
    )
    .unwrap();
    fs::write(temp_dir.path().join("file3.txt"), "More normal content").unwrap();

    let (result, events) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Find which file contains 'DEPRECATED', then change it to 'LEGACY' in that file",
            None,
        ),
    )
    .await;

    // Verify multiple tools were used
    let tool_executions: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolExecuting(_)))
        .collect();
    assert!(
        tool_executions.len() >= 2,
        "Should use at least 2 tools (grep/search + edit)"
    );

    // Verify the correct file was modified
    let file2_content = fs::read_to_string(temp_dir.path().join("file2.txt")).unwrap();
    assert!(
        file2_content.contains("LEGACY") && !file2_content.contains("DEPRECATED"),
        "file2.txt should have LEGACY instead of DEPRECATED: {}",
        file2_content
    );

    assert_response_semantic(
        &client,
        "User asked to find DEPRECATED and change to LEGACY. file2.txt had it.",
        &result.response,
        "Does the response indicate the model found DEPRECATED in file2.txt and changed it to LEGACY?",
    )
    .await;
}

// =============================================================================
// Test 6: Error recovery across multiple attempts
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_error_recovery_multiple_attempts() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a file with specific content
    fs::write(
        temp_dir.path().join("data.txt"),
        "line one\nline two\nline three",
    )
    .unwrap();

    // Ask to edit with slightly wrong text (should recover)
    let (result, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "In data.txt, change 'line TWO' to 'LINE TWO' (use exact case I specified)",
            None,
        ),
    )
    .await;

    // The model should recover from case mismatch
    let _contents = fs::read_to_string(temp_dir.path().join("data.txt")).unwrap();

    // Either it changed the actual text or explained why it couldn't
    assert_response_semantic(
        &client,
        "File has 'line two' but user asked for 'line TWO' (case mismatch)",
        &result.response,
        "Does the response indicate the model recovered from the case mismatch and made a reasonable edit, or explained the issue?",
    )
    .await;
}

// =============================================================================
// Test 7: Interactive file exploration (glob + read pattern)
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_file_exploration_glob_read_pattern() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create nested directory structure
    fs::create_dir_all(temp_dir.path().join("src/utils")).unwrap();
    fs::create_dir_all(temp_dir.path().join("tests")).unwrap();

    fs::write(
        temp_dir.path().join("src/main.rs"),
        "fn main() { utils::helper(); }",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("src/utils/mod.rs"),
        "pub fn helper() { println!(\"helper called\"); }",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("tests/test_main.rs"),
        "#[test] fn test_it() { assert!(true); }",
    )
    .unwrap();

    let (result, events) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Find all .rs files in this project and summarize what each one does",
            None,
        ),
    )
    .await;

    // Should use glob to find files and read to examine them
    let tool_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolExecuting(_)))
        .count();
    assert!(
        tool_count >= 3,
        "Should use multiple tools (glob + reads): got {}",
        tool_count
    );

    assert_response_semantic(
        &client,
        "Project has src/main.rs (calls helper), src/utils/mod.rs (defines helper), tests/test_main.rs (test)",
        &result.response,
        "Does the response summarize all three .rs files and their purposes?",
    )
    .await;
}

// =============================================================================
// Test 8: Context window management awareness
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_handles_large_file_content() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a larger file with structured content
    let mut content = String::new();
    for i in 1..=100 {
        content.push_str(&format!("// Function {} of 100\n", i));
        content.push_str(&format!("fn function_{}() -> i32 {{\n", i));
        content.push_str(&format!("    {}\n", i * 10));
        content.push_str("}\n\n");
    }
    fs::write(temp_dir.path().join("large_file.rs"), &content).unwrap();

    let (result, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Read large_file.rs and tell me what function_50 returns",
            None,
        ),
    )
    .await;

    // Model should correctly identify function_50 returns 500
    assert_response_semantic(
        &client,
        "large_file.rs has 100 functions, function_50 returns 500 (50 * 10)",
        &result.response,
        "Does the response correctly identify that function_50 returns 500?",
    )
    .await;
}

// =============================================================================
// Test 9: Write file then verify by reading
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_write_verify_cycle() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Ask to create and verify
    let (result, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Create a file called config.json with {\"version\": 1, \"enabled\": true}, then read it back and confirm the contents",
            None,
        ),
    )
    .await;

    // Verify file exists with correct content
    let contents = fs::read_to_string(temp_dir.path().join("config.json")).unwrap();
    assert!(
        contents.contains("version") && contents.contains("enabled"),
        "config.json should have proper content: {}",
        contents
    );

    assert_response_semantic(
        &client,
        "User asked to create config.json with version:1, enabled:true, then verify",
        &result.response,
        "Does the response indicate the file was created and verified with the correct contents?",
    )
    .await;
}

// =============================================================================
// Test 10: TodoWrite tool usage
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_todo_write_task_tracking() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Ask to plan with todo list
    let (result, events) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "I need to refactor a codebase. Create a todo list with these tasks: \
             1. Read existing code \
             2. Identify patterns \
             3. Write refactored code",
            None,
        ),
    )
    .await;

    // Check that todo_write was used
    let todo_used = events.iter().any(|e| {
        if let AgentEvent::ToolExecuting(calls) = e {
            calls.iter().any(|c| c.name == "todo_write")
        } else {
            false
        }
    });

    assert!(todo_used, "Should use todo_write tool for task list");

    assert_response_semantic(
        &client,
        "User asked to create a todo list with 3 refactoring tasks",
        &result.response,
        "Does the response indicate the todo list was created with the requested tasks?",
    )
    .await;
}

//! Semantic integration tests for clemini.
//!
//! These tests verify that the model correctly interprets tool results,
//! recovers from errors, maintains multi-turn state, and provides
//! appropriate responses.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test semantic_integration_tests -- --include-ignored --nocapture
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
// Test 1: Multi-turn state preservation
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_multiturn_remembers_file_modifications() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create initial file
    let test_file = temp_dir.path().join("config.json");
    fs::write(&test_file, r#"{"version": "1.0", "debug": false}"#).unwrap();

    // Turn 1: Ask to update the version
    let (result1, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!(
                "In {}, change the version from 1.0 to 2.0",
                test_file.display()
            ),
            None,
        ),
    )
    .await;

    // Turn 2: Ask what was changed (should remember from turn 1)
    let (result2, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "What change did you just make to the config file?",
            result1.id.as_deref(),
        ),
    )
    .await;

    // Semantic validation: model should remember it changed version to 2.0
    assert_response_semantic(
        &client,
        "In turn 1, the model changed version from 1.0 to 2.0 in config.json",
        &result2.response,
        "Does the response correctly recall that the version was changed from 1.0 to 2.0?",
    )
    .await;
}

// =============================================================================
// Test 2: Error recovery with edit tool suggestions
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_edit_error_recovery_uses_suggestions() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create file with similar but not exact text
    let test_file = temp_dir.path().join("greeting.txt");
    fs::write(&test_file, "Hello, World!").unwrap();

    // Ask to change text that doesn't exist exactly (typo: "world" vs "World")
    let (result, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!(
                "In {}, change 'Hello, world!' to 'Goodbye, world!' (note: use exact case)",
                test_file.display()
            ),
            None,
        ),
    )
    .await;

    // The model should either:
    // 1. Notice the case difference and fix it
    // 2. Use the similarity suggestions from the error
    // 3. Read the file first to get exact text
    let contents = fs::read_to_string(&test_file).unwrap();

    // Semantic validation: model should have adapted to the actual content
    assert_response_semantic(
        &client,
        &format!(
            "User asked to change 'Hello, world!' but file contained 'Hello, World!' (case difference). Final file contents: {}",
            contents
        ),
        &result.response,
        "Does the response indicate the model understood and handled the case mismatch appropriately?",
    )
    .await;
}

// =============================================================================
// Test 3: Code understanding and analysis
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_code_analysis_accuracy() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a Rust file with a deliberate bug
    let test_file = temp_dir.path().join("buggy.rs");
    fs::write(
        &test_file,
        r#"fn calculate_average(numbers: &[i32]) -> i32 {
    let sum: i32 = numbers.iter().sum();
    sum / numbers.len() as i32  // Bug: doesn't handle empty slice
}

fn main() {
    let nums = vec![10, 20, 30];
    println!("Average: {}", calculate_average(&nums));
}
"#,
    )
    .unwrap();

    let (result, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            &format!(
                "Read {} and identify any bugs or issues in the code",
                test_file.display()
            ),
            None,
        ),
    )
    .await;

    // Semantic validation: model should identify the division by zero risk
    assert_response_semantic(
        &client,
        "Code divides by numbers.len() which could be zero for empty slice, causing panic",
        &result.response,
        "Does the response identify the potential division by zero bug when the slice is empty?",
    )
    .await;
}

// =============================================================================
// Test 4: Multi-tool chaining (grep then edit)
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_grep_then_edit_workflow() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create multiple files, only one needs changing
    fs::write(temp_dir.path().join("main.rs"), "fn main() { app::run(); }").unwrap();
    fs::write(
        temp_dir.path().join("app.rs"),
        "pub fn run() { println!(\"TODO: implement\"); }",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("utils.rs"),
        "pub fn helper() { println!(\"helper\"); }",
    )
    .unwrap();

    let (result, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Find which file contains 'TODO' and replace that TODO comment with 'Application started'",
            None,
        ),
    )
    .await;

    // Verify the correct file was modified
    let app_contents = fs::read_to_string(temp_dir.path().join("app.rs")).unwrap();

    assert_response_semantic(
        &client,
        &format!(
            "User asked to find TODO and replace it. app.rs now contains: {}",
            app_contents
        ),
        &result.response,
        "Does the response indicate the model found the TODO in app.rs and replaced it?",
    )
    .await;

    assert!(
        app_contents.contains("Application started"),
        "app.rs should contain the replacement text"
    );
}

// =============================================================================
// Test 5: Safe vs dangerous command classification
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_command_safety_classification() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();

    // Use MCP mode to enable confirmation flow
    let tool_service = Arc::new(CleminiToolService::new(
        temp_dir.path().to_path_buf(),
        120,
        true, // mcp_mode = true for confirmation
        vec![temp_dir.path().to_path_buf()],
        api_key.clone(),
    ));

    // Create a file
    let test_file = temp_dir.path().join("data.txt");
    fs::write(&test_file, "important data").unwrap();

    // Ask to list files (safe) and delete file (dangerous) in same request
    let (result, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "First list all files in the current directory, then delete data.txt",
            None,
        ),
    )
    .await;

    // File should still exist (not deleted without confirmation)
    assert!(
        test_file.exists(),
        "File should still exist - deletion should require confirmation"
    );

    // When confirmation is triggered, needs_confirmation is set
    // The model may have listed files before hitting the delete confirmation
    if result.needs_confirmation.is_some() {
        // Good - confirmation was requested for the dangerous command
        println!("Confirmation requested as expected");
    } else {
        // If no confirmation needed, the response should explain what happened
        assert_response_semantic(
            &client,
            "User asked to list files (safe) and delete a file (dangerous)",
            &result.response,
            "Does the response indicate that listing worked but deletion requires confirmation or was blocked?",
        )
        .await;
    }
}

// =============================================================================
// Test 6: Code generation with immediate validation
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_code_generation_and_execution() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    let (result, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Write a Python script called fizzbuzz.py that prints FizzBuzz for numbers 1-15, then run it to verify it works",
            None,
        ),
    )
    .await;

    // Verify the file was created
    let script_path = temp_dir.path().join("fizzbuzz.py");
    assert!(script_path.exists(), "fizzbuzz.py should be created");

    // Semantic validation: response should show the output
    assert_response_semantic(
        &client,
        "User asked for FizzBuzz 1-15. Expected output includes: 1, 2, Fizz, 4, Buzz, Fizz, 7, 8, Fizz, Buzz, 11, Fizz, 13, 14, FizzBuzz",
        &result.response,
        "Does the response show the FizzBuzz output with correct Fizz/Buzz/FizzBuzz patterns?",
    )
    .await;
}

// =============================================================================
// Test 7: File not found recovery
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_file_not_found_helpful_response() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create some files but not the one we'll ask for
    fs::write(temp_dir.path().join("config.json"), "{}").unwrap();
    fs::write(temp_dir.path().join("settings.yaml"), "key: value").unwrap();

    // Use extended timeout - model may try multiple approaches before giving up
    let (result, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Read the file config.yaml and show me its contents",
            None,
        ),
    )
    .await;

    // Model should explain file doesn't exist and possibly suggest alternatives
    assert_response_semantic(
        &client,
        "User asked for config.yaml but only config.json and settings.yaml exist",
        &result.response,
        "Does the response explain that config.yaml doesn't exist and suggest the available alternatives (config.json or settings.yaml)?",
    )
    .await;
}

// =============================================================================
// Test 8: Complex refactoring task
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_refactoring_across_files() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create files with a function to rename
    fs::write(
        temp_dir.path().join("lib.rs"),
        r#"pub fn get_user_name() -> String {
    "Alice".to_string()
}
"#,
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("main.rs"),
        r#"mod lib;

fn main() {
    let name = lib::get_user_name();
    println!("Hello, {}", name);
}
"#,
    )
    .unwrap();

    let (result, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Rename the function get_user_name to fetch_username in all files",
            None,
        ),
    )
    .await;

    // Verify both files were updated
    let lib_contents = fs::read_to_string(temp_dir.path().join("lib.rs")).unwrap();
    let main_contents = fs::read_to_string(temp_dir.path().join("main.rs")).unwrap();

    assert!(
        lib_contents.contains("fetch_username"),
        "lib.rs should have renamed function"
    );
    assert!(
        main_contents.contains("fetch_username"),
        "main.rs should have renamed function call"
    );

    assert_response_semantic(
        &client,
        "User asked to rename get_user_name to fetch_username across files",
        &result.response,
        "Does the response indicate both files were updated with the renamed function?",
    )
    .await;
}

// =============================================================================
// Test 9: Understanding structured data
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_json_data_comprehension() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a JSON file with data to analyze
    fs::write(
        temp_dir.path().join("sales.json"),
        r#"{
  "q1": {"revenue": 50000, "expenses": 35000},
  "q2": {"revenue": 62000, "expenses": 41000},
  "q3": {"revenue": 48000, "expenses": 52000},
  "q4": {"revenue": 71000, "expenses": 45000}
}"#,
    )
    .unwrap();

    let (result, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Read sales.json and tell me which quarter had a loss and which had the highest profit",
            None,
        ),
    )
    .await;

    // Q3 had a loss (48000 - 52000 = -4000)
    // Q4 had highest profit (71000 - 45000 = 26000)
    assert_response_semantic(
        &client,
        "Q1: profit 15000, Q2: profit 21000, Q3: LOSS 4000, Q4: profit 26000",
        &result.response,
        "Does the response correctly identify Q3 as having a loss and Q4 as having the highest profit?",
    )
    .await;
}

// =============================================================================
// Test 10: Incremental problem solving
// =============================================================================

#[tokio::test]
#[ignore = "Requires GEMINI_API_KEY"]
async fn test_incremental_debugging() {
    init_test_logging();

    let Some(client) = get_client() else {
        println!("Skipping: GEMINI_API_KEY not set");
        return;
    };

    let api_key = get_api_key().expect("API key required");
    let temp_dir = create_temp_dir();
    let tool_service = create_test_tool_service(&temp_dir, &api_key);

    // Create a script with a bug
    fs::write(
        temp_dir.path().join("calculator.py"),
        r#"def divide(a, b):
    return a / b

result = divide(10, 0)
print(f"Result: {result}")
"#,
    )
    .unwrap();

    // Turn 1: Run the script (will fail)
    let (result1, _) = with_timeout(
        DEFAULT_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Run calculator.py and tell me what happens",
            None,
        ),
    )
    .await;

    // Turn 2: Ask to fix it
    let (result2, _) = with_timeout(
        EXTENDED_TIMEOUT,
        run_test_interaction(
            &client,
            &tool_service,
            "Fix the bug and run it again",
            result1.id.as_deref(),
        ),
    )
    .await;

    // Verify the fix was applied
    let fixed_contents = fs::read_to_string(temp_dir.path().join("calculator.py")).unwrap();

    assert_response_semantic(
        &client,
        "Turn 1 showed division by zero error. Turn 2 should fix it (add zero check or change divisor)",
        &result2.response,
        "Does the response indicate the division by zero bug was fixed and the script now runs successfully?",
    )
    .await;

    // The fix should handle the zero case somehow
    assert!(
        fixed_contents.contains("if")
            || fixed_contents.contains("!= 0")
            || !fixed_contents.contains("divide(10, 0)"),
        "Code should be modified to handle or avoid division by zero"
    );
}

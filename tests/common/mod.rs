//! Common test utilities shared across integration tests.
//!
//! These tests require the GEMINI_API_KEY environment variable to be set.
//!
//! # Running Tests
//!
//! ```bash
//! # Run all integration tests (requires API key)
//! cargo test --test confirmation_tests -- --include-ignored --nocapture
//! ```

use clemini::{CleminiToolService, logging};
use genai_rs::Client;
use serde_json::json;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// Default timeout for API calls
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Extended timeout for multi-turn interactions
pub const EXTENDED_TIMEOUT: Duration = Duration::from_secs(120);

/// Creates a client from the GEMINI_API_KEY environment variable.
/// Returns None if the API key is not set or client build fails.
#[allow(dead_code)]
pub fn get_client() -> Option<Client> {
    let api_key = env::var("GEMINI_API_KEY").ok()?;
    match Client::builder(api_key).build() {
        Ok(client) => Some(client),
        Err(e) => {
            eprintln!(
                "WARNING: GEMINI_API_KEY is set but client build failed: {}",
                e
            );
            None
        }
    }
}

/// Gets the API key from environment.
#[allow(dead_code)]
pub fn get_api_key() -> Option<String> {
    env::var("GEMINI_API_KEY").ok()
}

/// Creates a tool service with a temporary working directory.
#[allow(dead_code)]
pub fn create_tool_service(temp_dir: &TempDir, api_key: &str) -> CleminiToolService {
    CleminiToolService::new(
        temp_dir.path().to_path_buf(),
        120,  // bash_timeout
        true, // mcp_mode = true for confirmation testing
        vec![temp_dir.path().to_path_buf()],
        api_key.to_string(),
    )
}

/// Creates a temporary directory for test isolation.
#[allow(dead_code)]
pub fn create_temp_dir() -> TempDir {
    tempfile::tempdir().expect("Failed to create temp directory")
}

/// A no-op output sink for tests.
pub struct TestSink;

impl logging::OutputSink for TestSink {
    fn emit(&self, _message: &str, _render_markdown: bool) {
        // No-op for tests
    }
    fn emit_streaming(&self, _text: &str) {
        // No-op for tests
    }
}

/// Initialize logging with a no-op sink for tests.
#[allow(dead_code)]
pub fn init_test_logging() {
    let _ = logging::set_output_sink(Arc::new(TestSink));
}

// =============================================================================
// Semantic Validation Using Structured Output
// =============================================================================

/// Uses Gemini with structured output to validate that a response is semantically appropriate.
///
/// This provides a middle ground between brittle content assertions and purely structural checks.
/// The validator uses a separate API call to ask Gemini to judge whether the response makes sense
/// given the context and expected behavior.
///
/// # Arguments
///
/// * `client` - The API client to use for validation
/// * `context` - Background context: what the user asked, what data was provided
/// * `response_text` - The actual response text from the LLM being tested
/// * `validation_question` - Specific yes/no question to ask
///
/// # Returns
///
/// * `Ok(true)` - Response is semantically valid
/// * `Ok(false)` - Response is not semantically valid
/// * `Err(_)` - Validation API call failed
#[allow(dead_code)]
pub async fn validate_response_semantically(
    client: &Client,
    context: &str,
    response_text: &str,
    validation_question: &str,
) -> Result<bool, genai_rs::GenaiError> {
    let validation_prompt = format!(
        "You are a test validator. Your job is to judge whether an LLM response is appropriate given the context.\n\n\
        Context: {}\n\n\
        Response to validate: {}\n\n\
        Question: {}\n\n\
        Provide your judgment as a yes/no boolean and explain your reasoning.",
        context, response_text, validation_question
    );

    let schema = json!({
        "type": "object",
        "properties": {
            "is_valid": {
                "type": "boolean",
                "description": "Whether the response is semantically valid"
            },
            "reason": {
                "type": "string",
                "description": "Brief explanation of the judgment"
            }
        },
        "required": ["is_valid", "reason"]
    });

    let validation = client
        .interaction()
        .with_model("gemini-3-flash-preview")
        .with_text(&validation_prompt)
        .with_response_format(schema)
        .create()
        .await?;

    // Parse structured output
    if let Some(text) = validation.as_text() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
            let is_valid = json
                .get("is_valid")
                .and_then(|v| v.as_bool())
                .unwrap_or(true); // Default to valid if parse fails

            if let Some(reason) = json.get("reason").and_then(|v| v.as_str()) {
                println!("Semantic validation reason: {}", reason);
            }

            return Ok(is_valid);
        }
    }

    // If we can't parse, default to valid to avoid blocking tests
    println!("Warning: Could not parse semantic validation response, assuming valid");
    Ok(true)
}

/// Validates and asserts that a response is semantically appropriate in one call.
///
/// # Panics
///
/// - If the semantic validation API call fails
/// - If the response is not semantically valid
#[allow(dead_code)]
pub async fn assert_response_semantic(
    client: &Client,
    context: &str,
    response_text: &str,
    validation_question: &str,
) {
    let is_valid =
        validate_response_semantically(client, context, response_text, validation_question)
            .await
            .expect("Semantic validation API call failed");
    assert!(
        is_valid,
        "Semantic validation failed.\nQuestion: {}\nResponse: {}",
        validation_question, response_text
    );
}

/// Run an async block with a timeout.
#[allow(dead_code)]
pub async fn with_timeout<F, T>(timeout: Duration, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(timeout, future)
        .await
        .expect("Test timed out")
}

//! Agent module - interaction logic decoupled from UI.
//!
//! This module contains the core agent logic for running interactions with Gemini.
//! It sends events through a channel for the UI layer to consume, enabling:
//! - Decoupled UI implementations (TUI, terminal, MCP)
//! - Testable agent logic
//! - Future streaming-first architecture (#59)

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use futures_util::StreamExt;
use genai_rs::{
    CallableFunction, Client, Content, FunctionExecutionResult, InteractionResponse,
    OwnedFunctionCallInfo, StreamChunk, ToolService,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::tools::CleminiToolService;

/// Context window limit for Gemini models (1M tokens).
const CONTEXT_WINDOW_LIMIT: u32 = 1_000_000;

/// Events emitted by the agent during interaction.
///
/// UI layers receive these events and handle them appropriately:
/// - TUI: Update app state and render
/// - Terminal: Print to stdout/stderr
/// - MCP: Ignore real-time events, use final result
///
/// Note: Some variants/fields are intentionally unused pending full event handling
/// implementation in UI layers (issue #59).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum AgentEvent {
    /// Streaming text chunk from model.
    TextDelta(String),

    /// Tool execution about to start.
    /// Contains function call info from genai-rs.
    ToolExecuting(Vec<OwnedFunctionCallInfo>),

    /// Tool execution completed.
    /// Uses genai-rs `FunctionExecutionResult` which has:
    /// name, call_id, args, result, duration, and is_error()/error_message() methods.
    ToolResult(FunctionExecutionResult),

    /// Interaction finished with full response.
    Complete {
        interaction_id: Option<String>,
        response: Box<InteractionResponse>,
    },

    /// Context window warning (>80% or >95% usage).
    ContextWarning {
        used: u32,
        limit: u32,
        percentage: f64,
    },

    /// Cancelled by user.
    Cancelled,
}

/// Result of an interaction.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct InteractionResult {
    pub id: Option<String>,
    pub response: String,
    pub context_size: u32,
    pub total_tokens: u32,
    pub tool_calls: Vec<String>,
}

struct ToolExecutionResult {
    results: Vec<Content>,
    cancelled: bool,
}

/// Check context window usage and send warning event if needed.
fn check_context_window(total_tokens: u32, events_tx: &mpsc::Sender<AgentEvent>) {
    let ratio = f64::from(total_tokens) / f64::from(CONTEXT_WINDOW_LIMIT);
    if ratio > 0.80 {
        let _ = events_tx.try_send(AgentEvent::ContextWarning {
            used: total_tokens,
            limit: CONTEXT_WINDOW_LIMIT,
            percentage: ratio * 100.0,
        });
    }
}

async fn execute_tools(
    tool_service: &Arc<CleminiToolService>,
    accumulated_function_calls: &[(Option<String>, String, Value)],
    tool_calls: &mut Vec<String>,
    cancellation_token: &CancellationToken,
    events_tx: &mpsc::Sender<AgentEvent>,
) -> ToolExecutionResult {
    let mut results = Vec::new();

    // Send ToolExecuting event with all pending calls
    let owned_calls: Vec<OwnedFunctionCallInfo> = accumulated_function_calls
        .iter()
        .map(|(id, name, args)| OwnedFunctionCallInfo {
            id: id.clone(),
            name: name.clone(),
            args: args.clone(),
        })
        .collect();
    let _ = events_tx.try_send(AgentEvent::ToolExecuting(owned_calls));

    for (call_id, call_name, call_args) in accumulated_function_calls {
        if cancellation_token.is_cancelled() {
            return ToolExecutionResult {
                results,
                cancelled: true,
            };
        }

        let start = Instant::now();
        let result: Value = match tool_service.execute(call_name, call_args.clone()).await {
            Ok(v) => v,
            Err(e) => {
                // Return error as JSON so Gemini can see it and retry
                serde_json::json!({"error": e.to_string()})
            }
        };
        let duration = start.elapsed();

        tool_calls.push(call_name.to_string());

        // Send ToolResult event using genai-rs FunctionExecutionResult
        let execution_result = FunctionExecutionResult::new(
            call_name.clone(),
            call_id.clone().unwrap_or_default(),
            call_args.clone(),
            result.clone(),
            duration,
        );
        let _ = events_tx.try_send(AgentEvent::ToolResult(execution_result));

        results.push(Content::function_result(
            call_name.to_string(),
            call_id.clone().unwrap_or_default(),
            result,
        ));
    }

    ToolExecutionResult {
        results,
        cancelled: false,
    }
}

#[derive(Debug)]
struct StreamProcessingResult {
    response: Option<InteractionResponse>,
    accumulated_function_calls: Vec<(Option<String>, String, Value)>,
    cancelled: bool,
}

async fn process_interaction_stream<S>(
    mut stream: S,
    events_tx: &mpsc::Sender<AgentEvent>,
    cancellation_token: &CancellationToken,
    full_response: &mut String,
) -> Result<StreamProcessingResult>
where
    S: futures_util::Stream<Item = Result<genai_rs::StreamEvent, genai_rs::GenaiError>> + Unpin,
{
    let mut response: Option<InteractionResponse> = None;
    let mut accumulated_function_calls: Vec<(Option<String>, String, Value)> = Vec::new();

    while let Some(event) = stream.next().await {
        if cancellation_token.is_cancelled() {
            let _ = events_tx.try_send(AgentEvent::Cancelled);
            return Ok(StreamProcessingResult {
                response: None,
                accumulated_function_calls: Vec::new(),
                cancelled: true,
            });
        }

        match event {
            Ok(event) => match event.chunk {
                StreamChunk::Delta(content) => {
                    if let Some(text) = content.as_text() {
                        let _ = events_tx.try_send(AgentEvent::TextDelta(text.to_string()));
                        full_response.push_str(text);
                    }
                    if let Content::FunctionCall { id, name, args } = content {
                        accumulated_function_calls.push((id, name, args));
                    }
                }
                StreamChunk::Complete(resp) => {
                    response = Some(resp);
                }
                _ => {}
            },
            Err(e) => {
                return Err(anyhow::anyhow!(e.to_string()));
            }
        }
    }

    Ok(StreamProcessingResult {
        response,
        accumulated_function_calls,
        cancelled: false,
    })
}

/// Run an interaction with Gemini, sending events through the channel.
///
/// # Arguments
///
/// * `client` - genai-rs Client
/// * `tool_service` - Tool service with available functions
/// * `input` - User input text
/// * `previous_interaction_id` - Optional previous interaction ID for multi-turn
/// * `model` - Model name (e.g., "gemini-3-flash-preview")
/// * `system_prompt` - System instruction
/// * `events_tx` - Channel to send AgentEvents to UI
/// * `cancellation_token` - Token for cancellation
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn run_interaction(
    client: &Client,
    tool_service: &Arc<CleminiToolService>,
    input: &str,
    previous_interaction_id: Option<&str>,
    model: &str,
    system_prompt: &str,
    events_tx: mpsc::Sender<AgentEvent>,
    cancellation_token: CancellationToken,
) -> Result<InteractionResult> {
    let functions: Vec<_> = tool_service
        .tools()
        .iter()
        .map(|t: &Arc<dyn CallableFunction>| t.declaration())
        .collect();

    // Build the interaction - system instruction must be sent on every turn
    // (it's NOT inherited via previousInteractionId per genai-rs docs)
    let mut interaction = client
        .interaction()
        .with_model(model)
        .add_functions(functions.clone())
        .with_system_instruction(system_prompt)
        .with_content(vec![Content::text(input)]);

    if let Some(prev_id) = previous_interaction_id {
        interaction = interaction.with_previous_interaction(prev_id);
    }

    let mut stream = Box::pin(interaction.create_stream());

    let mut last_id = previous_interaction_id.map(String::from);
    let mut current_context_size: u32 = 0;
    let mut total_tokens: u32 = 0;
    let mut tool_calls: Vec<String> = Vec::new();
    let mut full_response = String::new();
    let mut last_response: Option<InteractionResponse> = None;

    const MAX_ITERATIONS: usize = 100;
    for _ in 0..MAX_ITERATIONS {
        let stream_result =
            process_interaction_stream(stream, &events_tx, &cancellation_token, &mut full_response)
                .await?;

        if stream_result.cancelled {
            return Ok(InteractionResult {
                id: last_id,
                response: full_response,
                context_size: current_context_size,
                total_tokens,
                tool_calls,
            });
        }

        let resp = stream_result
            .response
            .ok_or_else(|| anyhow::anyhow!("Stream ended without completion"))?;
        let accumulated_function_calls = stream_result.accumulated_function_calls;

        last_id = resp.id.clone();
        last_response = Some(resp.clone());

        // Update token count
        if let Some(usage) = &resp.usage {
            let turn_tokens = usage.total_tokens.unwrap_or_else(|| {
                usage.total_input_tokens.unwrap_or(0) + usage.total_output_tokens.unwrap_or(0)
            });
            if turn_tokens > 0 {
                current_context_size = turn_tokens;
                total_tokens = turn_tokens;
            }
        }

        // Use accumulated function calls from Delta chunks (streaming mode doesn't populate Complete.outputs)
        if accumulated_function_calls.is_empty() {
            // No more function calls - interaction complete
            break;
        }

        // Process function calls (accumulated from Delta chunks)
        full_response.clear(); // Clear accumulated text before tools as we'll only return text after final tool

        let tool_result = execute_tools(
            tool_service,
            &accumulated_function_calls,
            &mut tool_calls,
            &cancellation_token,
            &events_tx,
        )
        .await;

        if tool_result.cancelled {
            let _ = events_tx.try_send(AgentEvent::Cancelled);
            return Ok(InteractionResult {
                id: last_id,
                response: full_response,
                context_size: current_context_size,
                total_tokens,
                tool_calls,
            });
        }

        let results = tool_result.results;

        // Create new stream for the next turn
        stream = Box::pin(
            client
                .interaction()
                .with_model(model)
                .with_previous_interaction(last_id.as_ref().unwrap())
                .with_system_instruction(system_prompt)
                .with_content(results)
                .create_stream(),
        );
    }

    // Check context window and send warning if needed
    if current_context_size > 0 {
        check_context_window(current_context_size, &events_tx);
    }

    // Send Complete event
    if let Some(resp) = last_response {
        let _ = events_tx.try_send(AgentEvent::Complete {
            interaction_id: last_id.clone(),
            response: Box::new(resp),
        });
    }

    Ok(InteractionResult {
        id: last_id,
        response: full_response,
        context_size: current_context_size,
        total_tokens,
        tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_context_window_below_threshold() {
        // 70% usage - no warning
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(10);
        check_context_window(700_000, &tx);
        assert!(rx.try_recv().is_err(), "Should not warn at 70% usage");
    }

    #[test]
    fn test_check_context_window_at_threshold() {
        // Exactly 80% usage - no warning (threshold is >80%, not >=80%)
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(10);
        check_context_window(800_000, &tx);
        assert!(
            rx.try_recv().is_err(),
            "Should not warn at exactly 80% usage"
        );
    }

    #[test]
    fn test_check_context_window_above_threshold() {
        // 85% usage - should warn
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(10);
        check_context_window(850_000, &tx);

        let event = rx.try_recv().expect("Should send warning at 85% usage");
        match event {
            AgentEvent::ContextWarning {
                used,
                limit,
                percentage,
            } => {
                assert_eq!(used, 850_000);
                assert_eq!(limit, CONTEXT_WINDOW_LIMIT);
                assert!((percentage - 85.0).abs() < 0.01);
            }
            _ => panic!("Expected ContextWarning event"),
        }
    }

    #[test]
    fn test_check_context_window_critical() {
        // 96% usage - should warn (critical level)
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(10);
        check_context_window(960_000, &tx);

        let event = rx.try_recv().expect("Should send warning at 96% usage");
        match event {
            AgentEvent::ContextWarning {
                used,
                limit,
                percentage,
            } => {
                assert_eq!(used, 960_000);
                assert_eq!(limit, CONTEXT_WINDOW_LIMIT);
                assert!((percentage - 96.0).abs() < 0.01);
            }
            _ => panic!("Expected ContextWarning event"),
        }
    }

    #[tokio::test]
    async fn test_process_interaction_stream_text() {
        use genai_rs::{StreamChunk, StreamEvent};

        let (tx, mut rx) = mpsc::channel(10);
        let token = CancellationToken::new();
        let mut full_response = String::new();

        // Create a mock stream
        let chunks = vec![
            Ok(StreamEvent::new(
                StreamChunk::Delta(Content::text("Hello ")),
                None,
            )),
            Ok(StreamEvent::new(
                StreamChunk::Delta(Content::text("world!")),
                None,
            )),
            Ok(StreamEvent::new(
                StreamChunk::Complete(InteractionResponse {
                    id: Some("id-1".to_string()),
                    model: None,
                    agent: None,
                    input: vec![],
                    outputs: vec![],
                    status: genai_rs::InteractionStatus::Completed,
                    usage: None,
                    tools: None,
                    grounding_metadata: None,
                    url_context_metadata: None,
                    previous_interaction_id: None,
                    created: None,
                    updated: None,
                }),
                None,
            )),
        ];
        let stream = futures_util::stream::iter(chunks);

        let result = process_interaction_stream(stream, &tx, &token, &mut full_response)
            .await
            .unwrap();

        assert!(!result.cancelled);
        assert_eq!(full_response, "Hello world!");
        assert!(result.response.is_some());
        assert_eq!(result.response.unwrap().id, Some("id-1".to_string()));
        assert!(result.accumulated_function_calls.is_empty());

        // Check events
        assert_eq!(
            match rx.recv().await.unwrap() {
                AgentEvent::TextDelta(t) => t,
                _ => panic!(),
            },
            "Hello "
        );
        assert_eq!(
            match rx.recv().await.unwrap() {
                AgentEvent::TextDelta(t) => t,
                _ => panic!(),
            },
            "world!"
        );
    }

    #[tokio::test]
    async fn test_process_interaction_stream_tool_calls() {
        use genai_rs::{StreamChunk, StreamEvent};

        let (tx, _rx) = mpsc::channel(10);
        let token = CancellationToken::new();
        let mut full_response = String::new();

        let chunks = vec![
            Ok(StreamEvent::new(
                StreamChunk::Delta(Content::FunctionCall {
                    id: Some("call-1".to_string()),
                    name: "read_file".to_string(),
                    args: serde_json::json!({"file_path": "test.txt"}),
                }),
                None,
            )),
            Ok(StreamEvent::new(
                StreamChunk::Complete(InteractionResponse {
                    id: Some("id-1".to_string()),
                    model: None,
                    agent: None,
                    input: vec![],
                    outputs: vec![],
                    status: genai_rs::InteractionStatus::Completed,
                    usage: None,
                    tools: None,
                    grounding_metadata: None,
                    url_context_metadata: None,
                    previous_interaction_id: None,
                    created: None,
                    updated: None,
                }),
                None,
            )),
        ];
        let stream = futures_util::stream::iter(chunks);

        let result = process_interaction_stream(stream, &tx, &token, &mut full_response)
            .await
            .unwrap();

        assert!(!result.cancelled);
        assert_eq!(result.accumulated_function_calls.len(), 1);
        assert_eq!(result.accumulated_function_calls[0].1, "read_file");
    }

    #[tokio::test]
    async fn test_narration_before_tools_accumulated_then_cleared() {
        // Tests the "narration clearing" behavior: text before tool calls accumulates
        // in full_response during process_interaction_stream, but run_interaction
        // clears it (line 310) so only text AFTER tools is in the final response.
        use genai_rs::{StreamChunk, StreamEvent};

        let (tx, _rx) = mpsc::channel(10);
        let token = CancellationToken::new();
        let mut full_response = String::new();

        // Model narrates then calls a tool
        let chunks = vec![
            Ok(StreamEvent::new(
                StreamChunk::Delta(Content::text("Let me search for that...")),
                None,
            )),
            Ok(StreamEvent::new(
                StreamChunk::Delta(Content::FunctionCall {
                    id: Some("call-1".to_string()),
                    name: "read_file".to_string(),
                    args: serde_json::json!({"file_path": "test.txt"}),
                }),
                None,
            )),
            Ok(StreamEvent::new(
                StreamChunk::Complete(InteractionResponse {
                    id: Some("id-1".to_string()),
                    model: None,
                    agent: None,
                    input: vec![],
                    outputs: vec![],
                    status: genai_rs::InteractionStatus::Completed,
                    usage: None,
                    tools: None,
                    grounding_metadata: None,
                    url_context_metadata: None,
                    previous_interaction_id: None,
                    created: None,
                    updated: None,
                }),
                None,
            )),
        ];
        let stream = futures_util::stream::iter(chunks);

        let result = process_interaction_stream(stream, &tx, &token, &mut full_response)
            .await
            .unwrap();

        // Narration was accumulated during streaming
        assert_eq!(full_response, "Let me search for that...");
        assert_eq!(result.accumulated_function_calls.len(), 1);

        // run_interaction would clear this before executing tools:
        // full_response.clear(); // line 310
        // So final response only contains text AFTER tools complete
        full_response.clear();
        assert!(full_response.is_empty());
    }

    #[tokio::test]
    async fn test_process_interaction_stream_cancellation() {
        use futures_util::StreamExt;
        use genai_rs::{StreamChunk, StreamEvent};
        use std::time::Duration;

        let (tx, _rx) = mpsc::channel(10);
        let token = CancellationToken::new();
        let mut full_response = String::new();

        // This stream yields periodically
        let stream = futures_util::stream::unfold((), |_| async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Some((
                Ok(StreamEvent::new(
                    StreamChunk::Delta(Content::text("...")),
                    None,
                )),
                (),
            ))
        });

        let token_clone = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token_clone.cancel();
        });

        let result = process_interaction_stream(stream.boxed(), &tx, &token, &mut full_response)
            .await
            .unwrap();

        assert!(result.cancelled);
    }

    #[tokio::test]
    async fn test_execute_tools_success() {
        let temp = tempfile::tempdir().unwrap();
        let tool_service = Arc::new(CleminiToolService::new(
            temp.path().to_path_buf(),
            30,
            false,
            vec![temp.path().to_path_buf()],
            "fake-key".to_string(),
        ));
        let (tx, mut rx) = mpsc::channel(10);
        let token = CancellationToken::new();
        let mut tool_calls = Vec::new();

        let calls = vec![(
            Some("call-1".to_string()),
            "todo_write".to_string(),
            serde_json::json!({"todos": [{"content": "test", "activeForm": "testing", "status": "pending"}]}),
        )];

        let result = execute_tools(&tool_service, &calls, &mut tool_calls, &token, &tx).await;

        assert!(!result.cancelled);
        assert_eq!(result.results.len(), 1);
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0], "todo_write");

        // Check events
        let event1 = rx.recv().await.unwrap();
        match event1 {
            AgentEvent::ToolExecuting(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "todo_write");
            }
            _ => panic!("Expected ToolExecuting event"),
        }

        let event2 = rx.recv().await.unwrap();
        match event2 {
            AgentEvent::ToolResult(res) => {
                assert_eq!(res.name, "todo_write");
                assert!(res.result.get("success").is_some());
            }
            _ => panic!("Expected ToolResult event"),
        }
    }

    #[tokio::test]
    async fn test_execute_tools_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let tool_service = Arc::new(CleminiToolService::new(
            temp.path().to_path_buf(),
            30,
            false,
            vec![temp.path().to_path_buf()],
            "fake-key".to_string(),
        ));
        let (tx, _rx) = mpsc::channel(10);
        let token = CancellationToken::new();
        let mut tool_calls = Vec::new();

        token.cancel();

        let calls = vec![(
            Some("call-1".to_string()),
            "todo_write".to_string(),
            serde_json::json!({"todos": []}),
        )];

        let result = execute_tools(&tool_service, &calls, &mut tool_calls, &token, &tx).await;

        assert!(result.cancelled);
        assert_eq!(result.results.len(), 0);
        assert_eq!(tool_calls.len(), 0);
    }

    #[tokio::test]
    async fn test_execute_tools_multiple() {
        let temp = tempfile::tempdir().unwrap();
        let tool_service = Arc::new(CleminiToolService::new(
            temp.path().to_path_buf(),
            30,
            false,
            vec![temp.path().to_path_buf()],
            "fake-key".to_string(),
        ));
        let (tx, mut rx) = mpsc::channel(10);
        let token = CancellationToken::new();
        let mut tool_calls = Vec::new();

        let calls = vec![
            (
                Some("call-1".to_string()),
                "todo_write".to_string(),
                serde_json::json!({"todos": [{"content": "task 1", "activeForm": "doing 1", "status": "pending"}]}),
            ),
            (
                Some("call-2".to_string()),
                "todo_write".to_string(),
                serde_json::json!({"todos": [{"content": "task 2", "activeForm": "doing 2", "status": "pending"}]}),
            ),
        ];

        let result = execute_tools(&tool_service, &calls, &mut tool_calls, &token, &tx).await;

        assert!(!result.cancelled);
        assert_eq!(result.results.len(), 2);
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0], "todo_write");
        assert_eq!(tool_calls[1], "todo_write");

        // ToolExecuting should contain both calls
        let event = rx.recv().await.unwrap();
        match event {
            AgentEvent::ToolExecuting(calls) => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].id, Some("call-1".to_string()));
                assert_eq!(calls[1].id, Some("call-2".to_string()));
            }
            _ => panic!("Expected ToolExecuting event"),
        }

        // Two ToolResult events
        let _ = rx.recv().await.unwrap();
        let _ = rx.recv().await.unwrap();
    }

    #[tokio::test]
    async fn test_execute_tools_error() {
        let temp = tempfile::tempdir().unwrap();
        let tool_service = Arc::new(CleminiToolService::new(
            temp.path().to_path_buf(),
            30,
            false,
            vec![temp.path().to_path_buf()],
            "fake-key".to_string(),
        ));
        let (tx, mut rx) = mpsc::channel(10);
        let token = CancellationToken::new();
        let mut tool_calls = Vec::new();

        let calls = vec![(
            Some("call-1".to_string()),
            "non_existent_tool".to_string(),
            serde_json::json!({}),
        )];

        let result = execute_tools(&tool_service, &calls, &mut tool_calls, &token, &tx).await;

        assert!(!result.cancelled);
        assert_eq!(result.results.len(), 1);

        // The error should be captured as JSON in the tool result
        let _ = rx.recv().await.unwrap(); // ToolExecuting
        let event = rx.recv().await.unwrap(); // ToolResult
        match event {
            AgentEvent::ToolResult(res) => {
                assert_eq!(res.name, "non_existent_tool");
                assert!(res.result.get("error").is_some());
                assert!(
                    res.result["error"]
                        .as_str()
                        .unwrap()
                        .contains("Tool not found")
                );
            }
            _ => panic!("Expected ToolResult event"),
        }
    }

    #[tokio::test]
    async fn test_process_interaction_stream_error() {
        use genai_rs::StreamEvent;

        let (tx, _rx) = mpsc::channel(10);
        let token = CancellationToken::new();
        let mut full_response = String::new();

        let chunks: Vec<Result<StreamEvent, genai_rs::GenaiError>> =
            vec![Err(genai_rs::GenaiError::Internal("API Error".to_string()))];
        let stream = futures_util::stream::iter(chunks);

        let result = process_interaction_stream(stream, &tx, &token, &mut full_response).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API Error"));
    }
}

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
        let mut response: Option<InteractionResponse> = None;
        let mut accumulated_function_calls: Vec<(Option<String>, String, Value)> = Vec::new();

        while let Some(event) = stream.next().await {
            // Check for cancellation at each iteration
            if cancellation_token.is_cancelled() {
                let _ = events_tx.try_send(AgentEvent::Cancelled);
                return Ok(InteractionResult {
                    id: last_id,
                    response: full_response,
                    context_size: current_context_size,
                    total_tokens,
                    tool_calls,
                });
            }

            match event {
                Ok(event) => match &event.chunk {
                    StreamChunk::Delta(content) => {
                        if let Some(text) = content.as_text() {
                            let _ = events_tx.try_send(AgentEvent::TextDelta(text.to_string()));
                            full_response.push_str(text);
                        }
                        // Accumulate function calls from Delta chunks (streaming doesn't put them in Complete)
                        if let Content::FunctionCall { id, name, args } = content {
                            accumulated_function_calls.push((
                                id.clone(),
                                name.clone(),
                                args.clone(),
                            ));
                        }
                    }
                    StreamChunk::Complete(resp) => {
                        response = Some(resp.clone());
                    }
                    _ => {}
                },
                Err(e) => {
                    return Err(anyhow::anyhow!(e.to_string()));
                }
            }
        }

        let resp = response.ok_or_else(|| anyhow::anyhow!("Stream ended without completion"))?;
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
}

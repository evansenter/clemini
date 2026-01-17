//! Agent module - interaction logic decoupled from UI.
//!
//! This module contains the core agent logic for running interactions with Gemini.
//! It sends events through a channel for the UI layer to consume, enabling:
//! - Decoupled UI implementations (TUI, terminal, MCP)
//! - Testable agent logic
//! - Future streaming-first architecture (#59)

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use colored::Colorize;
use futures_util::StreamExt;
use genai_rs::{
    CallableFunction, Client, Content, FunctionExecutionResult, InteractionResponse,
    OwnedFunctionCallInfo, StreamChunk, ToolService,
};
use serde::Serialize;
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

/// Progress update for tool execution (used by TUI for Activity display).
#[derive(Debug, Serialize, Clone)]
pub struct InteractionProgress {
    pub tool: String,
    pub status: String, // "executing" or "completed"
    pub args: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

struct ToolExecutionResult {
    results: Vec<Content>,
    cancelled: bool,
}

/// Format function call arguments for display.
#[allow(dead_code)]
pub fn format_tool_args(args: &Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };

    let mut parts = Vec::new();
    for (k, v) in obj {
        let val_str = match v {
            Value::String(s) => {
                let trimmed = s.replace('\n', " ");
                if trimmed.len() > 80 {
                    format!("\"{}...\"", &trimmed[..77])
                } else {
                    format!("\"{trimmed}\"")
                }
            }
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".to_string(),
            _ => "...".to_string(),
        };
        parts.push(format!("{k}={val_str}"));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("{} ", parts.join(" "))
    }
}

/// Rough token estimate: ~4 chars per token.
#[allow(dead_code)]
pub fn estimate_tokens(value: &Value) -> u32 {
    (value.to_string().len() / 4) as u32
}

/// Format tool result for display.
#[allow(dead_code)]
pub fn format_tool_result(
    name: &str,
    duration: Duration,
    estimated_tokens: u32,
    has_error: bool,
) -> String {
    let error_suffix = if has_error {
        " ERROR".bright_red().bold().to_string()
    } else {
        String::new()
    };
    let elapsed_secs = duration.as_secs_f32();

    let duration_str = if elapsed_secs < 0.001 {
        format!("{:.3}s", elapsed_secs)
    } else {
        format!("{:.2}s", elapsed_secs)
    };

    format!(
        "[{}] {}, ~{} tok{}",
        name.cyan(),
        duration_str.yellow(),
        estimated_tokens,
        error_suffix
    )
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
    progress_fn: &Option<Arc<dyn Fn(InteractionProgress) + Send + Sync>>,
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

        if let Some(cb) = progress_fn {
            cb(InteractionProgress {
                tool: call_name.to_string(),
                status: "executing".to_string(),
                args: call_args.clone(),
                duration_ms: None,
            });
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

        if let Some(cb) = progress_fn {
            cb(InteractionProgress {
                tool: call_name.to_string(),
                status: "completed".to_string(),
                args: call_args.clone(),
                duration_ms: Some(duration.as_millis() as u64),
            });
        }

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
/// * `progress_fn` - Optional callback for TUI Activity updates
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
    progress_fn: Option<Arc<dyn Fn(InteractionProgress) + Send + Sync>>,
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
            &progress_fn,
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
    use serde_json::json;

    #[test]
    fn test_format_tool_args_empty() {
        assert_eq!(format_tool_args(&json!({})), "");
        assert_eq!(format_tool_args(&json!(null)), "");
        assert_eq!(format_tool_args(&json!("not an object")), "");
    }

    #[test]
    fn test_format_tool_args_types() {
        let args = json!({
            "bool": true,
            "num": 42,
            "null": null,
            "str": "hello"
        });
        let formatted = format_tool_args(&args);
        // serde_json::Map is sorted by key
        assert_eq!(formatted, "bool=true null=null num=42 str=\"hello\" ");
    }

    #[test]
    fn test_format_tool_args_complex_types() {
        let args = json!({
            "arr": [1, 2],
            "obj": {"a": 1}
        });
        let formatted = format_tool_args(&args);
        assert_eq!(formatted, "arr=... obj=... ");
    }

    #[test]
    fn test_format_tool_args_truncation() {
        let long_str = "a".repeat(100);
        let args = json!({"long": long_str});
        let formatted = format_tool_args(&args);
        let expected_val = format!("\"{}...\"", "a".repeat(77));
        assert_eq!(formatted, format!("long={} ", expected_val));
    }

    #[test]
    fn test_format_tool_args_newlines() {
        let args = json!({"text": "hello\nworld"});
        let formatted = format_tool_args(&args);
        assert_eq!(formatted, "text=\"hello world\" ");
    }

    #[test]
    fn test_estimate_tokens() {
        // ~4 chars per token
        assert_eq!(estimate_tokens(&json!("hello")), 1); // "hello" = 7 chars / 4 = 1
        assert_eq!(estimate_tokens(&json!({"key": "value"})), 3); // {"key":"value"} = 15 chars / 4 = 3
    }

    #[test]
    fn test_format_tool_result_duration() {
        colored::control::set_override(false);

        // < 1ms (100us) -> 3 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_micros(100), 10, false),
            "[test] 0.000s, ~10 tok"
        );

        // < 1ms (900us) -> 3 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_micros(900), 10, false),
            "[test] 0.001s, ~10 tok"
        );

        // >= 1ms (1.1ms) -> 2 decimals (shows 0.00s due to threshold)
        assert_eq!(
            format_tool_result("test", Duration::from_micros(1100), 10, false),
            "[test] 0.00s, ~10 tok"
        );

        // >= 1ms (20ms) -> 2 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_millis(20), 10, false),
            "[test] 0.02s, ~10 tok"
        );

        // >= 1ms (1450ms) -> 2 decimals
        assert_eq!(
            format_tool_result("test", Duration::from_millis(1450), 10, false),
            "[test] 1.45s, ~10 tok"
        );

        colored::control::unset_override();
    }

    #[test]
    fn test_format_tool_result_error() {
        colored::control::set_override(false);

        let res = format_tool_result("test", Duration::from_millis(10), 25, true);
        assert_eq!(res, "[test] 0.01s, ~25 tok ERROR");

        let res = format_tool_result("test", Duration::from_millis(10), 25, false);
        assert_eq!(res, "[test] 0.01s, ~25 tok");

        colored::control::unset_override();
    }

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

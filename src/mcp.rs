use anyhow::{Result, anyhow};
use axum::{
    Json, Router,
    extract::State,
    response::sse::{Event, Sse},
    routing::{get, post},
};
use colored::Colorize;
use futures_util::stream::Stream;
use genai_rs::{Client, FunctionExecutionResult, OwnedFunctionCallInfo};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tracing::instrument;
// Note: info! macro goes to JSON logs only. For human-readable logs, use crate::logging::log_event()

use crate::agent::{AgentEvent, run_interaction};
use crate::events::{EventHandler, TextBuffer, write_to_streaming_log};
use crate::tools::CleminiToolService;

#[derive(Debug, Deserialize, Clone)]
pub struct JsonRpcRequest {
    #[serde(rename = "jsonrpc", default)]
    pub _jsonrpc: Option<String>,
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    pub id: Option<Value>,
}

pub struct McpServer {
    client: Client,
    tool_service: Arc<CleminiToolService>,
    model: String,
    system_prompt: String,
    notification_tx: broadcast::Sender<String>,
}

/// Extract detail and message body from MCP request params.
fn extract_request_parts(method: &str, params: &Option<Value>) -> (String, Option<String>) {
    let mut detail = String::new();
    let mut msg_body = String::new();
    if method == "tools/call"
        && let Some(params) = params
    {
        if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
            detail.push_str(&format!(" {}", name.purple()));
        }
        if let Some(args) = params.get("arguments") {
            if let Some(interaction_id) = args.get("interaction_id").and_then(|v| v.as_str()) {
                detail.push_str(&format!(
                    " {}={}",
                    "interaction".dimmed(),
                    format!("\"{}\"", interaction_id).yellow()
                ));
            }
            if let Some(msg) = args.get("message").and_then(|v| v.as_str()) {
                for line in msg.lines() {
                    msg_body.push_str(&format!("\n{}", line));
                }
            }
        }
    }
    let body = if msg_body.is_empty() {
        None
    } else {
        Some(msg_body)
    };
    (detail, body)
}

/// Format MCP request log block (IN line + optional body).
/// Spacing between blocks is handled by write_to_log_file.
fn format_mcp_request(method: &str, params: &Option<Value>) -> String {
    let (detail, body) = extract_request_parts(method, params);
    let mut output = format!("{} {}{}", "IN".green(), method.bold(), detail);
    if let Some(msg) = body {
        // Body with background for visibility
        output.push_str(&format!("\n\n{}", msg.trim().on_bright_black()));
    }
    output
}

/// Format complete MCP response log block (OUT line + optional body with spacing).
/// Body is included if non-empty.
fn format_mcp_response(
    method: &str,
    status: &colored::ColoredString,
    detail: &str,
    body: &str,
) -> String {
    let mut output = format!("{} {} ({}){}", "OUT".cyan(), method.bold(), status, detail);
    let trimmed = body.trim();
    if !trimmed.is_empty() {
        output.push_str(&format!("\n\n{}", trimmed));
    }
    output
}

fn format_status(response: &JsonRpcResponse) -> colored::ColoredString {
    if response.error.is_some() {
        "ERROR".red()
    } else {
        "OK".green()
    }
}

/// Create a progress notification for a tool starting execution.
fn create_tool_executing_notification(name: &str, args: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {
            "tool": name,
            "status": "executing",
            "args": args
        }
    })
}

/// Create a progress notification for a completed tool execution.
fn create_tool_result_notification(name: &str, duration_ms: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {
            "tool": name,
            "status": "completed",
            "duration_ms": duration_ms
        }
    })
}

/// MCP-specific event handler that logs events and sends MCP progress notifications.
struct McpEventHandler {
    notification_tx: mpsc::UnboundedSender<String>,
    text_buffer: TextBuffer,
}

impl McpEventHandler {
    fn new(notification_tx: mpsc::UnboundedSender<String>) -> Self {
        Self {
            notification_tx,
            text_buffer: TextBuffer::new(),
        }
    }

    fn send_notification(&self, notification: Value) {
        if let Ok(s) = serde_json::to_string(&notification) {
            let _ = self.notification_tx.send(format!("{}\n", s));
        }
    }
}

impl EventHandler for McpEventHandler {
    fn on_text_delta(&mut self, text: &str) {
        self.text_buffer.push(text);
    }

    fn on_tool_executing(&mut self, call: &OwnedFunctionCallInfo) {
        // Flush buffer before tool output (normalizes to \n\n for spacing)
        // Logging is handled by dispatch_event() after this method returns
        if let Some(rendered) = self.text_buffer.flush() {
            write_to_streaming_log(&rendered);
        }
        // Send MCP notification
        self.send_notification(create_tool_executing_notification(&call.name, &call.args));
    }

    fn on_tool_result(&mut self, result: &FunctionExecutionResult) {
        // Logging is handled by dispatch_event() after this method returns
        // Send MCP notification
        self.send_notification(create_tool_result_notification(
            &result.name,
            result.duration.as_millis() as u64,
        ));
    }

    fn on_context_warning(&mut self, _warning: &crate::agent::ContextWarning) {
        // Logging is handled by dispatch_event() after this method returns
    }

    fn on_complete(
        &mut self,
        _interaction_id: Option<&str>,
        _response: &genai_rs::InteractionResponse,
    ) {
        // Flush any remaining buffered text (normalizes to \n\n)
        if let Some(rendered) = self.text_buffer.flush() {
            write_to_streaming_log(&rendered);
        }
    }
}

#[instrument(skip(server, request))]
async fn handle_post(
    State(server): State<Arc<McpServer>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    crate::logging::log_event(&format_mcp_request(&request.method, &request.params));

    // For HTTP, we use the server's broadcast channel for notifications
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let notification_tx = server.notification_tx.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let _ = notification_tx.send(msg);
        }
    });

    let cancellation_token = CancellationToken::new();
    match server
        .handle_request(request.clone(), tx, cancellation_token)
        .await
    {
        Ok(response) => {
            crate::logging::log_event(&format_mcp_response(
                &request.method,
                &format_status(&response),
                "",
                "",
            ));
            Json(response)
        }
        Err(e) => Json(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(json!({"code": -32603, "message": format!("{}", e)})),
            id: request.id,
        }),
    }
}

async fn handle_sse(
    State(server): State<Arc<McpServer>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = server.notification_tx.subscribe();

    let stream = async_stream::stream! {
        while let Ok(msg) = rx.recv().await {
            yield Ok(Event::default().data(msg));
        }
    };

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

impl McpServer {
    pub fn new(
        client: Client,
        tool_service: Arc<CleminiToolService>,
        model: String,
        system_prompt: String,
    ) -> Self {
        let (notification_tx, _) = broadcast::channel(100);
        Self {
            client,
            tool_service,
            model,
            system_prompt,
            notification_tx,
        }
    }

    #[instrument(skip(self))]
    pub async fn run_http(self: Arc<Self>, port: u16) -> Result<()> {
        crate::logging::log_event(&format!(
            "MCP HTTP server starting on {} ({} enable multi-turn conversations)",
            format!("http://0.0.0.0:{}", port).cyan(),
            "interaction IDs".cyan()
        ));

        let app = Router::new()
            .route("/", post(handle_post))
            .route("/sse", get(handle_sse))
            .layer(CorsLayer::permissive())
            .with_state(self);

        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn run_stdio(self: Arc<Self>) -> Result<()> {
        crate::logging::log_event(&format!(
            "MCP server starting ({} enable multi-turn conversations)",
            "interaction IDs".cyan()
        ));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        // Bridge broadcast to this stdio session
        let mut bcast_rx = self.notification_tx.subscribe();
        let tx_for_bcast = tx.clone();
        tokio::spawn(async move {
            while let Ok(msg) = bcast_rx.recv().await {
                let _ = tx_for_bcast.send(msg);
            }
        });

        // Spawn a dedicated writer task for stdout to handle concurrent notifications
        tokio::spawn(async move {
            let mut stdout = io::stdout();
            while let Some(msg) = rx.recv().await {
                if stdout.write_all(msg.as_bytes()).await.is_err() {
                    break;
                }
                if stdout.flush().await.is_err() {
                    break;
                }
            }
        });

        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin).lines();
        let mut current_task: Option<(tokio::task::JoinHandle<()>, CancellationToken)> = None;

        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            let request: JsonRpcRequest = match serde_json::from_str::<JsonRpcRequest>(&line) {
                Ok(req) => {
                    crate::logging::log_event(&format_mcp_request(&req.method, &req.params));
                    req
                }
                Err(e) => {
                    let error_resp = JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        result: None,
                        error: Some(json!({
                            "code": -32700,
                            "message": format!("Parse error: {}", e)
                        })),
                        id: None,
                    };
                    let _ = tx.send(format!("{}\n", serde_json::to_string(&error_resp)?));
                    continue;
                }
            };

            // JSON-RPC notifications don't get responses
            if request.method.starts_with("notifications/") {
                if request.method == "notifications/cancelled"
                    && let Some((handle, token)) = current_task.take()
                {
                    token.cancel();
                    handle.abort();
                    crate::logging::log_event(&format!(
                        "{} task cancelled by client",
                        "ABORTED".red()
                    ));
                }
                continue;
            }

            // Spawn tools/call requests concurrently so the main loop stays responsive
            if request.method == "tools/call" {
                let self_clone = Arc::clone(&self);
                let tx_clone = tx.clone();
                let request_clone = request.clone();
                let cancellation_token = CancellationToken::new();
                let ct_clone = cancellation_token.clone();
                let handle = tokio::spawn(async move {
                    let response = match self_clone
                        .handle_request(request_clone.clone(), tx_clone.clone(), ct_clone)
                        .await
                    {
                        Ok(r) => r,
                        Err(e) => JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            result: None,
                            error: Some(json!({"code": -32603, "message": format!("{}", e)})),
                            id: request_clone.id.clone(),
                        },
                    };
                    let mut detail = String::new();
                    let mut resp_body = String::new();
                    if let Some(res) = &response.result {
                        if let Some(interaction_id) =
                            res.get("interaction_id").and_then(|v| v.as_str())
                        {
                            detail.push_str(&format!(
                                " {}={}",
                                "interaction".dimmed(),
                                format!("\"{}\"", interaction_id).yellow()
                            ));
                        }
                        if let Some(content) = res.get("content").and_then(|v| v.as_array())
                            && let Some(text) = content
                                .first()
                                .and_then(|v| v.get("text"))
                                .and_then(|v| v.as_str())
                        {
                            // For clemini_chat, avoid duplicating streamed text
                            let tool_name = request_clone
                                .params
                                .as_ref()
                                .and_then(|p| p.get("name"))
                                .and_then(|v| v.as_str());
                            if tool_name == Some("clemini_chat") {
                                // Check if this is a confirmation response (not streamed)
                                let has_confirmation =
                                    res.get("needs_confirmation").is_some_and(|v| !v.is_null());
                                if has_confirmation {
                                    // Confirmation text wasn't streamed, log it all
                                    resp_body.push_str(&format!("\n{}", text));
                                }
                                // Normal responses: text was streamed, interaction_id is in OUT line - nothing more to log
                            } else {
                                resp_body.push_str(&format!("\n{}", text));
                            }
                        }
                    } else if let Some(err) = &response.error
                        && let Some(msg) = err.get("message").and_then(|v| v.as_str())
                    {
                        resp_body.push_str(&format!("\n{}", msg.red()));
                    }
                    if let Ok(resp_str) = serde_json::to_string(&response) {
                        crate::logging::log_event(&format_mcp_response(
                            &request_clone.method,
                            &format_status(&response),
                            &detail,
                            &resp_body,
                        ));
                        let _ = tx_clone.send(format!("{}\n", resp_str));
                    }
                });
                current_task = Some((handle, cancellation_token));
                continue;
            }

            // Handle other requests synchronously
            let cancellation_token = CancellationToken::new();
            let response = self
                .handle_request(request.clone(), tx.clone(), cancellation_token)
                .await?;
            let resp_str = serde_json::to_string(&response)?;
            crate::logging::log_event(&format_mcp_response(
                &request.method,
                &format_status(&response),
                "",
                "",
            ));
            let _ = tx.send(format!("{}\n", resp_str));
        }

        Ok(())
    }

    async fn handle_request(
        &self,
        request: JsonRpcRequest,
        tx: UnboundedSender<String>,
        cancellation_token: CancellationToken,
    ) -> Result<JsonRpcResponse> {
        let id = request.id.clone();
        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params).await,
            "tools/list" => self.handle_tools_list().await,
            "tools/call" => {
                self.handle_tools_call(request.params, tx, cancellation_token)
                    .await
            }
            "notifications/initialized" => {
                return Ok(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: Some(json!({})),
                    error: None,
                    id,
                });
            }
            _ => Err(anyhow!("Method not found: {}", request.method)),
        };

        match result {
            Ok(res) => Ok(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: Some(res),
                error: None,
                id,
            }),
            Err(e) => Ok(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(json!({
                    "code": -32603,
                    "message": e.to_string()
                })),
                id,
            }),
        }
    }

    async fn handle_initialize(&self, _params: Option<Value>) -> Result<Value> {
        Ok(json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "clemini",
                "version": env!("CARGO_PKG_VERSION")
            }
        }))
    }

    async fn handle_tools_list(&self) -> Result<Value> {
        Ok(json!({
            "tools": [
                {
                    "name": "clemini_chat",
                    "description": "Send a message to clemini and get a response. Clemini is a Gemini-powered coding assistant with access to local tools like bash, edit, and read_file.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "message": {
                                "type": "string",
                                "description": "The message to send to clemini"
                            },
                            "interaction_id": {
                                "type": "string",
                                "description": "Optional interaction ID for multi-turn conversation"
                            }
                        },
                        "required": ["message"]
                    }
                },
                {
                    "name": "clemini_rebuild",
                    "description": "Rebuild clemini and restart the server",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                }
            ]
        }))
    }

    async fn handle_tools_call(
        &self,
        params: Option<Value>,
        tx: UnboundedSender<String>,
        cancellation_token: CancellationToken,
    ) -> Result<Value> {
        let params = params.ok_or_else(|| anyhow!("Missing parameters"))?;
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing tool name"))?;
        let arguments = params
            .get("arguments")
            .ok_or_else(|| anyhow!("Missing tool arguments"))?;

        match name {
            "clemini_chat" => {
                self.call_clemini_chat(arguments, tx, cancellation_token)
                    .await
            }
            "clemini_rebuild" => self.call_clemini_rebuild(arguments).await,
            _ => Err(anyhow!("Unknown tool: {}", name)),
        }
    }

    async fn call_clemini_rebuild(&self, _arguments: &Value) -> Result<Value> {
        #[cfg(unix)]
        {
            // Spawn build in background thread so we can return response immediately
            let args: Vec<String> = std::env::args().collect();
            std::thread::spawn(move || {
                // Small delay to let the response go out
                std::thread::sleep(std::time::Duration::from_millis(100));

                let status = std::process::Command::new("cargo")
                    .args(["build", "--release"])
                    .status();

                match status {
                    Ok(s) if s.success() => {
                        use std::os::unix::process::CommandExt;
                        let mut cmd = std::process::Command::new("target/release/clemini");
                        cmd.args(&args[1..]);
                        let _ = cmd.exec(); // This replaces the process
                    }
                    _ => {
                        // Build failed, just exit - CC will show the disconnect
                        std::process::exit(1);
                    }
                }
            });

            Ok(json!({
                "content": [
                    {
                        "type": "text",
                        "text": "Rebuilding clemini... Connection will restart when complete."
                    }
                ],
                "isError": false
            }))
        }
        #[cfg(not(unix))]
        {
            Err(anyhow!("clemini_rebuild is only supported on Unix systems"))
        }
    }

    async fn call_clemini_chat(
        &self,
        arguments: &Value,
        tx: UnboundedSender<String>,
        cancellation_token: CancellationToken,
    ) -> Result<Value> {
        let message = arguments
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing message"))?;
        let interaction_id = arguments
            .get("interaction_id")
            .or_else(|| arguments.get("session_id")) // Backwards compat
            .and_then(|v| v.as_str());

        // Create channel for agent events
        let (events_tx, mut events_rx) = mpsc::channel::<AgentEvent>(100);

        // Spawn task to handle AgentEvents via McpEventHandler
        let tx_clone = tx.clone();
        let event_handler = tokio::spawn(async move {
            let mut handler = McpEventHandler::new(tx_clone);
            while let Some(event) = events_rx.recv().await {
                crate::events::dispatch_event(&mut handler, &event);
            }
        });

        // Set events_tx so tools emit ToolOutput events instead of calling log_event directly
        self.tool_service.set_events_tx(Some(events_tx.clone()));

        let result = run_interaction(
            &self.client,
            &self.tool_service,
            message,
            interaction_id,
            &self.model,
            &self.system_prompt,
            events_tx,
            cancellation_token,
        )
        .await?;

        // Clear events_tx after interaction completes - this closes the channel
        // which causes the event handler loop to exit
        self.tool_service.set_events_tx(None);

        // Wait for event handler to finish processing all events before continuing
        // This ensures streaming output is fully flushed before we return the response
        let _ = event_handler.await;

        // Include interaction_id and needs_confirmation in the content text so it's visible to the LLM
        // (MCP protocol only guarantees content array is surfaced, not extra fields)
        let mut response_text = result.response;
        if let Some(ref confirmation) = result.needs_confirmation {
            response_text = format!(
                "Command requires confirmation: {}\n\nTo proceed, continue this conversation and explicitly approve.",
                confirmation
            );
        }
        if let Some(ref id) = result.id {
            response_text = format!("{}\n\ninteraction_id: {}", response_text, id);
        }

        Ok(json!({
            "content": [
                {
                    "type": "text",
                    "text": response_text
                }
            ],
            "tool_calls": result.tool_calls,
            "interaction_id": result.id,
            "needs_confirmation": result.needs_confirmation,
            "isError": false
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_jsonrpc_request_parsing() {
        // Full request
        let json = json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {"some": "param"},
            "id": 1
        });
        let req: JsonRpcRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(json!(1)));
        assert_eq!(req.params, Some(json!({"some": "param"})));

        // Missing jsonrpc (should work because of #[serde(default)])
        let json = json!({
            "method": "tools/list",
            "id": "abc"
        });
        let req: JsonRpcRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.method, "tools/list");
        assert_eq!(req.id, Some(json!("abc")));

        // Notification (no id)
        let json = json!({
            "method": "notifications/initialized"
        });
        let req: JsonRpcRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.method, "notifications/initialized");
        assert!(req.id.is_none());
    }

    #[test]
    fn test_jsonrpc_response_serialization() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(json!({"status": "ok"})),
            error: None,
            id: Some(json!(1)),
        };
        let val = serde_json::to_value(resp).unwrap();
        assert_eq!(val["jsonrpc"], "2.0");
        assert_eq!(val["result"]["status"], "ok");
        assert!(val.get("error").is_none());
        assert_eq!(val["id"], 1);

        let resp_err = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(json!({"code": -32603, "message": "error"})),
            id: None,
        };
        let val_err = serde_json::to_value(resp_err).unwrap();
        assert!(val_err.get("result").is_none());
        assert_eq!(val_err["error"]["code"], -32603);
    }

    #[test]
    fn test_extract_request_parts() {
        let (detail, body) = extract_request_parts("tools/list", &None);
        assert!(detail.is_empty());
        assert!(body.is_none());

        let params = json!({
            "name": "clemini_chat",
            "arguments": {
                "message": "hello\nworld",
                "interaction_id": "test-id"
            }
        });
        let (detail, body) = extract_request_parts("tools/call", &Some(params));
        assert!(detail.contains("clemini_chat"));
        assert!(detail.contains("interaction"));
        assert!(detail.contains("test-id"));
        let body = body.unwrap();
        assert!(body.contains("hello"));
        assert!(body.contains("world"));
    }

    #[test]
    fn test_format_mcp_request() {
        colored::control::set_override(false);

        // Simple request without body
        let output = format_mcp_request("tools/list", &None);
        assert!(output.starts_with("IN"));
        assert!(output.contains("tools/list"));

        // Request with body
        let params = json!({
            "name": "clemini_chat",
            "arguments": {
                "message": "hello world",
                "interaction_id": "test-id"
            }
        });
        let output = format_mcp_request("tools/call", &Some(params));
        assert!(output.contains("IN"));
        assert!(output.contains("clemini_chat"));
        assert!(output.contains("hello world"));

        colored::control::unset_override();
    }

    #[test]
    fn test_format_status() {
        // Disable colors for consistent test output
        colored::control::set_override(false);

        let ok_resp = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: Some(json!({})),
            error: None,
            id: None,
        };
        assert_eq!(format_status(&ok_resp).to_string(), "OK");

        let err_resp = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(json!({})),
            id: None,
        };
        assert_eq!(format_status(&err_resp).to_string(), "ERROR");

        // Re-enable colors
        colored::control::unset_override();
    }

    fn create_test_server() -> McpServer {
        let client = Client::new("dummy-key".to_string());
        let cwd = std::env::current_dir().unwrap();
        let tool_service = Arc::new(CleminiToolService::new(
            cwd.clone(),
            30,
            true,
            vec![cwd],
            "dummy-key".to_string(),
        ));
        // Note: events_tx is left as None for tests (tools fall back to log_event)
        McpServer::new(
            client,
            tool_service,
            "gemini-1.5-flash".to_string(),
            "system prompt".to_string(),
        )
    }

    #[tokio::test]
    async fn test_handle_initialize() {
        let server = create_test_server();
        let result = server.handle_initialize(None).await.unwrap();
        assert_eq!(result["protocolVersion"], "2025-11-25");
        assert_eq!(result["serverInfo"]["name"], "clemini");
    }

    #[tokio::test]
    async fn test_handle_tools_list() {
        let server = create_test_server();
        let result = server.handle_tools_list().await.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "clemini_chat"));
        assert!(tools.iter().any(|t| t["name"] == "clemini_rebuild"));
    }

    #[tokio::test]
    async fn test_handle_request_logic() {
        let server = create_test_server();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ct = tokio_util::sync::CancellationToken::new();

        // Test unknown method
        let req = JsonRpcRequest {
            _jsonrpc: None,
            method: "unknown".to_string(),
            params: None,
            id: Some(json!(1)),
        };
        let resp = server
            .handle_request(req, tx.clone(), ct.clone())
            .await
            .unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.id, Some(json!(1)));

        // Test notifications/initialized
        let req = JsonRpcRequest {
            _jsonrpc: None,
            method: "notifications/initialized".to_string(),
            params: None,
            id: Some(json!(2)),
        };
        let resp = server.handle_request(req, tx, ct).await.unwrap();
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_create_tool_executing_notification() {
        let args = json!({"file_path": "test.txt"});
        let notif = create_tool_executing_notification("read_file", &args);

        assert_eq!(notif["jsonrpc"], "2.0");
        assert_eq!(notif["method"], "notifications/progress");
        assert_eq!(notif["params"]["tool"], "read_file");
        assert_eq!(notif["params"]["status"], "executing");
        assert_eq!(notif["params"]["args"]["file_path"], "test.txt");
    }

    #[test]
    fn test_create_tool_executing_notification_complex_args() {
        let args = json!({
            "pattern": "*.rs",
            "path": "src",
            "limit": 100
        });
        let notif = create_tool_executing_notification("glob", &args);

        assert_eq!(notif["params"]["tool"], "glob");
        assert_eq!(notif["params"]["args"]["pattern"], "*.rs");
        assert_eq!(notif["params"]["args"]["path"], "src");
        assert_eq!(notif["params"]["args"]["limit"], 100);
    }

    #[test]
    fn test_create_tool_result_notification() {
        let notif = create_tool_result_notification("bash", 1500);

        assert_eq!(notif["jsonrpc"], "2.0");
        assert_eq!(notif["method"], "notifications/progress");
        assert_eq!(notif["params"]["tool"], "bash");
        assert_eq!(notif["params"]["status"], "completed");
        assert_eq!(notif["params"]["duration_ms"], 1500);
    }

    #[test]
    fn test_create_tool_result_notification_zero_duration() {
        let notif = create_tool_result_notification("read_file", 0);

        assert_eq!(notif["params"]["tool"], "read_file");
        assert_eq!(notif["params"]["duration_ms"], 0);
    }

    // =========================================
    // McpEventHandler tests
    // =========================================

    #[test]
    fn test_mcp_handler_tool_executing_sends_notification() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut handler = McpEventHandler::new(tx);

        let call = OwnedFunctionCallInfo {
            name: "read_file".to_string(),
            args: json!({"file_path": "test.rs"}),
            id: None,
        };
        handler.on_tool_executing(&call);

        let notification = rx.try_recv().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&notification).unwrap();

        assert_eq!(parsed["method"], "notifications/progress");
        assert_eq!(parsed["params"]["tool"], "read_file");
        assert_eq!(parsed["params"]["status"], "executing");
    }

    #[test]
    fn test_mcp_handler_tool_result_sends_notification() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut handler = McpEventHandler::new(tx);

        let result = FunctionExecutionResult::new(
            "bash".to_string(),
            "call-1".to_string(),
            json!({}),
            json!({"output": "ok"}),
            std::time::Duration::from_millis(150),
        );
        handler.on_tool_result(&result);

        let notification = rx.try_recv().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&notification).unwrap();

        assert_eq!(parsed["method"], "notifications/progress");
        assert_eq!(parsed["params"]["tool"], "bash");
        assert_eq!(parsed["params"]["status"], "completed");
        assert_eq!(parsed["params"]["duration_ms"], 150);
    }

    #[test]
    fn test_mcp_handler_text_delta_no_notification() {
        crate::logging::disable_logging();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut handler = McpEventHandler::new(tx);

        // TextDelta should only log, not send notification
        handler.on_text_delta("Hello world");

        // No notification should be sent for text deltas
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_mcp_handler_context_warning_no_notification() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut handler = McpEventHandler::new(tx);

        // Context warning should only log, not send notification
        handler.on_context_warning(&crate::agent::ContextWarning::new(855_000, 1_000_000));

        // No notification should be sent for context warnings
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_handle_tools_call_errors() {
        let server = create_test_server();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ct = tokio_util::sync::CancellationToken::new();

        // Missing parameters
        let result = server.handle_tools_call(None, tx.clone(), ct.clone()).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Missing parameters")
        );

        // Missing tool name
        let params = json!({ "arguments": {} });
        let result = server
            .handle_tools_call(Some(params), tx.clone(), ct.clone())
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Missing tool name")
        );

        // Unknown tool
        let params = json!({ "name": "unknown_tool", "arguments": {} });
        let result = server
            .handle_tools_call(Some(params), tx.clone(), ct.clone())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown tool"));
    }

    #[test]
    fn test_mcp_handler_on_tool_output() {
        crate::logging::disable_logging();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut handler = McpEventHandler::new(tx);

        // Tool output is used for internal logging/UI display (like bash progress)
        // but does not map to any MCP protocol notification, so it should be ignored by the handler.
        handler.on_tool_output("some output");
        assert!(rx.try_recv().is_err());
    }
}

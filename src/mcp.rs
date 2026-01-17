use anyhow::{anyhow, Result};
use axum::{
    extract::State,
    response::sse::{Event, Sse},
    routing::{get, post},
    Json, Router,
};
use colored::Colorize;
use futures_util::stream::Stream;
use genai_rs::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;

use crate::tools::CleminiToolService;
use crate::InteractionProgress;

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
    sessions: Mutex<HashMap<String, McpSession>>,
    notification_tx: broadcast::Sender<String>,
}

struct McpSession {
    last_interaction_id: Option<String>,
}

async fn handle_post(
    State(server): State<Arc<McpServer>>,
    Json(request): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let mut detail = String::new();
    let mut msg_body = String::new();
    if request.method == "tools/call" {
        if let Some(params) = &request.params {
            if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                detail.push_str(&format!(" {}", name.purple()));
            }
            if let Some(args) = params.get("arguments") {
                if let Some(session_id) = args.get("session_id").and_then(|v| v.as_str()) {
                    detail.push_str(&format!(
                        " {}={}",
                        "session".dimmed(),
                        format!("\"{}\"", session_id).yellow()
                    ));
                }
                if let Some(msg) = args.get("message").and_then(|v| v.as_str()) {
                    for line in msg.lines() {
                        msg_body.push_str(&format!("\n> {}", line));
                    }
                }
            }
        }
    }
    crate::log_event(&format!(
        "{} {}{}{}",
        "IN".green(),
        request.method.bold(),
        detail,
        msg_body
    ));

    // For HTTP, we use the server's broadcast channel for notifications
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let notification_tx = server.notification_tx.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let _ = notification_tx.send(msg);
        }
    });

    match server.handle_request(request.clone(), tx).await {
        Ok(response) => {
            let status = if response.error.is_some() {
                "ERROR"
            } else {
                "OK"
            };
            let status_color = if response.error.is_some() {
                status.red()
            } else {
                status.green()
            };
            crate::log_event(&format!(
                "{} {} ({})",
                "OUT".cyan(),
                request.method.bold(),
                status_color
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
    pub fn new(client: Client, tool_service: Arc<CleminiToolService>, model: String) -> Self {
        let (notification_tx, _) = broadcast::channel(100);
        Self {
            client,
            tool_service,
            model,
            sessions: Mutex::new(HashMap::new()),
            notification_tx,
        }
    }

    pub async fn run_http(self: Arc<Self>, port: u16) -> Result<()> {
        crate::log_event(&format!(
            "MCP HTTP server starting on {} ({} enable multi-turn conversations)",
            format!("http://0.0.0.0:{}", port).cyan(),
            "session IDs".cyan()
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

    pub async fn run_stdio(self: Arc<Self>) -> Result<()> {
        crate::log_event(&format!(
            "MCP server starting ({} enable multi-turn conversations)",
            "session IDs".cyan()
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

        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            let request: JsonRpcRequest = match serde_json::from_str::<JsonRpcRequest>(&line) {
                Ok(req) => {
                    let mut detail = String::new();
                    let mut msg_body = String::new();
                    if req.method == "tools/call" {
                        if let Some(params) = &req.params {
                            if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                                detail.push_str(&format!(" {}", name.purple()));
                            }
                            if let Some(args) = params.get("arguments") {
                                if let Some(session_id) = args.get("session_id").and_then(|v| v.as_str()) {
                                    detail.push_str(&format!(
                                        " {}={}",
                                        "session".dimmed(),
                                        format!("\"{}\"", session_id).yellow()
                                    ));
                                }
                                if let Some(msg) = args.get("message").and_then(|v| v.as_str()) {
                                    for line in msg.lines() {
                                        msg_body.push_str(&format!("\n> {}", line));
                                    }
                                }
                            }
                        }
                    }
                    crate::log_event(&format!(
                        "{} {}{}{}",
                        "IN".green(),
                        req.method.bold(),
                        detail,
                        msg_body
                    ));
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
                continue;
            }

            // Spawn tools/call requests concurrently so the main loop stays responsive
            if request.method == "tools/call" {
                let self_clone = Arc::clone(&self);
                let tx_clone = tx.clone();
                let request_clone = request.clone();
                tokio::spawn(async move {
                    let response = match self_clone
                        .handle_request(request_clone.clone(), tx_clone.clone())
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
                    let status = if response.error.is_some() {
                        "ERROR"
                    } else {
                        "OK"
                    };
                    let status_color = if response.error.is_some() {
                        status.red()
                    } else {
                        status.green()
                    };
                    let mut detail = String::new();
                    let mut resp_body = String::new();
                    if let Some(res) = &response.result {
                        if let Some(session_id) = res.get("session_id").and_then(|v| v.as_str()) {
                            detail.push_str(&format!(
                                " {}={}",
                                "session".dimmed(),
                                format!("\"{}\"", session_id).yellow()
                            ));
                        }
                        if let Some(content) = res.get("content").and_then(|v| v.as_array()) {
                            if let Some(text) = content
                                .get(0)
                                .and_then(|v| v.get("text"))
                                .and_then(|v| v.as_str())
                            {
                                resp_body.push_str(&format!("\n{}", text));
                            }
                        }
                    } else if let Some(err) = &response.error {
                        if let Some(msg) = err.get("message").and_then(|v| v.as_str()) {
                            resp_body.push_str(&format!("\n{}", msg.red()));
                        }
                    }
                    if let Ok(resp_str) = serde_json::to_string(&response) {
                        crate::log_event(&format!(
                            "{} {} ({}){}{}",
                            "OUT".cyan(),
                            request_clone.method.bold(),
                            status_color,
                            detail,
                            resp_body
                        ));
                        let _ = tx_clone.send(format!("{}\n", resp_str));
                    }
                });
                continue;
            }

            // Handle other requests synchronously
            let response = self.handle_request(request.clone(), tx.clone()).await?;
            let resp_str = serde_json::to_string(&response)?;
            crate::log_event(&format!(
                "{} {} ({})",
                "OUT".cyan(),
                request.method.bold(),
                if response.error.is_some() {
                    "ERROR".red()
                } else {
                    "OK".green()
                }
            ));
            let _ = tx.send(format!("{}\n", resp_str));
        }

        Ok(())
    }

    async fn handle_request(
        &self,
        request: JsonRpcRequest,
        tx: UnboundedSender<String>,
    ) -> Result<JsonRpcResponse> {
        let id = request.id.clone();
        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params).await,
            "tools/list" => self.handle_tools_list().await,
            "tools/call" => self.handle_tools_call(request.params, tx).await,
            "notifications/initialized" => {
                return Ok(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: Some(json!({})),
                    error: None,
                    id,
                })
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
                            "session_id": {
                                "type": "string",
                                "description": "Optional session ID for multi-turn conversation"
                            }
                        },
                        "required": ["message"]
                    }
                },
                {
                    "name": "clemini_reset",
                    "description": "Reset clemini session state",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "session_id": {
                                "type": "string",
                                "description": "Optional session ID to reset"
                            }
                        }
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
            "clemini_chat" => self.call_clemini_chat(arguments, tx).await,
            "clemini_reset" => self.call_clemini_reset(arguments).await,
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
    ) -> Result<Value> {
        let message = arguments
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing message"))?;
        let session_id = arguments
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();

        let mut sessions = self.sessions.lock().await;
        let session = sessions.entry(session_id.clone()).or_insert(McpSession {
            last_interaction_id: None,
        });

        let tx_clone = tx.clone();
        let progress_fn = Arc::new(move |p: InteractionProgress| {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": p
            });
            if let Ok(s) = serde_json::to_string(&notification) {
                let _ = tx_clone.send(format!("{}\n", s));
            }
        });

        let result = crate::run_interaction(
            &self.client,
            &self.tool_service,
            message,
            session.last_interaction_id.as_deref(),
            &self.model,
            false,
            Some(progress_fn),
        )
        .await?;

        session.last_interaction_id = result.id.clone();

        Ok(json!({
            "content": [
                {
                    "type": "text",
                    "text": result.response
                }
            ],
            "tool_calls": result.tool_calls,
            "session_id": session_id,
            "isError": false
        }))
    }

    async fn call_clemini_reset(&self, arguments: &Value) -> Result<Value> {
        let session_id = arguments
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        let mut sessions = self.sessions.lock().await;
        sessions.remove(&session_id);

        Ok(json!({
            "content": [
                {
                    "type": "text",
                    "text": format!("Session {} reset.", session_id)
                }
            ],
            "isError": false
        }))
    }
}

use anyhow::{anyhow, Result};
use genai_rs::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::tools::CleminiToolService;

#[derive(Debug, Deserialize)]
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
}

struct McpSession {
    last_interaction_id: Option<String>,
}

impl McpServer {
    pub fn new(client: Client, tool_service: Arc<CleminiToolService>, model: String) -> Self {
        Self {
            client,
            tool_service,
            model,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub async fn run_stdio(&self) -> Result<()> {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin).lines();
        let mut stdout = io::stdout();

        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            let request: JsonRpcRequest = match serde_json::from_str::<JsonRpcRequest>(&line) {
                Ok(req) => req,
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
                    stdout
                        .write_all(format!("{}\n", serde_json::to_string(&error_resp)?).as_bytes())
                        .await?;
                    stdout.flush().await?;
                    continue;
                }
            };

            // JSON-RPC notifications don't get responses
            if request.method.starts_with("notifications/") {
                continue;
            }

            let response = self.handle_request(request).await?;
            stdout
                .write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())
                .await?;
            stdout.flush().await?;
        }

        Ok(())
    }

    async fn handle_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse> {
        let id = request.id.clone();
        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params).await,
            "tools/list" => self.handle_tools_list().await,
            "tools/call" => self.handle_tools_call(request.params).await,
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

    async fn handle_tools_call(&self, params: Option<Value>) -> Result<Value> {
        let params = params.ok_or_else(|| anyhow!("Missing parameters"))?;
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing tool name"))?;
        let arguments = params
            .get("arguments")
            .ok_or_else(|| anyhow!("Missing tool arguments"))?;

        match name {
            "clemini_chat" => self.call_clemini_chat(arguments).await,
            "clemini_reset" => self.call_clemini_reset(arguments).await,
            "clemini_rebuild" => self.call_clemini_rebuild(arguments).await,
            _ => Err(anyhow!("Unknown tool: {}", name)),
        }
    }

    async fn call_clemini_rebuild(&self, _arguments: &Value) -> Result<Value> {
        #[cfg(unix)]
        {
            let status = std::process::Command::new("cargo")
                .args(["build", "--release"])
                .status()?;

            if !status.success() {
                return Ok(json!({
                    "content": [
                        {
                            "type": "text",
                            "text": format!("Build failed with status: {}", status)
                        }
                    ],
                    "isError": true
                }));
            }

            use std::os::unix::process::CommandExt;

            let args: Vec<String> = std::env::args().collect();
            let mut cmd = std::process::Command::new("target/release/clemini");
            cmd.args(&args[1..]);

            let err = cmd.exec();
            Err(anyhow!("Failed to exec: {}", err))
        }
        #[cfg(not(unix))]
        {
            Err(anyhow!("clemini_rebuild is only supported on Unix systems"))
        }
    }

    async fn call_clemini_chat(&self, arguments: &Value) -> Result<Value> {
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

        let result = crate::run_interaction(
            &self.client,
            &self.tool_service,
            message,
            session.last_interaction_id.as_deref(),
            &self.model,
            false,
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

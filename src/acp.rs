//! ACP (Agent Client Protocol) server implementation.
//!
//! This module provides ACP server support for clemini, enabling:
//! - Toad TUI integration as a frontend
//! - Structured subagent communication
//! - Plan mode support
//!
//! # Architecture
//!
//! clemini can operate as an ACP server (spawned by Toad/parent) or
//! as an ACP client (spawning subagents). This module handles the server side.
//!
//! The Agent Client Protocol uses a trait-based approach where we implement
//! the `acp::Agent` trait and pass it to `AgentSideConnection`.
//!
//! See also:
//! - `src/events.rs` for the EventHandler trait
//! - <https://agentclientprotocol.com/libraries/rust>

use acp::Client as AcpClient; // Import trait for session_notification
use agent_client_protocol as acp;
use anyhow::Result;
use async_trait::async_trait;
use genai_rs::Client;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::instrument;

use crate::agent::RetryConfig;
use crate::tools::CleminiToolService;

/// ACP server for clemini.
///
/// Handles incoming ACP requests and delegates to the Gemini agent.
pub struct AcpServer {
    client: Client,
    tool_service: Arc<CleminiToolService>,
    model: String,
    system_prompt: String,
    retry_config: RetryConfig,
}

impl AcpServer {
    pub fn new(
        client: Client,
        tool_service: Arc<CleminiToolService>,
        model: String,
        system_prompt: String,
        retry_config: RetryConfig,
    ) -> Self {
        Self {
            client,
            tool_service,
            model,
            system_prompt,
            retry_config,
        }
    }

    /// Run the ACP server on stdio.
    ///
    /// This handles the ACP protocol handshake and session management.
    #[instrument(skip(self))]
    pub async fn run_stdio(self: Arc<Self>) -> Result<()> {
        crate::logging::log_event("ACP server starting...");

        // Use LocalSet for non-Send futures from the protocol crate
        let local = LocalSet::new();

        local
            .run_until(async move {
                // Create streams with compat wrappers for futures-io
                let stdin = tokio::io::stdin().compat();
                let stdout = tokio::io::stdout().compat_write();

                // Channel for session notifications
                let (session_update_tx, mut session_update_rx) =
                    mpsc::unbounded_channel::<acp::SessionNotification>();

                // Create the agent handler
                let agent = CleminiAgent::new(
                    self.client.clone(),
                    self.tool_service.clone(),
                    self.model.clone(),
                    self.system_prompt.clone(),
                    self.retry_config,
                    session_update_tx,
                );

                // Create server-side connection
                let (connection, handle_io) =
                    acp::AgentSideConnection::new(agent, stdout, stdin, |fut| {
                        tokio::task::spawn_local(fut);
                    });

                // Spawn task to forward session notifications to client
                tokio::task::spawn_local(async move {
                    while let Some(notification) = session_update_rx.recv().await {
                        let result = connection.session_notification(notification).await;
                        if let Err(e) = result {
                            crate::logging::log_event(&format!(
                                "Failed to send session notification: {}",
                                e
                            ));
                        }
                    }
                });

                // Run the connection until EOF
                if let Err(e) = handle_io.await {
                    crate::logging::log_event(&format!("ACP connection error: {}", e));
                }

                crate::logging::log_event("ACP server shutting down");
            })
            .await;

        Ok(())
    }
}

/// Clemini's implementation of the ACP Agent trait.
struct CleminiAgent {
    #[allow(dead_code)]
    client: Client,
    #[allow(dead_code)]
    tool_service: Arc<CleminiToolService>,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    system_prompt: String,
    #[allow(dead_code)]
    retry_config: RetryConfig,
    session_update_tx: mpsc::UnboundedSender<acp::SessionNotification>,
    next_session_id: AtomicU64,
}

impl CleminiAgent {
    fn new(
        client: Client,
        tool_service: Arc<CleminiToolService>,
        model: String,
        system_prompt: String,
        retry_config: RetryConfig,
        session_update_tx: mpsc::UnboundedSender<acp::SessionNotification>,
    ) -> Self {
        Self {
            client,
            tool_service,
            model,
            system_prompt,
            retry_config,
            session_update_tx,
            next_session_id: AtomicU64::new(1),
        }
    }

    /// Send a session update notification.
    fn send_update(&self, session_id: &str, update: acp::SessionUpdate) {
        let notification = acp::SessionNotification::new(session_id.to_string(), update);
        if self.session_update_tx.send(notification).is_err() {
            crate::logging::log_event(&format!(
                "Failed to queue session notification for session {}",
                session_id
            ));
        }
    }
}

#[async_trait(?Send)]
impl acp::Agent for CleminiAgent {
    async fn initialize(
        &self,
        _init_request: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        crate::logging::log_event("ACP: initialize request received");

        Ok(
            acp::InitializeResponse::new(acp::ProtocolVersion::LATEST).agent_info(
                acp::Implementation::new(
                    env!("CARGO_PKG_NAME").to_string(),
                    env!("CARGO_PKG_VERSION").to_string(),
                ),
            ),
        )
    }

    async fn authenticate(
        &self,
        _auth_request: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        crate::logging::log_event("ACP: authenticate request received");
        // For now, accept all authentication
        Ok(acp::AuthenticateResponse::new())
    }

    async fn new_session(
        &self,
        _session_request: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        let session_id = self.next_session_id.fetch_add(1, Ordering::SeqCst);

        crate::logging::log_event(&format!("ACP: new session created: {}", session_id));

        Ok(acp::NewSessionResponse::new(session_id.to_string()))
    }

    async fn load_session(
        &self,
        _load_request: acp::LoadSessionRequest,
    ) -> acp::Result<acp::LoadSessionResponse> {
        crate::logging::log_event("ACP: load_session request received");
        // Return error - session loading not supported yet
        Err(acp::Error::new(
            acp::ErrorCode::InvalidRequest.into(),
            "Session loading not yet supported".to_string(),
        ))
    }

    async fn set_session_mode(
        &self,
        _request: acp::SetSessionModeRequest,
    ) -> acp::Result<acp::SetSessionModeResponse> {
        crate::logging::log_event("ACP: set_session_mode request received");
        Ok(acp::SetSessionModeResponse::new())
    }

    async fn prompt(&self, prompt_request: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let session_id = prompt_request.session_id.to_string();
        crate::logging::log_event(&format!("ACP: prompt request for session {}", session_id));

        // Extract the prompt text from content blocks
        let prompt_text = prompt_request
            .prompt
            .iter()
            .filter_map(|block| {
                if let acp::ContentBlock::Text(text_content) = block {
                    Some(text_content.text.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        if prompt_text.is_empty() {
            return Err(acp::Error::new(
                acp::ErrorCode::InvalidParams.into(),
                "No text content in prompt".to_string(),
            ));
        }

        // Send initial status update
        self.send_update(
            &session_id,
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new("Processing...".to_string()),
            ))),
        );

        // TODO: Implement actual interaction with Gemini agent
        // For now, return a simple response
        self.send_update(
            &session_id,
            acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
                acp::TextContent::new(format!("Received prompt: {}", prompt_text)),
            ))),
        );

        Ok(acp::PromptResponse::new(acp::StopReason::EndTurn))
    }

    async fn cancel(&self, cancel_request: acp::CancelNotification) -> acp::Result<()> {
        crate::logging::log_event(&format!(
            "ACP: cancel request for session {}",
            cancel_request.session_id
        ));
        // TODO: Implement cancellation
        Ok(())
    }

    async fn ext_method(&self, request: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        crate::logging::log_event(&format!("ACP: ext_method request: {}", request.method));
        Err(acp::Error::new(
            acp::ErrorCode::MethodNotFound.into(),
            format!("Unknown extension method: {}", request.method),
        ))
    }

    async fn ext_notification(&self, notification: acp::ExtNotification) -> acp::Result<()> {
        crate::logging::log_event(&format!("ACP: ext_notification: {}", notification.method));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acp::Agent;

    fn create_test_agent() -> CleminiAgent {
        let api_key = "test-key".to_string();
        let client = Client::new(api_key.clone());
        let tool_service = Arc::new(CleminiToolService::new(
            std::path::PathBuf::from("/tmp"),
            120,
            false,
            vec![std::path::PathBuf::from("/tmp")],
            api_key,
        ));
        let (tx, _rx) = mpsc::unbounded_channel();

        CleminiAgent::new(
            client,
            tool_service,
            "test-model".to_string(),
            "test prompt".to_string(),
            RetryConfig::default(),
            tx,
        )
    }

    #[test]
    fn test_acp_server_creation() {
        // Just test that we can create the types
        let api_key = "test-key".to_string();
        let client = Client::new(api_key.clone());
        let tool_service = Arc::new(CleminiToolService::new(
            std::path::PathBuf::from("/tmp"),
            120,
            false,
            vec![std::path::PathBuf::from("/tmp")],
            api_key,
        ));

        let _server = AcpServer::new(
            client,
            tool_service,
            "test-model".to_string(),
            "test prompt".to_string(),
            RetryConfig::default(),
        );
    }

    #[test]
    fn test_session_id_increments_atomically() {
        let agent = create_test_agent();

        // Each call should return a unique, incrementing ID
        let id1 = agent.next_session_id.fetch_add(1, Ordering::SeqCst);
        let id2 = agent.next_session_id.fetch_add(1, Ordering::SeqCst);
        let id3 = agent.next_session_id.fetch_add(1, Ordering::SeqCst);

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[tokio::test]
    async fn test_prompt_rejects_empty_content() {
        let agent = create_test_agent();

        // Create a prompt request with no text content
        let request = acp::PromptRequest::new("1".to_string(), vec![]);

        let result: acp::Result<acp::PromptResponse> = agent.prompt(request).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.message, "No text content in prompt");
    }

    #[tokio::test]
    async fn test_prompt_extracts_text_from_content_blocks() {
        let agent = create_test_agent();

        // Create a prompt request with multiple text blocks
        let request = acp::PromptRequest::new(
            "1".to_string(),
            vec![
                acp::ContentBlock::Text(acp::TextContent::new("Hello".to_string())),
                acp::ContentBlock::Text(acp::TextContent::new("World".to_string())),
            ],
        );

        let result: acp::Result<acp::PromptResponse> = agent.prompt(request).await;

        // Should succeed (stub returns EndTurn)
        assert!(result.is_ok());
        assert_eq!(result.unwrap().stop_reason, acp::StopReason::EndTurn);
    }
}

//! Event bus tools for cross-session coordination.
//!
//! These tools allow clemini to communicate with other sessions via the event bus.

use crate::agent::AgentEvent;
use crate::event_bus::{EventBus, GetEventsOptions, format_timestamp};
use crate::tools::ToolEmitter;

use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Tool for registering a session with the event bus.
pub struct EventBusRegisterTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl EventBusRegisterTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for EventBusRegisterTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for EventBusRegisterTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "event_bus_register".to_string(),
            "Register with the event bus. Returns a session ID for publishing and receiving events. \
             Use client_id for session resumption across restarts."
                .to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "name": {
                        "type": "string",
                        "description": "Session name (e.g., branch name, task description)"
                    },
                    "machine": {
                        "type": "string",
                        "description": "Machine name (defaults to hostname)"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory (defaults to $PWD)"
                    },
                    "client_id": {
                        "type": "string",
                        "description": "Client ID for session resumption via (machine, client_id)"
                    }
                }),
                vec!["name".to_string()],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("name is required".into()))?;

        let machine = args.get("machine").and_then(|v| v.as_str());
        let cwd = args.get("cwd").and_then(|v| v.as_str());
        let client_id = args.get("client_id").and_then(|v| v.as_str());

        let bus = EventBus::open().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to open event bus: {}", e).into())
        })?;

        let session = bus
            .register_session(name, machine, cwd, client_id)
            .map_err(|e| {
                FunctionError::ExecutionError(format!("Failed to register: {}", e).into())
            })?;

        self.emit(&format!("Registered session: {}", session.id));

        Ok(json!({
            "session_id": session.id,
            "name": session.name,
            "machine": session.machine,
            "cwd": session.cwd,
            "cursor": session.cursor
        }))
    }
}

/// Tool for listing active sessions.
pub struct EventBusListSessionsTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl EventBusListSessionsTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for EventBusListSessionsTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for EventBusListSessionsTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "event_bus_list_sessions".to_string(),
            "List active sessions on the event bus, ordered by most recently active.".to_string(),
            FunctionParameters::new("object".to_string(), json!({}), vec![]),
        )
    }

    async fn call(&self, _args: Value) -> Result<Value, FunctionError> {
        let bus = EventBus::open().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to open event bus: {}", e).into())
        })?;

        let sessions = bus.list_sessions().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to list sessions: {}", e).into())
        })?;

        self.emit(&format!("Found {} active sessions", sessions.len()));

        let sessions_json: Vec<Value> = sessions
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "name": s.name,
                    "machine": s.machine,
                    "cwd": s.cwd,
                    "last_heartbeat": format_timestamp(s.last_heartbeat)
                })
            })
            .collect();

        Ok(json!({ "sessions": sessions_json }))
    }
}

/// Tool for listing channels.
pub struct EventBusListChannelsTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl EventBusListChannelsTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for EventBusListChannelsTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for EventBusListChannelsTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "event_bus_list_channels".to_string(),
            "List channels with subscriber counts.".to_string(),
            FunctionParameters::new("object".to_string(), json!({}), vec![]),
        )
    }

    async fn call(&self, _args: Value) -> Result<Value, FunctionError> {
        let bus = EventBus::open().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to open event bus: {}", e).into())
        })?;

        let channels = bus.list_channels().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to list channels: {}", e).into())
        })?;

        let channels_json: Vec<Value> = channels
            .iter()
            .map(|c| {
                json!({
                    "name": c.name,
                    "subscriber_count": c.subscriber_count
                })
            })
            .collect();

        Ok(json!({ "channels": channels_json }))
    }
}

/// Tool for publishing events.
pub struct EventBusPublishTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl EventBusPublishTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for EventBusPublishTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for EventBusPublishTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "event_bus_publish".to_string(),
            "Publish an event to the event bus. Auto-refreshes session heartbeat.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "event_type": {
                        "type": "string",
                        "description": "Event type (e.g., 'task_completed', 'help_needed', 'ci_completed')"
                    },
                    "payload": {
                        "type": "string",
                        "description": "Event message/payload"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Your session ID (for heartbeat and attribution)"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Target channel: 'all', 'session:<id>', 'repo:<name>', or 'machine:<name>'. Defaults to 'all'."
                    }
                }),
                vec!["event_type".to_string(), "payload".to_string()],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let event_type = args
            .get("event_type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("event_type is required".into()))?;

        let payload = args
            .get("payload")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("payload is required".into()))?;

        let session_id = args.get("session_id").and_then(|v| v.as_str());
        let channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .unwrap_or("all");

        let bus = EventBus::open().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to open event bus: {}", e).into())
        })?;

        let event = bus
            .publish_event(event_type, payload, session_id, channel)
            .map_err(|e| {
                FunctionError::ExecutionError(format!("Failed to publish: {}", e).into())
            })?;

        self.emit(&format!("Published event {} to {}", event.id, channel));

        Ok(json!({
            "event_id": event.id,
            "channel": event.channel,
            "created_at": format_timestamp(event.created_at)
        }))
    }
}

/// Tool for getting events.
pub struct EventBusGetEventsTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl EventBusGetEventsTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for EventBusGetEventsTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for EventBusGetEventsTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "event_bus_get_events".to_string(),
            "Get events from the event bus. Auto-refreshes session heartbeat.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "cursor": {
                        "type": "integer",
                        "description": "Start from this event ID (exclusive)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max events to return (default: 50)"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Your session ID (for cursor tracking and heartbeat)"
                    },
                    "order": {
                        "type": "string",
                        "description": "'desc' (newest first) or 'asc' (oldest first). Default: 'desc'."
                    },
                    "channel": {
                        "type": "string",
                        "description": "Filter to specific channel"
                    },
                    "resume": {
                        "type": "boolean",
                        "description": "Use session's saved cursor (requires session_id)"
                    },
                    "event_types": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Filter by event types (e.g., ['task_completed', 'ci_passed'])"
                    }
                }),
                vec![],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let cursor = args.get("cursor").and_then(|v| v.as_i64());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let session_id = args.get("session_id").and_then(|v| v.as_str());
        let order = args.get("order").and_then(|v| v.as_str()).unwrap_or("desc");
        let channel = args.get("channel").and_then(|v| v.as_str());
        let resume = args
            .get("resume")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let event_types: Option<Vec<String>> = args
            .get("event_types")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });

        let bus = EventBus::open().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to open event bus: {}", e).into())
        })?;

        let opts = GetEventsOptions {
            cursor,
            limit,
            session_id,
            order,
            channel,
            resume,
            event_types: event_types.as_deref(),
        };
        let (events, new_cursor) = bus.get_events(&opts).map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to get events: {}", e).into())
        })?;

        let events_json: Vec<Value> = events
            .iter()
            .map(|e| {
                json!({
                    "id": e.id,
                    "event_type": e.event_type,
                    "payload": e.payload,
                    "channel": e.channel,
                    "session_id": e.session_id,
                    "created_at": format_timestamp(e.created_at)
                })
            })
            .collect();

        Ok(json!({
            "events": events_json,
            "cursor": new_cursor
        }))
    }
}

/// Tool for unregistering a session.
pub struct EventBusUnregisterTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl EventBusUnregisterTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for EventBusUnregisterTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for EventBusUnregisterTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "event_bus_unregister".to_string(),
            "Unregister from the event bus. session_id takes precedence if both given.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "session_id": {
                        "type": "string",
                        "description": "Your session ID"
                    },
                    "client_id": {
                        "type": "string",
                        "description": "Alternative - looks up by (machine, client_id)"
                    }
                }),
                vec![],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let session_id = args.get("session_id").and_then(|v| v.as_str());
        let client_id = args.get("client_id").and_then(|v| v.as_str());

        if session_id.is_none() && client_id.is_none() {
            return Ok(json!({
                "error": "Either session_id or client_id is required"
            }));
        }

        let bus = EventBus::open().map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to open event bus: {}", e).into())
        })?;

        let success = bus.unregister_session(session_id, client_id).map_err(|e| {
            FunctionError::ExecutionError(format!("Failed to unregister: {}", e).into())
        })?;

        if success {
            self.emit("Session unregistered");
            Ok(json!({ "success": true }))
        } else {
            Ok(json!({
                "success": false,
                "error": "Session not found"
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_requires_name() {
        let tool = EventBusRegisterTool::new(None);
        let result = tool.call(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_publish_requires_event_type() {
        let tool = EventBusPublishTool::new(None);
        let result = tool.call(json!({ "payload": "test" })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_publish_requires_payload() {
        let tool = EventBusPublishTool::new(None);
        let result = tool.call(json!({ "event_type": "test" })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_unregister_requires_id() {
        let tool = EventBusUnregisterTool::new(None);
        let result = tool.call(json!({})).await.unwrap();
        assert!(result.get("error").is_some());
    }

    #[tokio::test]
    async fn test_register_success() {
        let tool = EventBusRegisterTool::new(None);
        let result = tool
            .call(json!({
                "name": "test-happy-path",
                "machine": "test-machine",
                "cwd": "/tmp/test"
            }))
            .await
            .unwrap();

        assert!(result.get("session_id").is_some());
        assert_eq!(
            result.get("name").and_then(|v| v.as_str()),
            Some("test-happy-path")
        );

        // Cleanup: unregister the session
        let session_id = result.get("session_id").and_then(|v| v.as_str()).unwrap();
        let unregister_tool = EventBusUnregisterTool::new(None);
        let _ = unregister_tool
            .call(json!({ "session_id": session_id }))
            .await;
    }

    #[tokio::test]
    async fn test_list_sessions_success() {
        let tool = EventBusListSessionsTool::new(None);
        let result = tool.call(json!({})).await.unwrap();

        // Should return an array of sessions (may be empty or have entries)
        assert!(result.get("sessions").is_some());
        assert!(result.get("sessions").unwrap().is_array());
    }

    #[tokio::test]
    async fn test_list_channels_success() {
        let tool = EventBusListChannelsTool::new(None);
        let result = tool.call(json!({})).await.unwrap();

        // Should return an array of channels
        assert!(result.get("channels").is_some());
        assert!(result.get("channels").unwrap().is_array());
    }

    #[tokio::test]
    async fn test_publish_and_get_events_success() {
        // First register a session
        let register_tool = EventBusRegisterTool::new(None);
        let session_result = register_tool
            .call(json!({
                "name": "test-publish-get",
                "machine": "test-machine"
            }))
            .await
            .unwrap();
        let session_id = session_result
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap();

        // Publish an event
        let publish_tool = EventBusPublishTool::new(None);
        let publish_result = publish_tool
            .call(json!({
                "event_type": "test_event",
                "payload": "test payload from happy path test",
                "session_id": session_id,
                "channel": "all"
            }))
            .await
            .unwrap();
        assert!(publish_result.get("event_id").is_some());

        // Get events
        let get_tool = EventBusGetEventsTool::new(None);
        let get_result = get_tool
            .call(json!({
                "session_id": session_id,
                "limit": 10
            }))
            .await
            .unwrap();
        assert!(get_result.get("events").is_some());

        // Cleanup
        let unregister_tool = EventBusUnregisterTool::new(None);
        let _ = unregister_tool
            .call(json!({ "session_id": session_id }))
            .await;
    }
}

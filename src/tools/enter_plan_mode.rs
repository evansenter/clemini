//! EnterPlanMode tool for transitioning to planning mode.
//!
//! In plan mode, only read-only tools are available. This allows clemini to
//! explore the codebase and design an approach before executing any changes.

use crate::agent::AgentEvent;
use crate::plan::PLAN_MANAGER;
use crate::tools::ToolEmitter;

use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Tool for entering plan mode.
pub struct EnterPlanModeTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl EnterPlanModeTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for EnterPlanModeTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for EnterPlanModeTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "enter_plan_mode".to_string(),
            "Transition to planning mode. In plan mode, only read-only tools are available \
             (read, glob, grep, web_fetch, web_search, ask_user, todo_write). Write tools \
             (edit, write, bash) are disabled. Use this when you need to explore the codebase \
             and design an approach before executing changes. Call exit_plan_mode when your plan \
             is ready for user review."
                .to_string(),
            FunctionParameters::new("object".to_string(), json!({}), vec![]),
        )
    }

    async fn call(&self, _args: Value) -> Result<Value, FunctionError> {
        let result = PLAN_MANAGER
            .write()
            .map_err(|e| FunctionError::ExecutionError(format!("Lock error: {}", e).into()))?
            .enter_plan_mode(None);

        match result {
            Ok(()) => {
                let plan_path = PLAN_MANAGER
                    .read()
                    .ok()
                    .and_then(|m| m.plan_file_path().map(|p| p.display().to_string()))
                    .unwrap_or_else(|| "unknown".to_string());

                self.emit(&format!("Entered plan mode. Plan file: {}", plan_path));

                Ok(json!({
                    "status": "entered_plan_mode",
                    "plan_file": plan_path,
                    "allowed_tools": ["read", "glob", "grep", "web_fetch", "web_search", "ask_user", "todo_write"],
                    "blocked_tools": ["edit", "write", "bash", "kill_shell", "task"]
                }))
            }
            Err(e) => Ok(json!({
                "error": e
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[tokio::test]
    #[serial]
    async fn test_enter_plan_mode() {
        // Reset plan manager state
        if let Ok(mut manager) = PLAN_MANAGER.write() {
            manager.exit_plan_mode();
        }

        let tool = EnterPlanModeTool::new(None);
        let result = tool.call(json!({})).await.unwrap();

        assert_eq!(result["status"], "entered_plan_mode");
        assert!(result["plan_file"].as_str().is_some());

        // Clean up
        if let Ok(mut manager) = PLAN_MANAGER.write() {
            manager.exit_plan_mode();
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_enter_plan_mode_twice_fails() {
        // Reset plan manager state
        if let Ok(mut manager) = PLAN_MANAGER.write() {
            manager.exit_plan_mode();
        }

        let tool = EnterPlanModeTool::new(None);

        // First call succeeds
        let result1 = tool.call(json!({})).await.unwrap();
        assert_eq!(result1["status"], "entered_plan_mode");

        // Second call fails
        let result2 = tool.call(json!({})).await.unwrap();
        assert!(result2["error"].as_str().is_some());

        // Clean up
        if let Ok(mut manager) = PLAN_MANAGER.write() {
            manager.exit_plan_mode();
        }
    }
}

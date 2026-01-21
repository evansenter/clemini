//! ExitPlanMode tool for completing plan mode and requesting user approval.
//!
//! When exiting plan mode, the tool returns the plan for user review and
//! optionally requests permissions for the implementation phase.

use crate::agent::AgentEvent;
use crate::plan::{AllowedPrompt, PLAN_MANAGER};
use crate::tools::ToolEmitter;

use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// Tool for exiting plan mode and requesting user approval.
pub struct ExitPlanModeTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl ExitPlanModeTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }
}

impl ToolEmitter for ExitPlanModeTool {
    fn events_tx(&self) -> &Option<mpsc::Sender<AgentEvent>> {
        &self.events_tx
    }
}

#[async_trait]
impl CallableFunction for ExitPlanModeTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "exit_plan_mode".to_string(),
            "Signal that your plan is complete and ready for user review. \
             Optionally specify permissions needed for the implementation phase. \
             The user will see the plan and approve or reject it before you proceed."
                .to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "allowed_prompts": {
                        "type": "array",
                        "description": "Permissions needed for implementation. Each item specifies a tool and semantic description of the allowed action. Example: [{\"tool\": \"bash\", \"prompt\": \"run tests\"}]",
                        "items": {
                            "type": "object",
                            "properties": {
                                "tool": {
                                    "type": "string",
                                    "description": "Tool name (e.g., 'bash', 'edit')"
                                },
                                "prompt": {
                                    "type": "string",
                                    "description": "Semantic description of allowed action (e.g., 'run tests')"
                                }
                            },
                            "required": ["tool", "prompt"]
                        }
                    }
                }),
                vec![],
            ),
        )
    }

    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        // Parse allowed_prompts if provided
        let allowed_prompts: Vec<AllowedPrompt> = args
            .get("allowed_prompts")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        // Get plan file path before exiting
        let plan_file = PLAN_MANAGER
            .read()
            .ok()
            .and_then(|m| m.plan_file_path().map(|p| p.display().to_string()));

        // Get current plan entries
        let plan_entries: Vec<Value> = PLAN_MANAGER
            .read()
            .ok()
            .and_then(|m| {
                m.current_plan().map(|p| {
                    p.entries
                        .iter()
                        .map(|e| {
                            json!({
                                "content": e.content,
                                "priority": format!("{:?}", e.priority),
                                "status": format!("{:?}", e.status)
                            })
                        })
                        .collect()
                })
            })
            .unwrap_or_default();

        // Exit plan mode
        let was_in_plan_mode = PLAN_MANAGER
            .write()
            .map_err(|e| FunctionError::ExecutionError(format!("Lock error: {}", e).into()))?
            .exit_plan_mode();

        if !was_in_plan_mode {
            return Ok(json!({
                "error": "Not in plan mode"
            }));
        }

        self.emit("Exited plan mode. Awaiting user approval.");

        Ok(json!({
            "status": "plan_ready_for_review",
            "plan_file": plan_file,
            "plan_entries": plan_entries,
            "allowed_prompts": allowed_prompts,
            "message": "Plan is ready for user review. User must approve before implementation begins."
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{PlanEntryInput, PlanEntryPriority};

    #[tokio::test]
    async fn test_exit_plan_mode_not_in_plan_mode() {
        // Ensure we're not in plan mode
        if let Ok(mut manager) = PLAN_MANAGER.write() {
            manager.exit_plan_mode();
        }

        let tool = ExitPlanModeTool::new(None);
        let result = tool.call(json!({})).await.unwrap();

        assert!(result["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_exit_plan_mode_with_plan() {
        // Reset and enter plan mode
        if let Ok(mut manager) = PLAN_MANAGER.write() {
            manager.exit_plan_mode();
            manager.enter_plan_mode(None).unwrap();
            manager.create_plan(vec![PlanEntryInput {
                content: "Step 1".to_string(),
                priority: PlanEntryPriority::High,
            }]);
        }

        let tool = ExitPlanModeTool::new(None);
        let result = tool
            .call(json!({
                "allowed_prompts": [
                    {"tool": "bash", "prompt": "run tests"}
                ]
            }))
            .await
            .unwrap();

        assert_eq!(result["status"], "plan_ready_for_review");
        assert!(result["plan_file"].as_str().is_some());
        assert_eq!(result["plan_entries"].as_array().unwrap().len(), 1);
        assert_eq!(result["allowed_prompts"].as_array().unwrap().len(), 1);
    }
}

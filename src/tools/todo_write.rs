use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tracing::instrument;

pub struct TodoWriteTool;

impl TodoWriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CallableFunction for TodoWriteTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "todo_write".to_string(),
            "Display a todo list to track progress on multi-step tasks.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                }),
                vec!["todos".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let todos = args
            .get("todos")
            .and_then(|v| v.as_array())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing todos array".to_string()))?;

        crate::log_event("");  // Leading newline before list
        for todo in todos {
            let content = todo.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let status = todo
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");

            let (icon, colored_content) = match status {
                "completed" => ("✓".green(), content.normal()),
                "in_progress" => ("→".yellow(), content.normal()),
                _ => ("○".dimmed(), content.dimmed()),
            };

            let line = format!("  {} {}", icon, colored_content);
            crate::log_event(&line);
        }

        Ok(json!({
            "success": true,
            "count": todos.len()
        }))
    }
}

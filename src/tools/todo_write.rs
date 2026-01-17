use async_trait::async_trait;
use colored::Colorize;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tracing::instrument;

pub struct TodoWriteTool;

#[derive(Debug, PartialEq, Clone)]
struct TodoItem {
    content: String,
    active_form: String,
    status: TodoStatus,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl From<&str> for TodoStatus {
    fn from(s: &str) -> Self {
        match s {
            "completed" => TodoStatus::Completed,
            "in_progress" => TodoStatus::InProgress,
            _ => TodoStatus::Pending,
        }
    }
}

impl TodoWriteTool {
    pub fn new() -> Self {
        Self
    }

    fn parse_args(&self, args: Value) -> Result<Vec<TodoItem>, FunctionError> {
        let todos_value = args
            .get("todos")
            .and_then(|v| v.as_array())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing todos array".to_string()))?;

        let mut todos = Vec::with_capacity(todos_value.len());
        let mut skipped_empty = 0;

        for todo in todos_value {
            let content = todo
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();

            let active_form = todo
                .get("activeForm")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();

            // Skip items with empty content or active_form
            if content.is_empty() || active_form.is_empty() {
                skipped_empty += 1;
                continue;
            }

            let status = todo
                .get("status")
                .and_then(|v| v.as_str())
                .map(TodoStatus::from)
                .unwrap_or(TodoStatus::Pending);

            todos.push(TodoItem {
                content,
                active_form,
                status,
            });
        }

        // Warn if some items were skipped
        if skipped_empty > 0 {
            tracing::warn!("Skipped {} todo item(s) with empty content", skipped_empty);
        }

        // Error if ALL items were empty
        if todos.is_empty() && skipped_empty > 0 {
            return Err(FunctionError::ArgumentMismatch(
                "All todo items have empty content - provide meaningful task descriptions"
                    .to_string(),
            ));
        }

        Ok(todos)
    }

    fn render_todo(todo: &TodoItem) -> String {
        let (icon, colored_content) = match todo.status {
            TodoStatus::Completed => ("✓".green(), todo.content.normal()),
            TodoStatus::InProgress => ("→".yellow(), todo.content.normal()),
            TodoStatus::Pending => ("○".dimmed(), todo.content.dimmed()),
        };

        format!("  {} {}", icon, colored_content)
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
                                "content": {
                                    "type": "string",
                                    "description": "The description of the task in imperative form (e.g., 'Run tests')"
                                },
                                "activeForm": {
                                    "type": "string",
                                    "description": "The description of the task in present continuous form (e.g., 'Running tests')"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["content", "activeForm", "status"]
                        }
                    }
                }),
                vec!["todos".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let todos = self.parse_args(args)?;

        crate::log_event(""); // Leading newline before list
        for todo in &todos {
            crate::log_event(&Self::render_todo(todo));
        }

        Ok(json!({
            "success": true,
            "count": todos.len()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genai_rs::CallableFunction;
    use serde_json::json;

    #[test]
    fn test_declaration() {
        let tool = TodoWriteTool::new();
        let decl = tool.declaration();

        assert_eq!(decl.name(), "todo_write");
        assert_eq!(
            decl.description(),
            "Display a todo list to track progress on multi-step tasks."
        );

        let params = decl.parameters();
        let params_json = serde_json::to_value(params).unwrap();
        assert_eq!(params_json["type"], "object");
        assert_eq!(params.required(), vec!["todos".to_string()]);

        let properties = params.properties();
        assert!(properties.get("todos").is_some());
        assert_eq!(properties["todos"]["type"], "array");
        assert_eq!(properties["todos"]["items"]["type"], "object");

        let todo_props = &properties["todos"]["items"]["properties"];
        assert_eq!(todo_props["content"]["type"], "string");
        assert_eq!(todo_props["activeForm"]["type"], "string");
        assert_eq!(todo_props["status"]["type"], "string");
        assert_eq!(
            todo_props["status"]["enum"],
            json!(["pending", "in_progress", "completed"])
        );
        assert_eq!(
            properties["todos"]["items"]["required"],
            json!(["content", "activeForm", "status"])
        );
    }

    #[test]
    fn test_parse_args_success() {
        let tool = TodoWriteTool::new();
        let args = json!({
            "todos": [
                { "content": "Task 1", "activeForm": "Running Task 1", "status": "completed" },
                { "content": "Task 2", "activeForm": "Running Task 2", "status": "in_progress" },
                { "content": "Task 3", "activeForm": "Running Task 3", "status": "pending" }
            ]
        });

        let todos = tool.parse_args(args).unwrap();
        assert_eq!(todos.len(), 3);
        assert_eq!(
            todos[0],
            TodoItem {
                content: "Task 1".to_string(),
                active_form: "Running Task 1".to_string(),
                status: TodoStatus::Completed
            }
        );
        assert_eq!(
            todos[1],
            TodoItem {
                content: "Task 2".to_string(),
                active_form: "Running Task 2".to_string(),
                status: TodoStatus::InProgress
            }
        );
        assert_eq!(
            todos[2],
            TodoItem {
                content: "Task 3".to_string(),
                active_form: "Running Task 3".to_string(),
                status: TodoStatus::Pending
            }
        );
    }

    #[test]
    fn test_parse_args_missing_todos() {
        let tool = TodoWriteTool::new();
        let args = json!({});

        let result = tool.parse_args(args);
        assert!(result.is_err());
        match result {
            Err(FunctionError::ArgumentMismatch(msg)) => assert_eq!(msg, "Missing todos array"),
            _ => panic!("Expected ArgumentMismatch error"),
        }
    }

    #[test]
    fn test_parse_args_empty_array() {
        let tool = TodoWriteTool::new();
        let args = json!({ "todos": [] });

        let todos = tool.parse_args(args).unwrap();
        assert!(todos.is_empty());
    }

    #[test]
    fn test_parse_args_invalid_status() {
        let tool = TodoWriteTool::new();
        let args = json!({
            "todos": [
                { "content": "Unknown status", "activeForm": "Doing something", "status": "something_else" }
            ]
        });

        let todos = tool.parse_args(args).unwrap();
        assert_eq!(todos[0].status, TodoStatus::Pending);
    }

    #[test]
    fn test_render_todo_output() {
        // We can't easily test colors without a terminal or checking escape codes,
        // but we can at least check if it contains the right icons and content.
        // Status: Completed -> ✓
        let completed = TodoItem {
            content: "Done".to_string(),
            active_form: "Doing".to_string(),
            status: TodoStatus::Completed,
        };
        let rendered_completed = TodoWriteTool::render_todo(&completed);
        assert!(rendered_completed.contains("✓"));
        assert!(rendered_completed.contains("Done"));

        // Status: InProgress -> →
        let in_progress = TodoItem {
            content: "Working".to_string(),
            active_form: "Working now".to_string(),
            status: TodoStatus::InProgress,
        };
        let rendered_in_progress = TodoWriteTool::render_todo(&in_progress);
        assert!(rendered_in_progress.contains("→"));
        assert!(rendered_in_progress.contains("Working"));

        // Status: Pending -> ○
        let pending = TodoItem {
            content: "Waiting".to_string(),
            active_form: "Waiting now".to_string(),
            status: TodoStatus::Pending,
        };
        let rendered_pending = TodoWriteTool::render_todo(&pending);
        assert!(rendered_pending.contains("○"));
        assert!(rendered_pending.contains("Waiting"));
    }

    #[test]
    fn test_parse_args_all_empty_content_errors() {
        let tool = TodoWriteTool::new();
        let args = json!({
            "todos": [
                { "content": "", "activeForm": "", "status": "pending" },
                { "content": "   ", "activeForm": " ", "status": "pending" },
                { "content": "", "activeForm": "Doing", "status": "in_progress" }
            ]
        });

        let result = tool.parse_args(args);
        assert!(result.is_err());
        match result {
            Err(FunctionError::ArgumentMismatch(msg)) => {
                assert!(msg.contains("empty content"));
            }
            _ => panic!("Expected ArgumentMismatch error"),
        }
    }

    #[test]
    fn test_parse_args_skips_empty_content() {
        let tool = TodoWriteTool::new();
        let args = json!({
            "todos": [
                { "content": "Valid task", "activeForm": "Doing valid task", "status": "pending" },
                { "content": "", "activeForm": "Missing content", "status": "pending" },
                { "content": "Another valid", "activeForm": "Doing another", "status": "completed" }
            ]
        });

        let todos = tool.parse_args(args).unwrap();
        // Should have 2 items, skipping the empty one
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].content, "Valid task");
        assert_eq!(todos[1].content, "Another valid");
    }

    #[test]
    fn test_parse_args_trims_whitespace() {
        let tool = TodoWriteTool::new();
        let args = json!({
            "todos": [
                { "content": "  Task with spaces  ", "activeForm": "  Doing task  ", "status": "pending" }
            ]
        });

        let todos = tool.parse_args(args).unwrap();
        assert_eq!(todos[0].content, "Task with spaces");
        assert_eq!(todos[0].active_form, "Doing task");
    }
}

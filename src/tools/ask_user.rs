use crate::agent::AgentEvent;
use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::io;
use tokio::sync::mpsc;
use tracing::instrument;

pub struct AskUserTool {
    events_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl AskUserTool {
    pub fn new(events_tx: Option<mpsc::Sender<AgentEvent>>) -> Self {
        Self { events_tx }
    }

    /// Emit tool output via events (if available) or fallback to log_event.
    fn emit(&self, output: &str) {
        if let Some(tx) = &self.events_tx {
            let _ = tx.try_send(AgentEvent::ToolOutput(output.to_string()));
        } else {
            crate::logging::log_event(output);
        }
    }

    /// Resolve user's answer - if they entered a number matching an option, return the option value
    fn resolve_answer(answer: &str, options: &Option<Vec<String>>) -> String {
        if let Some(opts) = options
            && let Ok(num) = answer.parse::<usize>()
            && num >= 1
            && num <= opts.len()
        {
            return opts[num - 1].clone();
        }
        answer.to_string()
    }

    fn parse_args(&self, args: Value) -> Result<(String, Option<Vec<String>>), FunctionError> {
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing question".to_string()))?
            .to_string();

        let options = args.get("options").and_then(|v| v.as_array()).map(|opts| {
            opts.iter()
                .filter_map(|opt| opt.as_str().map(|s| s.to_string()))
                .collect()
        });

        Ok((question, options))
    }
}

#[async_trait]
impl CallableFunction for AskUserTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "ask_user".to_string(),
            "Ask the user a question and wait for their response. Use this when you need clarification or a decision from the user. Returns: {answer}. When options are provided, they are displayed numbered and the user's selection is resolved to the option value.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "question": {
                        "type": "string",
                        "description": "The question to ask the user"
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of options for multiple choice"
                    }
                }),
                vec!["question".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let (question, options) = self.parse_args(args)?;

        self.emit(&format!("  {}", question));

        // Build numbered options for display and to return to model
        let numbered_options: Option<Vec<String>> = options.as_ref().map(|opts| {
            opts.iter()
                .enumerate()
                .map(|(i, opt)| format!("{}. {}", i + 1, opt))
                .collect()
        });

        if let Some(opts) = &numbered_options {
            for opt in opts {
                self.emit(&format!("  {}", opt));
            }
        }

        let mut answer = String::new();
        match io::stdin().read_line(&mut answer) {
            Ok(_) => {
                let answer = answer.trim();

                let resolved = Self::resolve_answer(answer, &options);
                Ok(json!({ "answer": resolved }))
            }
            Err(e) => Ok(json!({
                "error": format!("Failed to read from stdin: {}", e)
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genai_rs::CallableFunction;
    use serde_json::json;

    #[test]
    fn test_declaration() {
        let tool = AskUserTool::new(None);
        let decl = tool.declaration();

        assert_eq!(decl.name(), "ask_user");
        assert!(decl.description().contains("Ask the user a question"));
        assert!(decl.description().contains("displayed numbered"));

        let params = decl.parameters();
        let params_json = serde_json::to_value(params).unwrap();
        assert_eq!(params_json["type"], "object");
        assert_eq!(params.required(), vec!["question".to_string()]);

        let properties = params.properties();
        assert!(properties.get("question").is_some());
        assert!(properties.get("options").is_some());

        assert_eq!(properties["question"]["type"], "string");
        assert_eq!(properties["options"]["type"], "array");
    }

    #[test]
    fn test_parse_args_success() {
        let tool = AskUserTool::new(None);
        let args = json!({
            "question": "What is your favorite color?",
            "options": ["Red", "Blue", "Green"]
        });

        let (question, options) = tool.parse_args(args).unwrap();
        assert_eq!(question, "What is your favorite color?");
        assert_eq!(
            options,
            Some(vec![
                "Red".to_string(),
                "Blue".to_string(),
                "Green".to_string()
            ])
        );
    }

    #[test]
    fn test_parse_args_no_options() {
        let tool = AskUserTool::new(None);
        let args = json!({
            "question": "How are you?"
        });

        let (question, options) = tool.parse_args(args).unwrap();
        assert_eq!(question, "How are you?");
        assert_eq!(options, None);
    }

    #[test]
    fn test_parse_args_missing_question() {
        let tool = AskUserTool::new(None);
        let args = json!({
            "options": ["Yes", "No"]
        });

        let result = tool.parse_args(args);
        assert!(result.is_err());
        match result {
            Err(FunctionError::ArgumentMismatch(msg)) => assert_eq!(msg, "Missing question"),
            _ => panic!("Expected ArgumentMismatch error"),
        }
    }

    #[test]
    fn test_parse_args_empty_options() {
        let tool = AskUserTool::new(None);
        let args = json!({
            "question": "Empty options?",
            "options": []
        });

        let (question, options) = tool.parse_args(args).unwrap();
        assert_eq!(question, "Empty options?");
        assert_eq!(options, Some(vec![]));
    }

    #[test]
    fn test_parse_args_null_options() {
        let tool = AskUserTool::new(None);
        let args = json!({
            "question": "Null options?",
            "options": null
        });

        let (question, options) = tool.parse_args(args).unwrap();
        assert_eq!(question, "Null options?");
        assert_eq!(options, None);
    }

    #[test]
    fn test_parse_args_invalid_options_items() {
        let tool = AskUserTool::new(None);
        let args = json!({
            "question": "Mixed options?",
            "options": ["Valid", 123, null, "Also Valid"]
        });

        let (question, options) = tool.parse_args(args).unwrap();
        assert_eq!(question, "Mixed options?");
        assert_eq!(
            options,
            Some(vec!["Valid".to_string(), "Also Valid".to_string()])
        );
    }

    #[test]
    fn test_resolve_answer_with_number() {
        let options = Some(vec![
            "red".to_string(),
            "blue".to_string(),
            "green".to_string(),
        ]);
        assert_eq!(AskUserTool::resolve_answer("1", &options), "red");
        assert_eq!(AskUserTool::resolve_answer("2", &options), "blue");
        assert_eq!(AskUserTool::resolve_answer("3", &options), "green");
    }

    #[test]
    fn test_resolve_answer_out_of_range() {
        let options = Some(vec!["red".to_string(), "blue".to_string()]);
        // Out of range returns raw input
        assert_eq!(AskUserTool::resolve_answer("0", &options), "0");
        assert_eq!(AskUserTool::resolve_answer("3", &options), "3");
        assert_eq!(AskUserTool::resolve_answer("99", &options), "99");
    }

    #[test]
    fn test_resolve_answer_non_numeric() {
        let options = Some(vec!["red".to_string(), "blue".to_string()]);
        // Non-numeric returns raw input
        assert_eq!(AskUserTool::resolve_answer("red", &options), "red");
        assert_eq!(AskUserTool::resolve_answer("yes", &options), "yes");
    }

    #[test]
    fn test_resolve_answer_no_options() {
        // No options, return raw input
        assert_eq!(AskUserTool::resolve_answer("1", &None), "1");
        assert_eq!(AskUserTool::resolve_answer("hello", &None), "hello");
    }
}

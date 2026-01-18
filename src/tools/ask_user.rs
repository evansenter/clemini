use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::io::{self, Write};
use tracing::instrument;

pub struct AskUserTool;

impl AskUserTool {
    pub fn new() -> Self {
        Self
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
            "Ask the user a question and wait for their response. Use this when you need clarification or a decision from the user. Returns: {answer}".to_string(),
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

        crate::log_event(&format!("\n{}", question));

        if let Some(opts) = options {
            for (i, opt) in opts.iter().enumerate() {
                crate::log_event(&format!("{}. {}", i + 1, opt));
            }
        }
        eprint!("> "); // Keep prompt on same line as input
        let _ = io::stderr().flush();

        let mut answer = String::new();
        match io::stdin().read_line(&mut answer) {
            Ok(_) => Ok(json!({
                "answer": answer.trim()
            })),
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
        let tool = AskUserTool::new();
        let decl = tool.declaration();

        assert_eq!(decl.name(), "ask_user");
        assert_eq!(
            decl.description(),
            "Ask the user a question and wait for their response. Use this when you need clarification or a decision from the user. Returns: {answer}"
        );

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
        let tool = AskUserTool::new();
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
        let tool = AskUserTool::new();
        let args = json!({
            "question": "How are you?"
        });

        let (question, options) = tool.parse_args(args).unwrap();
        assert_eq!(question, "How are you?");
        assert_eq!(options, None);
    }

    #[test]
    fn test_parse_args_missing_question() {
        let tool = AskUserTool::new();
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
        let tool = AskUserTool::new();
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
        let tool = AskUserTool::new();
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
        let tool = AskUserTool::new();
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
}

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
}

#[async_trait]
impl CallableFunction for AskUserTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "ask_user".to_string(),
            "Ask the user a question and wait for their response. Use this when you need clarification or a decision from the user.".to_string(),
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
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing question".to_string()))?;

        let options = args.get("options").and_then(|v| v.as_array());

        crate::log_event(&format!("\n{}", question));

        if let Some(opts) = options {
            for (i, opt) in opts.iter().enumerate() {
                if let Some(opt_str) = opt.as_str() {
                    crate::log_event(&format!("{}. {}", i + 1, opt_str));
                }
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

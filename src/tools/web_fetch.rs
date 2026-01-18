use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tracing::instrument;

pub struct WebFetchTool {
    api_key: String,
}

impl WebFetchTool {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    fn parse_args(&self, args: Value) -> Result<(String, Option<String>), FunctionError> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing url".to_string()))?;

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok((url, prompt))
    }
}

#[async_trait]
impl CallableFunction for WebFetchTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "web_fetch".to_string(),
            "Fetch the content of a web page from a URL and optionally process it with a prompt."
                .to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Optional prompt to process the fetched content with Gemini"
                    }
                }),
                vec!["url".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let (url, prompt) = self.parse_args(args)?;

        let client = match super::create_http_client() {
            Ok(c) => c,
            Err(e) => return Ok(json!({ "error": e })),
        };

        match client.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    return Ok(json!({
                        "error": format!("HTTP error: {}", status),
                        "status": status.as_u16()
                    }));
                }

                match resp.text().await {
                    Ok(mut text) => {
                        let original_len = text.len();

                        if let Some(prompt) = prompt {
                            // Convert HTML to markdown
                            let markdown = html2md::parse_html(&text);
                            let mut truncated_md = markdown.clone();
                            if truncated_md.len() > 50000 {
                                truncated_md.truncate(50000);
                                truncated_md
                                    .push_str("\n\n[Content truncated to 50000 characters]");
                            }

                            // Process with Gemini
                            let ai_client = genai_rs::Client::new(self.api_key.clone());
                            let ai_result = ai_client
                                .interaction()
                                .with_model("gemini-3-flash-preview")
                                .with_system_instruction(
                                    "You are a helpful assistant that processes web content.",
                                )
                                .with_content(vec![genai_rs::Content::text(format!(
                                    "Content:\n---\n{}\n---\n\nPrompt: {}",
                                    truncated_md, prompt
                                ))])
                                .create()
                                .await;

                            match ai_result {
                                Ok(response) => {
                                    return Ok(json!({
                                        "url": url,
                                        "processed_content": response.as_text().unwrap_or_default(),
                                        "original_length": original_len
                                    }));
                                }
                                Err(e) => {
                                    // If LLM fails, return raw content with a note
                                    if text.len() > 50000 {
                                        text.truncate(50000);
                                        text.push_str(
                                            "\n\n[Content truncated to 50000 characters]",
                                        );
                                    }
                                    return Ok(json!({
                                        "url": url,
                                        "content": text,
                                        "length": original_len,
                                        "note": format!("LLM processing failed: {}", e)
                                    }));
                                }
                            }
                        }

                        if text.len() > 50000 {
                            text.truncate(50000);
                            text.push_str("\n\n[Content truncated to 50000 characters]");
                        }
                        Ok(json!({
                            "url": url,
                            "content": text,
                            "length": original_len
                        }))
                    }
                    Err(e) => {
                        Ok(json!({ "error": format!("Failed to read response body: {}", e) }))
                    }
                }
            }
            Err(e) => Ok(json!({ "error": format!("Network error: {}", e) })),
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
        let tool = WebFetchTool::new("test-key".to_string());
        let decl = tool.declaration();

        assert_eq!(decl.name(), "web_fetch");
        assert!(decl.description().contains("process it with a prompt"));

        let params = decl.parameters();
        let params_json = serde_json::to_value(params).unwrap();
        assert_eq!(params_json["type"], "object");
        assert_eq!(params.required(), vec!["url".to_string()]);

        let properties = params.properties();
        assert!(properties.get("url").is_some());
        assert_eq!(properties["url"]["type"], "string");
        assert!(properties.get("prompt").is_some());
        assert_eq!(properties["prompt"]["type"], "string");
    }

    #[test]
    fn test_parse_args_success() {
        let tool = WebFetchTool::new("test-key".to_string());
        let args = json!({
            "url": "https://example.com"
        });

        let (url, prompt) = tool.parse_args(args).unwrap();
        assert_eq!(url, "https://example.com");
        assert!(prompt.is_none());
    }

    #[test]
    fn test_parse_args_with_prompt() {
        let tool = WebFetchTool::new("test-key".to_string());
        let args = json!({
            "url": "https://example.com",
            "prompt": "summarize this"
        });

        let (url, prompt) = tool.parse_args(args).unwrap();
        assert_eq!(url, "https://example.com");
        assert_eq!(prompt.unwrap(), "summarize this");
    }

    #[test]
    fn test_parse_args_missing_url() {
        let tool = WebFetchTool::new("test-key".to_string());
        let args = json!({});

        let result = tool.parse_args(args);
        assert!(result.is_err());
        match result {
            Err(FunctionError::ArgumentMismatch(msg)) => assert_eq!(msg, "Missing url"),
            _ => panic!("Expected ArgumentMismatch error"),
        }
    }

    #[test]
    fn test_parse_args_invalid_url_type() {
        let tool = WebFetchTool::new("test-key".to_string());
        let args = json!({
            "url": 123
        });

        let result = tool.parse_args(args);
        assert!(result.is_err());
        match result {
            Err(FunctionError::ArgumentMismatch(msg)) => assert_eq!(msg, "Missing url"),
            _ => panic!("Expected ArgumentMismatch error"),
        }
    }
}

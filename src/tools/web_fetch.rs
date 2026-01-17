use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use std::path::PathBuf;
use tracing::instrument;

pub struct WebFetchTool {
    _cwd: PathBuf,
}

impl WebFetchTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { _cwd: cwd }
    }
}

#[async_trait]
impl CallableFunction for WebFetchTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "web_fetch".to_string(),
            "Fetch the content of a web page from a URL.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    }
                }),
                vec!["url".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing url".to_string()))?;

        let client = match super::create_http_client() {
            Ok(c) => c,
            Err(e) => return Ok(json!({ "error": e })),
        };

        match client.get(url).send().await {
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
                    Err(e) => Ok(json!({ "error": format!("Failed to read response body: {}", e) })),
                }
            }
            Err(e) => Ok(json!({ "error": format!("Network error: {}", e) })),
        }
    }
}

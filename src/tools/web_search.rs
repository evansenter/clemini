use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tracing::instrument;

pub struct WebSearchTool {}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl CallableFunction for WebSearchTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "web_search".to_string(),
            "Search the web using DuckDuckGo's instant answer API.".to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    }
                }),
                vec!["query".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing query".to_string()))?;

        let client = match super::create_http_client() {
            Ok(c) => c,
            Err(e) => return Ok(json!({ "error": e })),
        };

        let url = "https://api.duckduckgo.com/";
        match client
            .get(url)
            .query(&[("q", query), ("format", "json")])
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    return Ok(json!({
                        "error": format!("HTTP error: {}", status),
                        "status": status.as_u16()
                    }));
                }

                match resp.json::<Value>().await {
                    Ok(data) => {
                        let abstract_text = data
                            .get("AbstractText")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let related_topics = data.get("RelatedTopics").and_then(|v| v.as_array());

                        let mut results = Vec::new();
                        if let Some(topics) = related_topics {
                            for topic in topics {
                                if let Some(text) = topic.get("Text").and_then(|v| v.as_str()) {
                                    results.push(text.to_string());
                                }
                            }
                        }

                        Ok(json!({
                            "query": query,
                            "abstract": abstract_text,
                            "related_topics": results
                        }))
                    }
                    Err(e) => {
                        Ok(json!({ "error": format!("Failed to parse JSON response: {}", e) }))
                    }
                }
            }
            Err(e) => Ok(json!({ "error": format!("Network error: {}", e) })),
        }
    }
}

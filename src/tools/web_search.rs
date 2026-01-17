use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tracing::instrument;

pub struct WebSearchTool {}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {}
    }

    fn parse_args(&self, args: Value) -> Result<String, FunctionError> {
        args.get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing query".to_string()))
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
        let query = self.parse_args(args)?;

        let client = match super::create_http_client() {
            Ok(c) => c,
            Err(e) => return Ok(json!({ "error": e })),
        };

        let url = "https://api.duckduckgo.com/";
        match client
            .get(url)
            .query(&[("q", query.as_str()), ("format", "json")])
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

#[cfg(test)]
mod tests {
    use super::*;
    use genai_rs::CallableFunction;
    use serde_json::json;

    #[test]
    fn test_declaration() {
        let tool = WebSearchTool::new();
        let decl = tool.declaration();

        assert_eq!(decl.name(), "web_search");
        assert_eq!(
            decl.description(),
            "Search the web using DuckDuckGo's instant answer API."
        );

        let params = decl.parameters();
        let params_json = serde_json::to_value(params).unwrap();
        assert_eq!(params_json["type"], "object");
        assert_eq!(params.required(), vec!["query".to_string()]);

        let properties = params.properties();
        assert!(properties.get("query").is_some());
        assert_eq!(properties["query"]["type"], "string");
    }

    #[test]
    fn test_parse_args_success() {
        let tool = WebSearchTool::new();
        let args = json!({
            "query": "rust programming"
        });

        let query = tool.parse_args(args).unwrap();
        assert_eq!(query, "rust programming");
    }

    #[test]
    fn test_parse_args_missing_query() {
        let tool = WebSearchTool::new();
        let args = json!({});

        let result = tool.parse_args(args);
        assert!(result.is_err());
        match result {
            Err(FunctionError::ArgumentMismatch(msg)) => assert_eq!(msg, "Missing query"),
            _ => panic!("Expected ArgumentMismatch error"),
        }
    }

    #[test]
    fn test_parse_args_invalid_query_type() {
        let tool = WebSearchTool::new();
        let args = json!({
            "query": 123
        });

        let result = tool.parse_args(args);
        assert!(result.is_err());
        match result {
            Err(FunctionError::ArgumentMismatch(msg)) => assert_eq!(msg, "Missing query"),
            _ => panic!("Expected ArgumentMismatch error"),
        }
    }
}

use async_trait::async_trait;
use genai_rs::{CallableFunction, FunctionDeclaration, FunctionError, FunctionParameters};
use serde_json::{Value, json};
use tracing::instrument;

#[derive(Debug, PartialEq)]
struct SearchArgs {
    query: String,
    allowed_domains: Option<Vec<String>>,
    blocked_domains: Option<Vec<String>>,
}

#[derive(Default)]
pub struct WebSearchTool {}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {}
    }

    fn parse_args(&self, args: Value) -> Result<SearchArgs, FunctionError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| FunctionError::ArgumentMismatch("Missing query".to_string()))?;

        let allowed_domains = args.get("allowed_domains").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
        });

        let blocked_domains = args.get("blocked_domains").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
        });

        Ok(SearchArgs {
            query,
            allowed_domains,
            blocked_domains,
        })
    }
}

#[async_trait]
impl CallableFunction for WebSearchTool {
    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration::new(
            "web_search".to_string(),
            "Search the web using DuckDuckGo's instant answer API. Returns: {results[], query}"
                .to_string(),
            FunctionParameters::new(
                "object".to_string(),
                json!({
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "allowed_domains": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of domains to include (suffix matching)"
                    },
                    "blocked_domains": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of domains to exclude (suffix matching)"
                    }
                }),
                vec!["query".to_string()],
            ),
        )
    }

    #[instrument(skip(self, args))]
    async fn call(&self, args: Value) -> Result<Value, FunctionError> {
        let search_args = self.parse_args(args)?;

        let client = match super::create_http_client() {
            Ok(c) => c,
            Err(e) => return Ok(json!({ "error": e })),
        };

        let url = "https://api.duckduckgo.com/";
        match client
            .get(url)
            .query(&[("q", search_args.query.as_str()), ("format", "json")])
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
                                let text = topic.get("Text").and_then(|v| v.as_str());
                                let first_url = topic.get("FirstURL").and_then(|v| v.as_str());

                                if let Some(url_str) = first_url {
                                    if !self.should_include(url_str, &search_args) {
                                        continue;
                                    }
                                } else if search_args.allowed_domains.is_some() {
                                    // If we have no URL but allowed_domains is set,
                                    // we can't verify it belongs to an allowed domain.
                                    // For DDG "RelatedTopics", some might be just text.
                                    // We'll skip them if filtering is active and we can't verify.
                                    continue;
                                }

                                if let Some(t) = text {
                                    results.push(t.to_string());
                                }
                            }
                        }

                        Ok(json!({
                            "query": search_args.query,
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

impl WebSearchTool {
    fn should_include(&self, url_str: &str, args: &SearchArgs) -> bool {
        let domain = match url::Url::parse(url_str) {
            Ok(u) => u.host_str().map(|s| s.to_lowercase()),
            Err(_) => None,
        };

        let domain = match domain {
            Some(d) => d,
            None => return args.allowed_domains.is_none(),
        };

        if let Some(allowed) = &args.allowed_domains
            && !allowed.iter().any(|d| {
                domain == d.to_lowercase() || domain.ends_with(&format!(".{}", d.to_lowercase()))
            })
        {
            return false;
        }

        if let Some(blocked) = &args.blocked_domains
            && blocked.iter().any(|d| {
                domain == d.to_lowercase() || domain.ends_with(&format!(".{}", d.to_lowercase()))
            })
        {
            return false;
        }

        true
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
            "Search the web using DuckDuckGo's instant answer API. Returns: {results[], query}"
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

        let search_args = tool.parse_args(args).unwrap();
        assert_eq!(search_args.query, "rust programming");
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

    #[test]
    fn test_parse_args_with_domains() {
        let tool = WebSearchTool::new();
        let args = json!({
            "query": "rust",
            "allowed_domains": ["github.com", "rust-lang.org"],
            "blocked_domains": ["reddit.com"]
        });

        let search_args = tool.parse_args(args).unwrap();
        assert_eq!(search_args.query, "rust");
        assert_eq!(
            search_args.allowed_domains,
            Some(vec!["github.com".to_string(), "rust-lang.org".to_string()])
        );
        assert_eq!(
            search_args.blocked_domains,
            Some(vec!["reddit.com".to_string()])
        );
    }

    #[test]
    fn test_should_include_allowed() {
        let tool = WebSearchTool::new();
        let args = SearchArgs {
            query: "rust".to_string(),
            allowed_domains: Some(vec!["github.com".to_string()]),
            blocked_domains: None,
        };

        assert!(tool.should_include("https://github.com/rust-lang/rust", &args));
        assert!(tool.should_include("https://docs.github.com/en", &args));
        assert!(!tool.should_include("https://gitlab.com/repo", &args));
    }

    #[test]
    fn test_should_include_blocked() {
        let tool = WebSearchTool::new();
        let args = SearchArgs {
            query: "rust".to_string(),
            allowed_domains: None,
            blocked_domains: Some(vec!["reddit.com".to_string()]),
        };

        assert!(tool.should_include("https://github.com/rust-lang/rust", &args));
        assert!(!tool.should_include("https://reddit.com/r/rust", &args));
        assert!(!tool.should_include("https://old.reddit.com/r/rust", &args));
    }

    #[test]
    fn test_should_include_both() {
        let tool = WebSearchTool::new();
        let args = SearchArgs {
            query: "rust".to_string(),
            allowed_domains: Some(vec!["github.com".to_string()]),
            blocked_domains: Some(vec!["docs.github.com".to_string()]),
        };

        assert!(tool.should_include("https://github.com/rust-lang/rust", &args));
        assert!(!tool.should_include("https://docs.github.com/en", &args));
        assert!(!tool.should_include("https://google.com", &args));
    }
}

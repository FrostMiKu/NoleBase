//! Web tools: Tavily search and HTTP fetch.

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};

use super::util::{optional_usize, required_string};
use crate::agent::{Tool, ToolExecutionPolicy};

const TAVILY_SEARCH_URL: &str = "https://api.tavily.com/search";
const MAX_WEB_SEARCH_RESULTS: usize = 10;
pub const MAX_WEB_SEARCH_DOMAINS: usize = 300;
const MAX_FETCH_BYTES: u64 = 1_000_000;

pub struct WebSearch {
    pub client: Client,
    pub api_key: String,
}

#[async_trait::async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        "web_search"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::Network
    }

    fn description(&self) -> &'static str {
        "Search the web for current information. Returns an optional answer and ranked results with titles, URLs, snippets, scores, and publication dates when available."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1, "maxLength": 1000 },
                "topic": {
                    "type": "string", "enum": ["general", "news", "finance"],
                    "default": "general"
                },
                "search_depth": {
                    "type": "string", "enum": ["basic", "advanced"],
                    "default": "basic"
                },
                "max_results": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_WEB_SEARCH_RESULTS, "default": 5
                },
                "time_range": {
                    "type": "string", "enum": ["day", "week", "month", "year"]
                },
                "include_answer": { "type": "boolean", "default": false },
                "include_domains": {
                    "type": "array",
                    "items": { "type": "string", "minLength": 1 },
                    "minItems": 1,
                    "maxItems": MAX_WEB_SEARCH_DOMAINS,
                    "uniqueItems": true,
                    "description": "Only return results from these domains."
                },
                "exclude_domains": {
                    "type": "array",
                    "items": { "type": "string", "minLength": 1 },
                    "minItems": 1,
                    "maxItems": MAX_WEB_SEARCH_DOMAINS,
                    "uniqueItems": true,
                    "description": "Exclude results from these domains."
                }
            },
            "required": ["query"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let (query, request) = tavily_search_request(&self.api_key, input)?;
        let response = self
            .client
            .post(TAVILY_SEARCH_URL)
            .json(&request)
            .send()
            .await
            .context("calling Tavily Search API")?;
        let status = response.status();
        let bytes = read_limited_http_body(response, "Tavily response").await?;
        let body = String::from_utf8(bytes).context("Tavily response is not UTF-8")?;
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("detail")
                        .and_then(Value::as_str)
                        .or_else(|| value.get("error").and_then(Value::as_str))
                        .map(str::to_owned)
                })
                .unwrap_or(body);
            bail!("Tavily API returned {status}: {message}");
        }
        let response: Value =
            serde_json::from_str(&body).context("decoding Tavily search response")?;
        compact_tavily_response(&query, &response)
    }
}

pub fn tavily_search_request(api_key: &str, input: &Value) -> Result<(String, Value)> {
    let query = required_string(input, "query")?.trim();
    if query.is_empty() {
        bail!("search query must not be empty");
    }
    if query.chars().count() > 1_000 {
        bail!("search query exceeds 1000 characters");
    }
    let topic = optional_choice(input, "topic", "general", &["general", "news", "finance"])?;
    let search_depth = optional_choice(input, "search_depth", "basic", &["basic", "advanced"])?;
    let max_results = optional_usize(input, "max_results", 5, MAX_WEB_SEARCH_RESULTS)?;
    let include_answer = input
        .get("include_answer")
        .map(|value| {
            value
                .as_bool()
                .context("field include_answer must be a boolean")
        })
        .transpose()?
        .unwrap_or(false);
    let time_range = input
        .get("time_range")
        .map(|_| optional_choice(input, "time_range", "", &["day", "week", "month", "year"]))
        .transpose()?;

    let mut request = json!({
        "api_key": api_key,
        "query": query,
        "topic": topic,
        "search_depth": search_depth,
        "max_results": max_results,
        "include_answer": include_answer,
        "include_raw_content": false,
        "include_images": false
    });
    if let Some(time_range) = time_range {
        request["time_range"] = Value::String(time_range.to_string());
    }
    if let Some(domains) = optional_string_array(input, "include_domains", MAX_WEB_SEARCH_DOMAINS)?
    {
        request["include_domains"] = json!(domains);
    }
    if let Some(domains) = optional_string_array(input, "exclude_domains", MAX_WEB_SEARCH_DOMAINS)?
    {
        request["exclude_domains"] = json!(domains);
    }

    Ok((query.to_string(), request))
}

pub fn compact_tavily_response(query: &str, response: &Value) -> Result<String> {
    let results = response
        .get("results")
        .and_then(Value::as_array)
        .context("Tavily response has no results array")?
        .iter()
        .map(|result| {
            let mut compact = serde_json::Map::new();
            for field in ["title", "url", "content", "score", "published_date"] {
                if let Some(value) = result.get(field).filter(|value| !value.is_null()) {
                    compact.insert(field.to_string(), value.clone());
                }
            }
            Value::Object(compact)
        })
        .collect::<Vec<_>>();
    let mut compact = json!({ "query": query, "results": results });
    if let Some(answer) = response.get("answer").and_then(Value::as_str) {
        compact["answer"] = Value::String(answer.to_string());
    }
    serde_json::to_string(&compact).context("encoding Tavily search results")
}

fn optional_choice<'a>(
    input: &'a Value,
    field: &str,
    default: &'a str,
    choices: &[&str],
) -> Result<&'a str> {
    let value = input
        .get(field)
        .map(|value| {
            value
                .as_str()
                .with_context(|| format!("field {field} must be a string"))
        })
        .transpose()?
        .unwrap_or(default);
    if !choices.contains(&value) {
        bail!("field {field} must be one of {}", choices.join(", "));
    }
    Ok(value)
}

fn optional_string_array(
    input: &Value,
    field: &str,
    maximum: usize,
) -> Result<Option<Vec<String>>> {
    let Some(value) = input.get(field) else {
        return Ok(None);
    };
    let values = value
        .as_array()
        .with_context(|| format!("field {field} must be an array of strings"))?;
    if values.is_empty() || values.len() > maximum {
        bail!("field {field} must contain between 1 and {maximum} strings");
    }
    values
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .with_context(|| format!("field {field} must be an array of strings"))?
                .trim();
            if value.is_empty() {
                bail!("field {field} must not contain empty strings");
            }
            Ok(value.to_string())
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

pub(crate) async fn read_limited_http_body(response: reqwest::Response, label: &str) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_FETCH_BYTES)
    {
        bail!("{label} exceeds 1 MB");
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("reading {label}"))?;
        if bytes.len().saturating_add(chunk.len()) as u64 > MAX_FETCH_BYTES {
            bail!("{label} exceeds 1 MB");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub fn web_fetch_content(content_type: Option<&str>, bytes: Vec<u8>) -> Result<String> {
    let text = String::from_utf8(bytes).context("response is not UTF-8 text")?;
    let media_type = content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if !media_type.is_some_and(|value| {
        value.eq_ignore_ascii_case("text/html")
            || value.eq_ignore_ascii_case("application/xhtml+xml")
    }) {
        return Ok(text);
    }

    htmd::HtmlToMarkdown::builder()
        .skip_tags(vec![
            "script", "style", "noscript", "template", "svg", "canvas",
        ])
        .build()
        .convert(&text)
        .context("converting HTML response to Markdown")
}

//! Web tools: Tavily search and HTTP fetch.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::util::{backgrounded_job_result, display_path, optional_usize, required_string};
use super::workspace_quota::{
    check_workspace_write, workspace_destination, workspace_dir, workspace_used_bytes,
    MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_TOTAL_BYTES,
};
use crate::agent::{
    canonical_root, AgentJobsHandle, JobKind, Tool, ToolExecutionPolicy, ToolOutput,
};

const TAVILY_SEARCH_URL: &str = "https://api.tavily.com/search";
const MAX_WEB_SEARCH_RESULTS: usize = 10;
pub const MAX_WEB_SEARCH_DOMAINS: usize = 300;
const MAX_FETCH_BYTES: u64 = 1_000_000;
/// Largest body `http` returns inline; larger responses are truncated
/// with `truncated: true` and may be fetched in `range` slices or saved to disk.
const MAX_HTTP_RESPONSE_BYTES: u64 = 1_000_000;

pub struct SearchWeb {
    pub client: Client,
    pub api_key: String,
}

#[async_trait::async_trait]
impl Tool for SearchWeb {
    fn name(&self) -> &'static str {
        "search_web"
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

pub struct Http {
    root: PathBuf,
    client: Client,
    workspace_write_lock: Arc<tokio::sync::Mutex<()>>,
    jobs: AgentJobsHandle,
}

impl Http {
    pub fn new(root: &Path, client: Client, jobs: AgentJobsHandle) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            client,
            workspace_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            jobs,
        })
    }

    async fn render_response(&self, response: reqwest::Response) -> Result<String> {
        let status = response.status();
        let version = http_version(response.version());
        let final_url = response.url().to_string();
        let headers = response_headers(response.headers());
        let content_length = response_total_length(response.headers());
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading HTTP response")?;
            let next = bytes.len().saturating_add(chunk.len());
            if next as u64 > MAX_HTTP_RESPONSE_BYTES {
                bytes.extend_from_slice(&chunk[..MAX_HTTP_RESPONSE_BYTES as usize - bytes.len()]);
                truncated = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        let received_bytes = bytes.len() as u64;
        let (body, body_encoding) = match String::from_utf8(bytes) {
            Ok(text) => (text, "utf8"),
            Err(error) => (
                base64::engine::general_purpose::STANDARD.encode(error.into_bytes()),
                "base64",
            ),
        };
        let mut payload = json!({
            "status": status.as_u16(),
            "reason": status.canonical_reason().unwrap_or_default(),
            "version": version,
            "url": final_url,
            "headers": headers,
            "body": body,
            "body_encoding": body_encoding,
            "received_bytes": received_bytes,
            "truncated": truncated,
        });
        if let Some(length) = content_length {
            payload["content_length"] = json!(length);
        }
        serde_json::to_string_pretty(&payload).context("encoding HTTP response")
    }

    async fn save_response(
        root: PathBuf,
        workspace_write_lock: Arc<tokio::sync::Mutex<()>>,
        response: reqwest::Response,
        destination: PathBuf,
        destination_text: String,
    ) -> Result<String> {
        // Network downloads may execute concurrently, but quota accounting and
        // no-overwrite publication must observe one workspace state at a time.
        let _workspace_write_guard = workspace_write_lock.lock().await;
        let status = response.status();
        if !status.is_success() {
            bail!("download failed: HTTP {status}");
        }
        let final_url = response.url().to_string();
        let content_length = response.content_length();
        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .filter(|value| value.contains('/'))
            .map(str::to_owned);

        if let Some(length) = content_length {
            check_workspace_write(&root, &destination, length)?;
        }
        let workspace = workspace_dir(&root);
        let used = workspace_used_bytes(&workspace)?;
        let budget = MAX_WORKSPACE_FILE_BYTES.min(MAX_WORKSPACE_TOTAL_BYTES.saturating_sub(used));
        let staged = staged_path(&destination);
        let output = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .await
            .with_context(|| format!("creating staging file {}", staged.display()))?;
        let mut guard = StagedCleanup {
            path: staged.clone(),
            committed: false,
        };
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        {
            let mut output = output;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("reading download body")?;
                let next = written.saturating_add(chunk.len() as u64);
                if next > budget {
                    bail!("download exceeds the workspace limit: {next} bytes would exceed the allowed budget");
                }
                hasher.update(&chunk);
                output
                    .write_all(&chunk)
                    .await
                    .context("writing download staging file")?;
                written = next;
            }
            if let Some(length) = content_length {
                if written != length {
                    bail!("download ended after {written} bytes, expected {length}");
                }
            }
            output
                .sync_all()
                .await
                .context("syncing download staging file")?;
        }
        match fs::symlink_metadata(&destination) {
            Ok(_) => bail!("destination already exists: {destination_text}"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("checking destination {destination_text}"));
            }
        }
        tokio::fs::hard_link(&staged, &destination)
            .await
            .with_context(|| format!("publishing {}", destination.display()))?;
        let _ = tokio::fs::remove_file(&staged).await;
        guard.committed = true;

        let token = format!("sha256:{}", hex_lower(&hasher.finalize()));
        let mut result = json!({
            "path": display_path(&root, &destination),
            "bytes": written,
            "url": final_url,
            "sha256": token,
        });
        if let Some(media_type) = media_type {
            result["media_type"] = json!(media_type);
        }
        serde_json::to_string(&result).context("encoding download result")
    }
}

#[async_trait::async_trait]
impl Tool for Http {
    fn name(&self) -> &'static str {
        "http"
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::Network
    }

    fn description(&self) -> &'static str {
        "Low-level HTTP request tool. To fetch a web page or document as readable content, prefer `read` (it converts HTML to Markdown and extracts PDF/office text). Use this tool only when you must control the request precisely — a custom method, headers, body, or byte range — or save the raw response bytes to a file with `save_to`. Returns the unprocessed status, final URL, response headers, and body (UTF-8 text or base64); inline bodies are capped at 1 MiB and report `truncated` plus `content_length` when larger."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "minLength": 1,
                    "description": "HTTP or HTTPS URL"
                },
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"],
                    "default": "GET",
                    "description": "HTTP method; defaults to GET"
                },
                "headers": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "default": {},
                    "description": "Extra request headers keyed by header name"
                },
                "body": {
                    "type": "string",
                    "description": "Request body text; omit when the method needs no body"
                },
                "range": {
                    "type": "object",
                    "properties": {
                        "offset": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "First byte offset (0-based) to request"
                        },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Number of bytes to request"
                        }
                    },
                    "required": ["offset", "limit"],
                    "additionalProperties": false,
                    "description": "Optional byte range, sent as a Range header to page through a large response"
                },
                "save_to": {
                    "type": "string",
                    "minLength": 1,
                    "description": "New file path relative to workspace; streams the response body to disk instead of returning it inline and reports path, byte count, sha256, media type, and final URL"
                },
                "background": {
                    "type": "boolean",
                    "default": false,
                    "description": "With save_to: run the download as a background job and return immediately. The result is delivered automatically when the download finishes; the job keeps running when the Agent is interrupted and can be stopped with the jobs tool."
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let url = reqwest::Url::parse(required_string(input, "url")?)
            .context("parsing HTTP request URL")?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("http URL must use http or https");
        }
        let method_text = input
            .get("method")
            .map(|value| {
                value
                    .as_str()
                    .context("field method must be a string")
                    .map(str::to_uppercase)
            })
            .transpose()?
            .unwrap_or_else(|| "GET".to_string());
        let method = reqwest::Method::from_bytes(method_text.as_bytes())
            .context("field method is not a valid HTTP method")?;
        let save_to = match input.get("save_to") {
            None => None,
            Some(value) => {
                let text = value
                    .as_str()
                    .context("field save_to must be a string")?
                    .trim();
                if text.is_empty() {
                    bail!("field save_to must not be empty");
                }
                Some(text.to_string())
            }
        };
        let destination = match &save_to {
            Some(text) => Some(workspace_destination(&self.root, text)?),
            None => None,
        };
        let background = input
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if background && destination.is_none() {
            bail!("field background requires field save_to");
        }

        let mut request = self.client.request(method, url.clone());
        request = apply_headers(request, input.get("headers"))?;
        if let Some(range) = input.get("range") {
            request = request.header(reqwest::header::RANGE, range_header(range)?);
        }
        if let Some(body) = input.get("body") {
            request = request.body(
                body.as_str()
                    .context("field body must be a string")?
                    .to_string(),
            );
        }

        if let (Some(destination), Some(destination_text)) = (destination, save_to.clone()) {
            if background {
                let label = download_label(&url, &destination_text);
                let started = self.jobs.start_background(JobKind::Download, &label)?;
                let jobs = self.jobs.clone();
                let id = started.id.clone();
                let root = self.root.clone();
                let write_lock = Arc::clone(&self.workspace_write_lock);
                tokio::spawn(async move {
                    let outcome = async {
                        let response = request.send().await.context("sending HTTP request")?;
                        Self::save_response(
                            root,
                            write_lock,
                            response,
                            destination,
                            destination_text,
                        )
                        .await
                    }
                    .await
                    .map_err(|error| format!("{error:#}"));
                    jobs.settle(&id, outcome);
                });
                return backgrounded_job_result(
                    &started.id,
                    "The download keeps running in the background; its result will be delivered automatically as a [background job] frame. Continue with other work or end your turn—do not wait for it.",
                );
            }
            let response = request.send().await.context("sending HTTP request")?;
            return Self::save_response(
                self.root.clone(),
                Arc::clone(&self.workspace_write_lock),
                response,
                destination,
                destination_text,
            )
            .await;
        }
        let response = request.send().await.context("sending HTTP request")?;
        self.render_response(response).await
    }

    async fn execute_output(&self, input: &Value) -> Result<ToolOutput> {
        let text = self.execute(input).await?;
        ToolOutput::structured_json(&text, &["body"])
    }
}

/// Compact job label for a background download: file name plus host.
fn download_label(url: &reqwest::Url, destination_text: &str) -> String {
    let file = destination_text
        .rsplit('/')
        .next()
        .unwrap_or(destination_text);
    let host = url.host_str().map(str::to_string).unwrap_or_default();
    format!("{file} <- {host}")
}

fn apply_headers(
    mut request: reqwest::RequestBuilder,
    headers: Option<&Value>,
) -> Result<reqwest::RequestBuilder> {
    let Some(headers) = headers else {
        return Ok(request);
    };
    let object = headers
        .as_object()
        .context("field headers must be an object")?;
    for (name, value) in object {
        request = request.header(
            name,
            value.as_str().context("header values must be strings")?,
        );
    }
    Ok(request)
}

fn response_headers(headers: &reqwest::header::HeaderMap) -> Value {
    let mut output = serde_json::Map::new();
    for (name, value) in headers {
        let text = value.to_str().map(str::to_owned).unwrap_or_else(|_| {
            format!(
                "base64:{}",
                base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
            )
        });
        output
            .entry(name.as_str().to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("HTTP header entry is always an array")
            .push(Value::String(text));
    }
    Value::Object(output)
}

fn http_version(version: reqwest::Version) -> &'static str {
    match version {
        reqwest::Version::HTTP_09 => "HTTP/0.9",
        reqwest::Version::HTTP_10 => "HTTP/1.0",
        reqwest::Version::HTTP_11 => "HTTP/1.1",
        reqwest::Version::HTTP_2 => "HTTP/2",
        reqwest::Version::HTTP_3 => "HTTP/3",
        _ => "unknown",
    }
}

fn range_header(value: &Value) -> Result<String> {
    let object = value.as_object().context("field range must be an object")?;
    let offset = object
        .get("offset")
        .and_then(Value::as_u64)
        .context("field range.offset must be a non-negative integer")?;
    let limit = object
        .get("limit")
        .and_then(Value::as_u64)
        .context("field range.limit must be a positive integer")?;
    if limit == 0 {
        bail!("field range.limit must be greater than zero");
    }
    let end = offset.saturating_add(limit).saturating_sub(1);
    Ok(format!("bytes={offset}-{end}"))
}

/// The full resource length from `Content-Range` (partial responses) or the
/// declared `Content-Length`, whichever describes the complete body.
fn response_total_length(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if let Some(range) = headers
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(total) = range.rsplit('/').next().and_then(|text| text.parse().ok()) {
            return Some(total);
        }
    }
    headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.parse().ok())
}

/// A hidden staging name in the destination's parent directory so the final
/// rename stays on one filesystem and stays atomic.
fn staged_path(destination: &Path) -> PathBuf {
    let parent = destination
        .parent()
        .expect("workspace destination always has a parent");
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    parent.join(format!(".{name}.part-{:016x}", fastrand::u64(..)))
}

/// Removes the staging file unless the download was published.
struct StagedCleanup {
    path: PathBuf,
    committed: bool,
}

impl Drop for StagedCleanup {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Lowercase hex encoding used by content tokens.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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

pub(crate) async fn read_limited_http_body(
    response: reqwest::Response,
    label: &str,
) -> Result<Vec<u8>> {
    read_http_body_with_limit(response, label, MAX_FETCH_BYTES).await
}

pub(crate) async fn read_http_body_with_limit(
    response: reqwest::Response,
    label: &str,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        bail!("{label} exceeds the {max_bytes} byte limit");
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("reading {label}"))?;
        if bytes.len().saturating_add(chunk.len()) as u64 > max_bytes {
            bail!("{label} exceeds the {max_bytes} byte limit");
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

#[cfg(test)]
mod tests {
    use crate::agent::JobStatus;
    use std::fs;
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::net::TcpListener;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    use base64::Engine as _;

    use super::*;
    use crate::agent::test_support::TestFutureResultExt;
    use crate::storage::Storage;

    fn serve_response(
        status: &'static str,
        headers: Vec<(&'static str, &'static str)>,
        body: Vec<u8>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            write!(stream, "HTTP/1.1 {status}\r\n").unwrap();
            for (name, value) in headers {
                write!(stream, "{name}: {value}\r\n").unwrap();
            }
            write!(
                stream,
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        (format!("http://{address}/response"), server)
    }

    fn serve_echo() -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                reader.read_exact(&mut body).unwrap();
            }
            let echoed = format!(
                "{}|{}",
                request_line.trim_end(),
                String::from_utf8_lossy(&body)
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                echoed.len(),
                echoed
            )
            .unwrap();
        });
        (format!("http://{address}/echo"), server)
    }

    fn fresh_root() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        (directory, storage.root)
    }

    fn http_tool(root: &Path) -> Http {
        let (events, _receiver) = crate::agent::test_support::event_channel();
        Http::new(root, reqwest::Client::new(), AgentJobsHandle::new(events)).unwrap()
    }

    fn workspace(root: &Path) -> PathBuf {
        workspace_dir(root)
    }

    fn entries(workspace: &Path) -> Vec<String> {
        fs::read_dir(workspace)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// A scripted one-shot HTTP server on 127.0.0.1 serving `responses` in
    /// order (one per accepted connection).
    struct RawResponse {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    fn server(responses: Vec<RawResponse>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for expected in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).unwrap();
                let status_line = match expected.status {
                    200 => "HTTP/1.1 200 OK",
                    206 => "HTTP/1.1 206 Partial Content",
                    302 => "HTTP/1.1 302 Found",
                    404 => "HTTP/1.1 404 Not Found",
                    other => panic!("unexpected status {other}"),
                };
                let mut head = format!("{status_line}\r\n");
                for (name, value) in &expected.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&expected.body);
                let _ = stream.flush();
            }
        });
        (format!("http://{address}"), handle)
    }

    fn ok_response(body: Vec<u8>, content_type: Option<&str>) -> RawResponse {
        let mut headers = Vec::new();
        headers.push(("Content-Length".to_string(), body.len().to_string()));
        if let Some(content_type) = content_type {
            headers.push(("Content-Type".to_string(), content_type.to_string()));
        }
        RawResponse {
            status: 200,
            headers,
            body,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_preserves_json_body_status_and_repeated_headers() {
        let body = br#"{"answer":42,"raw":"  spaced  "}"#.to_vec();
        let (url, server) = serve_response(
            "422 Unprocessable Entity",
            vec![
                ("Content-Type", "application/json"),
                ("X-Trace", "first"),
                ("X-Trace", "second"),
            ],
            body.clone(),
        );
        let (_directory, root) = fresh_root();
        let tool = http_tool(&root);

        let output = tool.execute(&json!({ "url": url })).await.unwrap();
        server.join().unwrap();
        let response: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(response["status"], 422);
        assert_eq!(response["reason"], "Unprocessable Entity");
        assert_eq!(response["version"], "HTTP/1.1");
        assert_eq!(response["body"], String::from_utf8(body).unwrap());
        assert_eq!(response["body_encoding"], "utf8");
        assert_eq!(
            response["headers"]["content-type"],
            json!(["application/json"])
        );
        assert_eq!(response["headers"]["x-trace"], json!(["first", "second"]));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_base64_encodes_binary_body() {
        let body = vec![0xff, 0x00, 0x80];
        let (url, server) = serve_response(
            "200 OK",
            vec![("Content-Type", "application/octet-stream")],
            body.clone(),
        );
        let (_directory, root) = fresh_root();
        let tool = http_tool(&root);

        let output = tool.execute(&json!({ "url": url })).await.unwrap();
        server.join().unwrap();
        let response: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(response["status"], 200);
        assert_eq!(response["body_encoding"], "base64");
        assert_eq!(
            response["body"],
            base64::engine::general_purpose::STANDARD.encode(body)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_sends_method_headers_and_body() {
        let (url, server) = serve_echo();
        let (_directory, root) = fresh_root();
        let tool = http_tool(&root);

        let output = tool
            .execute(&json!({
                "url": url,
                "method": "POST",
                "headers": { "X-Token": "secret" },
                "body": "payload"
            }))
            .await
            .unwrap();
        server.join().unwrap();
        let response: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(response["status"], 200);
        let echoed = response["body"].as_str().unwrap();
        assert!(
            echoed.starts_with("POST /echo"),
            "echoed request line: {echoed}"
        );
        assert!(
            echoed.ends_with("|payload"),
            "echoed request body: {echoed}"
        );
    }

    #[test]
    fn schema_requires_url_and_uses_network_policy() {
        let (_directory, root) = fresh_root();
        let tool = http_tool(&root);
        let schema = tool.input_schema();
        let required = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(required, ["url"]);
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(tool.execution_policy(), ToolExecutionPolicy::Network);
        assert!(schema["properties"].get("range").is_some());
        assert!(schema["properties"].get("save_to").is_some());
    }

    #[test]
    fn rejects_non_http_urls_and_empty_save_to() {
        let (_directory, root) = fresh_root();
        let tool = http_tool(&root);
        let cases = [
            json!({"url": "ftp://example.com/a.bin", "save_to": "a.bin"}),
            json!({"url": "file:///etc/passwd", "save_to": "a.bin"}),
            json!({"url": "not a url", "save_to": "a.bin"}),
            json!({"url": "", "save_to": "a.bin"}),
            json!({"url": "   ", "save_to": "a.bin"}),
            json!({"url": "https://example.com/a.bin", "save_to": ""}),
            json!({"url": "https://example.com/a.bin", "save_to": "  "}),
        ];
        for input in cases {
            assert!(
                tool.execute(&input).returns_err(),
                "expected rejection for {input}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_to_streams_binary_content_and_reports_metadata_and_hash() {
        let (_directory, root) = fresh_root();
        let body = (0u8..=255).cycle().take(200_000).collect::<Vec<u8>>();
        let (url, handle) = server(vec![ok_response(
            body.clone(),
            Some("application/octet-stream; charset=binary"),
        )]);
        let result: Value = serde_json::from_str(
            &http_tool(&root)
                .execute(&json!({"url": url, "save_to": "binaries/blob.bin"}))
                .await
                .unwrap(),
        )
        .unwrap();
        handle.join().unwrap();

        assert_eq!(result["path"], "workspace/binaries/blob.bin");
        assert_eq!(result["bytes"], body.len() as u64);
        assert_eq!(result["media_type"], "application/octet-stream");
        assert!(result["url"].as_str().unwrap().starts_with("http://"));
        assert_eq!(
            result["sha256"],
            format!("sha256:{}", hex_lower(&Sha256::digest(&body)))
        );
        assert_eq!(
            fs::read(workspace(&root).join("binaries/blob.bin")).unwrap(),
            body
        );
        assert_eq!(
            entries(&workspace(&root)),
            vec!["binaries"],
            "no staging file may remain after a successful download"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_download_returns_immediately_and_delivers_result() {
        let (_directory, root) = fresh_root();
        let body = b"background payload".to_vec();
        let (url, handle) = server(vec![ok_response(body.clone(), None)]);
        let (events, _receiver) = crate::agent::test_support::event_channel();
        let jobs = AgentJobsHandle::new(events);
        let tool = Http::new(&root, reqwest::Client::new(), jobs.clone()).unwrap();
        let started = std::time::Instant::now();
        let output = tool
            .execute(&json!({
                "url": url,
                "save_to": "binaries/async.bin",
                "background": true
            }))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["backgrounded"], json!(true));
        let job = value["job"].as_str().unwrap().to_string();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if !jobs
                .rows()
                .iter()
                .any(|row| row.id == job && row.status == JobStatus::Running)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "download never settled"
            );
        }
        handle.join().unwrap();
        let deliveries = jobs.take_deliveries();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].status, JobStatus::Done);
        let result: Value = serde_json::from_str(&deliveries[0].result).unwrap();
        assert_eq!(result["bytes"], body.len() as u64);
        assert_eq!(
            fs::read(workspace(&root).join("binaries/async.bin")).unwrap(),
            body
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_requires_save_to() {
        let (_directory, root) = fresh_root();
        let (url, _handle) = server(vec![ok_response(b"x".to_vec(), None)]);
        let error = http_tool(&root)
            .execute(&json!({"url": url, "background": true}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("requires field save_to"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_to_follows_redirects_and_reports_the_final_url() {
        let (_directory, root) = fresh_root();
        let (url, handle) = server(vec![
            RawResponse {
                status: 302,
                headers: vec![("Location".to_string(), "/final.bin".to_string())],
                body: Vec::new(),
            },
            ok_response(b"redirected".to_vec(), None),
        ]);
        let result: Value = serde_json::from_str(
            &http_tool(&root)
                .execute(&json!({"url": url, "save_to": "redirected.bin"}))
                .await
                .unwrap(),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(result["bytes"], 10);
        assert!(result["url"].as_str().unwrap().ends_with("/final.bin"));
        assert_eq!(
            fs::read(workspace(&root).join("redirected.bin")).unwrap(),
            b"redirected"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_to_http_failure_leaves_no_file() {
        let (_directory, root) = fresh_root();
        let (url, handle) = server(vec![RawResponse {
            status: 404,
            headers: vec![("Content-Length".to_string(), "9".to_string())],
            body: b"not found".to_vec(),
        }]);
        let error = http_tool(&root)
            .execute(&json!({"url": url, "save_to": "missing.bin"}))
            .await
            .unwrap_err();
        handle.join().unwrap();
        assert!(error.to_string().contains("HTTP 404"));
        assert!(!workspace(&root).join("missing.bin").exists());
        assert!(entries(&workspace(&root)).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_to_refuses_existing_destinations_without_touching_them() {
        let (_directory, root) = fresh_root();
        fs::write(workspace(&root).join("existing.bin"), b"keep me").unwrap();
        let error = http_tool(&root)
            .execute(&json!({
                "url": "https://example.invalid/new.bin",
                "save_to": "existing.bin"
            }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("already exists"));
        assert_eq!(
            fs::read(workspace(&root).join("existing.bin")).unwrap(),
            b"keep me"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn save_to_rejects_absolute_traversal_and_symlinked_destinations() {
        let (_directory, root) = fresh_root();
        let url = "https://example.invalid/file.bin";
        let outside = tempfile::tempdir().unwrap();
        let escape = outside.path().join("outside.bin");
        std::os::unix::fs::symlink(&escape, workspace(&root).join("linked")).unwrap();
        let cases = [
            json!({"url": url, "save_to": "/etc/passwd"}),
            json!({"url": url, "save_to": "../outside.bin"}),
            json!({"url": url, "save_to": "linked/inside.bin"}),
            json!({"url": url, "save_to": "a/../../outside.bin"}),
        ];
        for input in cases {
            let error = http_tool(&root).execute(&input).await.unwrap_err();
            assert!(
                error.to_string().contains("workspace") || error.to_string().contains("symlink")
            );
            assert!(!escape.exists(), "no bytes may escape through a symlink");
        }
        assert!(!workspace(&root).join("outside.bin").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_to_declared_oversized_is_rejected_without_writing() {
        let (_directory, root) = fresh_root();
        let (url, handle) = server(vec![RawResponse {
            status: 200,
            headers: vec![(
                "Content-Length".to_string(),
                (MAX_WORKSPACE_FILE_BYTES + 1).to_string(),
            )],
            body: b"tiny".to_vec(),
        }]);
        let error = http_tool(&root)
            .execute(&json!({"url": url, "save_to": "oversized.bin"}))
            .await
            .unwrap_err();
        handle.join().unwrap();
        assert!(error.to_string().contains("64 MiB"));
        assert!(entries(&workspace(&root)).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_to_undeclared_oversized_stream_is_cut_off_and_cleaned_up() {
        let (_directory, root) = fresh_root();
        let sparse = workspace(&root).join("filler.bin");
        fs::File::create(&sparse)
            .unwrap()
            .set_len(MAX_WORKSPACE_TOTAL_BYTES - 1024 * 1024)
            .unwrap();
        let oversized = vec![0u8; 2 * 1024 * 1024];
        let (url, handle) = server(vec![RawResponse {
            status: 200,
            headers: vec![],
            body: oversized,
        }]);
        let error = http_tool(&root)
            .execute(&json!({"url": url, "save_to": "overflow.bin"}))
            .await
            .unwrap_err();
        handle.join().unwrap();
        assert!(error.to_string().contains("workspace limit"));
        assert!(!workspace(&root).join("overflow.bin").exists());
        assert_eq!(
            entries(&workspace(&root)),
            vec!["filler.bin"],
            "no partial file may remain"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_to_total_workspace_quota_is_enforced() {
        let (_directory, root) = fresh_root();
        fs::File::create(workspace(&root).join("full.bin"))
            .unwrap()
            .set_len(MAX_WORKSPACE_TOTAL_BYTES)
            .unwrap();
        let (url, handle) = server(vec![ok_response(b"one byte too many".to_vec(), None)]);
        let error = http_tool(&root)
            .execute(&json!({"url": url, "save_to": "nope.bin"}))
            .await
            .unwrap_err();
        handle.join().unwrap();
        assert!(error.to_string().contains("512 MiB"));
        assert!(entries(&workspace(&root)) == vec!["full.bin"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_to_concurrent_requests_serialize_workspace_quota_accounting() {
        let (_directory, root) = fresh_root();
        let headroom = 1;
        fs::File::create(workspace(&root).join("filler.bin"))
            .unwrap()
            .set_len(MAX_WORKSPACE_TOTAL_BYTES - headroom)
            .unwrap();
        let body = vec![0u8; headroom as usize];
        let (url, handle) = server(vec![
            ok_response(body.clone(), None),
            ok_response(body, None),
        ]);
        let tool = http_tool(&root);
        let first_input = json!({"url": url.clone(), "save_to": "first.bin"});
        let second_input = json!({"url": url, "save_to": "second.bin"});

        let (first, second) = tokio::join!(tool.execute(&first_input), tool.execute(&second_input));
        handle.join().unwrap();

        let outcomes = [("first.bin", first), ("second.bin", second)];
        assert_eq!(
            outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        for (file_name, result) in outcomes {
            let saved = workspace(&root).join(file_name);
            match result {
                Ok(_) => assert!(saved.exists()),
                Err(error) => {
                    assert!(error.to_string().contains("512 MiB"));
                    assert!(!saved.exists());
                }
            }
        }
        assert!(workspace_used_bytes(&workspace(&root)).unwrap() <= MAX_WORKSPACE_TOTAL_BYTES);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_to_cancelled_removes_its_staging_file() {
        let (_directory, root) = fresh_root();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n7\r\npartial\r\n",
                )
                .unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let tool = http_tool(&root);
        let input = json!({
            "url": format!("http://{address}"),
            "save_to": "cancelled.bin"
        });
        let mut future = Box::pin(tool.execute(&input));

        tokio::select! {
            result = &mut future => panic!("download unexpectedly finished: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        drop(future);
        handle.join().unwrap();

        assert!(!workspace(&root).join("cancelled.bin").exists());
        assert!(entries(&workspace(&root)).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_body_caps_at_one_mib_and_reports_truncation() {
        let total = MAX_HTTP_RESPONSE_BYTES + 10;
        let body = vec![b'a'; total as usize];
        let (url, handle) = server(vec![RawResponse {
            status: 200,
            headers: vec![("Content-Length".to_string(), total.to_string())],
            body,
        }]);
        let (_directory, root) = fresh_root();
        let output = http_tool(&root)
            .execute(&json!({"url": url}))
            .await
            .unwrap();
        handle.join().unwrap();
        let response: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(response["status"], 200);
        assert_eq!(response["truncated"], true);
        assert_eq!(response["received_bytes"], MAX_HTTP_RESPONSE_BYTES);
        assert_eq!(response["content_length"], total);
        assert_eq!(response["body_encoding"], "utf8");
        assert_eq!(
            response["body"].as_str().unwrap().len(),
            MAX_HTTP_RESPONSE_BYTES as usize
        );
    }

    #[test]
    fn range_header_formats_byte_ranges() {
        assert_eq!(
            range_header(&json!({"offset": 0, "limit": 100})).unwrap(),
            "bytes=0-99"
        );
        assert_eq!(
            range_header(&json!({"offset": 50, "limit": 10})).unwrap(),
            "bytes=50-59"
        );
        assert!(range_header(&json!({"offset": 0, "limit": 0})).is_err());
        assert!(range_header(&json!({"offset": 0})).is_err());
        assert!(range_header(&json!({"limit": 10})).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn range_response_reports_content_range_total() {
        let (url, handle) = server(vec![RawResponse {
            status: 206,
            headers: vec![
                ("Content-Range".to_string(), "bytes 0-99/5000".to_string()),
                ("Content-Length".to_string(), "100".to_string()),
            ],
            body: vec![b'x'; 100],
        }]);
        let (_directory, root) = fresh_root();
        let output = http_tool(&root)
            .execute(&json!({"url": url, "range": {"offset": 0, "limit": 100}}))
            .await
            .unwrap();
        handle.join().unwrap();
        let response: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(response["status"], 206);
        assert_eq!(response["truncated"], false);
        assert_eq!(response["received_bytes"], 100);
        assert_eq!(response["content_length"], 5000);
    }
}

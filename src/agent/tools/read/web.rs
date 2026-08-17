//! HTTP(S) parser for the unified `read` tool.
//!
//! Fetches with bounded body reads and reports categorized failures: transport
//! phases (`timeout`, `connect`, ...) and HTTP status errors with selected
//! headers plus a bounded, sanitized body preview.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::super::web::{read_http_body_with_limit, web_fetch_content};
use super::document::{self, DocumentFormat, DocumentSourceKey};
use super::paging::{add_structured_page, json_response_len, line_window, page_extracted_text};
use super::{ParseContext, ReadParser, ReadPayload, Target};

const MAX_WEB_READ_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WEB_ERROR_PREVIEW_BYTES: usize = 2 * 1024;

pub(crate) struct WebParser;

#[async_trait::async_trait]
impl ReadParser for WebParser {
    fn name(&self) -> &'static str {
        "web"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::Web { .. })
    }

    async fn parse(
        &self,
        ctx: &ParseContext,
        target: &Target,
        input: &Value,
    ) -> Result<ReadPayload> {
        let Target::Web { url, range } = target else {
            bail!("web parser received non-web target");
        };
        let (offset, limit) = line_window(*range, input)?;
        let response = ctx
            .client
            .get(url)
            .send()
            .await
            .map_err(web_request_error)?;
        if !response.status().is_success() {
            return Err(web_http_status_error(response).await);
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = read_http_body_with_limit(response, "response", MAX_WEB_READ_BYTES)
            .await
            .context("web fetch failed during response_body")?;
        let url_path = Path::new(url.split(['?', '#']).next().unwrap_or(url));
        let detected = DocumentFormat::from_bytes_or_path(&bytes, url_path);
        let (content, format) = if let Some(format) = detected {
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            let document = ctx
                .documents
                .get_or_extract(DocumentSourceKey::Content(digest), || {
                    document::extract_markdown(bytes, format)
                })
                .await
                .context("web fetch failed during document_extraction")?;
            (document.markdown.clone(), document.format.label())
        } else {
            (
                Arc::<str>::from(
                    web_fetch_content(content_type.as_deref(), bytes)
                        .context("web fetch failed during content_processing")?,
                ),
                "text",
            )
        };
        let page = page_extracted_text(&content, offset, limit, json_response_len)?;
        let mut payload = json!({
            "format": format,
            "content_type": content_type,
        });
        add_structured_page(&mut payload, page, url, offset, limit);
        Ok(ReadPayload::Structured(payload))
    }
}

fn web_request_error(error: reqwest::Error) -> anyhow::Error {
    let phase = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_redirect() {
        "redirect"
    } else if error.is_builder() {
        "request_build"
    } else if error.is_body() {
        "request_body"
    } else if error.is_decode() {
        "response_decode"
    } else {
        "request_send"
    };
    anyhow::Error::new(error).context(format!("web fetch failed during {phase}"))
}

async fn web_http_status_error(mut response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let final_url = response.url().to_string();
    let headers = response.headers().clone();
    let mut preview = Vec::with_capacity(MAX_WEB_ERROR_PREVIEW_BYTES);
    let mut truncated = false;
    let mut preview_error = None;

    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = MAX_WEB_ERROR_PREVIEW_BYTES.saturating_sub(preview.len());
                if chunk.len() > remaining {
                    preview.extend_from_slice(&chunk[..remaining]);
                    truncated = true;
                    break;
                }
                preview.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                preview_error = Some(error);
                break;
            }
        }
    }

    let mut details = vec![
        "web fetch failed during http_status".to_string(),
        format!("HTTP: {status}"),
        format!("URL: {final_url}"),
    ];
    for (label, name) in [
        ("Content-Type", "content-type"),
        ("Retry-After", "retry-after"),
        ("X-Request-ID", "x-request-id"),
        ("CF-Ray", "cf-ray"),
    ] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            details.push(format!("{label}: {value}"));
        }
    }
    let sanitized_body = String::from_utf8_lossy(&preview)
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let body = sanitized_body
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !body.is_empty() {
        let suffix = if truncated { " [truncated]" } else { "" };
        details.push(format!("Response body{suffix}: {body}"));
    }
    if let Some(error) = preview_error {
        details.push(format!("Response body read error: {error}"));
    }
    anyhow::anyhow!(details.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Arc;

    use serde_json::{json, Value};

    use super::super::test_support::{serve_once, simple_pdf};
    use super::super::Read;
    use super::MAX_WEB_ERROR_PREVIEW_BYTES;
    use crate::agent::{ReadTracker, Tool};

    #[tokio::test(flavor = "current_thread")]
    async fn url_selectors_page_reader_mode_text() {
        let (url, server) = serve_once("text/plain", b"first\nsecond\nthird\n".to_vec());
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute(&json!({"path": format!("{url}:2-2")}))
            .await
            .unwrap();
        server.join().unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["items"], json!(["second"]));
        assert_eq!(parsed["next"], format!("{url}:3-3"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn web_http_failures_report_status_headers_and_bounded_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = format!(
            "rate limited {}",
            "x".repeat(MAX_WEB_ERROR_PREVIEW_BYTES * 2)
        );
        let body_len = body.len();
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
            write!(
                stream,
                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: text/plain\r\nRetry-After: 42\r\nX-Request-ID: request-123\r\nContent-Length: {body_len}\r\nConnection: close\r\n\r\n{body}"
            )
            .unwrap();
        });
        let url = format!("http://{address}");
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let error = read.execute(&json!({"path": url})).await.unwrap_err();
        server.join().unwrap();
        let message = error.to_string();
        assert!(message.contains("web fetch failed during http_status"));
        assert!(message.contains("HTTP: 429 Too Many Requests"));
        assert!(message.contains("Retry-After: 42"));
        assert!(message.contains("X-Request-ID: request-123"));
        assert!(message.contains("Response body [truncated]: rate limited"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn web_transport_failures_report_phase_and_complete_cause_chain() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{address}");
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let error = read.execute(&json!({"path": url})).await.unwrap_err();
        assert_eq!(error.to_string(), "web fetch failed during connect");
        let message = crate::agent::tool_error_message(&error);
        assert!(message.contains("web fetch failed during connect"));
        assert!(message.contains("error sending request for url"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_pdf_urls_extract_text_without_a_pdf_scheme() {
        let (url, server) = serve_once("application/pdf", simple_pdf("Remote PDF marker"));
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute(&json!({"path": format!("{url}/report.pdf:1-20")}))
            .await
            .unwrap();
        server.join().unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["format"], "pdf");
        assert!(parsed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("Remote PDF marker")));
    }
}

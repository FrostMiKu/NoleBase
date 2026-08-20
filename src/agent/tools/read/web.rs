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
use crate::agent::images::image_block_from_bytes;
use crate::image_data::{detect_image_format, MAX_IMAGE_BYTES};
use crate::provider::ImageSource;

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
        let (response, final_url, content_type) = fetch_web_response(&ctx.client, url).await?;
        let is_image_content_type = content_type
            .as_deref()
            .is_some_and(|value| value.starts_with("image/"));
        let body_limit = if is_image_content_type {
            MAX_IMAGE_BYTES
        } else {
            MAX_WEB_READ_BYTES
        };
        let bytes = read_http_body_with_limit(response, "response", body_limit)
            .await
            .context("web fetch failed during response_body")?;
        let magic_format = detect_image_format(&bytes);
        if is_image_content_type || magic_format.is_some() {
            // A declared image content type must decode as an image; a faked
            // content type or corrupted payload fails here explicitly instead
            // of falling through to document/UTF-8 parsing.
            magic_format.ok_or_else(|| anyhow::anyhow!("web fetch failed during image_decode"))?;
            if range.is_some() {
                bail!("line selectors are not supported for image targets");
            }
            let block = tokio::task::spawn_blocking(move || {
                image_block_from_bytes(
                    ImageSource::Url {
                        url: final_url.clone(),
                    },
                    final_url.clone(),
                    bytes,
                )
            })
            .await
            .context("web fetch failed during image_decode")??;
            return Ok(ReadPayload::Image(block));
        }
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

/// Extract the shared fetch boundary used by both the `read` web parser and
/// the agent's image source resolver: request send with transport phase
/// classification, HTTP status errors with a bounded preview, and the final
/// (post-redirect) URL plus response content type on success.
pub(crate) async fn fetch_web_response(
    client: &reqwest::Client,
    url: &str,
) -> Result<(reqwest::Response, String, Option<String>)> {
    let response = client.get(url).send().await.map_err(web_request_error)?;
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if !response.status().is_success() {
        return Err(web_http_status_error(response).await);
    }
    Ok((response, final_url, content_type))
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
    use crate::agent::{SnapshotStore, Tool};
    use crate::provider::ImageSource;

    #[tokio::test(flavor = "current_thread")]
    async fn url_selectors_page_reader_mode_text() {
        let (url, server) = serve_once("text/plain", b"first\nsecond\nthird\n".to_vec());
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(SnapshotStore::default()),
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
    async fn url_response_converts_html_by_default() {
        let html = b"<!doctype html><html><body><h1>Reader heading</h1><script>ignored()</script></body></html>";
        let (url, server) = serve_once("text/html; charset=utf-8", html.to_vec());
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read.execute(&json!({"path": url})).await.unwrap();
        server.join().unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["format"], "text");
        assert_eq!(parsed["items"], json!(["# Reader heading"]));
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
            Arc::new(SnapshotStore::default()),
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
            Arc::new(SnapshotStore::default()),
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
            Arc::new(SnapshotStore::default()),
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

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = image::DynamicImage::new_rgb8(width, height);
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn url_images_return_native_tool_output_without_utf8_error() {
        let png = png_bytes(4, 2);
        let (url, server) = serve_once("image/png", png);
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute_output(&json!({"path": url.clone()}))
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(output.images.len(), 1);
        let block = &output.images[0];
        assert!(block.bytes.is_some());
        let final_url = reqwest::Url::parse(&url).unwrap().to_string();
        assert!(
            matches!(&block.source, ImageSource::Url { url: resolved } if resolved == &final_url)
        );
        assert!(!output.text.contains("base64"));
        assert!(output
            .text
            .starts_with(&format!("Read image {url} (4x2, image/png, ")));

        // A real image served with a non-image content type is still detected
        // by magic bytes and returned natively.
        let (url, server) = serve_once("text/plain", png_bytes(4, 2));
        let output = read.execute_output(&json!({"path": url})).await.unwrap();
        server.join().unwrap();
        assert_eq!(output.images.len(), 1);
        assert!(output.images[0].bytes.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn url_image_reads_preserve_existing_network_policy() {
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        // Redirect: the final URL becomes the image source.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let png = png_bytes(4, 2);
        let png_for_thread = png.clone();
        let redirect_location = format!("http://{address}/real.png");
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
                "HTTP/1.1 302 Found\r\nLocation: {redirect_location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            drop((stream, reader));
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
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                png_for_thread.len()
            )
            .unwrap();
            stream.write_all(&png_for_thread).unwrap();
            stream.flush().unwrap();
        });
        let output = read
            .execute_output(&json!({"path": format!("http://{address}/start")}))
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(output.images.len(), 1);
        let final_url = format!("http://{address}/real.png");
        assert!(matches!(&output.images[0].source, ImageSource::Url { url } if url == &final_url));

        // Fake image content type (no real image bytes) errors explicitly.
        let (url, server) = serve_once("image/png", b"this is not an image".to_vec());
        let error = read
            .execute_output(&json!({"path": url}))
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(error
            .to_string()
            .contains("web fetch failed during image_decode"));

        // Error status still reports the HTTP status preview.
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
            write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Type: image/png\r\nContent-Length: 11\r\nConnection: close\r\n\r\nmissing file"
            )
            .unwrap();
        });
        let error = read
            .execute_output(&json!({"path": format!("http://{address}/missing.png")}))
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(error.to_string().contains("HTTP: 404 Not Found"));
        assert!(error
            .to_string()
            .contains("web fetch failed during http_status"));

        // Oversized image payload fails the bounded read, not UTF-8 decoding.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let big_len = (8 * 1024 * 1024) + 1024;
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
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {big_len}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream.flush().unwrap();
        });
        let error = read
            .execute_output(&json!({"path": format!("http://{address}/big.png")}))
            .await
            .unwrap_err();
        server.join().unwrap();
        assert!(format!("{error:#}").contains("web fetch failed during response_body"));
        assert!(!format!("{error:#}").contains("not valid UTF-8"));
    }
}

//! Shared fixtures for the `read` tool tests, compiled only under `cargo test`.

use std::io::{BufRead as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};

use super::document::DocumentCache;
use super::{ParseContext, ReadParser, ReadPayload, Target};
use crate::agent::ReadTracker;
use crate::attachment::AttachmentStore;
use crate::storage::ATTACHMENTS_DIR;

pub(crate) fn attachment_ctx(directory: &Path) -> ParseContext {
    ParseContext {
        root: directory.to_path_buf(),
        reads: Arc::new(ReadTracker::default()),
        client: reqwest::Client::new(),
        attachments: AttachmentStore::new(directory.join(ATTACHMENTS_DIR)),
        documents: DocumentCache::default(),
    }
}

pub(crate) fn large_text(line_count: usize) -> String {
    (0..line_count)
        .map(|line| format!("line {line:05} {}", "x".repeat(20)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn simple_pdf(text: &str) -> Vec<u8> {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)");
    let stream = format!("BT /F1 12 Tf 72 720 Td ({escaped}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    pdf
}

pub(crate) fn simple_rtf(lines: &[&str]) -> Vec<u8> {
    format!("{{\\rtf1\\ansi {} }}", lines.join("\\par ")).into_bytes()
}

pub(crate) fn serve_once(
    content_type: &'static str,
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
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    });
    (format!("http://{address}"), server)
}

pub(crate) struct FakeParser;

#[async_trait::async_trait]
impl ReadParser for FakeParser {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::File { .. })
    }

    async fn parse(
        &self,
        _ctx: &ParseContext,
        _target: &Target,
        _input: &Value,
    ) -> Result<ReadPayload> {
        Ok(ReadPayload::Structured(json!({ "parsed_by": "fake" })))
    }
}

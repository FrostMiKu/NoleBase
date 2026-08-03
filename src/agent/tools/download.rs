//! The `download` tool: stream remote HTTP(S) bytes into a new file under
//! `workspace/main` without inspecting the content.
//!
//! The response body is streamed into a hidden staging file inside the
//! destination's parent directory while the SHA-256 is computed
//! incrementally. Quotas (64 MiB per file, 512 MiB workspace total) are
//! checked against a declared `Content-Length` before any byte is written and
//! re-enforced on every streamed chunk, so a growing or undeclared body can
//! never push the sandbox past its limits. Only a fully received download is
//! renamed into place; every error, cancellation, or read failure removes the
//! staging file, so a failed call never leaves a partial destination behind.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::util::{display_path, required_string};
use super::workspace_quota::{
    check_workspace_write, workspace_destination, workspace_dir, workspace_used_bytes,
    MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_TOTAL_BYTES,
};
use crate::agent::{canonical_root, Tool, ToolExecutionPolicy};

/// Download a remote file over HTTP(S) into `workspace/main`, preserving the
/// exact response bytes. Unlike `read` (which inspects content), this tool
/// never decodes the body; use it to keep a remote file before editing it or
/// before an optional `import_attachment`.
pub struct Download {
    root: PathBuf,
    client: Client,
    workspace_write_lock: tokio::sync::Mutex<()>,
}

impl Download {
    pub fn new(root: &Path, client: Client) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            root,
            client,
            workspace_write_lock: tokio::sync::Mutex::new(()),
        })
    }
}

#[async_trait::async_trait]
impl Tool for Download {
    fn name(&self) -> &'static str {
        "download"
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::Network
    }

    fn description(&self) -> &'static str {
        "Download a remote file over HTTP(S) and save its exact bytes to a new file under workspace/main. Use this when you need to preserve a remote file instead of only inspecting it; returns the saved path, byte count, media type, final URL after redirects, and a sha256:<hex> content token. The destination must not already exist."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "minLength": 1,
                    "description": "http(s) URL of the remote file to download"
                },
                "destination": {
                    "type": "string",
                    "minLength": 1,
                    "description": "New file path relative to workspace/main; missing parent directories are created, existing files are never overwritten"
                }
            },
            "required": ["url", "destination"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let url = required_string(input, "url")?.trim();
        if url.is_empty() {
            bail!("url must not be empty");
        }
        let parsed = reqwest::Url::parse(url).context("url is not a valid URL")?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("url must use the http or https scheme");
        }
        let destination_text = required_string(input, "destination")?.trim();
        if destination_text.is_empty() {
            bail!("destination must not be empty");
        }
        let destination = workspace_destination(&self.root, destination_text)?;
        // Network downloads may execute concurrently, but quota accounting and
        // no-overwrite publication must observe one workspace state at a time.
        let _workspace_write_guard = self.workspace_write_lock.lock().await;

        let response = self
            .client
            .get(parsed)
            .send()
            .await
            .context("requesting download")?;
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

        // Reject a declared body that alone violates either quota before any
        // byte is streamed; the streaming budget below then re-enforces both
        // limits against the actual bytes.
        if let Some(length) = content_length {
            check_workspace_write(&self.root, &destination, length)?;
        }
        let workspace = workspace_dir(&self.root);
        let used = workspace_used_bytes(&workspace)?;
        let budget = MAX_WORKSPACE_FILE_BYTES.min(MAX_WORKSPACE_TOTAL_BYTES.saturating_sub(used));

        let staged = staged_path(&destination);
        let output = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .await
            .with_context(|| format!("creating staging file {}", staged.display()))?;
        // The guard removes the staging file on every exit path — including a
        // cancelled task dropping this future mid-stream — until the complete
        // download has been renamed into place.
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
                    bail!(
                        "download exceeds the workspace limit: {next} bytes would exceed the allowed budget"
                    );
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
        // A hard link publishes without replacing an entry created after the
        // check above; staging and destination are always on one filesystem.
        tokio::fs::hard_link(&staged, &destination)
            .await
            .with_context(|| format!("publishing {}", destination.display()))?;
        let _ = tokio::fs::remove_file(&staged).await;
        guard.committed = true;

        let token = format!("sha256:{}", hex_lower(&hasher.finalize()));
        let mut result = json!({
            "path": display_path(&self.root, &destination),
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::agent::test_support::TestFutureResultExt;
    use crate::storage::Storage;

    fn fresh_root() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        (directory, storage.root)
    }

    fn download(root: &Path) -> Download {
        Download::new(root, reqwest::Client::new()).unwrap()
    }

    fn workspace(root: &Path) -> PathBuf {
        Storage::new(root).unwrap().agent_workspace_dir()
    }

    fn entries(workspace: &Path) -> Vec<String> {
        fs::read_dir(workspace)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// A scripted one-shot HTTP server on 127.0.0.1 serving `responses` in
    /// order (one per accepted connection). Each response controls status,
    /// headers, and body; the body is written verbatim after the header block.
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
                    302 => "HTTP/1.1 302 Found",
                    404 => "HTTP/1.1 404 Not Found",
                    other => panic!("unexpected status {other}"),
                };
                let mut head = format!("{status_line}\r\n");
                for (name, value) in &expected.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                // The client may disconnect mid-body (quota aborts); ignore
                // write failures so the serving thread never panics.
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

    #[test]
    fn schema_requires_url_and_destination_and_uses_network_policy() {
        let root = tempfile::tempdir().unwrap();
        let tool = download(root.path());
        let schema = tool.input_schema();
        let required = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(required, ["url", "destination"]);
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(tool.execution_policy(), ToolExecutionPolicy::Network);
    }

    #[test]
    fn rejects_non_http_urls_and_empty_arguments() {
        let root = tempfile::tempdir().unwrap();
        let tool = download(root.path());
        let cases = [
            json!({"url": "ftp://example.com/a.bin", "destination": "a.bin"}),
            json!({"url": "file:///etc/passwd", "destination": "a.bin"}),
            json!({"url": "not a url", "destination": "a.bin"}),
            json!({"url": "", "destination": "a.bin"}),
            json!({"url": "   ", "destination": "a.bin"}),
            json!({"url": "https://example.com/a.bin", "destination": ""}),
            json!({"url": "https://example.com/a.bin", "destination": "  "}),
        ];
        for input in cases {
            assert!(
                tool.execute(&input).returns_err(),
                "expected rejection for {input}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streams_binary_content_and_reports_metadata_and_hash() {
        let (_directory, root) = fresh_root();
        let body = (0u8..=255).cycle().take(200_000).collect::<Vec<u8>>();
        let (url, handle) = server(vec![ok_response(
            body.clone(),
            Some("application/octet-stream; charset=binary"),
        )]);
        let result: Value = serde_json::from_str(
            &download(&root)
                .execute(&json!({"url": url, "destination": "binaries/blob.bin"}))
                .await
                .unwrap(),
        )
        .unwrap();
        handle.join().unwrap();

        assert_eq!(result["path"], "workspace/main/binaries/blob.bin");
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
    async fn follows_redirects_and_reports_the_final_url() {
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
            &download(&root)
                .execute(&json!({"url": url, "destination": "redirected.bin"}))
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
    async fn http_failure_leaves_no_file() {
        let (_directory, root) = fresh_root();
        let (url, handle) = server(vec![RawResponse {
            status: 404,
            headers: vec![("Content-Length".to_string(), "9".to_string())],
            body: b"not found".to_vec(),
        }]);
        let error = download(&root)
            .execute(&json!({"url": url, "destination": "missing.bin"}))
            .await
            .unwrap_err();
        handle.join().unwrap();
        assert!(error.to_string().contains("HTTP 404"));
        assert!(!workspace(&root).join("missing.bin").exists());
        assert!(entries(&workspace(&root)).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refuses_existing_destinations_without_touching_them() {
        let (_directory, root) = fresh_root();
        fs::write(workspace(&root).join("existing.bin"), b"keep me").unwrap();
        let error = download(&root)
            .execute(&json!({
                "url": "https://example.invalid/new.bin",
                "destination": "existing.bin"
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
    async fn rejects_absolute_traversal_and_symlinked_destinations() {
        let (_directory, root) = fresh_root();
        let url = "https://example.invalid/file.bin";
        let outside = tempfile::tempdir().unwrap();
        let escape = outside.path().join("outside.bin");
        std::os::unix::fs::symlink(&escape, workspace(&root).join("linked")).unwrap();
        let cases = [
            json!({"url": url, "destination": "/etc/passwd"}),
            json!({"url": url, "destination": "../outside.bin"}),
            json!({"url": url, "destination": "linked/inside.bin"}),
            json!({"url": url, "destination": "a/../../outside.bin"}),
        ];
        for input in cases {
            let error = download(&root).execute(&input).await.unwrap_err();
            assert!(
                error.to_string().contains("workspace") || error.to_string().contains("symlink")
            );
            assert!(!escape.exists(), "no bytes may escape through a symlink");
        }
        assert!(!workspace(&root).join("outside.bin").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn declared_oversized_download_is_rejected_without_writing() {
        let (_directory, root) = fresh_root();
        let (url, handle) = server(vec![RawResponse {
            status: 200,
            headers: vec![(
                "Content-Length".to_string(),
                (MAX_WORKSPACE_FILE_BYTES + 1).to_string(),
            )],
            body: b"tiny".to_vec(),
        }]);
        let error = download(&root)
            .execute(&json!({"url": url, "destination": "oversized.bin"}))
            .await
            .unwrap_err();
        handle.join().unwrap();
        assert!(error.to_string().contains("64 MiB"));
        assert!(entries(&workspace(&root)).is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn undeclared_oversized_stream_is_cut_off_and_cleaned_up() {
        let (_directory, root) = fresh_root();
        // Leave only 1 MiB of headroom: a server that never declares a length
        // and sends more must be cut off mid-stream.
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
        let error = download(&root)
            .execute(&json!({"url": url, "destination": "overflow.bin"}))
            .await
            .unwrap_err();
        handle.join().unwrap();
        assert!(error.to_string().contains("workspace limit"));
        assert!(!workspace(&root).join("overflow.bin").exists());
        let remaining = entries(&workspace(&root));
        assert_eq!(remaining, vec!["filler.bin"], "no partial file may remain");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn total_workspace_quota_is_enforced() {
        let (_directory, root) = fresh_root();
        fs::File::create(workspace(&root).join("full.bin"))
            .unwrap()
            .set_len(MAX_WORKSPACE_TOTAL_BYTES)
            .unwrap();
        let (url, handle) = server(vec![ok_response(b"one byte too many".to_vec(), None)]);
        let error = download(&root)
            .execute(&json!({"url": url, "destination": "nope.bin"}))
            .await
            .unwrap_err();
        handle.join().unwrap();
        assert!(error.to_string().contains("512 MiB"));
        assert!(entries(&workspace(&root)) == vec!["full.bin"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_downloads_serialize_workspace_quota_accounting() {
        let (_directory, root) = fresh_root();
        let headroom = 1024 * 1024;
        fs::File::create(workspace(&root).join("filler.bin"))
            .unwrap()
            .set_len(MAX_WORKSPACE_TOTAL_BYTES - headroom)
            .unwrap();
        let body = vec![0u8; 700 * 1024];
        let rejected_length = body.len();
        let (url, handle) = server(vec![
            ok_response(body, None),
            // The second request is rejected from Content-Length before its
            // body is consumed. Omitting that body keeps the scripted server
            // from blocking in write_all on macOS after the client drops it.
            RawResponse {
                status: 200,
                headers: vec![("Content-Length".to_string(), rejected_length.to_string())],
                body: Vec::new(),
            },
        ]);
        let tool = download(&root);
        let first_input = json!({"url": url.clone(), "destination": "first.bin"});
        let second_input = json!({"url": url, "destination": "second.bin"});

        let (first, second) = tokio::join!(tool.execute(&first_input), tool.execute(&second_input));
        handle.join().unwrap();

        assert!(first.is_ok());
        assert!(second.unwrap_err().to_string().contains("512 MiB"));
        assert!(workspace(&root).join("first.bin").exists());
        assert!(!workspace(&root).join("second.bin").exists());
        assert!(workspace_used_bytes(&workspace(&root)).unwrap() <= MAX_WORKSPACE_TOTAL_BYTES);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_download_removes_its_staging_file() {
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
        let tool = download(&root);
        let input = json!({
            "url": format!("http://{address}"),
            "destination": "cancelled.bin"
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
}

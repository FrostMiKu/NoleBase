//! Markdown persistence for the note app.
//!
//! `CHAT.md` stores each message as a hidden HTML-comment block so that
//! delete/move stay reliable even when pasted content contains blank lines or
//! markdown:
//!
//! ```text
//! <!-- note-msg id="<uuid>" created_at="<rfc3339>" -->
//! message body (may be multi-line)
//! <!-- /note-msg -->
//! ```
//!
//! Mutations are *surgical* (append a block, or remove the exact block for an
//! id) rather than full rewrites, so manual edits made via `$EDITOR` are never
//! clobbered.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};

use crate::model::Message;

const OPEN_PREFIX: &str = "<!-- note-msg";
const OPEN_SUFFIX: &str = "-->";
const CLOSE_MARKER: &str = "<!-- /note-msg -->";

const CHAT_FILE: &str = "CHAT.md";
const TODO_FILE: &str = "TODO.md";

/// Filesystem locations backing the notes.
#[derive(Debug, Clone)]
pub struct Storage {
    pub root: PathBuf,
    pub chat_path: PathBuf,
    pub todo_path: PathBuf,
}

impl Storage {
    /// Build a storage rooted at `~/.note`.
    pub fn default_root() -> Result<Self> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        let root = home.join(".note");
        Self::new(root)
    }

    /// Build a storage rooted at the given directory (used by tests).
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        Ok(Self {
            chat_path: root.join(CHAT_FILE),
            todo_path: root.join(TODO_FILE),
            root,
        })
    }

    /// Create the root directory and default files if they are missing.
    pub fn ensure_files(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        if !self.chat_path.exists() {
            fs::write(&self.chat_path, "")?;
        }
        if !self.todo_path.exists() {
            fs::write(&self.todo_path, "# TODO\n\n")?;
        }
        Ok(())
    }

    /// Parse all message blocks from `CHAT.md`.
    pub fn load_messages(&self) -> Result<Vec<Message>> {
        let text = fs::read_to_string(&self.chat_path).unwrap_or_default();
        Ok(parse_messages(&text))
    }

    /// Append a new message block and return the constructed message.
    pub fn append_chat_message(&self, body: &str) -> Result<Message> {
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = Local::now();
        let msg = Message {
            id,
            created_at,
            body: body.to_string(),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.chat_path)
            .with_context(|| format!("opening {}", self.chat_path.display()))?;
        file.write_all(render_block(&msg).as_bytes())?;
        Ok(msg)
    }

    /// Remove the message block matching `id`. Returns `true` if found.
    pub fn remove_message_by_id(&self, id: &str) -> Result<bool> {
        let text = fs::read_to_string(&self.chat_path).unwrap_or_default();
        let Some((start, end)) = find_block_range(&text, id) else {
            return Ok(false);
        };
        // Consume one trailing newline so we don't leave a blank line behind.
        let mut end = end;
        if text[end..].starts_with('\n') {
            end += 1;
        }
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..start]);
        out.push_str(&text[end..]);
        fs::write(&self.chat_path, out)?;
        Ok(true)
    }

    /// Append a message to `TODO.md` as a markdown task, then remove it from
    /// the chat. Append happens first so a failure cannot lose data.
    pub fn move_to_todo(&self, msg: &Message) -> Result<()> {
        append_todo_task(&self.todo_path, msg)?;
        let removed = self.remove_message_by_id(&msg.id)?;
        debug_assert!(removed, "moved message not found in chat after append");
        Ok(())
    }

    /// Append a message to a target markdown file under the root, then remove
    /// it from the chat. Append happens first.
    pub fn move_to_markdown(&self, target: &Path, msg: &Message) -> Result<()> {
        let safe = self.validate_target(target)?;
        append_markdown_section(&safe, msg)?;
        let removed = self.remove_message_by_id(&msg.id)?;
        debug_assert!(removed, "moved message not found in chat after append");
        Ok(())
    }

    /// Create a new markdown file from a user-entered name, returning its path.
    pub fn create_named_file(&self, name: &str) -> Result<PathBuf> {
        let file_name = normalize_new_name(name)?;
        let path = self.root.join(&file_name);
        if !path.exists() {
            fs::write(&path, format!("# {}\n\n", stem(&file_name)))?;
        }
        Ok(path)
    }

    /// List `.md` files in the root, excluding `CHAT.md`, sorted by name.
    pub fn list_markdown_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        fs::create_dir_all(&self.root).ok();
        for entry in fs::read_dir(&self.root)? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md")
                && path.file_name().and_then(|n| n.to_str()) != Some(CHAT_FILE)
            {
                files.push(path);
            }
        }
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        Ok(files)
    }

    /// Ensure a target path resolves to a `.md` file inside the root.
    pub fn validate_target(&self, target: &Path) -> Result<PathBuf> {
        let canonical_root = canonicalize_or(&self.root);
        let parent_ok = target
            .parent()
            .map(|p| canonicalize_or(p) == canonical_root)
            .unwrap_or(false);
        let is_md = target.extension().and_then(|e| e.to_str()) == Some("md");
        if !(parent_ok && is_md) {
            bail!("target must be a .md file inside ~/.note");
        }
        Ok(target.to_path_buf())
    }
}

fn canonicalize_or(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Normalize a user-entered file name: trim, require `.md`, reject traversal.
pub fn normalize_new_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("file name is empty");
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        bail!("file name must not contain path separators or '..'");
    }
    if Path::new(trimmed).is_absolute() {
        bail!("file name must not be absolute");
    }
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.ends_with(".md") || lower.ends_with(".markdown")) {
        Ok(format!("{trimmed}.md"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn stem(file_name: &str) -> &str {
    Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file_name)
}

/// Render a message as its persisted block.
pub fn render_block(msg: &Message) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<!-- note-msg id=\"{}\" created_at=\"{}\" -->\n",
        msg.id,
        msg.created_at.to_rfc3339()
    ));
    out.push_str(&msg.body);
    if !msg.body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(CLOSE_MARKER);
    out.push('\n');
    out
}

/// Parse all message blocks out of raw `CHAT.md` text.
pub fn parse_messages(text: &str) -> Vec<Message> {
    let mut messages = Vec::new();
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.starts_with(OPEN_PREFIX) && trimmed.ends_with(OPEN_SUFFIX) {
            if let Some((id, created_at)) = parse_open_marker(trimmed) {
                let mut body = String::new();
                i += 1;
                while i < lines.len() {
                    if lines[i].trim() == CLOSE_MARKER {
                        break;
                    }
                    body.push_str(lines[i]);
                    i += 1;
                }
                // Drop a single trailing newline so display matches input.
                if body.ends_with('\n') {
                    body.pop();
                }
                if let Ok(created_at) = DateTime::parse_from_rfc3339(&created_at) {
                    messages.push(Message {
                        id,
                        created_at: created_at.with_timezone(&Local),
                        body,
                    });
                }
            }
        }
        i += 1;
    }
    messages
}

fn parse_open_marker(line: &str) -> Option<(String, String)> {
    let id = extract_attr(line, "id=")?;
    let created_at = extract_attr(line, "created_at=")?;
    Some((id, created_at))
}

fn extract_attr(line: &str, key: &str) -> Option<String> {
    let idx = line.find(key)?;
    let after = &line[idx + key.len()..];
    let after = after.trim_start();
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &after[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

/// Return the byte range `[start, end)` of the block for `id`, markers included.
fn find_block_range(text: &str, id: &str) -> Option<(usize, usize)> {
    let needle_open = format!("<!-- note-msg id=\"{id}\"");
    let open_start = text.find(&needle_open)?;
    // End of the open marker line (including its newline).
    let after_open = &text[open_start..];
    let open_line_end = after_open.find('\n').map(|n| open_start + n + 1)?;
    let rest = &text[open_line_end..];
    let close_rel = rest.find(CLOSE_MARKER)?;
    let close_start = open_line_end + close_rel;
    let close_end = close_start + CLOSE_MARKER.len();
    Some((open_start, close_end))
}

fn append_todo_task(todo_path: &Path, msg: &Message) -> Result<()> {
    let mut content = String::new();
    content.push('\n');
    let mut lines = msg.body.lines();
    if let Some(first) = lines.next() {
        content.push_str(&format!("- [ ] {first}\n"));
    } else {
        content.push_str("- [ ] \n");
    }
    for cont in lines {
        content.push_str(&format!("      {cont}\n"));
    }
    append_text(todo_path, &content)?;
    Ok(())
}

fn append_markdown_section(path: &Path, msg: &Message) -> Result<()> {
    let ts = msg.created_at.format("%Y-%m-%d %H:%M");
    let mut content = String::new();
    content.push_str(&format!("\n## {ts}\n\n"));
    content.push_str(&msg.body);
    if !msg.body.ends_with('\n') {
        content.push('\n');
    }
    append_text(path, &content)?;
    Ok(())
}

fn append_text(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    // Ensure existing content ends in a newline before appending.
    if let Ok(meta) = file.metadata() {
        if meta.len() > 0 {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = OpenOptions::new().read(true).open(path)?;
            let mut tail = [0u8; 1];
            f.seek(SeekFrom::End(-1))?;
            if f.read(&mut tail)? == 1 && tail[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
    }
    file.write_all(content.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn msg(id: &str, body: &str) -> Message {
        Message {
            id: id.to_string(),
            created_at: Local.with_ymd_and_hms(2026, 6, 18, 17, 20, 0).unwrap(),
            body: body.to_string(),
        }
    }

    fn fresh() -> (tempfile::TempDir, Storage) {
        let dir = tempdir().unwrap();
        let st = Storage::new(dir.path()).unwrap();
        st.ensure_files().unwrap();
        (dir, st)
    }

    #[test]
    fn round_trip_single_message() {
        let (_dir, st) = fresh();
        st.append_chat_message("hello world").unwrap();
        let msgs = st.load_messages().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].body, "hello world");
    }

    #[test]
    fn preserves_multiline_body() {
        let (_dir, st) = fresh();
        let body = "line one\n\nline three\n- [ ] a checkbox";
        st.append_chat_message(body).unwrap();
        let msgs = st.load_messages().unwrap();
        assert_eq!(msgs[0].body, body);
    }

    #[test]
    fn parse_multiple_messages() {
        let (_dir, st) = fresh();
        st.append_chat_message("first").unwrap();
        st.append_chat_message("second").unwrap();
        st.append_chat_message("third").unwrap();
        let msgs = st.load_messages().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].body, "first");
        assert_eq!(msgs[2].body, "third");
    }

    #[test]
    fn remove_by_id_preserves_others() {
        let (_dir, st) = fresh();
        let a = st.append_chat_message("keep").unwrap();
        st.append_chat_message("drop").unwrap();
        assert!(st.remove_message_by_id(&a.id).unwrap());
        let msgs = st.load_messages().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].body, "drop");
    }

    #[test]
    fn move_to_todo_writes_checkbox() {
        let (_dir, st) = fresh();
        let m = st.append_chat_message("buy milk\nsecond line").unwrap();
        st.move_to_todo(&m).unwrap();
        let todo = fs::read_to_string(&st.todo_path).unwrap();
        assert!(todo.contains("- [ ] buy milk"));
        assert!(todo.contains("      second line"));
        assert!(st.load_messages().unwrap().is_empty());
    }

    #[test]
    fn move_to_markdown_writes_section() {
        let (_dir, st) = fresh();
        // First seed the chat so we can verify removal afterwards.
        let _seed = st.append_chat_message("idea!").unwrap();
        let m = msg("fixed-id", "idea!");
        // Drop a block with the fixed id into the chat file manually.
        fs::write(&st.chat_path, render_block(&m)).unwrap();
        let target = st.create_named_file("工作记录").unwrap();
        st.move_to_markdown(&target, &m).unwrap();
        let body = fs::read_to_string(&target).unwrap();
        assert!(body.contains("## 2026-06-18 17:20"));
        assert!(body.contains("idea!"));
        assert!(st.load_messages().unwrap().is_empty());
    }

    #[test]
    fn create_named_file_adds_extension() {
        let (_dir, st) = fresh();
        let p = st.create_named_file("笔记").unwrap();
        assert_eq!(p.file_name().unwrap(), "笔记.md");
        assert!(p.exists());
    }

    #[test]
    fn normalize_rejects_traversal() {
        assert!(normalize_new_name("../etc").is_err());
        assert!(normalize_new_name("a/b").is_err());
        assert!(normalize_new_name("/abs").is_err());
        assert!(normalize_new_name("").is_err());
        assert_eq!(normalize_new_name("ok").unwrap(), "ok.md");
        assert_eq!(normalize_new_name("ok.md").unwrap(), "ok.md");
    }

    #[test]
    fn list_excludes_chat() {
        let (_dir, st) = fresh();
        st.create_named_file("alpha").unwrap();
        st.create_named_file("beta").unwrap();
        let names: Vec<_> = st
            .list_markdown_files()
            .unwrap()
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
            .collect();
        assert!(names.contains(&"alpha.md".to_string()));
        assert!(names.contains(&"TODO.md".to_string()));
        assert!(!names.iter().any(|n| n == "CHAT.md"));
    }

    #[test]
    fn surgical_edit_preserves_unmanaged_text() {
        // Even if a user hand-edits content around blocks, removals must not
        // eat unrelated lines.
        let (_dir, st) = fresh();
        let text = "free text at top\n<!-- note-msg id=\"abc\" created_at=\"2026-06-18T17:20:00+08:00\" -->\nbody\n<!-- /note-msg -->\ntrailing\n";
        fs::write(&st.chat_path, text).unwrap();
        assert!(st.remove_message_by_id("abc").unwrap());
        let after = fs::read_to_string(&st.chat_path).unwrap();
        assert!(after.contains("free text at top"));
        assert!(after.contains("trailing"));
        assert!(!after.contains("note-msg"));
    }
}

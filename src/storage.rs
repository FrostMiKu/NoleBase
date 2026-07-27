//! Markdown persistence for Nole.
//!
//! `CHAT.md` stores each message as a hidden HTML-comment block so that
//! delete/move stay reliable even when pasted content contains blank lines or
//! markdown:
//!
//! ```text
//! <!-- nole-msg id="<uuid>" created_at="<rfc3339>" -->
//! message body (may be multi-line)
//! <!-- /nole-msg -->
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

use crate::model::{Message, NoteFile, SearchHit, TodoItem};

const OPEN_PREFIX: &str = "<!-- nole-msg";
const OPEN_SUFFIX: &str = "-->";
const CLOSE_MARKER: &str = "<!-- /nole-msg -->";

const CHAT_FILE: &str = "CHAT.md";
const TODO_FILE: &str = "TODO.md";
const ARCHIVE_FILE: &str = "ARCHIVE.md";
const CONFIG_DIR: &str = "config";
const AI_CONFIG_FILE: &str = "ai.toml";
const DATA_DIR: &str = "data";

/// Filesystem locations backing the notes.
#[derive(Debug, Clone)]
pub struct Storage {
    pub root: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub chat_path: PathBuf,
    pub todo_path: PathBuf,
    pub archive_path: PathBuf,
    pub ai_config_path: PathBuf,
}

impl Storage {
    /// Build a storage rooted at `~/.nole`.
    pub fn default_root() -> Result<Self> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        let root = home.join(".nole");
        Self::new(root)
    }

    /// Build a storage rooted at the given directory (used by tests).
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        Ok(Self {
            config_dir: root.join(CONFIG_DIR),
            data_dir: root.join(DATA_DIR),
            chat_path: root.join(CHAT_FILE),
            todo_path: root.join(TODO_FILE),
            archive_path: root.join(ARCHIVE_FILE),
            ai_config_path: root.join(CONFIG_DIR).join(AI_CONFIG_FILE),
            root,
        })
    }

    /// Create the storage layout and default files, migrating legacy root notes.
    pub fn ensure_files(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating {}", self.config_dir.display()))?;
        fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating {}", self.data_dir.display()))?;
        if !self.chat_path.exists() {
            fs::write(&self.chat_path, "")?;
        }
        if !self.todo_path.exists() {
            fs::write(&self.todo_path, "# TODO\n\n")?;
        }
        if !self.archive_path.exists() {
            fs::write(&self.archive_path, "# Archive\n\n")?;
        }
        if !self.ai_config_path.exists() {
            self.write_default_ai_config()?;
        }
        self.migrate_legacy_root_notes()?;
        Ok(())
    }

    fn write_default_ai_config(&self) -> Result<()> {
        const DEFAULT: &str = concat!(
            "# Anthropic Messages API configuration. Keep this file private.\n",
            "api_key = \"\"\n",
            "model = \"claude-sonnet-4-5\"\n",
            "base_url = \"https://api.anthropic.com\"\n",
            "max_tokens = 4096\n",
        );
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&self.ai_config_path)
            .with_context(|| format!("creating {}", self.ai_config_path.display()))?;
        file.write_all(DEFAULT.as_bytes())?;
        Ok(())
    }

    fn migrate_legacy_root_notes(&self) -> Result<()> {
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if !is_note_path(&path) || is_protected_note_name(&path) {
                continue;
            }
            let destination = self.data_dir.join(entry.file_name());
            if destination.exists() {
                continue;
            }
            fs::rename(&path, &destination).with_context(|| {
                format!(
                    "migrating legacy note {} to {}",
                    path.display(),
                    destination.display()
                )
            })?;
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
    /// the chat. Append happens first so a failure cannot lose data. Returns
    /// the bytes appended to `TODO.md` (used by undo).
    pub fn move_to_todo(&self, msg: &Message) -> Result<String> {
        let content = append_todo_task(&self.todo_path, msg)?;
        let removed = self.remove_message_by_id(&msg.id)?;
        debug_assert!(removed, "moved message not found in chat after append");
        Ok(content)
    }

    /// Parse the `- [ ]` / `- [x]` tasks out of `TODO.md`, in order.
    /// Continuation lines fold into the preceding task's text.
    pub fn load_todo_tasks(&self) -> Vec<TodoItem> {
        let text = fs::read_to_string(&self.todo_path).unwrap_or_default();
        let mut items: Vec<TodoItem> = Vec::new();
        for line in text.lines() {
            if let Some((checked, body)) = parse_task_line(line) {
                items.push(TodoItem {
                    checked,
                    text: body.to_string(),
                });
            } else if !line.trim().is_empty() {
                // A non-blank line that isn't a task belongs to the task above.
                if let Some(item) = items.last_mut() {
                    item.text.push('\n');
                    item.text.push_str(line.trim());
                }
            }
        }
        items
    }

    /// Flip the completion state of the `index`-th task in `TODO.md`.
    /// Returns `true` if a task at that index was toggled.
    pub fn toggle_todo_task(&self, index: usize) -> Result<bool> {
        let text = fs::read_to_string(&self.todo_path).unwrap_or_default();
        let mut out = String::with_capacity(text.len());
        let mut count = 0usize;
        let mut toggled = false;
        for line in text.split_inclusive('\n') {
            let is_task = parse_task_line(line.trim_end_matches('\n')).is_some();
            if is_task && count == index {
                if let Some(flip) = flip_task_line(line) {
                    out.push_str(&flip);
                    toggled = true;
                } else {
                    out.push_str(line);
                }
            } else {
                out.push_str(line);
            }
            if is_task {
                count += 1;
            }
        }
        if toggled {
            fs::write(&self.todo_path, out)?;
        }
        Ok(toggled)
    }

    /// Case-insensitive substring search across every managed `.md`/`.mb` file
    /// in `data/`. One hit per matching non-blank line. Capped to keep the
    /// result list bounded.
    pub fn search_file_lines(&self, query: &str) -> Vec<SearchHit> {
        let q = query.to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        const CAP: usize = 200;
        let mut out: Vec<SearchHit> = Vec::new();
        for path in self.list_markdown_files().unwrap_or_default() {
            let Ok(text) = self.read_note_file(&path) else {
                continue;
            };
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&q) {
                    let t = line.trim();
                    if t.is_empty() {
                        continue;
                    }
                    out.push(SearchHit::FileLine {
                        path: path.clone(),
                        line_no: i + 1,
                        text: t.to_string(),
                    });
                    if out.len() >= CAP {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Append a message to a managed data note or the root archive, then remove
    /// it from chat. Append happens first. Returns the bytes appended to the
    /// target (used by undo).
    pub fn move_to_markdown(&self, target: &Path, msg: &Message) -> Result<String> {
        let safe = self.validate_target(target)?;
        let content = append_markdown_section(&safe, msg)?;
        let removed = self.remove_message_by_id(&msg.id)?;
        debug_assert!(removed, "moved message not found in chat after append");
        Ok(content)
    }

    /// Re-insert a previously removed message block into `CHAT.md` (used by
    /// undo). Keeps the original id and timestamp.
    pub fn restore_message_to_chat(&self, msg: &Message) -> Result<()> {
        append_text(&self.chat_path, &render_block(msg))?;
        Ok(())
    }

    /// Remove the first occurrence of `needle` from `path`. Returns `true` if
    /// found and removed. Used by undo to clean up a filed copy.
    pub fn remove_first_occurrence(&self, path: &Path, needle: &str) -> Result<bool> {
        let text = fs::read_to_string(path).unwrap_or_default();
        if let Some(idx) = text.find(needle) {
            let mut out = String::with_capacity(text.len() - needle.len());
            out.push_str(&text[..idx]);
            out.push_str(&text[idx + needle.len()..]);
            fs::write(path, out)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Rewrite the block for `msg.id` with `msg`'s current body, preserving the
    /// id and timestamp. Returns `false` if no block matches. Used by in-app
    /// message editing (and undo of an edit).
    pub fn replace_message(&self, msg: &Message) -> Result<bool> {
        let text = fs::read_to_string(&self.chat_path).unwrap_or_default();
        let Some((start, mut end)) = find_block_range(&text, &msg.id) else {
            return Ok(false);
        };
        if text[end..].starts_with('\n') {
            end += 1;
        }
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..start]);
        out.push_str(&render_block(msg));
        out.push_str(&text[end..]);
        fs::write(&self.chat_path, out)?;
        Ok(true)
    }

    /// Create a new note in `data/` from a user-entered name, returning its path.
    /// Existing filesystem entries are never overwritten.
    pub fn create_named_file(&self, name: &str) -> Result<PathBuf> {
        let file_name = normalize_new_name(name)?;
        if is_protected_note_name(Path::new(&file_name)) {
            bail!("{file_name} is reserved and cannot be created");
        }

        fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating {}", self.data_dir.display()))?;
        let path = self.data_dir.join(&file_name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("creating new note file {}", path.display()))?;
        file.write_all(format!("# {}\n\n", stem(&file_name)).as_bytes())?;
        Ok(path)
    }

    /// Rename a managed data note to `new_name` (normalized), returning the
    /// new path. Refuses protected names and never overwrites an existing entry.
    pub fn rename_file(&self, from: &Path, new_name: &str) -> Result<PathBuf> {
        let from = self.validate_target(from)?;
        if is_protected_note_name(&from) {
            bail!("{} is protected and cannot be renamed", from.display());
        }

        let name = normalize_new_name(new_name)?;
        if is_protected_note_name(Path::new(&name)) {
            bail!("{name} is reserved and cannot be used as a rename target");
        }
        let to = self.data_dir.join(&name);
        if to.file_name() == from.file_name() {
            return Ok(from);
        }
        match fs::symlink_metadata(&to) {
            Ok(_) => {
                bail!("a file named {name} already exists");
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", to.display()));
            }
        }
        fs::rename(&from, &to)
            .with_context(|| format!("renaming {} to {}", from.display(), to.display()))?;
        Ok(to)
    }

    /// Delete a managed data note. Protected files cannot be deleted.
    pub fn delete_file(&self, path: &Path) -> Result<()> {
        let path = self.validate_target(path)?;
        if is_protected_note_name(&path) {
            bail!("{} is protected and cannot be deleted", path.display());
        }
        fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
        Ok(())
    }

    /// Read a managed note after applying the same path checks used by
    /// mutating operations.
    pub fn read_note_file(&self, path: &Path) -> Result<String> {
        let path = self.validate_target(path)?;
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }

    /// List flat `.md` and `.mb` notes under `data/`, most recently modified first.
    pub fn list_note_files(&self) -> Result<Vec<NoteFile>> {
        let mut files = Vec::new();
        fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating {}", self.data_dir.display()))?;
        for entry in fs::read_dir(&self.data_dir)? {
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }

            let path = entry.path();
            if is_note_path(&path) {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                files.push(NoteFile { path, modified });
            }
        }
        files.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
        });
        Ok(files)
    }

    /// List only the paths, preserving the sidebar's recent-first order.
    pub fn list_markdown_files(&self) -> Result<Vec<PathBuf>> {
        Ok(self
            .list_note_files()?
            .into_iter()
            .map(|file| file.path)
            .collect())
    }

    /// Ensure a target is a flat data note or one of the protected root files.
    /// Existing targets are canonicalized in full; symlinks are always rejected.
    pub fn validate_target(&self, target: &Path) -> Result<PathBuf> {
        if !is_note_path(target) {
            bail!(
                "target must have a .md or .mb extension: {}",
                target.display()
            );
        }

        let canonical_root = fs::canonicalize(&self.root)
            .with_context(|| format!("resolving note root {}", self.root.display()))?;
        let canonical_data = fs::canonicalize(&self.data_dir).with_context(|| {
            format!("resolving note data directory {}", self.data_dir.display())
        })?;
        match fs::symlink_metadata(target) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    bail!("symlink note targets are not allowed: {}", target.display());
                }
                if !metadata.file_type().is_file() {
                    bail!("note target is not a regular file: {}", target.display());
                }
                let canonical_target = fs::canonicalize(target)
                    .with_context(|| format!("resolving note target {}", target.display()))?;
                let parent = canonical_target.parent();
                let is_data_note = parent == Some(canonical_data.as_path());
                let is_special = parent == Some(canonical_root.as_path())
                    && is_protected_note_name(&canonical_target);
                if !is_data_note && !is_special {
                    bail!(
                        "target must be a direct child of {}: {}",
                        self.data_dir.display(),
                        target.display()
                    );
                }
                Ok(canonical_target)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = target
                    .parent()
                    .context("note target has no parent directory")?;
                let canonical_parent = fs::canonicalize(parent)
                    .with_context(|| format!("resolving target parent {}", parent.display()))?;
                let is_data_note = canonical_parent == canonical_data;
                let is_special =
                    canonical_parent == canonical_root && is_protected_note_name(target);
                if !is_data_note && !is_special {
                    bail!(
                        "target must be a direct child of {}: {}",
                        self.data_dir.display(),
                        target.display()
                    );
                }
                Ok(target.to_path_buf())
            }
            Err(error) => {
                Err(error).with_context(|| format!("checking note target {}", target.display()))
            }
        }
    }
}

fn is_note_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("mb")
        })
}

fn is_protected_note_name(path: &Path) -> bool {
    is_note_path(path)
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| {
                ["CHAT", "TODO", "ARCHIVE"]
                    .iter()
                    .any(|protected| stem.eq_ignore_ascii_case(protected))
            })
}

/// Normalize a user-entered file name: trim, default to `.md`, reject traversal.
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
    if !(lower.ends_with(".md") || lower.ends_with(".mb")) {
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
        "<!-- nole-msg id=\"{}\" created_at=\"{}\" -->\n",
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
    let needle_open = format!("<!-- nole-msg id=\"{id}\"");
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

fn append_todo_task(todo_path: &Path, msg: &Message) -> Result<String> {
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
    Ok(content)
}

/// If `line` is a `- [ ]` / `- [x]` task, return `(checked, body_text)`.
fn parse_task_line(line: &str) -> Option<(bool, &str)> {
    let after_bullet = line.trim_start().strip_prefix("- ")?;
    let (checked, rest) = if let Some(r) = after_bullet.strip_prefix("[ ]") {
        (false, r)
    } else if let Some(r) = after_bullet.strip_prefix("[x]") {
        (true, r)
    } else if let Some(r) = after_bullet.strip_prefix("[X]") {
        (true, r)
    } else {
        return None;
    };
    Some((checked, rest.trim_start()))
}

/// Return `line` with its first `- [ ]`/`- [x]` checkbox flipped.
fn flip_task_line(line: &str) -> Option<String> {
    let (idx, was_checked) = if let Some(i) = line.find("[ ]") {
        (i, false)
    } else if let Some(i) = line.find("[x]").or_else(|| line.find("[X]")) {
        (i, true)
    } else {
        return None;
    };
    let mut s = line.to_string();
    let new = if was_checked { "[ ]" } else { "[x]" };
    s.replace_range(idx..idx + 3, new);
    Some(s)
}

fn append_markdown_section(path: &Path, msg: &Message) -> Result<String> {
    let ts = msg.created_at.format("%Y-%m-%d %H:%M");
    let mut content = String::new();
    content.push_str(&format!("\n## {ts}\n\n"));
    content.push_str(&msg.body);
    if !msg.body.ends_with('\n') {
        content.push('\n');
    }
    append_text(path, &content)?;
    Ok(content)
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
    fn load_and_toggle_todo_tasks() {
        let (_dir, st) = fresh();
        st.append_chat_message("first").unwrap();
        st.append_chat_message("second").unwrap();
        // move_to_todo consumes the message; append fresh ones for each task.
        let m1 = st.append_chat_message("buy milk").unwrap();
        let m2 = st.append_chat_message("write docs").unwrap();
        st.move_to_todo(&m1).unwrap();
        st.move_to_todo(&m2).unwrap();

        let items = st.load_todo_tasks();
        assert_eq!(items.len(), 2);
        assert!(!items[0].checked);
        assert_eq!(items[0].text, "buy milk");
        assert_eq!(items[1].text, "write docs");

        // Toggle the first task on, then back off.
        assert!(st.toggle_todo_task(0).unwrap());
        let on = st.load_todo_tasks();
        assert!(on[0].checked);
        assert!(!on[1].checked, "other tasks untouched");

        assert!(st.toggle_todo_task(0).unwrap());
        let off = st.load_todo_tasks();
        assert!(!off[0].checked);

        // Out-of-range index toggles nothing.
        assert!(!st.toggle_todo_task(99).unwrap());
    }

    #[test]
    fn search_file_lines_finds_matches_case_insensitively() {
        let (_dir, st) = fresh();
        st.create_named_file("Project").unwrap();
        let p = st.data_dir.join("Project.md");
        fs::write(&p, "# Project\n\nA note about Rust speed\n\nunrelated\n").unwrap();

        let hits = st.search_file_lines("rust");
        assert!(hits.iter().any(|h| matches!(h,
            SearchHit::FileLine { text, .. } if text.to_lowercase().contains("rust"))));
        // Case-insensitive.
        let hi = st.search_file_lines("RUST");
        assert!(!hi.is_empty());
        // Empty query returns nothing.
        assert!(st.search_file_lines("").is_empty());
        // Skips CHAT.md (no markers leak into results).
        assert!(hits.iter().all(|h| !matches!(h,
            SearchHit::FileLine { path, .. } if path.file_name().unwrap() == "CHAT.md")));
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
    fn rename_file_moves_and_renames() {
        let (_dir, st) = fresh();
        let from = st.create_named_file("old").unwrap();
        let to = st.rename_file(&from, "new name").unwrap();
        assert_eq!(to.file_name().unwrap(), "new name.md");
        assert!(!from.exists());
        assert!(to.exists());
        let names: Vec<String> = st
            .list_markdown_files()
            .unwrap()
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
            .collect();
        assert!(names.contains(&"new name.md".to_string()));
        assert!(!names.contains(&"old.md".to_string()));
    }

    #[test]
    fn rename_file_rejects_existing_target() {
        let (_dir, st) = fresh();
        let a = st.create_named_file("a").unwrap();
        st.create_named_file("b").unwrap();
        assert!(st.rename_file(&a, "b").is_err());
        // Original is untouched on failure.
        assert!(a.exists());
    }

    #[test]
    fn delete_file_removes() {
        let (_dir, st) = fresh();
        let p = st.create_named_file("goner").unwrap();
        st.delete_file(&p).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn delete_file_rejects_outside_root() {
        let (_dir, st) = fresh();
        assert!(st.delete_file(Path::new("/etc/hosts")).is_err());
    }

    #[test]
    fn restore_message_and_remove_first_occurrence() {
        let (_dir, st) = fresh();
        let m = st.append_chat_message("hello").unwrap();
        assert!(st.remove_message_by_id(&m.id).unwrap());
        assert!(st.load_messages().unwrap().is_empty());

        // Restore re-inserts the block with the original id.
        st.restore_message_to_chat(&m).unwrap();
        let msgs = st.load_messages().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, m.id);
        assert_eq!(msgs[0].body, "hello");

        // remove_first_occurrence on a file.
        let p = st.create_named_file("X").unwrap();
        fs::write(&p, "keep\nNEEDLE here\nmore\n").unwrap();
        assert!(st.remove_first_occurrence(&p, "NEEDLE here").unwrap());
        assert!(!fs::read_to_string(&p).unwrap().contains("NEEDLE"));
        assert!(!st.remove_first_occurrence(&p, "nope").unwrap());
    }

    #[test]
    fn replace_message_updates_body_preserving_id() {
        let (_dir, st) = fresh();
        let m = st.append_chat_message("original").unwrap();
        let _other = st.append_chat_message("keep me").unwrap();

        let mut updated = m.clone();
        updated.body = "edited body".to_string();
        assert!(st.replace_message(&updated).unwrap());

        let msgs = st.load_messages().unwrap();
        assert_eq!(msgs.len(), 2);
        let got = msgs.iter().find(|x| x.id == m.id).unwrap();
        assert_eq!(got.body, "edited body");
        assert_eq!(got.created_at, m.created_at, "timestamp preserved");
        assert!(msgs.iter().any(|x| x.body == "keep me"), "others untouched");

        // Unknown id is a no-op.
        let unknown = Message {
            id: "nope".to_string(),
            created_at: m.created_at,
            body: "x".to_string(),
        };
        assert!(!st.replace_message(&unknown).unwrap());
    }

    #[test]
    fn normalize_rejects_traversal_and_preserves_note_extensions() {
        assert!(normalize_new_name("../etc").is_err());
        assert!(normalize_new_name("a/b").is_err());
        assert!(normalize_new_name("/abs").is_err());
        assert!(normalize_new_name("").is_err());
        assert_eq!(normalize_new_name("ok").unwrap(), "ok.md");
        assert_eq!(normalize_new_name("ok.MD").unwrap(), "ok.MD");
        assert_eq!(normalize_new_name("ok.MB").unwrap(), "ok.MB");
    }

    #[test]
    fn create_accepts_case_insensitive_extensions_and_refuses_existing_entries() {
        let (_dir, st) = fresh();
        let upper = st.create_named_file("upper.MD").unwrap();
        let mb = st.create_named_file("long.MB").unwrap();
        assert!(upper.is_file());
        assert!(mb.is_file());

        fs::write(&upper, "keep this").unwrap();
        assert!(st.create_named_file("upper.MD").is_err());
        assert_eq!(fs::read_to_string(&upper).unwrap(), "keep this");

        let directory = st.data_dir.join("directory.md");
        fs::create_dir(&directory).unwrap();
        assert!(st.create_named_file("directory.md").is_err());
    }

    #[test]
    fn create_and_rename_reject_protected_names_case_insensitively() {
        let (_dir, st) = fresh();
        for name in [
            "chat",
            "Chat.MB",
            "todo.Md",
            "TODO.mb",
            "archive",
            "Archive.MD",
        ] {
            assert!(
                st.create_named_file(name).is_err(),
                "protected name was created: {name}"
            );
        }

        let source = st.create_named_file("source").unwrap();
        for name in ["chat.md", "TODO.MB", "archive.MD"] {
            assert!(
                st.rename_file(&source, name).is_err(),
                "protected rename target was accepted: {name}"
            );
            assert!(source.exists());
        }
    }

    #[test]
    fn rename_and_delete_reject_protected_source_files() {
        let (_dir, st) = fresh();
        for path in [&st.chat_path, &st.todo_path, &st.archive_path] {
            assert!(st.rename_file(path, "ordinary.md").is_err());
            assert!(st.delete_file(path).is_err());
            assert!(path.exists());
        }

        let mixed_case = st.root.join("tOdO.mB");
        fs::write(&mixed_case, "protected").unwrap();
        assert!(st.rename_file(&mixed_case, "ordinary.md").is_err());
        assert!(st.delete_file(&mixed_case).is_err());
        assert!(mixed_case.exists());
    }

    #[test]
    fn rename_accepts_note_extensions_case_insensitively() {
        let (_dir, st) = fresh();
        let from = st.create_named_file("before.MB").unwrap();
        let to = st.rename_file(&from, "after.Md").unwrap();
        assert_eq!(to.file_name().unwrap(), "after.Md");
        assert!(to.is_file());
        assert!(!from.exists());
    }

    #[test]
    fn validate_and_read_accept_direct_case_insensitive_note_files() {
        let (_dir, st) = fresh();
        let path = st.data_dir.join("Readable.MB");
        fs::write(&path, "hello").unwrap();

        assert_eq!(
            st.validate_target(&path).unwrap(),
            fs::canonicalize(&path).unwrap()
        );
        assert_eq!(st.read_note_file(&path).unwrap(), "hello");
    }

    #[test]
    fn validate_rejects_nested_non_files_and_uses_configured_root_in_errors() {
        let (_dir, st) = fresh();
        let nested_dir = st.data_dir.join("nested");
        fs::create_dir(&nested_dir).unwrap();
        let nested = nested_dir.join("note.md");
        fs::write(&nested, "nested").unwrap();
        assert!(st.validate_target(&nested).is_err());

        let directory = st.data_dir.join("directory.md");
        fs::create_dir(&directory).unwrap();
        assert!(st.validate_target(&directory).is_err());

        let error = st.validate_target(Path::new("/outside.md")).unwrap_err();
        let message = format!("{error:#}");
        assert!(!message.contains("~/.nole"));
        assert!(message.contains(&st.root.display().to_string()));
    }

    #[test]
    fn list_reads_only_flat_md_and_mb_files_from_data() {
        let (_dir, st) = fresh();
        fs::write(st.data_dir.join("alpha.MD"), "alpha").unwrap();
        fs::write(st.data_dir.join("beta.mB"), "beta").unwrap();
        fs::write(st.root.join("root-note.md"), "hidden").unwrap();
        fs::write(st.data_dir.join("plain.txt"), "plain").unwrap();
        fs::create_dir(st.data_dir.join("directory.md")).unwrap();

        let names: Vec<_> = st
            .list_markdown_files()
            .unwrap()
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
            .collect();
        assert!(names.contains(&"alpha.MD".to_string()));
        assert!(names.contains(&"beta.mB".to_string()));
        assert!(!names.contains(&"root-note.md".to_string()));
        assert!(!names.contains(&"TODO.md".to_string()));
        assert!(!names.contains(&"plain.txt".to_string()));
        assert!(!names.contains(&"directory.md".to_string()));
    }

    #[test]
    fn list_remains_recent_first() {
        let (_dir, st) = fresh();
        let older = st.create_named_file("older").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newer = st.create_named_file("newer").unwrap();

        let files = st.list_markdown_files().unwrap();
        let older_index = files.iter().position(|path| path == &older).unwrap();
        let newer_index = files.iter().position(|path| path == &newer).unwrap();
        assert!(newer_index < older_index);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_notes_are_rejected_and_excluded() {
        use std::os::unix::fs::symlink;

        let (_root_dir, st) = fresh();
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.md");
        fs::write(&outside, "outside").unwrap();
        let link = st.data_dir.join("linked.MD");
        symlink(&outside, &link).unwrap();

        assert!(st.validate_target(&link).is_err());
        assert!(st.read_note_file(&link).is_err());
        assert!(st.rename_file(&link, "renamed.md").is_err());
        assert!(st.delete_file(&link).is_err());
        assert!(link.exists());
        assert!(!st.list_markdown_files().unwrap().contains(&link));
    }

    #[test]
    fn ensure_files_creates_structured_layout() {
        let (_dir, st) = fresh();
        assert!(st.config_dir.is_dir());
        assert!(st.data_dir.is_dir());
        assert_eq!(st.chat_path.parent(), Some(st.root.as_path()));
        assert_eq!(st.todo_path.parent(), Some(st.root.as_path()));
        assert!(st.archive_path.exists());
        assert_eq!(st.archive_path.parent(), Some(st.root.as_path()));
        assert_eq!(st.archive_path.file_name().unwrap(), "ARCHIVE.md");
        assert!(st.ai_config_path.exists());
        assert_eq!(st.ai_config_path.parent(), Some(st.config_dir.as_path()));
        assert!(fs::read_to_string(&st.ai_config_path)
            .unwrap()
            .contains("api_key = \"\""));
    }

    #[test]
    fn ensure_files_migrates_legacy_root_notes_without_overwriting() {
        let root_dir = tempdir().unwrap();
        let st = Storage::new(root_dir.path()).unwrap();
        fs::create_dir_all(&st.data_dir).unwrap();
        fs::write(st.root.join("Legacy.md"), "legacy").unwrap();
        fs::write(st.root.join("Game.MB"), "game").unwrap();
        fs::write(st.root.join("Conflict.md"), "root copy").unwrap();
        fs::write(st.data_dir.join("Conflict.md"), "data copy").unwrap();

        st.ensure_files().unwrap();

        assert_eq!(
            fs::read_to_string(st.data_dir.join("Legacy.md")).unwrap(),
            "legacy"
        );
        assert_eq!(
            fs::read_to_string(st.data_dir.join("Game.MB")).unwrap(),
            "game"
        );
        assert!(!st.root.join("Legacy.md").exists());
        assert!(!st.root.join("Game.MB").exists());
        assert_eq!(
            fs::read_to_string(st.root.join("Conflict.md")).unwrap(),
            "root copy"
        );
        assert_eq!(
            fs::read_to_string(st.data_dir.join("Conflict.md")).unwrap(),
            "data copy"
        );
    }

    #[test]
    fn surgical_edit_preserves_unmanaged_text() {
        // Even if a user hand-edits content around blocks, removals must not
        // eat unrelated lines.
        let (_dir, st) = fresh();
        let text = "free text at top\n<!-- nole-msg id=\"abc\" created_at=\"2026-06-18T17:20:00+08:00\" -->\nbody\n<!-- /nole-msg -->\ntrailing\n";
        fs::write(&st.chat_path, text).unwrap();
        assert!(st.remove_message_by_id("abc").unwrap());
        let after = fs::read_to_string(&st.chat_path).unwrap();
        assert!(after.contains("free text at top"));
        assert!(after.contains("trailing"));
        assert!(!after.contains("nole-msg"));
    }
}

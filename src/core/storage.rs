//! Markdown persistence for Nole.
//!
//! Daily entries are persisted as one Markdown file per day under `daily/`.
//! The first entry of a day creates `YYYY-MM-DD.md`; later entries append to it.
//! `archives/` is flat storage for articles moved from `data/`.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use chrono::{Local, NaiveDate};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::core::export::{ExportDiagnostic, ExportFormat};
use crate::core::model::{DailyNote, MutationConflict, NoteFile, TodoItem};

mod atomic;
mod export;
#[cfg(test)]
use export::*;
mod notes;
mod theme;

pub(crate) use theme::DEFAULT_THEME_TOML;

pub(crate) use atomic::replace_file_atomically;
pub(crate) use atomic::stage_replacement;

const CONFIG_DIR: &str = "config";
const AI_CONFIG_FILE: &str = "ai.toml";
const SETTINGS_FILE: &str = "settings.toml";
const DEFAULT_SETTINGS: &str = r#"theme = "default"

# Show complete thinking blocks instead of a five-line scrolling window.
show_full_thinking = false

# Default directory for File: Export… destinations. "~" is the user's home
# directory; absolute paths and paths relative to the parent of the Nole root
# are also accepted.
export_directory = "~"

# Command used to edit notes. Defaults to $EDITOR, then $VISUAL, then vi.
# editor = "code -w"

# Executable used by the floating terminal. Defaults to the system login shell.
# shell = "fish"
"#;

const AGENTS_FILE: &str = "AGENTS.md";
const MEMORY_FILE: &str = "MEMORY.md";
const TEMPLATE_FILE: &str = "template.md";
const THEMES_DIR: &str = "themes";
const DATA_DIR: &str = "data";
const DAILY_DIR: &str = "daily";
const ARCHIVES_DIR: &str = "archives";
const SKILLS_DIR: &str = "skills";
pub(crate) const ATTACHMENTS_DIR: &str = "attachments";
pub(crate) const WORKSPACE_DIR: &str = "workspace";

/// An append that succeeded on disk, in the exact form needed to undo it: the
/// file, its length before the append, the bytes appended, and whether the
/// append created the file. Opaque outside storage; workspace-level appends
/// hand it back to callers that keep an undo stack.
#[derive(Debug, Clone)]
pub struct AppendReceipt {
    path: PathBuf,
    original_len: u64,
    appended: Vec<u8>,
    created: bool,
}

impl AppendReceipt {
    /// The file the receipt's append touched.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    theme: String,
    editor: Option<String>,
    shell: Option<String>,
    #[serde(default)]
    show_full_thinking: bool,
    export_directory: String,
}

fn nonempty_setting(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn resolve_editor_command(
    configured: Option<&str>,
    editor_env: Option<&str>,
    visual_env: Option<&str>,
) -> String {
    configured
        .filter(|value| !value.trim().is_empty())
        .or_else(|| editor_env.filter(|value| !value.trim().is_empty()))
        .or_else(|| visual_env.filter(|value| !value.trim().is_empty()))
        .unwrap_or("vi")
        .to_string()
}
#[derive(Debug, Clone)]
struct ExportSourceIdentity {
    canonical_path: PathBuf,
    length: u64,
    modified: SystemTime,
    content_sha256: [u8; 32],
}

/// How the export publication may treat an existing destination. Every
/// non-interactive caller passes [`ExportDestinationPolicy::CreateNew`]; only
/// the interactive UI may pass [`ExportDestinationPolicy::ReplaceExisting`]
/// after the user explicitly confirms an overwrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportDestinationPolicy {
    /// Never overwrite an existing destination.
    CreateNew,
    /// Atomically replace an existing regular, non-symlink destination. The
    /// destination is re-validated at publish time and replaced by a rename
    /// relative to the parent directory handle opened during preparation, so
    /// the final file is always either the old content or the complete new content.
    ReplaceExisting,
}

#[derive(Debug)]
pub struct PreparedExport {
    source: PathBuf,
    /// Absolute destination, used for display and result reporting only.
    /// Every publish-time filesystem operation resolves relative to
    /// `parent_dir` plus `file_name`, so a rename or symlink swap of the
    /// parent path after preparation can never redirect the publication.
    destination: PathBuf,
    /// The single destination file name, operated on relative to `parent_dir`.
    file_name: OsString,
    /// The destination parent directory, validated and opened at prepare
    /// time. Binding temp creation, destination re-validation, and the final
    /// hard-link/rename to this handle closes the parent-directory-swap
    /// TOCTOU: even if the original parent path is renamed or replaced with a
    /// symlink afterwards, publish only ever touches this directory.
    parent_dir: cap_std::fs::Dir,
    format: ExportFormat,
    policy: ExportDestinationPolicy,
    identity: ExportSourceIdentity,
    /// UTF-8 source captured once at prepare time for rendered formats, so
    /// publishing renders from the validated snapshot instead of re-reading
    /// the file. `None` for `ExportFormat::Original`, which streams bytes.
    rendered_source: Option<String>,
}

impl PreparedExport {
    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn destination(&self) -> &Path {
        &self.destination
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    pub destination: PathBuf,
    pub bytes: u64,
    /// Non-fatal degradations surfaced by the renderer (engine warnings,
    /// missing/broken images). Empty for `ExportFormat::Original`.
    pub diagnostics: Vec<ExportDiagnostic>,
}

/// Filesystem locations backing the notes.
#[derive(Debug, Clone)]
pub struct Storage {
    pub root: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub daily_dir: PathBuf,
    pub archives_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub attachments_dir: PathBuf,
    pub workspace_dir: PathBuf,
    pub ai_config_path: PathBuf,
    pub settings_path: PathBuf,
    pub themes_dir: PathBuf,
    pub agents_path: PathBuf,
    pub memory_path: PathBuf,
    pub template_path: PathBuf,
}

impl Storage {
    /// Build a storage rooted at `~/.nole`, or at the directory named by the
    /// `NOLE_DIR` environment variable when it is set to a non-empty value.
    pub fn resolve_root() -> Result<Self> {
        match std::env::var("NOLE_DIR") {
            Ok(dir) if !dir.trim().is_empty() => Self::new(dir.trim()),
            _ => Self::default_root(),
        }
    }

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
            daily_dir: root.join(DAILY_DIR),
            archives_dir: root.join(ARCHIVES_DIR),
            skills_dir: root.join(SKILLS_DIR),
            attachments_dir: root.join(ATTACHMENTS_DIR),
            workspace_dir: root.join(WORKSPACE_DIR),
            ai_config_path: root.join(CONFIG_DIR).join(AI_CONFIG_FILE),
            settings_path: root.join(CONFIG_DIR).join(SETTINGS_FILE),
            themes_dir: root.join(THEMES_DIR),
            agents_path: root.join(CONFIG_DIR).join(AGENTS_FILE),
            memory_path: root.join(MEMORY_FILE),
            template_path: root.join(TEMPLATE_FILE),
            root,
        })
    }
    pub fn ensure_files(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating {}", self.config_dir.display()))?;
        fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating {}", self.data_dir.display()))?;
        fs::create_dir_all(&self.daily_dir)
            .with_context(|| format!("creating {}", self.daily_dir.display()))?;
        fs::create_dir_all(&self.archives_dir)
            .with_context(|| format!("creating {}", self.archives_dir.display()))?;
        crate::core::attachment::AttachmentStore::new(self.attachments_dir.clone())
            .ensure_layout()?;
        fs::create_dir_all(&self.workspace_dir)
            .with_context(|| format!("creating {}", self.workspace_dir.display()))?;
        crate::core::skill::ensure_skills_directory(&self.skills_dir)?;
        fs::create_dir_all(&self.themes_dir)
            .with_context(|| format!("creating {}", self.themes_dir.display()))?;
        let default_theme_path = self.themes_dir.join("default.toml");
        if !default_theme_path.exists() {
            self.write_default_theme(&default_theme_path)?;
        }
        if !self.ai_config_path.exists() {
            self.write_default_ai_config()?;
        }
        if !self.settings_path.exists() {
            self.write_default_settings()?;
        }
        create_empty_file(&self.agents_path)?;
        create_empty_file(&self.memory_path)?;
        create_empty_file(&self.template_path)?;
        Ok(())
    }

    fn write_default_ai_config(&self) -> Result<()> {
        const DEFAULT: &str = concat!(
            "# AI service credentials. Keep this file private.\n",
            "# API protocol: \"messages\" or \"completions\".\n",
            "api_format = \"messages\"\n",
            "api_key = \"\"\n",
            "tavily_api_key = \"\"\n",
            "model = \"claude-sonnet-4-5\"\n",
            "# Reasoning effort: \"low\", \"medium\", \"high\", \"xhigh\", or \"max\".\n",
            "# Omit or leave empty to keep the provider default (\"high\" for Anthropic).\n",
            "effort = \"high\"\n",
            "max_tokens = 8192\n",
            "context_window_tokens = 200000\n",
            "# Round budget for non-interactive subagents; the main Agent has no round limit.\n",
            "max_subagent_rounds = 25\n",
            "max_concurrent_local_reads = 8\n",
            "max_concurrent_network_tools = 8\n",
            "max_concurrent_subagents = 4\n",
            "# Enable only for a vision-capable model: native image input.\n",
            "supports_images = false\n",
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

    fn write_default_settings(&self) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.settings_path)
            .with_context(|| format!("creating {}", self.settings_path.display()))?;
        file.write_all(DEFAULT_SETTINGS.as_bytes())?;
        Ok(())
    }

    fn write_default_theme(&self, path: &Path) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        file.write_all(DEFAULT_THEME_TOML.as_bytes())?;
        Ok(())
    }

    pub fn load_theme_selection(&self) -> Result<String> {
        Ok(self.load_settings()?.theme)
    }

    pub fn editor_command(&self) -> Result<String> {
        let settings = self.load_settings()?;
        Ok(resolve_editor_command(
            settings.editor.as_deref(),
            std::env::var("EDITOR").ok().as_deref(),
            std::env::var("VISUAL").ok().as_deref(),
        ))
    }

    pub fn terminal_shell(&self) -> Result<Option<String>> {
        Ok(nonempty_setting(self.load_settings()?.shell))
    }

    pub fn show_full_thinking(&self) -> Result<bool> {
        Ok(self.load_settings()?.show_full_thinking)
    }

    /// Default export directory from settings: the trimmed `export_directory`
    /// value. The text is resolved later by the existing export destination
    /// parser, which accepts `~`, absolute, and relative paths. Blank values
    /// are rejected so the UI surfaces the misconfiguration instead of
    /// silently exporting somewhere unexpected.
    pub fn default_export_directory(&self) -> Result<String> {
        let configured = self.load_settings()?.export_directory;
        let trimmed = configured.trim();
        if trimmed.is_empty() {
            bail!("settings export_directory must not be blank");
        }
        Ok(trimmed.to_string())
    }

    pub fn write_theme_selection(&self, selection: &str) -> Result<()> {
        let source = self.read_settings_source()?;
        self.parse_settings(&source)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("parsing {}", self.settings_path.display()))?;
        document["theme"] = toml_edit::value(selection);
        fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating {}", self.config_dir.display()))?;
        fs::write(&self.settings_path, document.to_string())
            .with_context(|| format!("writing {}", self.settings_path.display()))
    }

    fn load_settings(&self) -> Result<SettingsFile> {
        let source = self.read_settings_source()?;
        self.parse_settings(&source)
    }

    fn read_settings_source(&self) -> Result<String> {
        let source = match fs::read_to_string(&self.settings_path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DEFAULT_SETTINGS.to_string()
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading {}", self.settings_path.display()));
            }
        };
        Ok(source)
    }

    fn parse_settings(&self, source: &str) -> Result<SettingsFile> {
        toml::from_str(source).with_context(|| format!("parsing {}", self.settings_path.display()))
    }
}

/// Expand a leading `~` path component to the user's home directory. The
/// check operates on path components rather than string prefixes, so both
/// Unix `~/name` and Windows `~\name` resolve under home. Only a bare leading
/// component equal to `~` is expanded; `~user` and relative paths are left
/// untouched.
fn expand_leading_home(input: &Path) -> Result<Option<PathBuf>> {
    let mut components = input.components();
    match components.next() {
        Some(Component::Normal(part)) if part == "~" => {
            let home = dirs::home_dir().context("could not determine home directory")?;
            let mut expanded = home;
            expanded.extend(components.map(Component::as_os_str));
            Ok(Some(expanded))
        }
        _ => Ok(None),
    }
}

/// Normalize `.` and `..` components without touching the filesystem.
///
/// Callers use this before canonicalization when they need to validate a
/// path's lexical boundary. Parent components that leave the path base are
/// rejected immediately.
pub(crate) fn normalize_lexical(path: PathBuf) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    bail!("path escapes its base");
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn reject_existing_symlink_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        // Prefix and root components only ever form the volume root
        // (`\\?\C:\`, `C:\`, `/`), which cannot be a symlink. On Windows a
        // bare prefix such as `\\?\C:` is not a valid path at all, so
        // probing it would reject every export even when the tree is safe.
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("symlink traversal is not allowed: {}", current.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("path component does not exist: {}", current.display());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", current.display()))
            }
        }
    }
    Ok(())
}

fn reject_symlink_components_below(base: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(base)
        .with_context(|| format!("{} is outside {}", path.display(), base.display()))?;
    let mut current =
        fs::canonicalize(base).with_context(|| format!("canonicalizing {}", base.display()))?;
    for component in relative.components() {
        let Component::Normal(part) = component else {
            bail!("invalid path component");
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("symlink traversal is not allowed: {}", current.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("path component does not exist: {}", current.display());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", current.display()))
            }
        }
    }
    Ok(())
}

fn create_empty_file(path: &Path) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("creating {}", path.display())),
    }
}

fn parse_daily_date(value: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .with_context(|| format!("invalid daily date {value}; expected YYYY-MM-DD"))
}

fn date_file_name(date: NaiveDate) -> String {
    format!("{}.md", date.format("%Y-%m-%d"))
}

fn date_from_path(path: &Path) -> Option<NaiveDate> {
    let stem = path.file_stem()?.to_str()?;
    if path.extension()?.to_str()? != "md" {
        return None;
    }
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}

/// Whether a path names a Markdown note by extension.
pub(crate) fn is_note_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn validate_direct_note(directory: &Path, target: &Path, label: &str) -> Result<PathBuf> {
    if !is_note_path(target) {
        bail!("target must have a .md extension: {}", target.display());
    }
    let canonical_directory = fs::canonicalize(directory)
        .with_context(|| format!("resolving {label} directory {}", directory.display()))?;
    let metadata = fs::symlink_metadata(target)
        .with_context(|| format!("checking {label} note {}", target.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("{label} note must be a regular file: {}", target.display());
    }
    let canonical = fs::canonicalize(target)
        .with_context(|| format!("resolving {label} note {}", target.display()))?;
    if canonical.parent() != Some(canonical_directory.as_path()) {
        bail!(
            "{label} note must be a direct child of {}",
            directory.display()
        );
    }
    Ok(canonical)
}

fn move_without_overwrite(from: &Path, to: &Path) -> Result<()> {
    match fs::symlink_metadata(to) {
        Ok(_) => {
            return Err(anyhow::Error::new(MutationConflict(format!(
                "a file named {} already exists",
                to.file_name().unwrap_or_default().to_string_lossy()
            ))));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("checking {}", to.display())),
    }
    fs::rename(from, to)
        .with_context(|| format!("moving {} to {}", from.display(), to.display()))?;
    Ok(())
}

fn rename_without_overwrite(from: &Path, to: &Path, name: &str) -> Result<PathBuf> {
    if to.file_name() == from.file_name() {
        return Ok(from.to_path_buf());
    }
    move_without_overwrite(from, to)
        .with_context(|| format!("renaming archived note to {name}"))?;
    Ok(to.to_path_buf())
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
    if !(lower.ends_with(".md")) {
        Ok(format!("{trimmed}.md"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn stem(file_name: &str) -> &str {
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file_name)
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

fn append_markdown_section(path: &Path, note: &DailyNote) -> Result<String> {
    let date = note.date.format("%Y-%m-%d");
    let mut content = String::new();
    content.push_str(&format!("\n## {date}\n\n"));
    content.push_str(&note.body);
    if !note.body.ends_with('\n') {
        content.push('\n');
    }
    append_text(path, &content)?;
    Ok(content)
}

fn append_text(path: &Path, content: &str) -> Result<()> {
    append_text_tracked(path, content).map(|_| ())
}

fn append_text_tracked(path: &Path, content: &str) -> Result<AppendReceipt> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory {}", parent.display()))?;
    }
    let created = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                bail!("refusing to append through symlink {}", path.display());
            }
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error).with_context(|| format!("checking {}", path.display())),
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let original_len = file.metadata()?.len();
    let mut appended = Vec::with_capacity(content.len() + 1);
    // Ensure existing content ends in a newline before appending.
    if original_len > 0 {
        let mut reader = OpenOptions::new().read(true).open(path)?;
        let mut tail = [0u8; 1];
        reader.seek(SeekFrom::End(-1))?;
        if reader.read(&mut tail)? == 1 && tail[0] != b'\n' {
            file.write_all(b"\n")?;
            appended.push(b'\n');
        }
    }
    file.write_all(content.as_bytes())?;
    appended.extend_from_slice(content.as_bytes());
    Ok(AppendReceipt {
        path: path.to_path_buf(),
        original_len,
        appended,
        created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fresh() -> (tempfile::TempDir, Storage) {
        let dir = tempdir().unwrap();
        let st = Storage::new(dir.path()).unwrap();
        st.ensure_files().unwrap();
        (dir, st)
    }

    #[test]
    fn round_trip_single_daily_note() {
        let (_dir, st) = fresh();
        st.append_to_today("hello world").unwrap();
        let notes = st.load_daily_notes().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body, "hello world");
    }

    #[test]
    fn preserves_multiline_body() {
        let (_dir, st) = fresh();
        let body = "line one\n\nline three\n- [ ] a checkbox";
        st.append_to_today(body).unwrap();
        let notes = st.load_daily_notes().unwrap();
        assert_eq!(notes[0].body, body);
    }

    #[test]
    fn multiple_sends_append_to_one_daily_card() {
        let (_dir, st) = fresh();
        st.append_to_today("first").unwrap();
        st.append_to_today("second").unwrap();
        st.append_to_today("third").unwrap();
        let notes = st.load_daily_notes().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body, "first\n\nsecond\n\nthird");
    }

    #[cfg(unix)]
    #[test]
    fn daily_append_rejects_symlink_targets() {
        use std::os::unix::fs::symlink;

        let (_dir, storage) = fresh();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("outside.md");
        fs::write(&target, "protected\n").unwrap();
        let daily = storage.daily_dir.join("2026-07-26.md");
        symlink(&target, &daily).unwrap();

        assert!(storage.append_daily("2026-07-26", "injected").is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "protected\n");
    }

    #[test]
    fn daily_cards_are_sorted_and_removed_by_date() {
        let (_dir, st) = fresh();
        st.append_daily("2026-07-26", "keep").unwrap();
        st.append_daily("2026-07-27", "drop").unwrap();
        assert!(st.remove_daily("2026-07-27").unwrap());
        let notes = st.load_daily_notes().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].date.to_string(), "2026-07-26");
    }

    #[test]
    fn load_and_toggle_todo_tasks() {
        let (_dir, st) = fresh();
        st.append_daily("2026-07-26", "- [ ] older task").unwrap();
        st.append_daily("2026-07-27", "- [ ] buy milk\n- [x] write docs")
            .unwrap();

        let items = st.load_todo_tasks().unwrap();
        assert_eq!(items.len(), 3);
        assert!(!items[0].checked);
        assert_eq!(items[0].text, "buy milk");
        assert!(items[1].checked);
        assert_eq!(items[2].text, "older task");

        // Toggle the first task on, then back off.
        assert!(st.toggle_todo_task(0).unwrap());
        let on = st.load_todo_tasks().unwrap();
        assert!(on[0].checked);
        assert!(on[1].checked, "other tasks untouched");

        assert!(st.toggle_todo_task(0).unwrap());
        let off = st.load_todo_tasks().unwrap();
        assert!(!off[0].checked);

        // Out-of-range index toggles nothing.
        assert!(!st.toggle_todo_task(99).unwrap());
    }

    #[test]
    fn move_to_markdown_writes_section() {
        let (_dir, st) = fresh();
        let m = st.append_daily("2026-06-18", "idea!").unwrap();
        let target = st.create_named_file("工作记录").unwrap();
        st.move_to_markdown(&target, &m).unwrap();
        let body = fs::read_to_string(&target).unwrap();
        assert!(body.contains("## 2026-06-18"));
        assert!(body.contains("idea!"));
        assert!(st.load_daily_notes().unwrap().is_empty());
    }

    #[test]
    fn create_named_file_adds_extension() {
        let (_dir, st) = fresh();
        fs::write(&st.template_path, "ignored template").unwrap();
        let p = st.create_named_file("笔记").unwrap();
        assert_eq!(p.file_name().unwrap(), "笔记.md");
        assert!(p.exists());
        assert_eq!(fs::read_to_string(p).unwrap(), "# 笔记\n\n");
    }

    #[test]
    fn create_named_file_uses_the_current_template_verbatim() {
        let (_dir, st) = fresh();
        fs::write(&st.template_path, "# Template\n\n- [ ] Next\n").unwrap();

        let path = st.create_named_file_from_template("Project").unwrap();

        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "# Template\n\n- [ ] Next\n"
        );
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
            .list_note_files()
            .unwrap()
            .iter()
            .filter_map(|file| {
                file.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(String::from)
            })
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
    fn local_files_must_exist_but_may_be_outside_the_workspace() {
        let (_directory, st) = fresh();
        let file = st.root.join("attachment.pdf");
        fs::write(&file, b"attachment").unwrap();
        assert_eq!(
            st.validate_local_file(&file).unwrap(),
            fs::canonicalize(&file).unwrap()
        );
        assert!(st
            .validate_local_file(&st.root.join("missing.pdf"))
            .is_err());

        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.pdf");
        fs::write(&outside_file, b"outside").unwrap();
        assert_eq!(
            st.validate_local_file(&outside_file).unwrap(),
            fs::canonicalize(&outside_file).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_files_follow_symlinks_to_regular_files() {
        use std::os::unix::fs::symlink;

        let (_directory, st) = fresh();
        let file = st.root.join("attachment.pdf");
        let link = st.root.join("attachment-link.pdf");
        fs::write(&file, b"attachment").unwrap();
        symlink(&file, &link).unwrap();
        assert_eq!(
            st.validate_local_file(&link).unwrap(),
            fs::canonicalize(&file).unwrap()
        );
    }

    #[test]
    fn restore_daily_and_remove_first_occurrence() {
        let (_dir, st) = fresh();
        let note = st.append_to_today("hello").unwrap();
        assert!(st.remove_daily(&note.date.to_string()).unwrap());
        assert!(st.load_daily_notes().unwrap().is_empty());

        st.restore_daily(&note).unwrap();
        let notes = st.load_daily_notes().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].date, note.date);
        assert_eq!(notes[0].body, "hello");

        // remove_first_occurrence on a file.
        let p = st.create_named_file("X").unwrap();
        fs::write(&p, "keep\nNEEDLE here\nmore\n").unwrap();
        assert!(st.remove_first_occurrence(&p, "NEEDLE here").unwrap());
        assert!(!fs::read_to_string(&p).unwrap().contains("NEEDLE"));
        assert!(!st.remove_first_occurrence(&p, "nope").unwrap());
    }

    #[test]
    fn normalize_rejects_traversal_and_preserves_note_extensions() {
        assert!(normalize_new_name("../etc").is_err());
        assert!(normalize_new_name("a/b").is_err());
        assert!(normalize_new_name("/abs").is_err());
        assert!(normalize_new_name("").is_err());
        assert_eq!(normalize_new_name("ok").unwrap(), "ok.md");
        assert_eq!(normalize_new_name("ok.MD").unwrap(), "ok.MD");
        assert_eq!(normalize_new_name("ok.MD").unwrap(), "ok.MD");
    }

    #[test]
    fn create_accepts_case_insensitive_extensions_and_refuses_existing_entries() {
        let (_dir, st) = fresh();
        let upper = st.create_named_file("upper.MD").unwrap();
        let note = st.create_named_file("long.MD").unwrap();
        assert!(upper.is_file());
        assert!(note.is_file());

        fs::write(&upper, "keep this").unwrap();
        assert!(st.create_named_file("upper.MD").is_err());
        assert_eq!(fs::read_to_string(&upper).unwrap(), "keep this");

        let directory = st.data_dir.join("directory.md");
        fs::create_dir(&directory).unwrap();
        assert!(st.create_named_file("directory.md").is_err());
    }

    #[test]
    fn data_notes_allow_chat_todo_and_archive_names() {
        let (_dir, st) = fresh();
        for name in ["chat", "todo.Md", "Archive.MD"] {
            assert!(st.create_named_file(name).unwrap().is_file());
        }
    }

    #[test]
    fn rename_accepts_note_extensions_case_insensitively() {
        let (_dir, st) = fresh();
        let from = st.create_named_file("before.MD").unwrap();
        let to = st.rename_file(&from, "after.Md").unwrap();
        assert_eq!(to.file_name().unwrap(), "after.Md");
        assert!(to.is_file());
        assert!(!from.exists());
    }

    #[test]
    fn validate_and_read_accept_direct_case_insensitive_note_files() {
        let (_dir, st) = fresh();
        let path = st.data_dir.join("Readable.MD");
        fs::write(&path, "hello").unwrap();

        assert_eq!(
            st.validate_target(&path).unwrap(),
            fs::canonicalize(&path).unwrap()
        );
        assert_eq!(st.read_note_file(&path).unwrap(), "hello");
    }

    #[test]
    fn append_document_adds_to_an_existing_managed_article_only() {
        let (_dir, st) = fresh();
        let path = st.create_named_file("Article").unwrap();
        fs::write(&path, "# Article\n").unwrap();
        st.append_document_tracked(&path, "new paragraph").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# Article\n\nnew paragraph\n"
        );
        assert!(st
            .append_document_tracked(Path::new("/tmp/outside.md"), "no")
            .is_err());
        assert!(st.append_document_tracked(&path, "   ").is_err());
    }

    #[test]
    fn tracked_document_append_can_be_undone_exactly() {
        let (_dir, st) = fresh();
        let path = st.create_named_file("article").unwrap();
        fs::write(&path, "original without newline").unwrap();

        let receipt = st
            .append_document_tracked(&path, "mistaken prompt")
            .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "original without newline\n\nmistaken prompt\n"
        );

        st.undo_append(&receipt).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "original without newline"
        );
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
    fn list_reads_only_flat_markdown_files_from_data() {
        let (_dir, st) = fresh();
        fs::write(st.data_dir.join("alpha.MD"), "alpha").unwrap();
        fs::write(st.data_dir.join("beta.mD"), "beta").unwrap();
        fs::write(st.root.join("root-note.md"), "hidden").unwrap();
        fs::write(st.data_dir.join("plain.txt"), "plain").unwrap();
        fs::create_dir(st.data_dir.join("directory.md")).unwrap();

        let names: Vec<_> = st
            .list_note_files()
            .unwrap()
            .iter()
            .filter_map(|file| {
                file.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(String::from)
            })
            .collect();
        assert!(names.contains(&"alpha.MD".to_string()));
        assert!(names.contains(&"beta.mD".to_string()));
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

        let files = st.list_note_files().unwrap();
        let older_index = files.iter().position(|file| file.path == older).unwrap();
        let newer_index = files.iter().position(|file| file.path == newer).unwrap();
        assert!(newer_index < older_index);
    }

    #[test]
    fn archived_notes_are_listed_and_read_separately_from_data() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let archived = storage.archives_dir.join("Project.MD");
        fs::write(&archived, "archived").unwrap();
        let files = storage.list_archived_note_files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, archived);
        assert_eq!(
            storage.read_archived_note_file(&archived).unwrap(),
            "archived"
        );
        assert!(storage.read_note_file(&archived).is_err());
    }

    #[test]
    fn archive_restore_and_archived_rename_never_overwrite() {
        let (_directory, storage) = fresh();
        let note = storage.create_named_file("Project").unwrap();
        fs::write(&note, "active").unwrap();
        let archived = storage.archive_note(&note).unwrap();
        assert!(!note.exists());
        assert_eq!(fs::read_to_string(&archived).unwrap(), "active");

        fs::write(&note, "replacement").unwrap();
        assert!(storage.restore_archived_note(&archived).is_err());
        assert_eq!(fs::read_to_string(&note).unwrap(), "replacement");
        fs::remove_file(&note).unwrap();

        let restored = storage.restore_archived_note(&archived).unwrap();
        assert_eq!(restored, note);
        let archived = storage.archive_note(&restored).unwrap();
        fs::write(storage.archives_dir.join("Taken.md"), "keep").unwrap();
        assert!(storage.rename_archived_file(&archived, "Taken").is_err());
        assert_eq!(fs::read_to_string(&archived).unwrap(), "active");
    }

    #[test]
    fn append_document_accepts_archived_articles() {
        let (_directory, storage) = fresh();
        let note = storage.create_named_file("Journal").unwrap();
        let archived = storage.archive_note(&note).unwrap();
        storage.append_document_tracked(&archived, "later").unwrap();
        assert!(fs::read_to_string(archived).unwrap().ends_with("\nlater\n"));
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
        assert!(!st
            .list_note_files()
            .unwrap()
            .iter()
            .any(|file| file.path == link));
    }

    #[test]
    fn ensure_files_creates_structured_layout() {
        let (_dir, st) = fresh();
        assert!(st.config_dir.is_dir());
        assert!(st.data_dir.is_dir());
        assert!(st.daily_dir.is_dir());
        assert!(st.archives_dir.is_dir());
        assert!(st.themes_dir.is_dir());
        assert!(st.workspace_dir.is_dir());
        assert_eq!(st.workspace_dir, st.root.join("workspace"));
        assert!(fs::read_dir(&st.daily_dir).unwrap().next().is_none());
        assert!(st.append_daily("2026-07-27", "   ").is_err());
        assert!(!st.daily_dir.join("2026-07-27.md").exists());
        assert!(st.ai_config_path.exists());
        assert!(st.settings_path.exists());
        let default_theme_path = st.themes_dir.join("default.toml");
        assert!(default_theme_path.exists());
        assert!(st.agents_path.exists());
        assert!(st.memory_path.exists());
        assert!(st.template_path.exists());
        assert_eq!(fs::read_to_string(&st.agents_path).unwrap(), "");
        assert_eq!(fs::read_to_string(&st.memory_path).unwrap(), "");
        assert_eq!(fs::read_to_string(&st.template_path).unwrap(), "");
        assert_eq!(st.ai_config_path.parent(), Some(st.config_dir.as_path()));
        assert_eq!(st.settings_path.parent(), Some(st.config_dir.as_path()));
        assert_eq!(st.themes_dir.parent(), Some(st.root.as_path()));
        assert_eq!(st.template_path.parent(), Some(st.root.as_path()));
        let config = fs::read_to_string(&st.ai_config_path).unwrap();
        assert!(config.contains("# API protocol: \"messages\" or \"completions\"."));
        assert!(config.contains("api_format = \"messages\""));
        assert!(config.contains("api_key = \"\""));
        assert!(config.contains("tavily_api_key = \"\""));
        assert!(config.contains("max_tokens = 8192"));
        assert!(config.contains("effort = \"high\""));
        assert!(config.contains("context_window_tokens = 200000"));
        assert!(config.contains("max_subagent_rounds = 25"));
        assert!(config.contains("max_concurrent_local_reads = 8"));
        assert!(config.contains("max_concurrent_network_tools = 8"));
        assert!(config.contains("max_concurrent_subagents = 4"));
        assert_eq!(
            fs::read_to_string(&st.settings_path).unwrap(),
            DEFAULT_SETTINGS
        );
        let settings = fs::read_to_string(&st.settings_path).unwrap();
        assert!(settings
            .contains("# Show complete thinking blocks instead of a five-line scrolling window."));
        assert!(settings.contains("show_full_thinking = false"));
        assert!(settings.contains("export_directory = \"~\""));
        assert_eq!(st.default_export_directory().unwrap(), "~");
        assert_eq!(
            fs::read_to_string(default_theme_path).unwrap(),
            DEFAULT_THEME_TOML
        );
    }

    #[test]
    fn ensure_files_preserves_agent_instructions_and_memory() {
        let (_directory, storage) = fresh();
        fs::write(&storage.agents_path, "user instruction\n").unwrap();
        fs::write(&storage.memory_path, "remember this\n").unwrap();

        storage.ensure_files().unwrap();

        assert_eq!(
            fs::read_to_string(&storage.agents_path).unwrap(),
            "user instruction\n"
        );
        assert_eq!(
            fs::read_to_string(&storage.memory_path).unwrap(),
            "remember this\n"
        );
    }

    #[test]
    fn editor_setting_precedes_environment_fallbacks() {
        assert_eq!(
            resolve_editor_command(Some("code -w"), Some("nvim"), Some("vim")),
            "code -w"
        );
        assert_eq!(
            resolve_editor_command(None, Some("nvim"), Some("vim")),
            "nvim"
        );
        assert_eq!(resolve_editor_command(None, None, Some("vim")), "vim");
        assert_eq!(resolve_editor_command(None, None, None), "vi");
        assert_eq!(
            resolve_editor_command(Some("  "), Some("nvim"), None),
            "nvim"
        );
    }

    #[test]
    fn optional_commands_and_theme_changes_preserve_settings_comments() {
        let defaults: SettingsFile = toml::from_str(DEFAULT_SETTINGS).unwrap();
        assert_eq!(defaults.editor, None);
        assert_eq!(defaults.shell, None);
        assert!(!defaults.show_full_thinking);
        assert_eq!(defaults.export_directory, "~");

        let (_directory, storage) = fresh();
        assert_eq!(storage.default_export_directory().unwrap(), "~");
        storage.write_theme_selection("custom").unwrap();
        assert_eq!(
            fs::read_to_string(&storage.settings_path).unwrap(),
            DEFAULT_SETTINGS.replacen("theme = \"default\"", "theme = \"custom\"", 1)
        );

        fs::write(
            &storage.settings_path,
            "theme = \"default\"\neditor = \"hx\"\nshell = \"fish\"\nexport_directory = \"~\"\n",
        )
        .unwrap();
        storage.write_theme_selection("custom").unwrap();
        let settings = storage.load_settings().unwrap();
        assert_eq!(settings.theme, "custom");
        assert_eq!(settings.editor.as_deref(), Some("hx"));
        assert_eq!(settings.shell.as_deref(), Some("fish"));
        assert!(!settings.show_full_thinking);
        assert_eq!(settings.export_directory, "~");
        assert_eq!(storage.editor_command().unwrap(), "hx");
        assert_eq!(storage.terminal_shell().unwrap().as_deref(), Some("fish"));

        fs::write(
            &storage.settings_path,
            "theme = \"default\"\neditor = \"  \"\nshell = \"  \"\nexport_directory = \"~\"\n",
        )
        .unwrap();
        // The storage-level fallback reads the ambient environment, so pin it
        // down to keep this test hermetic.
        let old_editor = std::env::var("EDITOR").ok();
        let old_visual = std::env::var("VISUAL").ok();
        std::env::set_var("EDITOR", "");
        std::env::set_var("VISUAL", "");
        assert_eq!(storage.editor_command().unwrap(), "vi");
        assert_eq!(storage.terminal_shell().unwrap(), None);
        match old_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }
        match old_visual {
            Some(value) => std::env::set_var("VISUAL", value),
            None => std::env::remove_var("VISUAL"),
        }
    }

    #[test]
    fn default_export_directory_returns_trimmed_config_and_rejects_blank() {
        let (_directory, storage) = fresh();
        assert_eq!(storage.default_export_directory().unwrap(), "~");

        fs::write(
            &storage.settings_path,
            "theme = \"default\"\nexport_directory = \"  /tmp/exports  \"\n",
        )
        .unwrap();
        assert_eq!(storage.default_export_directory().unwrap(), "/tmp/exports");

        fs::write(
            &storage.settings_path,
            "theme = \"default\"\nexport_directory = \"   \"\n",
        )
        .unwrap();
        let error = storage.default_export_directory().unwrap_err();
        assert!(format!("{error:#}").contains("export_directory"));

        // A missing field fails parsing instead of silently falling back.
        fs::write(&storage.settings_path, "theme = \"default\"\n").unwrap();
        assert!(storage.default_export_directory().is_err());
    }

    #[test]
    fn ensure_files_never_moves_existing_root_files() {
        let root_dir = tempdir().unwrap();
        let st = Storage::new(root_dir.path()).unwrap();
        fs::create_dir_all(&st.root).unwrap();
        fs::write(st.root.join("Legacy.md"), "legacy").unwrap();
        fs::write(st.root.join("CHAT.md"), "old chat").unwrap();
        fs::write(st.root.join("TODO.md"), "old todo").unwrap();
        fs::write(st.root.join("ARCHIVE.md"), "old archive").unwrap();

        st.ensure_files().unwrap();

        assert_eq!(
            fs::read_to_string(st.root.join("Legacy.md")).unwrap(),
            "legacy"
        );
        assert_eq!(
            fs::read_to_string(st.root.join("CHAT.md")).unwrap(),
            "old chat"
        );
        assert_eq!(
            fs::read_to_string(st.root.join("TODO.md")).unwrap(),
            "old todo"
        );
        assert_eq!(
            fs::read_to_string(st.root.join("ARCHIVE.md")).unwrap(),
            "old archive"
        );
        assert!(!st.data_dir.join("Legacy.md").exists());
        assert!(fs::read_dir(&st.daily_dir).unwrap().next().is_none());
        assert!(!st.config_dir.join("legacy").exists());
    }

    fn export_storage() -> (tempfile::TempDir, Storage) {
        let directory = tempdir().unwrap();
        let storage = Storage::new(directory.path().canonicalize().unwrap().join(".nole")).unwrap();
        storage.ensure_files().unwrap();
        (directory, storage)
    }

    fn assert_no_export_temp_residue(directory: &Path) {
        let names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|name| name.contains(".nole-export-")),
            "export temp residue: {names:?}"
        );
    }

    #[test]
    fn export_temp_files_are_unique_siblings() {
        let (_directory, storage) = export_storage();
        let parent = storage.root.parent().unwrap();
        let dir = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority()).unwrap();
        let destination = parent.join("unique.md");
        let (first, first_name) =
            create_export_temp(&dir, OsStr::new("unique.md"), &destination).unwrap();
        let (second, second_name) =
            create_export_temp(&dir, OsStr::new("unique.md"), &destination).unwrap();
        assert_ne!(first_name, second_name);
        assert!(first_name
            .to_string_lossy()
            .starts_with(".unique.md.nole-export-"));
        assert_eq!(Path::new(&first_name).parent(), Some(Path::new("")));
        drop(first);
        drop(second);
        dir.remove_file(&first_name).unwrap();
        dir.remove_file(&second_name).unwrap();
    }

    #[test]
    fn atomic_publish_never_clobbers_an_existing_destination() {
        let (_directory, storage) = export_storage();
        let parent = storage.root.parent().unwrap();
        let dir = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority()).unwrap();
        let temp_name = ".target.md.nole-export-test.tmp";
        let destination = parent.join("target.md");
        dir.write(temp_name, "complete").unwrap();
        fs::write(&destination, "winner").unwrap();
        let error = publish_no_overwrite(
            &dir,
            OsStr::new(temp_name),
            OsStr::new("target.md"),
            &destination,
        )
        .unwrap_err();
        assert!(error.to_string().contains("already exists"));
        assert_eq!(fs::read_to_string(&destination).unwrap(), "winner");
        assert!(dir.exists(temp_name), "caller still owns the temp file");
        dir.remove_file(temp_name).unwrap();
        fs::remove_file(&destination).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn export_failure_leaves_no_destination_or_temp_fragment() {
        use std::os::unix::fs::PermissionsExt;
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("write-fail.md");
        fs::write(&source, "content").unwrap();
        let prepared = storage
            .prepare_export(
                &source,
                Path::new("out.md"),
                ExportFormat::Original,
                ExportDestinationPolicy::CreateNew,
            )
            .unwrap();
        let parent = storage.root.parent().unwrap();
        // A read-only parent makes the write path fail after preparation.
        let locked = fs::metadata(parent).unwrap().permissions();
        fs::set_permissions(parent, fs::Permissions::from_mode(0o555)).unwrap();
        let result = storage.publish_export(&prepared);
        fs::set_permissions(parent, locked).unwrap();
        assert!(result.is_err());
        assert!(!parent.join("out.md").exists());
        assert_no_export_temp_residue(parent);
    }

    #[test]
    fn original_export_preserves_binary_bytes_and_never_overwrites() {
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("binary.dat");
        let bytes = b"\0\xffexact\r\n";
        fs::write(&source, bytes).unwrap();
        let prepared = storage
            .prepare_export(
                &source,
                Path::new("published.dat"),
                ExportFormat::Original,
                ExportDestinationPolicy::CreateNew,
            )
            .unwrap();
        let outcome = storage.publish_export(&prepared).unwrap();
        assert_eq!(
            outcome.destination,
            storage.root.parent().unwrap().join("published.dat")
        );
        assert_eq!(outcome.bytes, bytes.len() as u64);
        assert!(outcome.diagnostics.is_empty());
        assert_eq!(fs::read(&outcome.destination).unwrap(), bytes);
        assert_no_export_temp_residue(storage.root.parent().unwrap());
        assert!(storage
            .prepare_export(
                &source,
                Path::new("published.dat"),
                ExportFormat::Original,
                ExportDestinationPolicy::CreateNew,
            )
            .is_err());
    }

    #[test]
    fn html_export_is_complete_and_inert() {
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("unsafe.md");
        fs::write(
            &source,
            "# Title\n\n<script>alert(1)</script> [bad](javascript:alert(1))",
        )
        .unwrap();
        let prepared = storage
            .prepare_export(
                &source,
                Path::new("safe.html"),
                ExportFormat::Html,
                ExportDestinationPolicy::CreateNew,
            )
            .unwrap();
        let outcome = storage.publish_export(&prepared).unwrap();
        assert!(outcome.diagnostics.is_empty());
        assert_eq!(
            outcome.bytes,
            fs::metadata(storage.root.parent().unwrap().join("safe.html"))
                .unwrap()
                .len()
        );
        assert_no_export_temp_residue(storage.root.parent().unwrap());
        let html = fs::read_to_string(storage.root.parent().unwrap().join("safe.html")).unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Content-Security-Policy"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("href=\"javascript:"));
    }

    #[test]
    fn publication_rejects_source_and_destination_races() {
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("source.md");
        fs::write(&source, "before").unwrap();
        let stale = storage
            .prepare_export(
                &source,
                Path::new("stale.md"),
                ExportFormat::Original,
                ExportDestinationPolicy::CreateNew,
            )
            .unwrap();
        fs::write(&source, "changed length").unwrap();
        assert!(storage.publish_export(&stale).is_err());
        assert!(!storage.root.parent().unwrap().join("stale.md").exists());
        assert_no_export_temp_residue(storage.root.parent().unwrap());

        let raced = storage
            .prepare_export(
                &source,
                Path::new("race.md"),
                ExportFormat::Original,
                ExportDestinationPolicy::CreateNew,
            )
            .unwrap();
        fs::write(storage.root.parent().unwrap().join("race.md"), "winner").unwrap();
        assert!(storage.publish_export(&raced).is_err());
        assert_eq!(
            fs::read_to_string(storage.root.parent().unwrap().join("race.md")).unwrap(),
            "winner"
        );
        assert_no_export_temp_residue(storage.root.parent().unwrap());
    }

    #[test]
    fn publication_rejects_same_length_content_with_matching_metadata() {
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("same-length.md");
        fs::write(&source, "before").unwrap();
        let mut prepared = storage
            .prepare_export(
                &source,
                Path::new("same-length-out.md"),
                ExportFormat::Original,
                ExportDestinationPolicy::CreateNew,
            )
            .unwrap();
        fs::write(&source, "differ").unwrap();
        let metadata = fs::metadata(&source).unwrap();
        prepared.identity.length = metadata.len();
        prepared.identity.modified = metadata.modified().unwrap();

        let error = storage.publish_export(&prepared).unwrap_err();
        assert!(format!("{error:#}").contains("content changed"));
        assert!(!storage
            .root
            .parent()
            .unwrap()
            .join("same-length-out.md")
            .exists());
        assert_no_export_temp_residue(storage.root.parent().unwrap());
    }

    #[test]
    fn opened_original_source_detects_in_place_changes() {
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("changing.bin");
        fs::write(&source, b"approved").unwrap();
        let prepared = storage
            .prepare_export(
                &source,
                Path::new("changing.bin"),
                ExportFormat::Original,
                ExportDestinationPolicy::CreateNew,
            )
            .unwrap();
        let opened = File::open(&source).unwrap();
        validate_open_export_source(&opened, &prepared).unwrap();
        fs::write(&source, b"changed after approval").unwrap();
        assert!(validate_open_export_source(&opened, &prepared).is_err());
    }

    #[test]
    fn replace_existing_export_atomically_replaces_a_regular_file() {
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("replace-source.md");
        fs::write(&source, "new content").unwrap();
        let parent = storage.root.parent().unwrap();
        let destination = parent.join("replace-target.md");
        fs::write(&destination, "old content").unwrap();
        let prepared = storage
            .prepare_export(
                &source,
                Path::new("replace-target.md"),
                ExportFormat::Original,
                ExportDestinationPolicy::ReplaceExisting,
            )
            .unwrap();
        let outcome = storage.publish_export(&prepared).unwrap();
        assert_eq!(outcome.destination, destination);
        assert_eq!(outcome.bytes, "new content".len() as u64);
        assert!(outcome.diagnostics.is_empty());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "new content");
        assert_no_export_temp_residue(parent);
    }

    #[cfg(unix)]
    #[test]
    fn replace_existing_prepare_rejects_missing_directories_symlinks_and_special_files() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("replace-kind-source.md");
        fs::write(&source, "content").unwrap();
        let parent = storage.root.parent().unwrap();
        let prepare = |name: &str| {
            storage
                .prepare_export(
                    &source,
                    Path::new(name),
                    ExportFormat::Original,
                    ExportDestinationPolicy::ReplaceExisting,
                )
                .unwrap_err()
                .to_string()
        };

        assert!(prepare("missing-target.md").contains("does not exist"));

        let directory = parent.join("replace-kind-dir");
        fs::create_dir(&directory).unwrap();
        assert!(prepare("replace-kind-dir").contains("not a regular file"));

        let symlink_target = parent.join("replace-kind-real.md");
        fs::write(&symlink_target, "target").unwrap();
        let symlink_path = parent.join("replace-kind-link.md");
        symlink(&symlink_target, &symlink_path).unwrap();
        assert!(prepare("replace-kind-link.md").contains("symlink"));

        let socket = parent.join("replace-kind.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        assert!(prepare("replace-kind.sock").contains("not a regular file"));
        drop(listener);
    }

    #[cfg(unix)]
    #[test]
    fn replace_existing_publish_revalidates_the_destination_before_swapping() {
        use std::os::unix::fs::symlink;
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("replace-race-source.md");
        fs::write(&source, "content").unwrap();
        let parent = storage.root.parent().unwrap();
        let destination = parent.join("replace-race.md");
        fs::write(&destination, "old").unwrap();
        let prepared = storage
            .prepare_export(
                &source,
                Path::new("replace-race.md"),
                ExportFormat::Original,
                ExportDestinationPolicy::ReplaceExisting,
            )
            .unwrap();

        // The destination is swapped for a symlink after preparation: the
        // publish must refuse and leave the symlink and its target intact.
        fs::remove_file(&destination).unwrap();
        let target = parent.join("replace-race-target.md");
        fs::write(&target, "precious").unwrap();
        symlink(&target, &destination).unwrap();
        let error = storage.publish_export(&prepared).unwrap_err();
        assert!(format!("{error:#}").contains("symlink"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "precious");
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_no_export_temp_residue(parent);
    }

    #[cfg(unix)]
    #[test]
    fn export_destination_is_overwritable_reports_regular_files_only() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;
        let (_directory, storage) = export_storage();
        let parent = storage.root.parent().unwrap();
        let overwritable = |name: &str| {
            storage
                .export_destination_is_overwritable(Path::new(name))
                .unwrap()
        };

        assert!(!overwritable("missing.md"));

        let regular = parent.join("overwritable-regular.md");
        fs::write(&regular, "content").unwrap();
        assert!(overwritable("overwritable-regular.md"));

        let directory = parent.join("overwritable-dir");
        fs::create_dir(&directory).unwrap();
        assert!(!overwritable("overwritable-dir"));

        let symlink_path = parent.join("overwritable-link.md");
        symlink(&regular, &symlink_path).unwrap();
        assert!(!overwritable("overwritable-link.md"));

        let dangling = parent.join("overwritable-dangling.md");
        symlink(parent.join("overwritable-nowhere.md"), &dangling).unwrap();
        assert!(!overwritable("overwritable-dangling.md"));

        let socket = parent.join("overwritable.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        assert!(!overwritable("overwritable.sock"));
        drop(listener);
    }

    #[test]
    fn leading_home_component_expands_to_home_directory() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            expand_leading_home(Path::new("~")).unwrap().as_deref(),
            Some(home.as_path())
        );
        assert_eq!(
            expand_leading_home(Path::new("~/docs/export.md")).unwrap(),
            Some(home.join("docs/export.md"))
        );
        // The native separator (backslash on Windows) must expand the same
        // way, which string-prefix matching on "~/" misses.
        let mut native = PathBuf::from("~");
        native.push("docs");
        native.push("export.md");
        assert_eq!(
            expand_leading_home(&native).unwrap(),
            Some(home.join("docs/export.md"))
        );
        assert_eq!(expand_leading_home(Path::new("~user")).unwrap(), None);
        assert_eq!(expand_leading_home(Path::new("relative.md")).unwrap(), None);
        assert_eq!(expand_leading_home(Path::new("/abs/out.md")).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn publish_is_bound_to_the_parent_directory_handle_opened_at_prepare_time() {
        use std::os::unix::fs::symlink;
        let (_directory, storage) = export_storage();
        let source = storage.data_dir.join("bound-source.md");
        fs::write(&source, "payload").unwrap();
        let parent = storage.root.parent().unwrap();
        let exports = parent.join("exports");
        fs::create_dir(&exports).unwrap();
        let prepared = storage
            .prepare_export(
                &source,
                Path::new("exports/out.md"),
                ExportFormat::Original,
                ExportDestinationPolicy::CreateNew,
            )
            .unwrap();

        // After preparation the parent directory is renamed away and the
        // original path is replaced with a symlink to a decoy directory. The
        // publish must only ever act on the directory handle bound at prepare
        // time: it may error because the display path moved, or it may
        // publish into the original (moved) directory, but it must never
        // write through the symlink into the decoy.
        let moved = parent.join("exports-moved");
        fs::rename(&exports, &moved).unwrap();
        let decoy = parent.join("exports-decoy");
        fs::create_dir(&decoy).unwrap();
        symlink(&decoy, &exports).unwrap();

        match storage.publish_export(&prepared) {
            Ok(outcome) => {
                assert_eq!(fs::read_to_string(moved.join("out.md")).unwrap(), "payload");
                assert_eq!(outcome.destination, exports.join("out.md"));
            }
            Err(error) => {
                let message = format!("{error:#}");
                assert!(
                    message.contains("symlink") || message.contains("does not exist"),
                    "unexpected publish error: {message}"
                );
            }
        }
        assert!(
            !decoy.join("out.md").exists(),
            "publish must never write through the swapped parent path"
        );
        assert!(
            fs::read_dir(&decoy).unwrap().next().is_none(),
            "decoy directory must be completely untouched"
        );
        assert_no_export_temp_residue(&moved);
        assert_no_export_temp_residue(parent);
    }
}

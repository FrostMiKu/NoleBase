//! Markdown persistence for Nole.
//!
//! Daily entries are persisted as one Markdown file per day under `daily/`.
//! The first entry of a day creates `YYYY-MM-DD.md`; later entries append to it.
//! `archives/` is a flat archive containing both whole daily files and articles
//! moved from `data/`; archived daily files retain their `YYYY-MM-DD.md` names.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::agent_session::AgentSession;
use crate::model::{DailyNote, NoteFile, SearchHit, TodoItem};
use crate::theme::Theme;

const CONFIG_DIR: &str = "config";
const AI_CONFIG_FILE: &str = "ai.toml";
const SETTINGS_FILE: &str = "settings.toml";
const AGENT_SESSION_FILE: &str = "agent-session.json";
const AGENTS_FILE: &str = "AGENTS.md";
const MEMORY_FILE: &str = "MEMORY.md";
const TEMPLATE_FILE: &str = "template.mb";
const THEMES_DIR: &str = "themes";
const DATA_DIR: &str = "data";
const DAILY_DIR: &str = "daily";
const ARCHIVES_DIR: &str = "archives";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    theme: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedTheme {
    pub requested: String,
    pub active: String,
    pub source: Option<PathBuf>,
    pub theme: Theme,
}

impl LoadedTheme {
    fn built_in_default_for(requested: &str) -> Self {
        Self {
            requested: requested.to_string(),
            active: "default".to_string(),
            source: None,
            theme: Theme::default(),
        }
    }
}

fn serialize_settings(theme: &str) -> Result<String> {
    toml::to_string_pretty(&SettingsFile {
        theme: theme.to_string(),
    })
    .context("serializing settings")
}

/// Filesystem locations backing the notes.
#[derive(Debug, Clone)]
pub struct Storage {
    pub root: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub daily_dir: PathBuf,
    pub archives_dir: PathBuf,
    pub ai_config_path: PathBuf,
    pub settings_path: PathBuf,
    pub agent_session_path: PathBuf,
    pub themes_dir: PathBuf,
    pub agents_path: PathBuf,
    pub memory_path: PathBuf,
    pub template_path: PathBuf,
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
            daily_dir: root.join(DAILY_DIR),
            archives_dir: root.join(ARCHIVES_DIR),
            ai_config_path: root.join(CONFIG_DIR).join(AI_CONFIG_FILE),
            settings_path: root.join(CONFIG_DIR).join(SETTINGS_FILE),
            agent_session_path: root.join(CONFIG_DIR).join(AGENT_SESSION_FILE),
            themes_dir: root.join(THEMES_DIR),
            agents_path: root.join(CONFIG_DIR).join(AGENTS_FILE),
            memory_path: root.join(MEMORY_FILE),
            template_path: root.join(TEMPLATE_FILE),
            root,
        })
    }

    /// Create the storage layout and default configuration.
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
            "api_key = \"\"\n",
            "tavily_api_key = \"\"\n",
            "model = \"claude-sonnet-4-5\"\n",
            "base_url = \"https://api.anthropic.com\"\n",
            "max_tokens = 8192\n",
            "context_window_tokens = 200000\n",
            "max_rounds = 25\n",
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
        file.write_all(serialize_settings("default")?.as_bytes())?;
        Ok(())
    }

    fn write_default_theme(&self, path: &Path) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        file.write_all(crate::theme::DEFAULT_THEME_TOML.as_bytes())?;
        Ok(())
    }

    pub fn load_theme(&self, previous_random_source: Option<&Path>) -> Result<LoadedTheme> {
        let requested = self.load_theme_selection()?;
        self.resolve_theme(&requested, previous_random_source)
    }

    pub fn load_theme_selection(&self) -> Result<String> {
        let source = match fs::read_to_string(&self.settings_path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok("default".to_string())
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading {}", self.settings_path.display()))
            }
        };
        let settings: SettingsFile = toml::from_str(&source)
            .with_context(|| format!("parsing {}", self.settings_path.display()))?;
        Ok(settings.theme)
    }

    pub fn write_theme_selection(&self, selection: &str) -> Result<()> {
        fs::write(&self.settings_path, serialize_settings(selection)?)
            .with_context(|| format!("writing {}", self.settings_path.display()))
    }

    pub fn load_agent_session(&self) -> Result<Option<AgentSession>> {
        let file = match fs::File::open(&self.agent_session_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("opening {}", self.agent_session_path.display()))
            }
        };
        serde_json::from_reader(file)
            .with_context(|| format!("parsing {}", self.agent_session_path.display()))
            .map(Some)
    }

    pub fn write_agent_session(&self, session: &AgentSession) -> Result<()> {
        if session.is_empty() {
            self.clear_agent_session()?;
            return Ok(());
        }
        fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating {}", self.config_dir.display()))?;
        let temporary_path = self
            .config_dir
            .join(format!(".{AGENT_SESSION_FILE}.{}.tmp", std::process::id()));
        let result = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary_path)
                .with_context(|| format!("creating {}", temporary_path.display()))?;
            serde_json::to_writer_pretty(&mut file, session)
                .context("serializing Agent session")?;
            file.write_all(b"\n")?;
            file.sync_all()
                .with_context(|| format!("syncing {}", temporary_path.display()))?;
            fs::rename(&temporary_path, &self.agent_session_path).with_context(|| {
                format!(
                    "replacing {} using {}",
                    self.agent_session_path.display(),
                    temporary_path.display()
                )
            })?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result
    }

    pub fn clear_agent_session(&self) -> Result<bool> {
        match fs::remove_file(&self.agent_session_path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("removing {}", self.agent_session_path.display())),
        }
    }

    pub fn select_theme(&self, selection: &str) -> Result<LoadedTheme> {
        let loaded = self.resolve_theme(selection, None)?;
        self.write_theme_selection(selection)?;
        Ok(loaded)
    }

    pub fn list_theme_names(&self) -> Result<Vec<String>> {
        Ok(self
            .theme_files()?
            .into_iter()
            .map(|(name, _)| name)
            .collect())
    }

    fn resolve_theme(
        &self,
        requested: &str,
        previous_random_source: Option<&Path>,
    ) -> Result<LoadedTheme> {
        if requested == "default" {
            return self.load_default_theme(requested);
        }

        let files = self.theme_files()?;
        if requested == "random" {
            if let Some(previous) = previous_random_source {
                if let Some((name, path)) = files.iter().find(|(_, path)| path == previous) {
                    return self.load_theme_file(requested, name, path);
                }
            }

            let mut valid = Vec::new();
            for (name, path) in &files {
                if let Ok(theme) = self.parse_theme_file(path) {
                    valid.push((name, path, theme));
                }
            }
            if valid.is_empty() {
                return self.load_default_theme(requested);
            }
            let (name, path, theme) = valid.swap_remove(fastrand::usize(..valid.len()));
            return Ok(LoadedTheme {
                requested: requested.to_string(),
                active: name.clone(),
                source: Some(path.clone()),
                theme,
            });
        }

        match files.into_iter().find(|(name, _)| name == requested) {
            Some((name, path)) => self.load_theme_file(requested, &name, &path),
            None => self.load_default_theme(requested),
        }
    }

    fn load_default_theme(&self, requested: &str) -> Result<LoadedTheme> {
        let path = self.themes_dir.join("default.toml");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                self.load_theme_file(requested, "default", &path)
            }
            Ok(_) => Ok(LoadedTheme::built_in_default_for(requested)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(LoadedTheme::built_in_default_for(requested))
            }
            Err(error) => Err(error).with_context(|| format!("reading theme {}", path.display())),
        }
    }

    fn load_theme_file(&self, requested: &str, name: &str, path: &Path) -> Result<LoadedTheme> {
        Ok(LoadedTheme {
            requested: requested.to_string(),
            active: name.to_string(),
            source: Some(path.to_path_buf()),
            theme: self.parse_theme_file(path)?,
        })
    }

    fn parse_theme_file(&self, path: &Path) -> Result<Theme> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("reading theme {}", path.display()))?;
        Theme::from_toml(&source).with_context(|| format!("loading theme {}", path.display()))
    }

    fn theme_files(&self) -> Result<Vec<(String, PathBuf)>> {
        let mut themes = Vec::new();
        for entry in fs::read_dir(&self.themes_dir)
            .with_context(|| format!("reading {}", self.themes_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let is_toml = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"));
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if is_toml && !name.is_empty() && name != "default" && name != "random" {
                themes.push((name.to_string(), path));
            }
        }
        themes.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(themes)
    }

    /// Load one card per daily file, oldest first.
    pub fn load_daily_notes(&self) -> Result<Vec<DailyNote>> {
        self.daily_dates()?
            .into_iter()
            .map(|date| self.read_daily(date))
            .collect()
    }

    /// Append content to today's daily note, creating it on first send.
    pub fn append_to_today(&self, body: &str) -> Result<DailyNote> {
        let date = Local::now().date_naive();
        self.append_daily_for_date(date, body)?;
        self.read_daily(date)
    }

    pub fn append_daily(&self, date: &str, body: &str) -> Result<DailyNote> {
        let date = parse_daily_date(date)?;
        self.append_daily_for_date(date, body)?;
        self.read_daily(date)
    }

    fn append_daily_for_date(&self, date: NaiveDate, body: &str) -> Result<()> {
        if body.trim().is_empty() {
            bail!("daily content must not be empty");
        }
        fs::create_dir_all(&self.daily_dir)?;
        let path = self.daily_path(date);
        let content = if path.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            format!("\n{body}\n")
        } else {
            format!("{body}\n")
        };
        append_text(&path, &content)
    }

    pub fn read_daily_by_date(&self, date: &str) -> Result<DailyNote> {
        self.read_daily(parse_daily_date(date)?)
    }

    fn read_daily(&self, date: NaiveDate) -> Result<DailyNote> {
        let path = self.daily_path(date);
        let mut body = fs::read_to_string(&path)
            .with_context(|| format!("reading daily note {}", path.display()))?;
        if body.ends_with('\n') {
            body.pop();
        }
        Ok(DailyNote { date, body })
    }

    pub fn remove_daily(&self, date: &str) -> Result<bool> {
        let path = self.daily_file_path(date)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| format!("deleting {}", path.display())),
        }
    }

    /// Scan task-list items from every daily file, newest day first.
    pub fn load_todo_tasks(&self) -> Vec<TodoItem> {
        let mut items: Vec<TodoItem> = Vec::new();
        let mut dates = self.daily_dates().unwrap_or_default();
        dates.reverse();
        for date in dates {
            let text = fs::read_to_string(self.daily_path(date)).unwrap_or_default();
            let mut active_task = None;
            for line in text.lines() {
                if let Some((checked, body)) = parse_task_line(line) {
                    items.push(TodoItem {
                        checked,
                        text: body.to_string(),
                    });
                    active_task = Some(items.len() - 1);
                } else if !line.trim().is_empty()
                    && (line.starts_with(' ') || line.starts_with('\t'))
                {
                    if let Some(item) = active_task.and_then(|index| items.get_mut(index)) {
                        item.text.push('\n');
                        item.text.push_str(line.trim());
                    }
                } else if !line.trim().is_empty() {
                    active_task = None;
                }
            }
        }
        items
    }

    /// Flip the completion state of the indexed task across all daily files.
    /// Returns `true` if a task at that index was toggled.
    pub fn toggle_todo_task(&self, index: usize) -> Result<bool> {
        let mut dates = self.daily_dates()?;
        dates.reverse();
        let mut count = 0usize;
        for date in dates {
            let path = self.daily_path(date);
            let text = fs::read_to_string(&path)?;
            let mut out = String::with_capacity(text.len());
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
                fs::write(path, out)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Perform an explicit content scan for Agent tools and other non-interactive callers.
    pub fn search_file_lines(&self, query: &str) -> Vec<SearchHit> {
        let query = query.to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        const CAP: usize = 200;
        let mut hits = Vec::new();
        let daily = self
            .list_note_files_in(&self.daily_dir, false)
            .unwrap_or_default();
        let notes = self.list_note_files().unwrap_or_default();
        let archives = self.list_archived_note_files().unwrap_or_default();
        for file in daily.into_iter().chain(notes).chain(archives) {
            let Ok(source) = fs::read_to_string(&file.path) else {
                continue;
            };
            if append_search_hits(&mut hits, &file.path, &source, &query, CAP) {
                break;
            }
        }
        hits
    }

    /// Append a DailyNote to a managed note, then remove its daily file.
    pub fn move_to_markdown(&self, target: &Path, note: &DailyNote) -> Result<String> {
        let safe = self.validate_target(target)?;
        let content = append_markdown_section(&safe, note)?;
        let removed = self.remove_daily(&note.date.to_string())?;
        debug_assert!(removed, "moved DailyNote not found after append");
        Ok(content)
    }

    /// Move one daily file into archives without rewriting its contents.
    pub fn archive_daily(&self, date: &str) -> Result<PathBuf> {
        let date = parse_daily_date(date)?;
        let source = self.daily_path(date);
        let destination = self.archives_dir.join(date_file_name(date));
        if destination.exists() {
            bail!("archive already exists for {date}");
        }
        fs::rename(&source, &destination).with_context(|| {
            format!(
                "archiving daily note {} to {}",
                source.display(),
                destination.display()
            )
        })?;
        Ok(destination)
    }

    pub fn restore_archived_daily(&self, date: &str) -> Result<()> {
        let date = parse_daily_date(date)?;
        let source = self.archives_dir.join(date_file_name(date));
        let destination = self.daily_path(date);
        if destination.exists() {
            bail!("daily note already exists for {date}");
        }
        fs::rename(source, destination)?;
        Ok(())
    }

    /// Restore a deleted or filed DailyNote.
    pub fn restore_daily(&self, note: &DailyNote) -> Result<()> {
        let path = self.daily_path(note.date);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(note.body.as_bytes())?;
        if !note.body.ends_with('\n') {
            file.write_all(b"\n")?;
        }
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

    /// Replace a daily file's complete card body.
    pub fn replace_daily(&self, note: &DailyNote) -> Result<bool> {
        let path = self.daily_path(note.date);
        if !path.is_file() {
            return Ok(false);
        }
        let mut body = note.body.clone();
        if !body.ends_with('\n') {
            body.push('\n');
        }
        fs::write(path, body)?;
        Ok(true)
    }

    fn daily_path(&self, date: NaiveDate) -> PathBuf {
        self.daily_dir.join(date_file_name(date))
    }

    /// Return the physical path for a validated `YYYY-MM-DD` daily date.
    pub fn daily_file_path(&self, date: &str) -> Result<PathBuf> {
        Ok(self.daily_path(parse_daily_date(date)?))
    }

    pub fn daily_date_for_path(&self, path: &Path) -> Option<NaiveDate> {
        (path.parent() == Some(self.daily_dir.as_path()))
            .then(|| date_from_path(path))
            .flatten()
    }

    fn daily_dates(&self) -> Result<Vec<NaiveDate>> {
        let mut dates = Vec::new();
        for entry in fs::read_dir(&self.daily_dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_file() && !file_type.is_symlink() {
                if let Some(date) = date_from_path(&entry.path()) {
                    dates.push(date);
                }
            }
        }
        dates.sort_unstable();
        Ok(dates)
    }

    /// Create a new note in `data/` from a user-entered name, returning its path.
    /// Existing filesystem entries are never overwritten.
    pub fn create_named_file(&self, name: &str) -> Result<PathBuf> {
        let file_name = normalize_new_name(name)?;
        self.create_named_file_with_content(&file_name, &format!("# {}\n\n", stem(&file_name)))
    }

    pub fn create_named_file_from_template(&self, name: &str) -> Result<PathBuf> {
        let file_name = normalize_new_name(name)?;
        let template = fs::read_to_string(&self.template_path)
            .with_context(|| format!("reading {}", self.template_path.display()))?;
        self.create_named_file_with_content(&file_name, &template)
    }

    fn create_named_file_with_content(&self, file_name: &str, content: &str) -> Result<PathBuf> {
        fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating {}", self.data_dir.display()))?;
        let path = self.data_dir.join(file_name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("creating new note file {}", path.display()))?;
        file.write_all(content.as_bytes())?;
        Ok(path)
    }

    /// Rename a managed data note to `new_name` (normalized), returning the
    /// new path. Refuses protected names and never overwrites an existing entry.
    pub fn rename_file(&self, from: &Path, new_name: &str) -> Result<PathBuf> {
        let from = self.validate_target(from)?;

        let name = normalize_new_name(new_name)?;
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

    /// Rename a direct archived note without overwriting an existing entry.
    pub fn rename_archived_file(&self, from: &Path, new_name: &str) -> Result<PathBuf> {
        let from = self.validate_archived_target(from)?;
        let name = normalize_new_name(new_name)?;
        let to = self.archives_dir.join(&name);
        rename_without_overwrite(&from, &to, &name)
    }

    /// Move a data note into `archives/` without overwriting an existing entry.
    pub fn archive_note(&self, path: &Path) -> Result<PathBuf> {
        let source = self.validate_target(path)?;
        let name = source.file_name().context("note has no file name")?;
        let destination = self.archives_dir.join(name);
        move_without_overwrite(&source, &destination)?;
        Ok(destination)
    }

    /// Restore an archived note into `data/` without overwriting an existing entry.
    pub fn restore_archived_note(&self, path: &Path) -> Result<PathBuf> {
        let source = self.validate_archived_target(path)?;
        let name = source
            .file_name()
            .context("archived note has no file name")?;
        let destination = self.data_dir.join(name);
        move_without_overwrite(&source, &destination)?;
        Ok(destination)
    }

    /// Delete a managed data note. Protected files cannot be deleted.
    pub fn delete_file(&self, path: &Path) -> Result<()> {
        let path = self.validate_target(path)?;
        fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
        Ok(())
    }

    pub fn delete_archived_file(&self, path: &Path) -> Result<()> {
        let path = self.validate_archived_target(path)?;
        fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
        Ok(())
    }

    /// Read a managed note after applying the same path checks used by
    /// mutating operations.
    pub fn read_note_file(&self, path: &Path) -> Result<String> {
        let path = self.validate_target(path)?;
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }

    /// Append to an article in either `data/` or `archives/`.
    pub fn append_document(&self, path: &Path, body: &str) -> Result<()> {
        if body.trim().is_empty() {
            bail!("note content must not be empty");
        }
        self.read_document_file(path)?;
        let content = if path.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            format!("\n{body}\n")
        } else {
            format!("{body}\n")
        };
        append_text(path, &content)
    }

    /// List flat `.md` and `.mb` notes under `data/`, most recently modified first.
    pub fn list_note_files(&self) -> Result<Vec<NoteFile>> {
        self.list_note_files_in(&self.data_dir, false)
    }

    /// List flat `.md` and `.mb` files under `archives/`, most recently modified first.
    pub fn list_archived_note_files(&self) -> Result<Vec<NoteFile>> {
        self.list_note_files_in(&self.archives_dir, true)
    }

    fn list_note_files_in(&self, directory: &Path, archived: bool) -> Result<Vec<NoteFile>> {
        let mut files = Vec::new();
        fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        for entry in fs::read_dir(directory)? {
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
                files.push(NoteFile {
                    path,
                    modified,
                    archived,
                });
            }
        }
        files.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
        });
        Ok(files)
    }

    /// Read a regular note from `archives/` after rejecting symlinks and paths
    /// outside that directory.
    pub fn read_archived_note_file(&self, path: &Path) -> Result<String> {
        let canonical = self.validate_archived_target(path)?;
        fs::read_to_string(canonical).context("reading archived note")
    }

    pub fn validate_archived_target(&self, target: &Path) -> Result<PathBuf> {
        validate_direct_note(&self.archives_dir, target, "archives")
    }

    /// Read an open article after it has been moved anywhere within the Nole
    /// workspace. DailyNotes and configuration remain owned by their
    /// dedicated APIs.
    pub fn read_document_file(&self, path: &Path) -> Result<String> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("checking document {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            bail!("document must be a regular file: {}", path.display());
        }
        let canonical_root = fs::canonicalize(&self.root)
            .with_context(|| format!("resolving Nole root {}", self.root.display()))?;
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("resolving document {}", path.display()))?;
        let config = fs::canonicalize(&self.config_dir).unwrap_or_else(|_| self.config_dir.clone());
        let daily = fs::canonicalize(&self.daily_dir).unwrap_or_else(|_| self.daily_dir.clone());
        if !canonical.starts_with(&canonical_root)
            || canonical.starts_with(config)
            || canonical.starts_with(daily)
            || !is_note_path(&canonical)
        {
            bail!(
                "document must be a managed .md or .mb file: {}",
                path.display()
            );
        }
        fs::read_to_string(canonical).context("reading document")
    }

    /// Resolve an existing regular file embedded by a document.
    pub fn validate_embedded_file(&self, path: &Path) -> Result<PathBuf> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("checking embedded file {}", path.display()))?;
        if !metadata.is_file() {
            bail!("embedded target must be a regular file: {}", path.display());
        }
        fs::canonicalize(path)
            .with_context(|| format!("resolving embedded file {}", path.display()))
    }

    /// Ensure a target is a flat data note.
    /// Existing targets are canonicalized in full; symlinks are always rejected.
    pub fn validate_target(&self, target: &Path) -> Result<PathBuf> {
        if !is_note_path(target) {
            bail!(
                "target must have a .md or .mb extension: {}",
                target.display()
            );
        }

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
                if !is_data_note {
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
                if !is_data_note {
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

fn create_empty_file(path: &Path) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("creating {}", path.display())),
    }
}

fn append_search_hits(
    hits: &mut Vec<SearchHit>,
    path: &Path,
    source: &str,
    query: &str,
    cap: usize,
) -> bool {
    for (index, line) in source.lines().enumerate() {
        let text = line.trim();
        if !text.is_empty() && line.to_lowercase().contains(query) {
            hits.push(SearchHit::FileLine {
                path: path.to_path_buf(),
                line_no: index + 1,
                text: text.to_string(),
            });
            if hits.len() >= cap {
                return true;
            }
        }
    }
    false
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

fn is_note_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("mb")
        })
}

fn validate_direct_note(directory: &Path, target: &Path, label: &str) -> Result<PathBuf> {
    if !is_note_path(target) {
        bail!(
            "target must have a .md or .mb extension: {}",
            target.display()
        );
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
        Ok(_) => bail!(
            "a file named {} already exists",
            to.file_name().unwrap_or_default().to_string_lossy()
        ),
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
    if !(lower.ends_with(".md") || lower.ends_with(".mb")) {
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
    fn archive_moves_the_complete_daily_file_and_can_restore_it() {
        let (_dir, st) = fresh();
        st.append_daily("2026-07-27", "first\n\n- [ ] task")
            .unwrap();
        let source = st.daily_dir.join("2026-07-27.md");
        let original = fs::read_to_string(&source).unwrap();

        let archived = st.archive_daily("2026-07-27").unwrap();
        assert!(!source.exists());
        assert_eq!(archived, st.archives_dir.join("2026-07-27.md"));
        assert_eq!(fs::read_to_string(&archived).unwrap(), original);

        st.restore_archived_daily("2026-07-27").unwrap();
        assert_eq!(fs::read_to_string(source).unwrap(), original);
        assert!(!archived.exists());
    }

    #[test]
    fn load_and_toggle_todo_tasks() {
        let (_dir, st) = fresh();
        st.append_daily("2026-07-26", "- [ ] older task").unwrap();
        st.append_daily("2026-07-27", "- [ ] buy milk\n- [x] write docs")
            .unwrap();

        let items = st.load_todo_tasks();
        assert_eq!(items.len(), 3);
        assert!(!items[0].checked);
        assert_eq!(items[0].text, "buy milk");
        assert!(items[1].checked);
        assert_eq!(items[2].text, "older task");

        // Toggle the first task on, then back off.
        assert!(st.toggle_todo_task(0).unwrap());
        let on = st.load_todo_tasks();
        assert!(on[0].checked);
        assert!(on[1].checked, "other tasks untouched");

        assert!(st.toggle_todo_task(0).unwrap());
        let off = st.load_todo_tasks();
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
    fn embedded_files_must_exist_but_may_be_outside_the_workspace() {
        let (_directory, st) = fresh();
        let file = st.root.join("attachment.pdf");
        fs::write(&file, b"attachment").unwrap();
        assert_eq!(
            st.validate_embedded_file(&file).unwrap(),
            fs::canonicalize(&file).unwrap()
        );
        assert!(st
            .validate_embedded_file(&st.root.join("missing.pdf"))
            .is_err());

        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.pdf");
        fs::write(&outside_file, b"outside").unwrap();
        assert_eq!(
            st.validate_embedded_file(&outside_file).unwrap(),
            fs::canonicalize(&outside_file).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn embedded_files_follow_symlinks_to_regular_files() {
        use std::os::unix::fs::symlink;

        let (_directory, st) = fresh();
        let file = st.root.join("attachment.pdf");
        let link = st.root.join("attachment-link.pdf");
        fs::write(&file, b"attachment").unwrap();
        symlink(&file, &link).unwrap();
        assert_eq!(
            st.validate_embedded_file(&link).unwrap(),
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
    fn replace_daily_updates_only_the_target_date() {
        let (_dir, st) = fresh();
        let note = st.append_to_today("original").unwrap();
        st.append_daily("2026-06-17", "keep me").unwrap();

        let mut updated = note.clone();
        updated.body = "edited body".to_string();
        assert!(st.replace_daily(&updated).unwrap());

        let notes = st.load_daily_notes().unwrap();
        assert_eq!(notes.len(), 2);
        let got = notes
            .iter()
            .find(|candidate| candidate.date == note.date)
            .unwrap();
        assert_eq!(got.body, "edited body");
        assert!(
            notes.iter().any(|x| x.body == "keep me"),
            "others untouched"
        );

        let unknown = DailyNote {
            date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            body: "x".to_string(),
        };
        assert!(!st.replace_daily(&unknown).unwrap());
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
    fn data_notes_allow_chat_todo_and_archive_names() {
        let (_dir, st) = fresh();
        for name in ["chat", "todo.Md", "Archive.MD"] {
            assert!(st.create_named_file(name).unwrap().is_file());
        }
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
    fn append_document_adds_to_an_existing_managed_article_only() {
        let (_dir, st) = fresh();
        let path = st.create_named_file("Article").unwrap();
        fs::write(&path, "# Article\n").unwrap();
        st.append_document(&path, "new paragraph").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# Article\n\nnew paragraph\n"
        );
        assert!(st
            .append_document(Path::new("/tmp/outside.md"), "no")
            .is_err());
        assert!(st.append_document(&path, "   ").is_err());
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
        let archived = storage.archives_dir.join("Project.MB");
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
        storage.append_document(&archived, "later").unwrap();
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
        assert!(fs::read_dir(&st.daily_dir).unwrap().next().is_none());
        assert!(st.append_daily("2026-07-27", "   ").is_err());
        assert!(!st.daily_dir.join("2026-07-27.md").exists());
        assert!(st.ai_config_path.exists());
        assert!(st.settings_path.exists());
        assert!(!st.agent_session_path.exists());
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
        assert_eq!(
            st.agent_session_path.parent(),
            Some(st.config_dir.as_path())
        );
        assert_eq!(st.themes_dir.parent(), Some(st.root.as_path()));
        assert_eq!(st.template_path.parent(), Some(st.root.as_path()));
        let config = fs::read_to_string(&st.ai_config_path).unwrap();
        assert!(config.contains("api_key = \"\""));
        assert!(config.contains("tavily_api_key = \"\""));
        assert!(config.contains("max_tokens = 8192"));
        assert!(config.contains("context_window_tokens = 200000"));
        assert!(config.contains("max_rounds = 25"));
        assert_eq!(
            fs::read_to_string(&st.settings_path).unwrap(),
            "theme = \"default\"\n"
        );
        let loaded = st.load_theme(None).unwrap();
        assert_eq!(loaded.requested, "default");
        assert_eq!(loaded.active, "default");
        assert_eq!(loaded.source.as_deref(), Some(default_theme_path.as_path()));
        assert_eq!(loaded.theme, crate::theme::Theme::default());
        assert_eq!(
            fs::read_to_string(default_theme_path).unwrap(),
            crate::theme::DEFAULT_THEME_TOML
        );
    }

    #[test]
    fn agent_session_round_trip_overwrites_the_single_file_and_clears() {
        use crate::agent_session::{AgentConversation, AgentPanelEntry};

        let (_directory, storage) = fresh();
        let first = AgentSession::from_parts(
            &AgentConversation {
                messages: vec![serde_json::json!({
                    "role": "user",
                    "content": "first"
                })],
            },
            &[AgentPanelEntry::Prompt {
                text: "first".to_string(),
                muted: false,
            }],
            crate::agent_session::TokenUsage::default(),
            0,
            std::time::Duration::ZERO,
        );
        storage.write_agent_session(&first).unwrap();
        assert_eq!(storage.load_agent_session().unwrap(), Some(first));

        let second = AgentSession::from_parts(
            &AgentConversation {
                messages: vec![serde_json::json!({
                    "role": "user",
                    "content": "second"
                })],
            },
            &[AgentPanelEntry::Assistant {
                text: "second reply".to_string(),
                streaming: false,
                final_output: true,
            }],
            crate::agent_session::TokenUsage::default(),
            0,
            std::time::Duration::ZERO,
        );
        storage.write_agent_session(&second).unwrap();
        assert_eq!(storage.load_agent_session().unwrap(), Some(second));
        assert_eq!(
            fs::read_to_string(&storage.agent_session_path)
                .unwrap()
                .matches("\"messages\"")
                .count(),
            1
        );

        assert!(storage.clear_agent_session().unwrap());
        assert!(!storage.agent_session_path.exists());
        assert!(!storage.clear_agent_session().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn agent_session_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, storage) = fresh();
        storage
            .write_agent_session(&AgentSession::from_parts(
                &crate::agent_session::AgentConversation::seeded_for_test(),
                &[],
                crate::agent_session::TokenUsage::default(),
                0,
                std::time::Duration::ZERO,
            ))
            .unwrap();

        let mode = fs::metadata(&storage.agent_session_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
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
    fn custom_theme_selection_is_preserved_and_loaded() {
        let (_directory, storage) = fresh();
        let custom = crate::theme::DEFAULT_THEME_TOML
            .replace("message_agent = \"#1e1e2e\"", "message_agent = \"#010203\"");
        let path = storage.themes_dir.join("custom.toml");
        fs::write(&path, &custom).unwrap();
        storage.write_theme_selection("custom").unwrap();

        storage.ensure_files().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), custom);
        assert_eq!(storage.load_theme_selection().unwrap(), "custom");
        let loaded = storage.load_theme(None).unwrap();
        assert_eq!(loaded.requested, "custom");
        assert_eq!(loaded.active, "custom");
        assert_eq!(loaded.source.as_deref(), Some(path.as_path()));
        assert_eq!(
            loaded.theme.surface_message_agent,
            ratatui::style::Color::Rgb(1, 2, 3)
        );
    }

    #[test]
    fn missing_and_random_theme_selections_fall_back_or_resolve() {
        let (_directory, storage) = fresh();
        let default_path = storage.themes_dir.join("default.toml");
        let edited_default =
            crate::theme::DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#090807\"");
        fs::write(&default_path, &edited_default).unwrap();
        storage.ensure_files().unwrap();
        assert_eq!(fs::read_to_string(&default_path).unwrap(), edited_default);
        storage.write_theme_selection("missing").unwrap();
        let missing = storage.load_theme(None).unwrap();
        assert_eq!(missing.requested, "missing");
        assert_eq!(missing.active, "default");
        assert_eq!(missing.source.as_deref(), Some(default_path.as_path()));
        assert_eq!(
            missing.theme.surface_panel,
            ratatui::style::Color::Rgb(9, 8, 7)
        );

        let custom =
            crate::theme::DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#010203\"");
        let path = storage.themes_dir.join("only.toml");
        fs::write(&path, custom).unwrap();
        let random = storage.select_theme("random").unwrap();
        assert_eq!(random.requested, "random");
        assert_eq!(random.active, "only");
        assert_eq!(random.source.as_deref(), Some(path.as_path()));
        assert_eq!(
            random.theme.surface_panel,
            ratatui::style::Color::Rgb(1, 2, 3)
        );

        let reloaded = storage.load_theme(random.source.as_deref()).unwrap();
        assert_eq!(reloaded.active, "only");
        assert_eq!(reloaded.source, random.source);

        fs::remove_file(default_path).unwrap();
        storage.write_theme_selection("missing").unwrap();
        let built_in = storage.load_theme(None).unwrap();
        assert_eq!(built_in.active, "default");
        assert_eq!(built_in.source, None);
        assert_eq!(built_in.theme, crate::theme::Theme::default());
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
}

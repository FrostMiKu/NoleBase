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

use crate::agent_session::AgentSession;
use crate::export::{ExportDiagnostic, ExportFormat};
use crate::model::{DailyNote, NoteFile, TodoItem};
use crate::theme::Theme;

mod atomic;
mod theme;

pub(crate) use atomic::replace_file_atomically;

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
const AGENT_SESSION_FILE: &str = "agent-session.json";
const AGENTS_FILE: &str = "AGENTS.md";
const MEMORY_FILE: &str = "MEMORY.md";
const TEMPLATE_FILE: &str = "template.mb";
const THEMES_DIR: &str = "themes";
const DATA_DIR: &str = "data";
const DAILY_DIR: &str = "daily";
const ARCHIVES_DIR: &str = "archives";
const SKILLS_DIR: &str = "skills";
pub(crate) const ATTACHMENTS_DIR: &str = "attachments";
pub(crate) const WORKSPACE_DIR: &str = "workspace";

#[derive(Debug, Clone)]
pub(crate) struct AppendReceipt {
    path: PathBuf,
    original_len: u64,
    appended: Vec<u8>,
    created: bool,
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
            skills_dir: root.join(SKILLS_DIR),
            attachments_dir: root.join(ATTACHMENTS_DIR),
            workspace_dir: root.join(WORKSPACE_DIR),
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
    pub fn prepare_export(
        &self,
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        format: ExportFormat,
        policy: ExportDestinationPolicy,
    ) -> Result<PreparedExport> {
        let canonical_root = fs::canonicalize(&self.root).context("canonicalizing Nole root")?;
        let source_input = source.as_ref();
        let source_candidate = if source_input.is_absolute() {
            normalize_lexical(source_input.to_path_buf())?
        } else {
            normalize_lexical(self.root.join(source_input))?
        };
        reject_symlink_components_below(&self.root, &source_candidate)?;
        let canonical_source = fs::canonicalize(&source_candidate)
            .with_context(|| format!("resolving export source {}", source_candidate.display()))?;
        if !canonical_source.starts_with(&canonical_root) {
            bail!("export source must be inside the Nole root");
        }
        let relative = canonical_source.strip_prefix(&canonical_root)?;
        if relative.starts_with(CONFIG_DIR) || relative.starts_with(ATTACHMENTS_DIR) {
            bail!("config and attachment internals cannot be exported");
        }
        let metadata = fs::metadata(&canonical_source)?;
        if !metadata.is_file() {
            bail!("export source must be a regular file");
        }
        let (rendered_source, content_sha256) = if format != ExportFormat::Original {
            let extension = canonical_source
                .extension()
                .and_then(|value| value.to_str());
            if !extension.is_some_and(|value| {
                value.eq_ignore_ascii_case("md") || value.eq_ignore_ascii_case("mb")
            }) {
                bail!(
                    "{} export requires a UTF-8 .md or .mb source",
                    format.label()
                );
            }
            let bytes = fs::read(&canonical_source)
                .with_context(|| format!("reading {}", canonical_source.display()))?;
            let size = u64::try_from(bytes.len()).context("export source is too large")?;
            if size > crate::export::MAX_RENDER_SOURCE_BYTES {
                bail!(
                    "{} export source exceeds the {}-byte limit",
                    format.label(),
                    crate::export::MAX_RENDER_SOURCE_BYTES
                );
            }
            let content_sha256 = sha256_bytes(&bytes);
            (
                Some(String::from_utf8(bytes).context("rendered export source is not UTF-8")?),
                content_sha256,
            )
        } else {
            (None, sha256_file(&canonical_source)?)
        };
        let destination = self.resolve_export_destination(destination.as_ref(), &canonical_root)?;
        format.validate_destination(&destination)?;
        // Open the validated parent directory and bind every publication
        // operation to it, so a later rename or symlink swap of the parent
        // path cannot redirect the publish. Destination policy checks run
        // against this handle too, keeping the prepare-time validation
        // race-free.
        let parent = destination
            .parent()
            .context("export destination has no parent directory")?;
        let file_name = destination
            .file_name()
            .context("export destination needs a file name")?
            .to_os_string();
        let parent_dir =
            cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())
                .with_context(|| format!("opening export directory {}", parent.display()))?;
        match policy {
            ExportDestinationPolicy::CreateNew => {
                if destination_entry_exists(&parent_dir, &file_name)? {
                    bail!(
                        "export destination already exists: {}",
                        destination.display()
                    );
                }
            }
            ExportDestinationPolicy::ReplaceExisting => {
                validate_replaceable_destination(&parent_dir, &file_name, &destination)?;
            }
        }
        Ok(PreparedExport {
            source: canonical_source.clone(),
            destination,
            file_name,
            parent_dir,
            format,
            policy,
            identity: ExportSourceIdentity {
                canonical_path: canonical_source,
                length: metadata.len(),
                modified: metadata
                    .modified()
                    .context("reading export source modification time")?,
                content_sha256,
            },
            rendered_source,
        })
    }

    pub fn publish_export(&self, prepared: &PreparedExport) -> Result<ExportOutcome> {
        self.revalidate_export(prepared)?;
        let (rendered_bytes, rendered_diagnostics) = match prepared.format {
            ExportFormat::Original => (None, Vec::new()),
            ExportFormat::Html => {
                let source = prepared
                    .rendered_source
                    .as_deref()
                    .context("rendered export is missing its prepared source")?;
                let title = prepared
                    .source
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("Nole export");
                let attachments =
                    crate::attachment::AttachmentStore::new(self.attachments_dir.clone());
                let rendered = crate::export::render_html(
                    source,
                    title,
                    &self.root,
                    &prepared.source,
                    &attachments,
                )?;
                (Some(rendered.bytes), rendered.diagnostics)
            }
        };
        let mut original_source = if prepared.format == ExportFormat::Original {
            let source = File::open(&prepared.source)
                .with_context(|| format!("opening {}", prepared.source.display()))?;
            validate_open_export_source(&source, prepared)?;
            Some(BufReader::with_capacity(64 * 1024, source))
        } else {
            None
        };
        // Write into a same-directory temp file and publish atomically, so a
        // failed write, flush, render, or race never leaves a partial final
        // target behind. The temp file is created relative to the parent
        // directory handle bound at prepare time and removed on every failure
        // path.
        let (mut temp_file, temp_name) = create_export_temp(
            &prepared.parent_dir,
            &prepared.file_name,
            &prepared.destination,
        )?;
        let publication = (|| -> Result<u64> {
            let bytes = if let Some(bytes) = &rendered_bytes {
                temp_file.write_all(bytes)?;
                u64::try_from(bytes.len()).context("export is too large")?
            } else {
                let source = original_source
                    .as_mut()
                    .context("missing Original source handle")?;
                let (bytes, copied_sha256) = copy_and_hash(source, &mut temp_file)?;
                if copied_sha256 != prepared.identity.content_sha256 {
                    bail!("export source content changed after preparation");
                }
                bytes
            };
            temp_file.flush()?;
            // Re-check the source and destination after the write, so a
            // change racing the export is caught before anything is linked
            // into place.
            if let Some(source) = original_source.as_ref() {
                validate_open_export_source(source.get_ref(), prepared)?;
            }
            self.revalidate_export_source(prepared)?;
            verify_export_source_content(prepared)?;
            // The destination is checked again right before publication, all
            // relative to the directory handle bound at prepare time, so a
            // change racing the export is caught before anything is linked
            // into place and a swapped parent path cannot redirect the
            // publish. CreateNew refuses any existing destination;
            // ReplaceExisting re-validates that the target is still an
            // existing regular non-symlink file before the atomic swap.
            match prepared.policy {
                ExportDestinationPolicy::CreateNew => {
                    if destination_entry_exists(&prepared.parent_dir, &prepared.file_name)? {
                        bail!(
                            "export destination already exists: {}",
                            prepared.destination.display()
                        );
                    }
                    publish_no_overwrite(
                        &prepared.parent_dir,
                        &temp_name,
                        &prepared.file_name,
                        &prepared.destination,
                    )?;
                }
                ExportDestinationPolicy::ReplaceExisting => {
                    validate_replaceable_destination(
                        &prepared.parent_dir,
                        &prepared.file_name,
                        &prepared.destination,
                    )?;
                    prepared
                        .parent_dir
                        .rename(&temp_name, &prepared.parent_dir, &prepared.file_name)
                        .with_context(|| {
                            format!(
                                "atomically replacing export destination {}",
                                prepared.destination.display()
                            )
                        })?;
                }
            }
            Ok(bytes)
        })();
        drop(temp_file);
        match publication {
            Ok(bytes) => {
                let _ = prepared.parent_dir.remove_file(&temp_name);
                Ok(ExportOutcome {
                    destination: prepared.destination.clone(),
                    bytes,
                    diagnostics: rendered_diagnostics,
                })
            }
            Err(error) => {
                let _ = prepared.parent_dir.remove_file(&temp_name);
                Err(error).context("publishing export")
            }
        }
    }

    fn resolve_export_destination(&self, input: &Path, canonical_root: &Path) -> Result<PathBuf> {
        let expanded = match expand_leading_home(input)? {
            Some(expanded) => expanded,
            None if input.is_absolute() => input.to_path_buf(),
            None => self
                .root
                .parent()
                .context("Nole root has no parent directory")?
                .join(input),
        };
        let normalized = normalize_lexical(expanded)?;
        if normalized.starts_with(canonical_root) {
            bail!("export destination must be outside the Nole root");
        }
        let parent = normalized
            .parent()
            .context("export destination has no parent")?;
        if let Some(base) = self.root.parent().filter(|base| parent.starts_with(base)) {
            reject_symlink_components_below(base, parent)?;
        } else {
            reject_existing_symlink_components(parent)?;
        }
        let canonical_parent = fs::canonicalize(parent).with_context(|| {
            format!(
                "export destination parent does not exist: {}",
                parent.display()
            )
        })?;
        if !fs::metadata(&canonical_parent)?.is_dir() {
            bail!("export destination parent is not a directory");
        }
        if canonical_parent.starts_with(canonical_root) {
            bail!("export destination must be outside the Nole root");
        }
        let name = normalized
            .file_name()
            .context("export destination needs a file name")?;
        Ok(canonical_parent.join(name))
    }

    /// Resolve `destination` exactly as [`Self::prepare_export`] would and
    /// report whether it currently names an existing regular file that is not
    /// a symlink — the only destination an explicit overwrite may replace.
    /// Directories, symlinks, special files, and missing paths report `false`;
    /// a resolution failure is returned so the caller can surface the real
    /// export error instead.
    pub fn export_destination_is_overwritable(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<bool> {
        let canonical_root = fs::canonicalize(&self.root).context("canonicalizing Nole root")?;
        let resolved = self.resolve_export_destination(destination.as_ref(), &canonical_root)?;
        let Ok(symlink_metadata) = fs::symlink_metadata(&resolved) else {
            return Ok(false);
        };
        if symlink_metadata.file_type().is_symlink() {
            return Ok(false);
        }
        Ok(fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_file()))
    }

    fn revalidate_export_source(&self, prepared: &PreparedExport) -> Result<()> {
        let canonical_root = fs::canonicalize(&self.root)?;
        reject_symlink_components_below(&canonical_root, &prepared.source)?;
        let canonical = fs::canonicalize(&prepared.source)?;
        let metadata = fs::metadata(&canonical)?;
        if canonical != prepared.identity.canonical_path
            || !metadata.is_file()
            || metadata.len() != prepared.identity.length
            || metadata.modified()? != prepared.identity.modified
        {
            bail!("export source changed after preparation");
        }
        Ok(())
    }

    fn revalidate_export(&self, prepared: &PreparedExport) -> Result<()> {
        self.revalidate_export_source(prepared)?;
        // The destination is display-only for the actual publication, which
        // runs against the directory handle bound at prepare time. Re-resolve
        // the stored absolute path purely as a consistency check so a moved
        // parent surfaces a clear error instead of silently publishing
        // somewhere the user is no longer looking.
        let canonical_root = fs::canonicalize(&self.root)?;
        let resolved = self.resolve_export_destination(&prepared.destination, &canonical_root)?;
        if resolved != prepared.destination {
            bail!("export destination changed after preparation");
        }
        Ok(())
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
        crate::attachment::AttachmentStore::new(self.attachments_dir.clone()).ensure_layout()?;
        fs::create_dir_all(&self.workspace_dir)
            .with_context(|| format!("creating {}", self.workspace_dir.display()))?;
        crate::skill::ensure_skills_directory(&self.skills_dir)?;
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
        file.write_all(crate::theme::DEFAULT_THEME_TOML.as_bytes())?;
        Ok(())
    }

    pub fn load_theme(&self, previous_random_source: Option<&Path>) -> Result<LoadedTheme> {
        let requested = self.load_theme_selection()?;
        self.resolve_theme(&requested, previous_random_source)
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

    pub fn load_agent_session(&self) -> Result<Option<AgentSession>> {
        let file = match fs::File::open(&self.agent_session_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("opening {}", self.agent_session_path.display()));
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
        let temporary_path = self.config_dir.join(format!(
            ".{AGENT_SESSION_FILE}.{}-{:016x}.tmp",
            std::process::id(),
            fastrand::u64(..)
        ));
        let result = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
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
            replace_file_atomically(&temporary_path, &self.agent_session_path).with_context(
                || {
                    format!(
                        "replacing {} using {}",
                        self.agent_session_path.display(),
                        temporary_path.display()
                    )
                },
            )?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result
    }

    /// Remove the persisted Agent session file. The Agent workspace is
    /// deliberately left untouched: it persists across sessions and the Agent
    /// maintains its contents itself.
    pub fn clear_agent_session(&self) -> Result<bool> {
        match fs::remove_file(&self.agent_session_path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("removing {}", self.agent_session_path.display())),
        }
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
        self.append_to_today_tracked(body).map(|(note, _)| note)
    }

    pub(crate) fn append_to_today_tracked(&self, body: &str) -> Result<(DailyNote, AppendReceipt)> {
        let date = Local::now().date_naive();
        let receipt = self.append_daily_for_date(date, body)?;
        Ok((self.read_daily(date)?, receipt))
    }

    pub fn append_daily(&self, date: &str, body: &str) -> Result<DailyNote> {
        self.append_daily_tracked(date, body).map(|(note, _)| note)
    }

    pub(crate) fn append_daily_tracked(
        &self,
        date: &str,
        body: &str,
    ) -> Result<(DailyNote, AppendReceipt)> {
        let date = parse_daily_date(date)?;
        let receipt = self.append_daily_for_date(date, body)?;
        Ok((self.read_daily(date)?, receipt))
    }

    fn append_daily_for_date(&self, date: NaiveDate, body: &str) -> Result<AppendReceipt> {
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
        append_text_tracked(&path, &content)
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

    /// Append a DailyNote to a managed note, then remove its daily file.
    pub fn move_to_markdown(&self, target: &Path, note: &DailyNote) -> Result<String> {
        let safe = self.validate_target(target)?;
        let content = append_markdown_section(&safe, note)?;
        let removed = self.remove_daily(&note.date.to_string())?;
        debug_assert!(removed, "moved DailyNote not found after append");
        Ok(content)
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
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {} for undo", path.display()))?;
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

    pub fn load_skills(&self) -> Result<crate::skill::SkillCatalog> {
        Ok(crate::skill::load_default_skill_catalog(&self.skills_dir))
    }

    pub fn read_skill(&self, path: &Path) -> Result<crate::skill::Skill> {
        let (_, path) = self.locate_skill(path)?;
        crate::skill::load_skill(&path)
    }

    pub fn rename_skill(&self, from: &Path, new_id: &str) -> Result<PathBuf> {
        let (root, _) = self.locate_skill(from)?;
        crate::skill::rename_skill(&root, from, new_id)
    }

    pub fn delete_skill(&self, path: &Path) -> Result<()> {
        let (root, _) = self.locate_skill(path)?;
        crate::skill::delete_skill(&root, path)
    }

    fn locate_skill(&self, path: &Path) -> Result<(PathBuf, PathBuf)> {
        let roots = crate::skill::default_skill_roots(&self.skills_dir);
        crate::skill::locate_managed_skill(&roots, path)
    }

    /// Read a managed note after applying the same path checks used by
    /// mutating operations.
    pub fn read_note_file(&self, path: &Path) -> Result<String> {
        let path = self.validate_target(path)?;
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }

    /// Append to an article in either `data/` or `archives/` and return the
    /// receipt required to undo that exact write.
    pub(crate) fn append_document_tracked(&self, path: &Path, body: &str) -> Result<AppendReceipt> {
        if body.trim().is_empty() {
            bail!("note content must not be empty");
        }
        self.read_document_file(path)?;
        let content = if path.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            format!("\n{body}\n")
        } else {
            format!("{body}\n")
        };
        append_text_tracked(path, &content)
    }

    pub(crate) fn undo_append(&self, receipt: &AppendReceipt) -> Result<()> {
        let current_len = receipt
            .path
            .metadata()
            .with_context(|| format!("reading metadata for {}", receipt.path.display()))?
            .len();
        let expected_len = receipt.original_len + receipt.appended.len() as u64;
        if current_len != expected_len {
            bail!("file changed after the append");
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&receipt.path)
            .with_context(|| format!("opening {}", receipt.path.display()))?;
        file.seek(SeekFrom::Start(receipt.original_len))?;
        let mut appended = Vec::with_capacity(receipt.appended.len());
        file.read_to_end(&mut appended)?;
        if appended != receipt.appended {
            bail!("file changed after the append");
        }

        drop(file);
        if receipt.created {
            fs::remove_file(&receipt.path)
                .with_context(|| format!("deleting {}", receipt.path.display()))?;
        } else {
            OpenOptions::new()
                .write(true)
                .open(&receipt.path)?
                .set_len(receipt.original_len)?;
        }
        Ok(())
    }

    /// List daily Markdown files, newest date first.
    pub fn list_daily_file_paths(&self) -> Result<Vec<PathBuf>> {
        let mut dates = self.daily_dates()?;
        dates.reverse();
        Ok(dates
            .into_iter()
            .map(|date| self.daily_path(date))
            .collect())
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

    /// Validate that `path` is a regular, non-symlink managed note directly
    /// inside one of the wiki-managed directories (daily/, data/, archives/).
    /// The path is resolved in full before the containment check, so a managed
    /// directory swapped for a symlink can never redirect the caller to a file
    /// outside the Nole root. Returns the canonical path.
    pub(crate) fn validate_wiki_note(&self, path: &Path) -> Result<PathBuf> {
        if !is_note_path(path) {
            bail!(
                "wiki note must have a .md or .mb extension: {}",
                path.display()
            );
        }
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("checking wiki note {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            bail!("wiki note must be a regular file: {}", path.display());
        }
        let canonical_root = fs::canonicalize(&self.root)
            .with_context(|| format!("resolving Nole root {}", self.root.display()))?;
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("resolving wiki note {}", path.display()))?;
        if !canonical.starts_with(&canonical_root) {
            bail!("wiki note escapes the Nole root: {}", path.display());
        }
        let managed = [&self.daily_dir, &self.data_dir, &self.archives_dir]
            .into_iter()
            .any(|directory| {
                fs::canonicalize(directory).is_ok_and(|canonical_directory| {
                    canonical.parent() == Some(canonical_directory.as_path())
                })
            });
        if !managed {
            bail!(
                "wiki note must live directly in daily/, data/, or archives/: {}",
                path.display()
            );
        }
        Ok(canonical)
    }

    /// Read an open Markdown article anywhere within the Nole workspace.
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
        if !canonical.starts_with(&canonical_root)
            || canonical.starts_with(config)
            || !is_note_path(&canonical)
        {
            bail!(
                "document must be a managed .md or .mb file: {}",
                path.display()
            );
        }
        fs::read_to_string(canonical).context("reading document")
    }

    /// Resolve an existing regular file referenced by a local Markdown link.
    pub fn validate_local_file(&self, path: &Path) -> Result<PathBuf> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("checking local file {}", path.display()))?;
        if !metadata.is_file() {
            bail!("local target must be a regular file: {}", path.display());
        }
        fs::canonicalize(path).with_context(|| format!("resolving local file {}", path.display()))
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

/// Create a unique hidden temp file in the destination's directory, relative
/// to the parent directory handle bound at prepare time. The temp lives next
/// to the final target so the atomic publish below stays on one filesystem;
/// `create_new` guarantees no two publishers collide.
fn create_export_temp(
    parent: &cap_std::fs::Dir,
    file_name: &OsStr,
    destination: &Path,
) -> Result<(cap_std::fs::File, OsString)> {
    let name = Path::new(file_name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("export");
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..16u32 {
        let temp_name = format!(
            ".{name}.nole-export-{}-{nonce}-{attempt}.tmp",
            std::process::id()
        );
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        match parent.open_with(&temp_name, &options) {
            Ok(file) => return Ok((file, temp_name.into())),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "creating export temp file next to {}",
                        destination.display()
                    )
                })
            }
        }
    }
    bail!(
        "could not allocate a unique export temp file next to {}",
        destination.display()
    )
}

/// Report whether a directory entry exists without following a symlink.
/// `Dir::exists` follows links on some platforms, which can report false for
/// dangling links even though CreateNew must reject their occupied names.
fn destination_entry_exists(parent: &cap_std::fs::Dir, file_name: &OsStr) -> io::Result<bool> {
    match parent.symlink_metadata(file_name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Verify the destination — a single file name relative to the bound parent
/// directory — is an existing regular file that is not a symlink: the only
/// target [`ExportDestinationPolicy::ReplaceExisting`] may replace.
/// Directories, symlinks, special files, and missing paths are all rejected so
/// an explicit overwrite can never clobber anything but a plain file.
fn validate_replaceable_destination(
    parent: &cap_std::fs::Dir,
    file_name: &OsStr,
    destination: &Path,
) -> Result<()> {
    let symlink_metadata = match parent.symlink_metadata(file_name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "export destination does not exist: {}",
                destination.display()
            )
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("inspecting export destination {}", destination.display())
            })
        }
    };
    if symlink_metadata.is_symlink() {
        bail!(
            "export destination is a symlink and cannot be replaced: {}",
            destination.display()
        );
    }
    let metadata = parent
        .metadata(file_name)
        .with_context(|| format!("inspecting export destination {}", destination.display()))?;
    if !metadata.is_file() {
        bail!(
            "export destination is not a regular file: {}",
            destination.display()
        );
    }
    Ok(())
}

/// Publish `temp_name` as `file_name`, both relative to the bound parent
/// directory, without ever overwriting an existing file.
///
/// A hard link is atomic and fails when the destination already exists, which
/// preserves the no-overwrite guarantee across the final publication race.
/// Filesystems that cannot create hard links fail explicitly; an
/// exists-then-rename fallback would be racy because rename may overwrite a
/// destination created between the check and the rename.
fn publish_no_overwrite(
    parent: &cap_std::fs::Dir,
    temp_name: &OsStr,
    file_name: &OsStr,
    destination: &Path,
) -> Result<()> {
    match parent.hard_link(temp_name, parent, file_name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "export destination already exists: {}",
                destination.display()
            )
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "atomically publishing export {}; the destination filesystem must support hard links",
                destination.display()
            )
        }),
    }
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let file =
        File::open(path).with_context(|| format!("opening {} for hashing", path.display()))?;
    sha256_reader(BufReader::with_capacity(64 * 1024, file))
}

fn sha256_reader(mut reader: impl Read) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn copy_and_hash(reader: &mut impl Read, writer: &mut impl Write) -> Result<(u64, [u8; 32])> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        total = total
            .checked_add(u64::try_from(read).context("export is too large")?)
            .context("export is too large")?;
    }
    Ok((total, hasher.finalize().into()))
}

fn verify_export_source_content(prepared: &PreparedExport) -> Result<()> {
    if sha256_file(&prepared.source)? != prepared.identity.content_sha256 {
        bail!("export source content changed after preparation");
    }
    Ok(())
}

fn validate_open_export_source(source: &File, prepared: &PreparedExport) -> Result<()> {
    let metadata = source
        .metadata()
        .context("reading opened export source metadata")?;
    if !metadata.is_file()
        || metadata.len() != prepared.identity.length
        || metadata.modified()? != prepared.identity.modified
    {
        bail!("export source changed after preparation");
    }
    Ok(())
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

/// Whether a path names a Markdown or MBDown note by extension.
pub(crate) fn is_note_path(path: &Path) -> bool {
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
                messages: vec![crate::provider::Message::user("first")],
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
                messages: vec![crate::provider::Message::user("second")],
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

    #[test]
    fn clearing_the_agent_session_leaves_the_workspace_intact() {
        use crate::agent_session::{AgentConversation, AgentPanelEntry};

        let (_directory, storage) = fresh();
        let workspace = storage.workspace_dir.clone();
        assert!(workspace.is_dir());
        // The workspace is Agent-maintained: it survives unrelated storage work.
        storage.append_daily("2026-07-27", "note").unwrap();
        assert!(workspace.is_dir());

        let session = AgentSession::from_parts(
            &AgentConversation::seeded_for_test(),
            &[AgentPanelEntry::Prompt {
                text: "work".to_string(),
                muted: false,
            }],
            crate::agent_session::TokenUsage::default(),
            0,
            std::time::Duration::ZERO,
        );
        storage.write_agent_session(&session).unwrap();
        fs::create_dir_all(workspace.join("sub")).unwrap();
        fs::write(workspace.join("sub/draft.md"), "wip").unwrap();

        // A successful clear removes the session file but never touches the
        // workspace, whose contents the Agent owns across sessions.
        assert!(storage.clear_agent_session().unwrap());
        assert!(!storage.agent_session_path.exists());
        assert!(
            workspace.join("sub/draft.md").exists(),
            "workspace content survives a session clear"
        );

        // A blocked session path still reports an error and touches nothing.
        fs::create_dir(&storage.agent_session_path).unwrap();
        assert!(storage.clear_agent_session().is_err());
        assert!(workspace.join("sub/draft.md").exists());
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
        assert_eq!(storage.list_theme_names().unwrap(), vec!["only"]);
        storage.write_theme_selection("random").unwrap();

        let default_random = storage.load_theme(Some(&default_path)).unwrap();
        assert_eq!(default_random.requested, "random");
        assert_eq!(default_random.active, "default");
        assert_eq!(
            default_random.source.as_deref(),
            Some(default_path.as_path())
        );
        assert_eq!(
            default_random.theme.surface_panel,
            ratatui::style::Color::Rgb(9, 8, 7)
        );

        let custom_random = storage.load_theme(Some(&path)).unwrap();
        assert_eq!(custom_random.requested, "random");
        assert_eq!(custom_random.active, "only");
        assert_eq!(custom_random.source.as_deref(), Some(path.as_path()));
        assert_eq!(
            custom_random.theme.surface_panel,
            ratatui::style::Color::Rgb(1, 2, 3)
        );

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

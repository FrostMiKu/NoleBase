//! Theme file discovery. Storage seeds the built-in theme files, lists theme
//! TOMLs and persists each client's theme selection string; resolving a
//! selection into a parsed theme lives in the shared theme module.

use super::*;

/// Seed content of `themes/default.toml`, the dark palette and the terminal
/// client's default. Every token here is also the fallback for a token a
/// theme file leaves out.
pub const DEFAULT_THEME_TOML: &str = r##"# Nole semantic color tokens. Values accept #RRGGBB or "terminal".
# For background tokens, "terminal" means the terminal's own default background color.
# A token left out of a theme file keeps its value from this default theme.
[surface]
canvas = "terminal"
panel = "#181825"
compose = "#313244"
overlay = "#11111b"
raised = "#24273a"
sunken = "#181825"
message_user = "#313244"
message_agent = "#1e1e2e"
status_bar = "terminal"
status_context = "#89b4fa"
status_mode = "#74c7ec"

[selection]
background = "#45475a"
background_inactive = "#313244"
foreground = "#cdd6f4"
indicator = "#cba6f7"

[text]
primary = "#cdd6f4"
secondary = "#bac2de"
muted = "#6c7086"
subtle = "#7f849c"
disabled = "#a6adc8"
on_accent = "#11111b"

[ui]
border = "#6c7086"
border_subtle = "#45475a"
focus_border = "#a6e3a1"
input_prompt = "#a6e3a1"
shortcut = "#a6e3a1"
page_heading = "#b4befe"
section_heading = "#74c7ec"
group_marker = "#94e2d5"
activity_marker = "#94e2d5"
task_open = "#94e2d5"
task_done = "#a6e3a1"
agent_user = "#74c7ec"
agent_assistant = "#a6e3a1"
search_marker = "#94e2d5"
dialog_choice = "#89dceb"
dialog_input = "#cba6f7"
action = "#94e2d5"
action_ai = "#f5c2e7"
success = "#a6e3a1"
warning = "#f9e2af"
error = "#f5c2e7"

[diff]
addition_background = "#29423c"
deletion_background = "#49313f"

[markdown]
heading_1 = "#b4befe"
heading_2 = "#89b4fa"
heading_3 = "#cba6f7"
heading_minor = "#74c7ec"
quote = "#cba6f7"
list = "#94e2d5"
rule = "#6c7086"
code = "#fab387"
code_block_text = "#cdd6f4"
code_block_background = "#313244"
code_label = "#7f849c"
link = "#89b4fa"
hashtag = "#f5c2e7"
wikilink = "#89dceb"

[syntax]
keyword = "#cba6f7"
string = "#a6e3a1"
comment = "#7f849c"
function = "#89b4fa"
type = "#f9e2af"
constant = "#fab387"

[animation]
gradient = ["#89dceb", "#89b4fa", "#cba6f7", "#f5c2e7", "#f9e2af", "#a6e3a1"]
"##;

/// Seed content of `themes/paper.toml`, the warm light palette and the
/// desktop client's default. Ink blue marks interaction, violet marks the
/// Agent, and headings keep the body ink.
pub const PAPER_THEME_TOML: &str = r##"# Paper: a warm light theme for reading and writing.
# Values accept #RRGGBB or "terminal". A token left out keeps its default-theme value.
[surface]
canvas = "#faf8f4"
panel = "#f3f0ea"
compose = "#ffffff"
overlay = "#ffffff"
raised = "#ffffff"
sunken = "#eeeae3"
message_user = "#eeeae3"
message_agent = "#ffffff"
status_bar = "#f3f0ea"
status_context = "#2f5d8a"
status_mode = "#5c7fa6"

[selection]
background = "#e3ecf5"
background_inactive = "#eeeae3"
foreground = "#2a2925"
indicator = "#2f5d8a"

[text]
primary = "#2a2925"
secondary = "#55524b"
muted = "#8c877e"
subtle = "#746f67"
disabled = "#b8b2a7"
on_accent = "#ffffff"

[ui]
border = "#d3ccc0"
border_subtle = "#e5e0d7"
focus_border = "#2f5d8a"
input_prompt = "#2f5d8a"
shortcut = "#2f5d8a"
page_heading = "#2a2925"
section_heading = "#55524b"
group_marker = "#8c877e"
activity_marker = "#7a5ac0"
task_open = "#8c877e"
task_done = "#3f8a55"
agent_user = "#2f5d8a"
agent_assistant = "#7a5ac0"
search_marker = "#b7791f"
dialog_choice = "#2f5d8a"
dialog_input = "#2f5d8a"
action = "#2f5d8a"
action_ai = "#7a5ac0"
success = "#3f8a55"
warning = "#b7791f"
error = "#c2453d"

[diff]
addition_background = "#e2f0e4"
deletion_background = "#f8e1de"

[markdown]
heading_1 = "#2a2925"
heading_2 = "#2a2925"
heading_3 = "#2a2925"
heading_minor = "#2a2925"
quote = "#55524b"
list = "#8c877e"
rule = "#e5e0d7"
code = "#8a4b2a"
code_block_text = "#3a3833"
code_block_background = "#f5f2ec"
code_label = "#8c877e"
link = "#2f5d8a"
hashtag = "#2f5d8a"
wikilink = "#2f5d8a"

[syntax]
keyword = "#8a3fa0"
string = "#3f7a4a"
comment = "#a39c90"
function = "#2f5d8a"
type = "#a5651e"
constant = "#b4532a"

[animation]
gradient = ["#2f5d8a", "#5c7fa6", "#7a5ac0", "#a86fb0", "#b7791f", "#3f8a55"]
"##;

/// Themes that ship with Nole. Each is seeded into `themes/` when its file is
/// missing, and resolves from this source when the file is removed later.
pub const BUILT_IN_THEMES: [(&str, &str); 2] =
    [("default", DEFAULT_THEME_TOML), ("paper", PAPER_THEME_TOML)];

/// The client a theme selection belongs to. Each client persists its own
/// selection so a light desktop theme leaves the terminal palette alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeTarget {
    Terminal,
    Desktop,
}

impl ThemeTarget {
    /// The `settings.toml` key holding this client's selection.
    fn setting_key(self) -> &'static str {
        match self {
            Self::Terminal => "theme",
            Self::Desktop => "desktop_theme",
        }
    }

    /// The built-in theme an unknown selection resolves to.
    pub fn fallback_theme(self) -> &'static str {
        match self {
            Self::Terminal => "default",
            Self::Desktop => "paper",
        }
    }
}

pub(super) fn default_desktop_theme() -> String {
    ThemeTarget::Desktop.fallback_theme().to_string()
}

impl Storage {
    pub(super) fn ensure_theme_files(&self) -> Result<()> {
        fs::create_dir_all(&self.themes_dir)
            .with_context(|| format!("creating {}", self.themes_dir.display()))?;
        for (name, source) in BUILT_IN_THEMES {
            let path = self.themes_dir.join(format!("{name}.toml"));
            if !path.exists() {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .with_context(|| format!("creating {}", path.display()))?;
                file.write_all(source.as_bytes())?;
            }
        }
        Ok(())
    }

    pub fn load_theme_selection(&self, target: ThemeTarget) -> Result<String> {
        let settings = self.load_settings()?;
        Ok(match target {
            ThemeTarget::Terminal => settings.theme,
            ThemeTarget::Desktop => settings.desktop_theme,
        })
    }

    pub fn write_theme_selection(&self, target: ThemeTarget, selection: &str) -> Result<()> {
        let source = self.read_settings_source()?;
        self.parse_settings(&source)?;
        let mut document = source
            .parse::<toml_edit::DocumentMut>()
            .with_context(|| format!("parsing {}", self.settings_path.display()))?;
        document[target.setting_key()] = toml_edit::value(selection);
        fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating {}", self.config_dir.display()))?;
        fs::write(&self.settings_path, document.to_string())
            .with_context(|| format!("writing {}", self.settings_path.display()))
    }

    pub fn list_theme_names(&self) -> Result<Vec<String>> {
        Ok(self
            .theme_files()?
            .into_iter()
            .filter(|(name, _)| name != "default")
            .map(|(name, _)| name)
            .collect())
    }

    pub(crate) fn theme_files(&self) -> Result<Vec<(String, PathBuf)>> {
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
            if is_toml && !name.is_empty() && name != "random" {
                themes.push((name.to_string(), path));
            }
        }
        themes.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(themes)
    }
}

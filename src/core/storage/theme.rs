//! Theme file discovery. Storage lists theme TOMLs and persists the theme
//! selection string; resolving a selection into a parsed theme lives in the
//! TUI theme module.

use super::*;

/// Seed content of `themes/default.toml`. Storage owns this default config
/// file; parsing it into presentation colors lives in the TUI theme module.
pub const DEFAULT_THEME_TOML: &str = r##"# Nole semantic color tokens. Values accept #RRGGBB or "terminal".
# For background tokens, "terminal" means the terminal's own default background color.
[surface]
canvas = "terminal"
panel = "#181825"
compose = "#313244"
overlay = "#11111b"
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
box_border = "#6c7086"

[animation]
gradient = ["#89dceb", "#89b4fa", "#cba6f7", "#f5c2e7", "#f9e2af", "#a6e3a1"]
"##;

impl Storage {
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

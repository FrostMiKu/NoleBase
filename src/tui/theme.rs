use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use mbtui::{CornerStyle, Theme as MarkdownTheme};
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use crate::core::storage::{Storage, DEFAULT_THEME_TOML};

#[cfg(test)]
pub mod catppuccin {
    use ratatui::style::Color;

    pub const PINK: Color = Color::Rgb(245, 194, 231);
    pub const MAUVE: Color = Color::Rgb(203, 166, 247);
    pub const GREEN: Color = Color::Rgb(166, 227, 161);
    pub const TEAL: Color = Color::Rgb(148, 226, 213);
    pub const SKY: Color = Color::Rgb(137, 220, 235);
    pub const SAPPHIRE: Color = Color::Rgb(116, 199, 236);
    pub const BLUE: Color = Color::Rgb(137, 180, 250);
    pub const OVERLAY_0: Color = Color::Rgb(108, 112, 134);
    pub const SURFACE_1: Color = Color::Rgb(69, 71, 90);
    pub const SURFACE_0: Color = Color::Rgb(49, 50, 68);
    pub const BASE: Color = Color::Rgb(30, 30, 46);
    pub const MANTLE: Color = Color::Rgb(24, 24, 37);
    pub const CRUST: Color = Color::Rgb(17, 17, 27);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub surface_canvas: Color,
    pub surface_panel: Color,
    pub surface_compose: Color,
    pub surface_overlay: Color,
    pub surface_message_user: Color,
    pub surface_message_agent: Color,
    pub surface_status_bar: Color,
    pub surface_status_context: Color,
    pub surface_status_mode: Color,
    pub selection_background: Color,
    pub selection_background_inactive: Color,
    pub selection_foreground: Color,
    pub selection_indicator: Color,
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub text_subtle: Color,
    pub text_disabled: Color,
    pub text_on_accent: Color,
    pub ui_border: Color,
    pub ui_border_subtle: Color,
    pub ui_focus_border: Color,
    pub ui_input_prompt: Color,
    pub ui_shortcut: Color,
    pub ui_page_heading: Color,
    pub ui_section_heading: Color,
    pub ui_group_marker: Color,
    pub ui_activity_marker: Color,
    pub ui_task_open: Color,
    pub ui_task_done: Color,
    pub ui_agent_user: Color,
    pub ui_agent_assistant: Color,
    pub ui_search_marker: Color,
    pub ui_dialog_choice: Color,
    pub ui_dialog_input: Color,
    pub ui_action: Color,
    pub ui_action_ai: Color,
    pub ui_warning: Color,
    pub ui_error: Color,
    pub diff_addition_background: Color,
    pub diff_deletion_background: Color,
    pub markdown_heading_1: Color,
    pub markdown_heading_2: Color,
    pub markdown_heading_3: Color,
    pub markdown_heading_minor: Color,
    pub markdown_quote: Color,
    pub markdown_list: Color,
    pub markdown_rule: Color,
    pub markdown_code: Color,
    pub markdown_code_block_text: Color,
    pub markdown_code_block_background: Color,
    pub markdown_code_label: Color,
    pub markdown_link: Color,
    pub markdown_hashtag: Color,
    pub markdown_wikilink: Color,
    pub markdown_box_border: Color,
    pub animation_gradient: [Color; 6],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    surface: SurfaceTokens,
    selection: SelectionTokens,
    text: TextTokens,
    ui: UiTokens,
    diff: DiffTokens,
    markdown: MarkdownTokens,
    animation: AnimationTokens,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceTokens {
    canvas: String,
    panel: String,
    compose: String,
    overlay: String,
    message_user: String,
    message_agent: String,
    status_bar: String,
    status_context: String,
    status_mode: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionTokens {
    background: String,
    background_inactive: String,
    foreground: String,
    indicator: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextTokens {
    primary: String,
    secondary: String,
    muted: String,
    subtle: String,
    disabled: String,
    on_accent: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UiTokens {
    border: String,
    border_subtle: String,
    focus_border: String,
    input_prompt: String,
    shortcut: String,
    page_heading: String,
    section_heading: String,
    group_marker: String,
    activity_marker: String,
    task_open: String,
    task_done: String,
    agent_user: String,
    agent_assistant: String,
    search_marker: String,
    dialog_choice: String,
    dialog_input: String,
    action: String,
    action_ai: String,
    warning: String,
    error: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffTokens {
    addition_background: String,
    deletion_background: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkdownTokens {
    heading_1: String,
    heading_2: String,
    heading_3: String,
    heading_minor: String,
    quote: String,
    list: String,
    rule: String,
    code: String,
    code_block_text: String,
    code_block_background: String,
    code_label: String,
    link: String,
    hashtag: String,
    wikilink: String,
    box_border: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnimationTokens {
    gradient: [String; 6],
}

impl Theme {
    pub fn from_toml(source: &str) -> Result<Self> {
        let file: ThemeFile = toml::from_str(source).context("parsing theme TOML")?;
        Ok(Self {
            surface_canvas: parse_color("surface.canvas", &file.surface.canvas)?,
            surface_panel: parse_color("surface.panel", &file.surface.panel)?,
            surface_compose: parse_color("surface.compose", &file.surface.compose)?,
            surface_overlay: parse_color("surface.overlay", &file.surface.overlay)?,
            surface_message_user: parse_color("surface.message_user", &file.surface.message_user)?,
            surface_message_agent: parse_color(
                "surface.message_agent",
                &file.surface.message_agent,
            )?,
            surface_status_bar: parse_color("surface.status_bar", &file.surface.status_bar)?,
            surface_status_context: parse_color(
                "surface.status_context",
                &file.surface.status_context,
            )?,
            surface_status_mode: parse_color("surface.status_mode", &file.surface.status_mode)?,
            selection_background: parse_color("selection.background", &file.selection.background)?,
            selection_background_inactive: parse_color(
                "selection.background_inactive",
                &file.selection.background_inactive,
            )?,
            selection_foreground: parse_color("selection.foreground", &file.selection.foreground)?,
            selection_indicator: parse_color("selection.indicator", &file.selection.indicator)?,
            text_primary: parse_color("text.primary", &file.text.primary)?,
            text_secondary: parse_color("text.secondary", &file.text.secondary)?,
            text_muted: parse_color("text.muted", &file.text.muted)?,
            text_subtle: parse_color("text.subtle", &file.text.subtle)?,
            text_disabled: parse_color("text.disabled", &file.text.disabled)?,
            text_on_accent: parse_color("text.on_accent", &file.text.on_accent)?,
            ui_border: parse_color("ui.border", &file.ui.border)?,
            ui_border_subtle: parse_color("ui.border_subtle", &file.ui.border_subtle)?,
            ui_focus_border: parse_color("ui.focus_border", &file.ui.focus_border)?,
            ui_input_prompt: parse_color("ui.input_prompt", &file.ui.input_prompt)?,
            ui_shortcut: parse_color("ui.shortcut", &file.ui.shortcut)?,
            ui_page_heading: parse_color("ui.page_heading", &file.ui.page_heading)?,
            ui_section_heading: parse_color("ui.section_heading", &file.ui.section_heading)?,
            ui_group_marker: parse_color("ui.group_marker", &file.ui.group_marker)?,
            ui_activity_marker: parse_color("ui.activity_marker", &file.ui.activity_marker)?,
            ui_task_open: parse_color("ui.task_open", &file.ui.task_open)?,
            ui_task_done: parse_color("ui.task_done", &file.ui.task_done)?,
            ui_agent_user: parse_color("ui.agent_user", &file.ui.agent_user)?,
            ui_agent_assistant: parse_color("ui.agent_assistant", &file.ui.agent_assistant)?,
            ui_search_marker: parse_color("ui.search_marker", &file.ui.search_marker)?,
            ui_dialog_choice: parse_color("ui.dialog_choice", &file.ui.dialog_choice)?,
            ui_dialog_input: parse_color("ui.dialog_input", &file.ui.dialog_input)?,
            ui_action: parse_color("ui.action", &file.ui.action)?,
            ui_action_ai: parse_color("ui.action_ai", &file.ui.action_ai)?,
            ui_warning: parse_color("ui.warning", &file.ui.warning)?,
            ui_error: parse_color("ui.error", &file.ui.error)?,
            diff_addition_background: parse_color(
                "diff.addition_background",
                &file.diff.addition_background,
            )?,
            diff_deletion_background: parse_color(
                "diff.deletion_background",
                &file.diff.deletion_background,
            )?,
            markdown_heading_1: parse_color("markdown.heading_1", &file.markdown.heading_1)?,
            markdown_heading_2: parse_color("markdown.heading_2", &file.markdown.heading_2)?,
            markdown_heading_3: parse_color("markdown.heading_3", &file.markdown.heading_3)?,
            markdown_heading_minor: parse_color(
                "markdown.heading_minor",
                &file.markdown.heading_minor,
            )?,
            markdown_quote: parse_color("markdown.quote", &file.markdown.quote)?,
            markdown_list: parse_color("markdown.list", &file.markdown.list)?,
            markdown_rule: parse_color("markdown.rule", &file.markdown.rule)?,
            markdown_code: parse_color("markdown.code", &file.markdown.code)?,
            markdown_code_block_text: parse_color(
                "markdown.code_block_text",
                &file.markdown.code_block_text,
            )?,
            markdown_code_block_background: parse_color(
                "markdown.code_block_background",
                &file.markdown.code_block_background,
            )?,
            markdown_code_label: parse_color("markdown.code_label", &file.markdown.code_label)?,
            markdown_link: parse_color("markdown.link", &file.markdown.link)?,
            markdown_hashtag: parse_color("markdown.hashtag", &file.markdown.hashtag)?,
            markdown_wikilink: parse_color("markdown.wikilink", &file.markdown.wikilink)?,
            markdown_box_border: parse_color("markdown.box_border", &file.markdown.box_border)?,
            animation_gradient: parse_gradient(&file.animation.gradient)?,
        })
    }

    pub fn markdown_theme(self) -> MarkdownTheme {
        let mut theme = MarkdownTheme::default().with_corner_style(CornerStyle::Rounded);
        theme.heading_1 = Style::default()
            .fg(self.markdown_heading_1)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        theme.heading_2 = Style::default()
            .fg(self.markdown_heading_2)
            .add_modifier(Modifier::BOLD);
        theme.heading_3 = Style::default()
            .fg(self.markdown_heading_3)
            .add_modifier(Modifier::BOLD);
        theme.heading_minor = Style::default()
            .fg(self.markdown_heading_minor)
            .add_modifier(Modifier::BOLD);
        theme.quote = Style::default().fg(self.markdown_quote);
        theme.list = Style::default()
            .fg(self.markdown_list)
            .add_modifier(Modifier::BOLD);
        theme.rule = Style::default().fg(self.markdown_rule);
        theme.code = Style::default().fg(self.markdown_code);
        theme.code_block = Style::default()
            .fg(self.markdown_code_block_text)
            .bg(self.markdown_code_block_background);
        theme.code_label = Style::default().fg(self.markdown_code_label);
        theme.link = Style::default()
            .fg(self.markdown_link)
            .underline_color(self.markdown_link)
            .add_modifier(Modifier::UNDERLINED);
        theme.hashtag = Style::default()
            .fg(self.markdown_hashtag)
            .add_modifier(Modifier::BOLD);
        theme.wikilink = Style::default()
            .fg(self.markdown_wikilink)
            .underline_color(self.markdown_link)
            .add_modifier(Modifier::UNDERLINED);
        theme.insert(
            "markdown-box",
            Style::default().fg(self.markdown_box_border),
        );
        theme
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_toml(DEFAULT_THEME_TOML).expect("the built-in theme must be valid")
    }
}

/// A theme resolved for presentation: which selection was requested, which
/// theme file ended up active, where it came from, and the parsed theme.
///
/// Storage owns the raw selection and theme files; turning them into
/// Ratatui colors belongs here, next to the token definitions.
#[derive(Debug, Clone)]
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

/// Resolve the theme named by the settings file.
pub fn load_theme(storage: &Storage, previous_random_source: Option<&Path>) -> Result<LoadedTheme> {
    let requested = storage.load_theme_selection()?;
    resolve_theme(storage, &requested, previous_random_source)
}

/// Resolve `selection`, persist it as the active theme selection, and return
/// the loaded theme.
pub fn select_theme(storage: &Storage, selection: &str) -> Result<LoadedTheme> {
    let loaded = resolve_theme(storage, selection, None)?;
    storage.write_theme_selection(selection)?;
    Ok(loaded)
}

fn resolve_theme(
    storage: &Storage,
    requested: &str,
    previous_random_source: Option<&Path>,
) -> Result<LoadedTheme> {
    if requested == "default" {
        return load_default_theme(storage, requested);
    }

    let files = storage.theme_files()?;
    if requested == "random" {
        if let Some(previous) = previous_random_source {
            if let Some((name, path)) = files.iter().find(|(_, path)| path == previous) {
                return load_theme_file(requested, name, path);
            }
        }

        let mut valid = Vec::new();
        for (name, path) in &files {
            if let Ok(theme) = parse_theme_file(path) {
                valid.push((name, path, theme));
            }
        }
        if valid.is_empty() {
            return load_default_theme(storage, requested);
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
        Some((name, path)) => load_theme_file(requested, &name, &path),
        None => load_default_theme(storage, requested),
    }
}

fn load_default_theme(storage: &Storage, requested: &str) -> Result<LoadedTheme> {
    let path = storage.themes_dir.join("default.toml");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            load_theme_file(requested, "default", &path)
        }
        Ok(_) => Ok(LoadedTheme::built_in_default_for(requested)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LoadedTheme::built_in_default_for(requested))
        }
        Err(error) => Err(error).with_context(|| format!("reading theme {}", path.display())),
    }
}

fn load_theme_file(requested: &str, name: &str, path: &Path) -> Result<LoadedTheme> {
    Ok(LoadedTheme {
        requested: requested.to_string(),
        active: name.to_string(),
        source: Some(path.to_path_buf()),
        theme: parse_theme_file(path)?,
    })
}

fn parse_theme_file(path: &Path) -> Result<Theme> {
    let source =
        fs::read_to_string(path).with_context(|| format!("reading theme {}", path.display()))?;
    Theme::from_toml(&source).with_context(|| format!("loading theme {}", path.display()))
}

fn parse_gradient(values: &[String; 6]) -> Result<[Color; 6]> {
    let mut colors = [Color::Reset; 6];
    for (index, value) in values.iter().enumerate() {
        colors[index] = parse_rgb_color(&format!("animation.gradient[{index}]"), value)?;
    }
    Ok(colors)
}

fn parse_color(field: &str, value: &str) -> Result<Color> {
    if value == "terminal" {
        return Ok(Color::Reset);
    }
    parse_rgb_color(field, value)
        .with_context(|| format!("{field} must use #RRGGBB or \"terminal\""))
}

fn parse_rgb_color(field: &str, value: &str) -> Result<Color> {
    let Some(hex) = value.strip_prefix('#').filter(|hex| hex.len() == 6) else {
        bail!("{field} must use #RRGGBB, got {value:?}");
    };
    let rgb = u32::from_str_radix(hex, 16)
        .with_context(|| format!("{field} must use #RRGGBB, got {value:?}"))?;
    Ok(Color::Rgb(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path()).unwrap();
        storage.ensure_files().unwrap();
        (dir, storage)
    }

    #[test]
    fn default_theme_selection_loads_the_seeded_default_file() {
        let (_dir, storage) = fresh();
        let default_theme_path = storage.themes_dir.join("default.toml");
        let loaded = load_theme(&storage, None).unwrap();
        assert_eq!(loaded.requested, "default");
        assert_eq!(loaded.active, "default");
        assert_eq!(loaded.source.as_deref(), Some(default_theme_path.as_path()));
        assert_eq!(loaded.theme, Theme::default());
    }

    #[test]
    fn custom_theme_selection_is_preserved_and_loaded() {
        let (_dir, storage) = fresh();
        let custom = DEFAULT_THEME_TOML
            .replace("message_agent = \"#1e1e2e\"", "message_agent = \"#010203\"");
        let path = storage.themes_dir.join("custom.toml");
        fs::write(&path, &custom).unwrap();
        storage.write_theme_selection("custom").unwrap();

        storage.ensure_files().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), custom);
        assert_eq!(storage.load_theme_selection().unwrap(), "custom");
        let loaded = load_theme(&storage, None).unwrap();
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
        let (_dir, storage) = fresh();
        let default_path = storage.themes_dir.join("default.toml");
        let edited_default =
            DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#090807\"");
        fs::write(&default_path, &edited_default).unwrap();
        storage.ensure_files().unwrap();
        assert_eq!(fs::read_to_string(&default_path).unwrap(), edited_default);
        storage.write_theme_selection("missing").unwrap();
        let missing = load_theme(&storage, None).unwrap();
        assert_eq!(missing.requested, "missing");
        assert_eq!(missing.active, "default");
        assert_eq!(missing.source.as_deref(), Some(default_path.as_path()));
        assert_eq!(
            missing.theme.surface_panel,
            ratatui::style::Color::Rgb(9, 8, 7)
        );

        let custom = DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#010203\"");
        let path = storage.themes_dir.join("only.toml");
        fs::write(&path, custom).unwrap();
        assert_eq!(storage.list_theme_names().unwrap(), vec!["only"]);
        storage.write_theme_selection("random").unwrap();

        let default_random = load_theme(&storage, Some(&default_path)).unwrap();
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

        let custom_random = load_theme(&storage, Some(&path)).unwrap();
        assert_eq!(custom_random.requested, "random");
        assert_eq!(custom_random.active, "only");
        assert_eq!(custom_random.source.as_deref(), Some(path.as_path()));
        assert_eq!(
            custom_random.theme.surface_panel,
            ratatui::style::Color::Rgb(1, 2, 3)
        );

        fs::remove_file(default_path).unwrap();
        storage.write_theme_selection("missing").unwrap();
        let built_in = load_theme(&storage, None).unwrap();
        assert_eq!(built_in.active, "default");
        assert_eq!(built_in.source, None);
        assert_eq!(built_in.theme, Theme::default());
    }

    #[test]
    fn default_file_reproduces_the_current_theme() {
        let theme = Theme::from_toml(DEFAULT_THEME_TOML).unwrap();
        assert_eq!(theme.surface_canvas, Color::Reset);
        assert_eq!(theme.surface_status_bar, Color::Reset);
        assert_eq!(theme.surface_compose, Color::Rgb(49, 50, 68));
        assert_eq!(theme.selection_background, Color::Rgb(69, 71, 90));
        assert_eq!(theme.selection_foreground, Color::Rgb(205, 214, 244));
        assert_eq!(theme.ui_page_heading, Color::Rgb(180, 190, 254));
        assert_eq!(theme.diff_addition_background, Color::Rgb(41, 66, 60));
        assert_eq!(theme.diff_deletion_background, Color::Rgb(73, 49, 63));
        assert_eq!(theme.markdown_code_block_background, Color::Rgb(49, 50, 68));
        assert_eq!(theme.surface_status_context, Color::Rgb(137, 180, 250));
        let markdown = theme.markdown_theme();
        assert_eq!(markdown.heading_1.fg, Some(theme.markdown_heading_1));
        assert_eq!(
            markdown.code_block.bg,
            Some(theme.markdown_code_block_background)
        );
        assert_eq!(markdown.link.fg, Some(theme.markdown_link));
    }

    #[test]
    fn semantic_tokens_can_vary_independently() {
        let custom = DEFAULT_THEME_TOML
            .replace("panel = \"#181825\"", "panel = \"#010203\"")
            .replace("foreground = \"#cdd6f4\"", "foreground = \"#0a0b0c\"")
            .replace("action = \"#94e2d5\"", "action = \"#070809\"")
            .replace(
                "addition_background = \"#29423c\"",
                "addition_background = \"#111213\"",
            )
            .replace(
                "code_block_background = \"#313244\"",
                "code_block_background = \"#040506\"",
            );
        let theme = Theme::from_toml(&custom).unwrap();
        assert_eq!(theme.surface_panel, Color::Rgb(1, 2, 3));
        assert_eq!(theme.selection_foreground, Color::Rgb(10, 11, 12));
        assert_eq!(theme.markdown_code_block_background, Color::Rgb(4, 5, 6));
        assert_eq!(theme.ui_action, Color::Rgb(7, 8, 9));
        assert_eq!(theme.diff_addition_background, Color::Rgb(17, 18, 19));
        assert_eq!(theme.ui_task_open, Color::Rgb(148, 226, 213));
    }

    #[test]
    fn rejects_invalid_and_unknown_tokens() {
        let invalid = DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"dark\"");
        assert!(Theme::from_toml(&invalid)
            .unwrap_err()
            .to_string()
            .contains("surface.panel"));

        let unknown = DEFAULT_THEME_TOML.replace("[ui]", "[ui]\nbrand = \"#ff0000\"");
        assert!(Theme::from_toml(&unknown)
            .unwrap_err()
            .to_string()
            .contains("parsing theme TOML"));

        let terminal_gradient =
            DEFAULT_THEME_TOML.replace("gradient = [\"#89dceb\"", "gradient = [\"terminal\"");
        assert!(Theme::from_toml(&terminal_gradient)
            .unwrap_err()
            .to_string()
            .contains("animation.gradient[0]"));
    }
}

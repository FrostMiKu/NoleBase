use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
/// A semantic color shared by terminal and desktop renderers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Reset,
    Rgb(u8, u8, u8),
}
use serde::Deserialize;

pub use crate::core::storage::ThemeTarget;
use crate::core::storage::{Storage, BUILT_IN_THEMES, DEFAULT_THEME_TOML};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme<C = Color> {
    pub surface_canvas: C,
    pub surface_panel: C,
    pub surface_compose: C,
    pub surface_overlay: C,
    /// A surface lifted above the canvas: cards, composers, active navigation.
    pub surface_raised: C,
    /// A surface recessed below the canvas: inline code, segmented tracks, wells.
    pub surface_sunken: C,
    pub surface_message_user: C,
    pub surface_message_agent: C,
    pub surface_status_bar: C,
    pub surface_status_context: C,
    pub surface_status_mode: C,
    pub selection_background: C,
    pub selection_background_inactive: C,
    pub selection_foreground: C,
    pub selection_indicator: C,
    pub text_primary: C,
    pub text_secondary: C,
    pub text_muted: C,
    pub text_subtle: C,
    pub text_disabled: C,
    pub text_on_accent: C,
    pub ui_border: C,
    pub ui_border_subtle: C,
    pub ui_focus_border: C,
    pub ui_input_prompt: C,
    pub ui_shortcut: C,
    pub ui_page_heading: C,
    pub ui_section_heading: C,
    pub ui_group_marker: C,
    pub ui_activity_marker: C,
    pub ui_task_open: C,
    pub ui_task_done: C,
    pub ui_agent_user: C,
    pub ui_agent_assistant: C,
    pub ui_search_marker: C,
    pub ui_dialog_choice: C,
    pub ui_dialog_input: C,
    pub ui_action: C,
    pub ui_action_ai: C,
    pub ui_success: C,
    pub ui_warning: C,
    pub ui_error: C,
    pub diff_addition_background: C,
    pub diff_deletion_background: C,
    pub markdown_heading_1: C,
    pub markdown_heading_2: C,
    pub markdown_heading_3: C,
    pub markdown_heading_minor: C,
    pub markdown_quote: C,
    pub markdown_list: C,
    pub markdown_rule: C,
    pub markdown_code: C,
    pub markdown_code_block_text: C,
    pub markdown_code_block_background: C,
    pub markdown_code_label: C,
    pub markdown_link: C,
    pub markdown_hashtag: C,
    pub markdown_wikilink: C,
    pub syntax_keyword: C,
    pub syntax_string: C,
    pub syntax_comment: C,
    pub syntax_function: C,
    pub syntax_type: C,
    pub syntax_constant: C,
    pub animation_gradient: [C; 6],
}

/// The token tables of a theme file. Unknown keys are ignored, and a token a
/// file leaves out keeps its value from [`DEFAULT_THEME_TOML`] (see
/// [`Theme::from_toml`]).
#[derive(Deserialize)]
struct ThemeFile {
    surface: SurfaceTokens,
    selection: SelectionTokens,
    text: TextTokens,
    ui: UiTokens,
    diff: DiffTokens,
    markdown: MarkdownTokens,
    syntax: SyntaxTokens,
    animation: AnimationTokens,
}

#[derive(Deserialize)]
struct SurfaceTokens {
    canvas: String,
    panel: String,
    compose: String,
    overlay: String,
    raised: String,
    sunken: String,
    message_user: String,
    message_agent: String,
    status_bar: String,
    status_context: String,
    status_mode: String,
}

#[derive(Deserialize)]
struct SelectionTokens {
    background: String,
    background_inactive: String,
    foreground: String,
    indicator: String,
}

#[derive(Deserialize)]
struct TextTokens {
    primary: String,
    secondary: String,
    muted: String,
    subtle: String,
    disabled: String,
    on_accent: String,
}

#[derive(Deserialize)]
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
    success: String,
    warning: String,
    error: String,
}

#[derive(Deserialize)]
struct DiffTokens {
    addition_background: String,
    deletion_background: String,
}

#[derive(Deserialize)]
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
}

#[derive(Deserialize)]
struct SyntaxTokens {
    keyword: String,
    string: String,
    comment: String,
    function: String,
    #[serde(rename = "type")]
    type_: String,
    constant: String,
}

#[derive(Deserialize)]
struct AnimationTokens {
    gradient: [String; 6],
}

impl<C: From<Color> + Copy> Theme<C> {
    /// Parse a theme file. Tables and tokens the file leaves out keep their
    /// values from the default theme, and keys Nole does not know are ignored,
    /// so a theme only has to name the colors it changes.
    pub fn from_toml(source: &str) -> Result<Self> {
        let mut merged: toml::Table =
            toml::from_str(DEFAULT_THEME_TOML).expect("the default theme is valid TOML");
        let custom: toml::Table = toml::from_str(source).context("parsing theme TOML")?;
        overlay_table(&mut merged, custom);
        let file: ThemeFile = toml::Value::Table(merged)
            .try_into()
            .context("parsing theme TOML")?;
        let color = |field: &str, value: &str| parse_color(field, value).map(C::from);
        let (s, sel, t, ui, d, m, syn) = (
            &file.surface,
            &file.selection,
            &file.text,
            &file.ui,
            &file.diff,
            &file.markdown,
            &file.syntax,
        );
        Ok(Self {
            surface_canvas: color("surface.canvas", &s.canvas)?,
            surface_panel: color("surface.panel", &s.panel)?,
            surface_compose: color("surface.compose", &s.compose)?,
            surface_overlay: color("surface.overlay", &s.overlay)?,
            surface_raised: color("surface.raised", &s.raised)?,
            surface_sunken: color("surface.sunken", &s.sunken)?,
            surface_message_user: color("surface.message_user", &s.message_user)?,
            surface_message_agent: color("surface.message_agent", &s.message_agent)?,
            surface_status_bar: color("surface.status_bar", &s.status_bar)?,
            surface_status_context: color("surface.status_context", &s.status_context)?,
            surface_status_mode: color("surface.status_mode", &s.status_mode)?,
            selection_background: color("selection.background", &sel.background)?,
            selection_background_inactive: color(
                "selection.background_inactive",
                &sel.background_inactive,
            )?,
            selection_foreground: color("selection.foreground", &sel.foreground)?,
            selection_indicator: color("selection.indicator", &sel.indicator)?,
            text_primary: color("text.primary", &t.primary)?,
            text_secondary: color("text.secondary", &t.secondary)?,
            text_muted: color("text.muted", &t.muted)?,
            text_subtle: color("text.subtle", &t.subtle)?,
            text_disabled: color("text.disabled", &t.disabled)?,
            text_on_accent: color("text.on_accent", &t.on_accent)?,
            ui_border: color("ui.border", &ui.border)?,
            ui_border_subtle: color("ui.border_subtle", &ui.border_subtle)?,
            ui_focus_border: color("ui.focus_border", &ui.focus_border)?,
            ui_input_prompt: color("ui.input_prompt", &ui.input_prompt)?,
            ui_shortcut: color("ui.shortcut", &ui.shortcut)?,
            ui_page_heading: color("ui.page_heading", &ui.page_heading)?,
            ui_section_heading: color("ui.section_heading", &ui.section_heading)?,
            ui_group_marker: color("ui.group_marker", &ui.group_marker)?,
            ui_activity_marker: color("ui.activity_marker", &ui.activity_marker)?,
            ui_task_open: color("ui.task_open", &ui.task_open)?,
            ui_task_done: color("ui.task_done", &ui.task_done)?,
            ui_agent_user: color("ui.agent_user", &ui.agent_user)?,
            ui_agent_assistant: color("ui.agent_assistant", &ui.agent_assistant)?,
            ui_search_marker: color("ui.search_marker", &ui.search_marker)?,
            ui_dialog_choice: color("ui.dialog_choice", &ui.dialog_choice)?,
            ui_dialog_input: color("ui.dialog_input", &ui.dialog_input)?,
            ui_action: color("ui.action", &ui.action)?,
            ui_action_ai: color("ui.action_ai", &ui.action_ai)?,
            ui_success: color("ui.success", &ui.success)?,
            ui_warning: color("ui.warning", &ui.warning)?,
            ui_error: color("ui.error", &ui.error)?,
            diff_addition_background: color("diff.addition_background", &d.addition_background)?,
            diff_deletion_background: color("diff.deletion_background", &d.deletion_background)?,
            markdown_heading_1: color("markdown.heading_1", &m.heading_1)?,
            markdown_heading_2: color("markdown.heading_2", &m.heading_2)?,
            markdown_heading_3: color("markdown.heading_3", &m.heading_3)?,
            markdown_heading_minor: color("markdown.heading_minor", &m.heading_minor)?,
            markdown_quote: color("markdown.quote", &m.quote)?,
            markdown_list: color("markdown.list", &m.list)?,
            markdown_rule: color("markdown.rule", &m.rule)?,
            markdown_code: color("markdown.code", &m.code)?,
            markdown_code_block_text: color("markdown.code_block_text", &m.code_block_text)?,
            markdown_code_block_background: color(
                "markdown.code_block_background",
                &m.code_block_background,
            )?,
            markdown_code_label: color("markdown.code_label", &m.code_label)?,
            markdown_link: color("markdown.link", &m.link)?,
            markdown_hashtag: color("markdown.hashtag", &m.hashtag)?,
            markdown_wikilink: color("markdown.wikilink", &m.wikilink)?,
            syntax_keyword: color("syntax.keyword", &syn.keyword)?,
            syntax_string: color("syntax.string", &syn.string)?,
            syntax_comment: color("syntax.comment", &syn.comment)?,
            syntax_function: color("syntax.function", &syn.function)?,
            syntax_type: color("syntax.type", &syn.type_)?,
            syntax_constant: color("syntax.constant", &syn.constant)?,
            animation_gradient: parse_gradient(&file.animation.gradient)?.map(C::from),
        })
    }
}

/// Replace `base` entries with `custom` ones, descending into tables both sides define.
fn overlay_table(base: &mut toml::Table, custom: toml::Table) {
    for (key, value) in custom {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(custom)) => {
                overlay_table(base, custom)
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

impl<C: From<Color> + Copy> Default for Theme<C> {
    fn default() -> Self {
        Self::from_toml(DEFAULT_THEME_TOML).expect("the built-in theme must be valid")
    }
}

/// A theme resolved for presentation: which selection was requested, which
/// theme file ended up active, where it came from, and the parsed theme.
///
/// Storage owns the raw selection and theme files; resolving their semantic colors lives here.
#[derive(Debug, Clone)]
pub struct LoadedTheme<C = Color> {
    pub requested: String,
    pub active: String,
    pub source: Option<PathBuf>,
    pub theme: Theme<C>,
}

/// Names a client can choose from: the built-in themes first, then the other
/// theme files in name order.
pub fn theme_names(storage: &Storage) -> Result<Vec<String>> {
    let mut names: Vec<String> = BUILT_IN_THEMES
        .iter()
        .map(|(name, _)| name.to_string())
        .collect();
    for (name, _) in storage.theme_files()? {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

/// Resolve the theme `target` names in the settings file.
pub fn load_theme<C: From<Color> + Copy>(
    storage: &Storage,
    target: ThemeTarget,
    previous_random_source: Option<&Path>,
) -> Result<LoadedTheme<C>> {
    let requested = storage.load_theme_selection(target)?;
    resolve_theme(storage, target, &requested, previous_random_source)
}

/// Resolve `selection`, persist it as `target`'s theme selection, and return
/// the loaded theme.
pub fn select_theme<C: From<Color> + Copy>(
    storage: &Storage,
    target: ThemeTarget,
    selection: &str,
) -> Result<LoadedTheme<C>> {
    let loaded = resolve_theme(storage, target, selection, None)?;
    storage.write_theme_selection(target, selection)?;
    Ok(loaded)
}

fn resolve_theme<C: From<Color> + Copy>(
    storage: &Storage,
    target: ThemeTarget,
    requested: &str,
    previous_random_source: Option<&Path>,
) -> Result<LoadedTheme<C>> {
    let files = storage.theme_files()?;
    if requested == "random" {
        if let Some(previous) = previous_random_source {
            if let Some((name, path)) = files.iter().find(|(_, path)| path == previous) {
                return load_theme_file(requested, name, path);
            }
        }

        let mut valid = Vec::new();
        for (name, path) in &files {
            if let Ok(theme) = parse_theme_file::<C>(path) {
                valid.push((name, path, theme));
            }
        }
        if valid.is_empty() {
            return load_named_theme(storage, requested, target.fallback_theme());
        }
        let (name, path, theme) = valid.swap_remove(fastrand::usize(..valid.len()));
        return Ok(LoadedTheme {
            requested: requested.to_string(),
            active: name.clone(),
            source: Some(path.clone()),
            theme,
        });
    }

    let known = files.iter().any(|(name, _)| name == requested)
        || BUILT_IN_THEMES.iter().any(|(name, _)| *name == requested);
    let name = if known {
        requested
    } else {
        target.fallback_theme()
    };
    load_named_theme(storage, requested, name)
}

/// Load the theme file `name`, or the built-in source of that name when the
/// file is missing.
fn load_named_theme<C: From<Color> + Copy>(
    storage: &Storage,
    requested: &str,
    name: &str,
) -> Result<LoadedTheme<C>> {
    let path = storage.themes_dir.join(format!("{name}.toml"));
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            return load_theme_file(requested, name, &path)
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading theme {}", path.display()))
        }
    }
    let (name, source) = BUILT_IN_THEMES
        .iter()
        .find(|(built_in, _)| *built_in == name)
        .unwrap_or(&BUILT_IN_THEMES[0]);
    Ok(LoadedTheme {
        requested: requested.to_string(),
        active: name.to_string(),
        source: None,
        theme: Theme::from_toml(source).expect("built-in themes are valid"),
    })
}

fn load_theme_file<C: From<Color> + Copy>(
    requested: &str,
    name: &str,
    path: &Path,
) -> Result<LoadedTheme<C>> {
    Ok(LoadedTheme {
        requested: requested.to_string(),
        active: name.to_string(),
        source: Some(path.to_path_buf()),
        theme: parse_theme_file(path)?,
    })
}

fn parse_theme_file<C: From<Color> + Copy>(path: &Path) -> Result<Theme<C>> {
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
    use super::{Color, Storage, ThemeTarget, DEFAULT_THEME_TOML};
    use crate::core::storage::BUILT_IN_THEMES;
    type Theme = super::Theme<Color>;
    fn load_theme(
        storage: &Storage,
        previous: Option<&std::path::Path>,
    ) -> anyhow::Result<super::LoadedTheme> {
        super::load_theme(storage, ThemeTarget::Terminal, previous)
    }
    use std::fs;

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
        storage
            .write_theme_selection(ThemeTarget::Terminal, "custom")
            .unwrap();

        storage.ensure_files().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), custom);
        assert_eq!(
            storage.load_theme_selection(ThemeTarget::Terminal).unwrap(),
            "custom"
        );
        let loaded = load_theme(&storage, None).unwrap();
        assert_eq!(loaded.requested, "custom");
        assert_eq!(loaded.active, "custom");
        assert_eq!(loaded.source.as_deref(), Some(path.as_path()));
        assert_eq!(loaded.theme.surface_message_agent, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn missing_and_random_theme_selections_fall_back_or_resolve() {
        let (_dir, storage) = fresh();
        for (name, _) in BUILT_IN_THEMES
            .iter()
            .filter(|(name, _)| *name != "default")
        {
            fs::remove_file(storage.themes_dir.join(format!("{name}.toml"))).unwrap();
        }
        let default_path = storage.themes_dir.join("default.toml");
        let edited_default =
            DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#090807\"");
        fs::write(&default_path, &edited_default).unwrap();
        assert_eq!(fs::read_to_string(&default_path).unwrap(), edited_default);
        storage
            .write_theme_selection(ThemeTarget::Terminal, "missing")
            .unwrap();
        let missing = load_theme(&storage, None).unwrap();
        assert_eq!(missing.requested, "missing");
        assert_eq!(missing.active, "default");
        assert_eq!(missing.source.as_deref(), Some(default_path.as_path()));
        assert_eq!(missing.theme.surface_panel, Color::Rgb(9, 8, 7));

        let custom = DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#010203\"");
        let path = storage.themes_dir.join("only.toml");
        fs::write(&path, custom).unwrap();
        assert_eq!(storage.list_theme_names().unwrap(), vec!["only"]);
        storage
            .write_theme_selection(ThemeTarget::Terminal, "random")
            .unwrap();

        let default_random = load_theme(&storage, Some(&default_path)).unwrap();
        assert_eq!(default_random.requested, "random");
        assert_eq!(default_random.active, "default");
        assert_eq!(
            default_random.source.as_deref(),
            Some(default_path.as_path())
        );
        assert_eq!(default_random.theme.surface_panel, Color::Rgb(9, 8, 7));

        let custom_random = load_theme(&storage, Some(&path)).unwrap();
        assert_eq!(custom_random.requested, "random");
        assert_eq!(custom_random.active, "only");
        assert_eq!(custom_random.source.as_deref(), Some(path.as_path()));
        assert_eq!(custom_random.theme.surface_panel, Color::Rgb(1, 2, 3));

        fs::remove_file(default_path).unwrap();
        storage
            .write_theme_selection(ThemeTarget::Terminal, "missing")
            .unwrap();
        let built_in = load_theme(&storage, None).unwrap();
        assert_eq!(built_in.active, "default");
        assert_eq!(built_in.source, None);
        assert_eq!(built_in.theme, Theme::default());
    }

    #[test]
    fn terminal_and_desktop_keep_separate_selections() {
        let (_dir, storage) = fresh();
        let desktop = super::load_theme::<Color>(&storage, ThemeTarget::Desktop, None).unwrap();
        assert_eq!(desktop.active, "paper");
        assert_eq!(desktop.theme.surface_canvas, Color::Rgb(0xfa, 0xf8, 0xf4));
        assert_eq!(load_theme(&storage, None).unwrap().active, "default");

        super::select_theme::<Color>(&storage, ThemeTarget::Desktop, "default").unwrap();
        super::select_theme::<Color>(&storage, ThemeTarget::Terminal, "paper").unwrap();
        assert_eq!(
            storage.load_theme_selection(ThemeTarget::Desktop).unwrap(),
            "default"
        );
        assert_eq!(
            storage.load_theme_selection(ThemeTarget::Terminal).unwrap(),
            "paper"
        );

        storage
            .write_theme_selection(ThemeTarget::Desktop, "missing")
            .unwrap();
        let fallback = super::load_theme::<Color>(&storage, ThemeTarget::Desktop, None).unwrap();
        assert_eq!(fallback.requested, "missing");
        assert_eq!(fallback.active, "paper");
    }

    #[test]
    fn built_in_themes_load_after_their_files_are_removed() {
        let (_dir, storage) = fresh();
        for (name, source) in BUILT_IN_THEMES {
            let path = storage.themes_dir.join(format!("{name}.toml"));
            assert_eq!(fs::read_to_string(&path).unwrap(), source);
            fs::remove_file(path).unwrap();
            let loaded =
                super::select_theme::<Color>(&storage, ThemeTarget::Desktop, name).unwrap();
            assert_eq!(loaded.active, name);
            assert_eq!(loaded.source, None);
            assert_eq!(loaded.theme, Theme::from_toml(source).unwrap());
        }
        assert_eq!(super::theme_names(&storage).unwrap(), ["default", "paper"]);
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
    fn partial_theme_keeps_default_tokens_and_ignores_unknown_keys() {
        let theme = Theme::from_toml(
            "[surface]\npanel = \"#010203\"\nglow = \"#ffffff\"\n\n[brand]\nname = \"Mine\"\n",
        )
        .unwrap();
        let default = Theme::default();
        assert_eq!(theme.surface_panel, Color::Rgb(1, 2, 3));
        assert_eq!(theme.surface_overlay, default.surface_overlay);
        assert_eq!(theme.ui_error, default.ui_error);
        assert_eq!(theme.syntax_keyword, default.syntax_keyword);
        assert_eq!(Theme::from_toml("").unwrap(), default);
    }

    #[test]
    fn rejects_invalid_colors_and_malformed_toml() {
        let invalid = DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"dark\"");
        assert!(Theme::from_toml(&invalid)
            .unwrap_err()
            .to_string()
            .contains("surface.panel"));

        assert!(Theme::from_toml("[surface\n")
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

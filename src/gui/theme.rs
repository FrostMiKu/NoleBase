//! Desktop projection of the same semantic colors used by the TUI.
use super::presentation::{RADIUS_LG, RADIUS_MD, TEXT_UI};
use crate::core::theme::{Color, Theme};
use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::ThemeMode;
use gpui_kit::{rgb, App, Hsla};
use std::sync::Arc;

pub fn color(value: Color) -> Hsla {
    match value {
        // A native window supplies its own opaque canvas for terminal transparency.
        Color::Reset => rgb(0x1e1e2e).into(),
        Color::Rgb(r, g, b) => {
            rgb((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)).into()
        }
    }
}

pub fn is_dark(tokens: Theme) -> bool {
    color(tokens.surface_canvas).l < 0.5
}

/// The wash painted over selected text. TextView paints it over the glyphs,
/// so it stays translucent to keep the ink readable.
pub fn selection(tokens: Theme) -> Hsla {
    color(tokens.selection_indicator).opacity(0.24)
}

/// `base` moved `amount` of lightness away from the canvas: darker on a light
/// theme, lighter on a dark one. Hover and press states use it so they keep
/// the hue of the color they emphasize.
pub fn emphasize(tokens: Theme, base: Hsla, amount: f32) -> Hsla {
    let l = if is_dark(tokens) {
        base.l + amount
    } else {
        base.l - amount
    };
    Hsla {
        l: l.clamp(0., 1.),
        ..base
    }
}

/// Code highlighting from the theme's syntax tokens.
fn highlight_theme(tokens: Theme, mode: ThemeMode) -> HighlightTheme {
    let hex = |value: Color| {
        let rgba: gpui_kit::Rgba = color(value).into();
        let channel = |value: f32| (value * 255.).round() as u8;
        format!(
            "#{:02x}{:02x}{:02x}",
            channel(rgba.r),
            channel(rgba.g),
            channel(rgba.b)
        )
    };
    let style = |value: Color| serde_json::json!({ "color": hex(value) });
    let italic = |value: Color| serde_json::json!({ "color": hex(value), "font_style": "italic" });
    let syntax = serde_json::json!({
        "keyword": style(tokens.syntax_keyword),
        "string": style(tokens.syntax_string),
        "string.escape": style(tokens.syntax_constant),
        "string.special": style(tokens.syntax_string),
        "comment": italic(tokens.syntax_comment),
        "comment.doc": italic(tokens.syntax_comment),
        "function": style(tokens.syntax_function),
        "constructor": style(tokens.syntax_type),
        "type": style(tokens.syntax_type),
        "enum": style(tokens.syntax_type),
        "constant": style(tokens.syntax_constant),
        "number": style(tokens.syntax_constant),
        "boolean": style(tokens.syntax_constant),
        "attribute": style(tokens.syntax_type),
        "tag": style(tokens.syntax_keyword),
        "title": style(tokens.syntax_function),
        "property": style(tokens.markdown_code_block_text),
        "variable": style(tokens.markdown_code_block_text),
        "operator": style(tokens.markdown_code_block_text),
        "punctuation": style(tokens.syntax_comment),
        "link_uri": style(tokens.markdown_link),
        "link_text": style(tokens.markdown_link),
    });
    serde_json::from_value(serde_json::json!({
        "name": "nole",
        "appearance": mode,
        "style": {
            "editor.background": hex(tokens.markdown_code_block_background),
            "editor.foreground": hex(tokens.markdown_code_block_text),
            "syntax": syntax,
        },
    }))
    .expect("syntax colors form a valid highlight theme")
}

pub fn apply(tokens: Theme, cx: &mut App) {
    let mode = if is_dark(tokens) {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    let shade = |value: Color, amount: f32| emphasize(tokens, color(value), amount);
    gpui_kit::component::Theme::change(mode, None, cx);
    gpui_kit::component::Theme::update(cx, |theme| {
        let c = &mut theme.colors;
        c.background = color(tokens.surface_canvas);
        c.foreground = color(tokens.text_primary);
        c.border = color(tokens.ui_border_subtle);
        c.input = color(tokens.ui_border);
        c.caret = color(tokens.ui_input_prompt);
        c.selection = selection(tokens);
        c.primary = color(tokens.ui_action);
        c.primary_foreground = color(tokens.text_on_accent);
        c.primary_hover = shade(tokens.ui_action, 0.06);
        c.primary_active = shade(tokens.ui_action, 0.12);
        c.secondary = color(tokens.surface_sunken);
        c.secondary_foreground = color(tokens.text_primary);
        c.secondary_hover = shade(tokens.surface_sunken, 0.03);
        c.secondary_active = shade(tokens.surface_sunken, 0.06);
        c.accent = color(tokens.selection_background);
        c.accent_foreground = color(tokens.selection_foreground);
        c.muted = color(tokens.surface_sunken);
        c.muted_foreground = color(tokens.text_muted);
        c.ring = color(tokens.ui_focus_border);
        c.link = color(tokens.markdown_link);
        c.link_hover = shade(tokens.markdown_link, 0.08);
        c.link_active = shade(tokens.markdown_link, 0.12);
        c.danger = color(tokens.ui_error);
        c.danger_foreground = color(tokens.text_on_accent);
        c.danger_hover = shade(tokens.ui_error, 0.06);
        c.danger_active = shade(tokens.ui_error, 0.12);
        c.success = color(tokens.ui_success);
        c.success_foreground = color(tokens.text_on_accent);
        c.success_hover = shade(tokens.ui_success, 0.06);
        c.success_active = shade(tokens.ui_success, 0.12);
        c.warning = color(tokens.ui_warning);
        c.warning_foreground = color(tokens.text_on_accent);
        c.warning_hover = shade(tokens.ui_warning, 0.06);
        c.warning_active = shade(tokens.ui_warning, 0.12);
        c.info = color(tokens.markdown_link);
        c.info_foreground = color(tokens.text_on_accent);
        c.popover = color(tokens.surface_overlay);
        c.popover_foreground = color(tokens.text_primary);
        c.list = color(tokens.surface_canvas);
        c.list_even = color(tokens.surface_canvas);
        c.list_hover = color(tokens.selection_background_inactive);
        c.list_active = color(tokens.selection_background);
        c.list_active_border = color(tokens.ui_action);
        c.scrollbar = color(tokens.surface_canvas).opacity(0.);
        c.scrollbar_thumb = color(tokens.ui_border);
        c.scrollbar_thumb_hover = color(tokens.text_muted);
        c.table = color(tokens.surface_canvas);
        c.table_even = color(tokens.surface_canvas);
        c.table_head = color(tokens.surface_sunken);
        c.table_head_foreground = color(tokens.text_secondary);
        c.table_hover = color(tokens.selection_background_inactive);
        c.table_row_border = color(tokens.ui_border_subtle);
        c.button_primary = c.primary;
        c.button_primary_foreground = c.primary_foreground;
        c.button_primary_hover = c.primary_hover;
        c.button_primary_active = c.primary_active;
        c.sidebar = color(tokens.surface_panel);
        c.sidebar_foreground = color(tokens.text_secondary);
        c.sidebar_accent = color(tokens.surface_raised);
        c.sidebar_accent_foreground = color(tokens.text_primary);
        c.sidebar_border = color(tokens.ui_border_subtle);
        c.title_bar = color(tokens.surface_panel);
        c.title_bar_border = color(tokens.ui_border_subtle);
        c.window_border = color(tokens.ui_border_subtle);
        theme.highlight_theme = Arc::new(highlight_theme(tokens, mode));
        theme.font_size = gpui_kit::px(TEXT_UI);
        theme.radius = gpui_kit::px(RADIUS_MD);
        theme.radius_lg = gpui_kit::px(RADIUS_LG);
        theme.shadow = true;
    });
}

#[cfg(all(test, feature = "gui-test"))]
mod tests {
    use super::*;
    use gpui_kit::component::ActiveTheme;

    fn luminance(color: gpui_kit::Rgba) -> f32 {
        fn channel(value: f32) -> f32 {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }

    fn contrast(foreground: Hsla, background: Hsla) -> f32 {
        let foreground = luminance(foreground.into());
        let background = luminance(background.into());
        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
    }

    fn built_in_themes() -> Vec<Theme> {
        crate::core::storage::BUILT_IN_THEMES
            .iter()
            .map(|(_, source)| Theme::from_toml(source).unwrap())
            .collect()
    }

    #[gpui_kit::test]
    fn selection_keeps_text_readable_on_every_built_in_theme(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            for tokens in built_in_themes() {
                apply(tokens, cx);
                let theme = cx.theme();
                let overlay: gpui_kit::Rgba = theme.selection.into();
                // The reading view paints selection above both the glyph and canvas.
                let ink: gpui_kit::Rgba = theme.foreground.into();
                let canvas: gpui_kit::Rgba = theme.background.into();
                let ratio = contrast(ink.blend(overlay).into(), canvas.blend(overlay).into());
                assert!(ratio >= 4.5, "selected text contrast: {ratio}");
            }
        });
    }

    #[test]
    fn built_in_themes_keep_body_text_and_primary_buttons_readable() {
        for tokens in built_in_themes() {
            assert!(contrast(color(tokens.text_primary), color(tokens.surface_canvas)) >= 7.);
            assert!(contrast(color(tokens.text_secondary), color(tokens.surface_raised)) >= 4.5);
            assert!(contrast(color(tokens.text_on_accent), color(tokens.ui_action)) >= 4.5);
        }
    }

    #[test]
    fn emphasis_moves_away_from_the_canvas() {
        let [dark, light] = [built_in_themes()[0], built_in_themes()[1]];
        let base = color(dark.ui_action);
        assert!(emphasize(dark, base, 0.1).l > base.l);
        let base = color(light.ui_action);
        assert!(emphasize(light, base, 0.1).l < base.l);
    }
}

//! Desktop projection of the same semantic colors used by the TUI.
use crate::core::theme::{Color, Theme};
use gpui_kit::{rgb, App, Hsla};

pub fn color(value: Color) -> Hsla {
    match value {
        // A native window supplies its own opaque canvas for terminal transparency.
        Color::Reset => rgb(0x1e1e2e).into(),
        Color::Rgb(r, g, b) => {
            rgb((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)).into()
        }
    }
}

pub fn apply(tokens: Theme, cx: &mut App) {
    let mode = if color(tokens.surface_canvas).l < 0.5 {
        gpui_kit::component::ThemeMode::Dark
    } else {
        gpui_kit::component::ThemeMode::Light
    };
    gpui_kit::component::Theme::change(mode, None, cx);
    gpui_kit::component::Theme::update(cx, |theme| {
        let c = &mut theme.colors;
        c.background = color(tokens.surface_canvas);
        c.foreground = color(tokens.text_primary);
        c.border = color(tokens.ui_border_subtle);
        c.input = color(tokens.ui_border_subtle);
        c.caret = color(tokens.ui_input_prompt);
        // TextView paints the highlight over the glyphs; keep the ink visible.
        c.selection = color(tokens.selection_indicator).opacity(0.24);
        c.primary = color(tokens.ui_action);
        c.primary_foreground = color(tokens.text_on_accent);
        c.primary_hover = color(tokens.selection_indicator);
        c.primary_active = color(tokens.selection_indicator);
        c.secondary = color(tokens.surface_compose);
        c.secondary_foreground = color(tokens.text_secondary);
        c.secondary_active = color(tokens.markdown_quote).opacity(0.4);
        c.secondary_hover = color(tokens.selection_background);
        c.accent = color(tokens.selection_background);
        c.accent_foreground = color(tokens.selection_foreground);
        c.muted = color(tokens.surface_panel);
        c.muted_foreground = color(tokens.text_muted);
        c.ring = color(tokens.ui_focus_border);
        c.link = color(tokens.markdown_link);
        c.link_hover = color(tokens.markdown_wikilink);
        c.scrollbar = color(tokens.surface_panel);
        c.scrollbar_thumb = color(tokens.ui_border_subtle);
        c.scrollbar_thumb_hover = color(tokens.ui_border);
        c.table = color(tokens.surface_canvas);
        c.table_even = color(tokens.surface_canvas);
        c.table_head = color(tokens.surface_panel);
        c.table_head_foreground = color(tokens.text_secondary);
        c.table_hover = color(tokens.selection_background_inactive);
        c.table_row_border = color(tokens.ui_border_subtle);
        c.button_primary = color(tokens.ui_action);
        c.button_primary_foreground = color(tokens.text_on_accent);
        c.button_primary_hover = color(tokens.selection_indicator);
        c.button_primary_active = color(tokens.selection_indicator);
        c.sidebar = color(tokens.surface_panel);
        c.sidebar_foreground = color(tokens.text_primary);
        c.sidebar_accent = color(tokens.selection_background_inactive);
        c.sidebar_accent_foreground = color(tokens.text_primary);
        c.sidebar_border = color(tokens.ui_border_subtle);
        theme.font_size = gpui_kit::px(15.);
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

    #[gpui_kit::test]
    fn selection_keeps_text_readable_on_light_and_dark_canvases(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            let dark = Theme::default();
            let light = Theme {
                surface_canvas: Color::Rgb(250, 250, 250),
                text_primary: Color::Rgb(25, 25, 30),
                ..dark
            };
            for tokens in [dark, light] {
                apply(tokens, cx);
                let theme = cx.theme();
                let overlay: gpui_kit::Rgba = theme.selection.into();
                // The reading view paints selection above both the glyph and canvas.
                let ink: gpui_kit::Rgba = theme.foreground.into();
                let canvas: gpui_kit::Rgba = theme.background.into();
                let ink = luminance(ink.blend(overlay));
                let canvas = luminance(canvas.blend(overlay));
                let contrast = (ink.max(canvas) + 0.05) / (ink.min(canvas) + 0.05);
                assert!(contrast >= 4.5, "selected text contrast: {contrast}");
            }
        });
    }
}

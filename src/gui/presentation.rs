//! Desktop design tokens: spacing, corner radii, the type scale, font families
//! and the reading measure, plus the Markdown style built from them. Views
//! reference these constants instead of literal sizes.
use super::theme::color;
use crate::core::theme::Theme;
use gpui_kit::base::TextViewStyle;
use gpui_kit::{px, rems, App, FontWeight, Global, HighlightStyle, SharedString};
use gpui_kit::{StyleRefinement, Styled};

pub const SPACE_XS: f32 = 4.;
pub const SPACE_SM: f32 = 8.;
pub const SPACE_MD: f32 = 12.;
pub const SPACE_LG: f32 = 16.;
pub const SPACE_XL: f32 = 24.;
pub const SPACE_2XL: f32 = 32.;
pub const SPACE_3XL: f32 = 48.;

/// Inline marks: code spans, swatches.
pub const RADIUS_SM: f32 = 4.;
/// Controls and rows: buttons, inputs, navigation items.
pub const RADIUS_MD: f32 = 8.;
/// Containers: cards, composers, code blocks, popovers.
pub const RADIUS_LG: f32 = 12.;
pub const RADIUS_PILL: f32 = 999.;

/// Small capitals over a section: "最近笔记", "大纲".
pub const TEXT_LABEL: f32 = 11.;
/// Metadata: dates, counts, hints.
pub const TEXT_META: f32 = 12.;
/// Navigation rows and secondary controls.
pub const TEXT_NAV: f32 = 13.;
/// Default interface text, the GPUI Kit base size, and toolbar titles.
pub const TEXT_UI: f32 = 14.;
pub const BODY_SIZE: f32 = 16.;
pub const BODY_LINE: f32 = 28.;
pub const CODE_SIZE: f32 = 13.;
pub const PARAGRAPH_GAP: f32 = 16.;

/// The reading measure shared by documents, Daily and the Agent transcript.
pub const READING_WIDTH: f32 = 680.;
pub const PAGE_INSET: f32 = SPACE_XL;
/// The document outline beside the reading column.
pub const OUTLINE_WIDTH: f32 = 208.;
pub const OUTLINE_GAP: f32 = SPACE_3XL;

pub fn heading_size(level: u8) -> f32 {
    match level {
        1 => 32.,
        2 => 22.,
        3 => 18.,
        4 => 16.,
        _ => 15.,
    }
}

/// Horizontal inset that centers the reading column, never smaller than the page inset.
pub fn document_inset(available: f32) -> f32 {
    ((available - READING_WIDTH) / 2.).max(PAGE_INSET)
}

/// Where a document's column and outline sit in `available` width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocumentLayout {
    pub column_left: f32,
    pub column_width: f32,
    /// Left edge of the outline, when it fits beside a full reading column.
    pub outline_left: Option<f32>,
}

/// Center the column, or the column and outline together when both fit.
pub fn document_layout(available: f32) -> DocumentLayout {
    let beside = OUTLINE_GAP + OUTLINE_WIDTH;
    if available >= READING_WIDTH + beside + PAGE_INSET * 2. {
        let column_left = (available - READING_WIDTH - beside) / 2.;
        DocumentLayout {
            column_left,
            column_width: READING_WIDTH,
            outline_left: Some(column_left + READING_WIDTH + OUTLINE_GAP),
        }
    } else {
        let column_left = document_inset(available);
        DocumentLayout {
            column_left,
            column_width: (available - column_left * 2.).max(0.),
            outline_left: None,
        }
    }
}

/// Serif families tried for headings, in order of preference. The interface
/// font serves when none is installed.
const SERIF_FAMILIES: [&str; 9] = [
    "Noto Serif CJK SC",
    "Source Han Serif SC",
    "Noto Serif SC",
    "Songti SC",
    "STSong",
    "Noto Serif",
    "Source Serif 4",
    "Georgia",
    "DejaVu Serif",
];

/// The first preferred serif family among `installed`.
pub fn preferred_serif<'a>(installed: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let installed: Vec<&str> = installed.into_iter().collect();
    SERIF_FAMILIES
        .into_iter()
        .find(|family| installed.contains(family))
}

/// Font families resolved once against the fonts installed on this system.
#[derive(Clone)]
pub struct Fonts {
    pub serif: SharedString,
    pub mono: SharedString,
}
impl Global for Fonts {}

pub fn fonts(cx: &mut App) -> Fonts {
    if let Some(fonts) = cx.try_global::<Fonts>() {
        return fonts.clone();
    }
    let theme = gpui_kit::component::ActiveTheme::theme(cx);
    let (ui, mono) = (theme.font_family.clone(), theme.mono_font_family.clone());
    let installed = cx.text_system().all_font_names();
    let serif = preferred_serif(installed.iter().map(String::as_str))
        .map(SharedString::from)
        .unwrap_or(ui);
    let fonts = Fonts { serif, mono };
    cx.set_global(fonts.clone());
    fonts
}

pub fn code_block_style(tokens: Theme, fonts: &Fonts) -> StyleRefinement {
    StyleRefinement::default()
        .bg(color(tokens.markdown_code_block_background))
        .text_color(color(tokens.markdown_code_block_text))
        .border_1()
        .border_color(color(tokens.ui_border_subtle))
        .rounded(px(RADIUS_LG))
        .font_family(fonts.mono.clone())
        .text_size(px(CODE_SIZE))
        .line_height(px(22.))
}

pub fn markdown_style(tokens: Theme, fonts: &Fonts, rem_size: f32) -> TextViewStyle {
    let serif = fonts.serif.clone();
    let headings = [
        tokens.markdown_heading_1,
        tokens.markdown_heading_2,
        tokens.markdown_heading_3,
        tokens.markdown_heading_minor,
    ];
    let dark = color(tokens.surface_canvas).l < 0.5;
    TextViewStyle::default()
        .with_foreground(color(tokens.text_primary))
        .with_muted_foreground(color(tokens.text_muted))
        .with_link(color(tokens.markdown_link))
        .with_selection(super::theme::selection(tokens))
        .with_code_background(color(tokens.surface_sunken))
        .with_border(color(tokens.ui_border_subtle))
        .with_paragraph_gap(rems(PARAGRAPH_GAP / rem_size))
        .with_heading(move |level| {
            StyleRefinement::default()
                .text_size(px(heading_size(level)))
                .font_family(serif.clone())
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color(headings[usize::from(level.clamp(1, 4)) - 1]))
        })
        .with_code_block(code_block_style(tokens, fonts))
        .with_inline_code(HighlightStyle {
            color: Some(color(tokens.markdown_code)),
            background_color: Some(color(tokens.surface_sunken)),
            ..Default::default()
        })
        .with_inline_code_style(
            StyleRefinement::default()
                .font_family(fonts.mono.clone())
                .rounded(px(RADIUS_SM)),
        )
        .with_list_style(StyleRefinement::default().text_color(color(tokens.markdown_list)))
        .with_blockquote_style(
            StyleRefinement::default()
                .border_l_2()
                .border_color(color(tokens.ui_border))
                .text_color(color(tokens.markdown_quote))
                .font_family(fonts.serif.clone())
                .italic(),
        )
        .with_table(
            StyleRefinement::default()
                .bg(color(tokens.surface_canvas))
                .rounded(px(RADIUS_MD)),
        )
        .with_table_head(
            StyleRefinement::default()
                .bg(color(tokens.surface_sunken))
                .text_color(color(tokens.text_secondary))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .with_table_row_hover(color(tokens.selection_background_inactive))
        .with_dark(dark)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serif_preference_follows_the_installed_fonts() {
        assert_eq!(
            preferred_serif(["DejaVu Serif", "Noto Serif CJK SC", "Inter"]),
            Some("Noto Serif CJK SC")
        );
        assert_eq!(preferred_serif(["Inter", "Georgia"]), Some("Georgia"));
        assert_eq!(preferred_serif(["Inter", "Noto Sans CJK SC"]), None);
    }

    #[test]
    fn the_outline_appears_only_beside_a_full_reading_column() {
        let needed = READING_WIDTH + OUTLINE_GAP + OUTLINE_WIDTH + PAGE_INSET * 2.;
        let wide = document_layout(needed + 100.);
        let outline = wide.outline_left.unwrap();
        assert_eq!(wide.column_width, READING_WIDTH);
        // Column and outline are centered together.
        assert_eq!(wide.column_left, needed + 100. - (outline + OUTLINE_WIDTH));
        assert_eq!(outline - (wide.column_left + READING_WIDTH), OUTLINE_GAP);
        assert!(document_layout(needed).outline_left.is_some());

        let narrow = document_layout(needed - 1.);
        assert_eq!(narrow.outline_left, None);
        assert_eq!(narrow.column_width, READING_WIDTH);
        let tight = document_layout(READING_WIDTH);
        assert_eq!(tight.column_left, PAGE_INSET);
        assert_eq!(tight.column_width, READING_WIDTH - PAGE_INSET * 2.);
    }
}

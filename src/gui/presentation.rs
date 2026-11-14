//! Document geometry and typography shared by reading and card previews.
//! Inspired by quiet-md's editor and thumbnail metrics (MIT; see assets/licenses/).
use super::theme::color;
use crate::core::theme::Theme;
use gpui_kit::component::text::TextViewStyle;
use gpui_kit::{prelude::*, px, rems, StyleRefinement};

pub const BODY_SIZE: f32 = 15.;
pub const BODY_LINE: f32 = 24.;
pub const PARAGRAPH_GAP: f32 = 13.;
pub const READING_WIDTH: f32 = 800.;
pub const PREVIEW_SCALE: f32 = 0.45;
pub const PAGE_INSET: f32 = 24.;
pub const CARD_INSET: f32 = 18.;
pub const CARD_FRAME: f32 = 4.;

pub fn heading_size(level: u8) -> f32 {
    match level {
        1 => 32.,
        2 => 24.,
        3 => 20.,
        4 => 18.,
        _ => 16.,
    }
}

/// Keep a comfortable reading measure on wide windows and a usable inset on small ones.
pub fn document_inset(available: f32) -> f32 {
    ((available - READING_WIDTH) / 2.).max(PAGE_INSET) - PAGE_INSET
}

pub fn markdown_style(tokens: Theme, rem_size: f32) -> TextViewStyle {
    TextViewStyle::default()
        .paragraph_gap(rems(PARAGRAPH_GAP / rem_size))
        .heading_font_size(|level, _| px(heading_size(level)))
        .table(StyleRefinement::default().bg(color(tokens.surface_canvas)))
        .code_block(
            StyleRefinement::default()
                .bg(color(tokens.markdown_code_block_background))
                .text_color(color(tokens.markdown_code_block_text))
                .border_1()
                .border_color(color(tokens.ui_border_subtle).opacity(0.45))
                .rounded(px(8.))
                .text_size(px(13.))
                .line_height(px(20.)),
        )
}

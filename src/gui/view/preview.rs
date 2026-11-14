use super::*;
use crate::gui::markdown::{PreviewBlock, PreviewKind};

impl DesktopView {
    pub(super) fn note_preview(&self, blocks: &[PreviewBlock]) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        div()
            .flex_1()
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(6.))
            .text_size(px(
                super::super::presentation::BODY_SIZE * super::super::presentation::PREVIEW_SCALE
            ))
            .line_height(gpui_kit::relative(1.6))
            .text_color(color(t.text_primary))
            .children(blocks.iter().enumerate().map(|(index, block)| {
                let row = div()
                    .debug_selector(move || format!("card-preview-block-{index}").into())
                    .w_full()
                    .flex_shrink_0()
                    .child(block.text.clone());
                match block.kind {
                    PreviewKind::Heading(level) => row
                        .text_size(px(super::super::presentation::heading_size(level)
                            * super::super::presentation::PREVIEW_SCALE))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(color(t.text_primary)),
                    PreviewKind::Quote => row
                        .pl(px(5.))
                        .border_l_2()
                        .border_color(color(t.markdown_quote))
                        .text_color(color(t.text_muted)),
                    PreviewKind::Code => row
                        .p(px(5.))
                        .rounded(px(5.))
                        .bg(color(t.markdown_code_block_background))
                        .text_color(color(t.markdown_code_block_text))
                        .font_family("monospace")
                        .text_size(px(6.)),
                    PreviewKind::Rule => div()
                        .h(px(1.))
                        .w_full()
                        .bg(color(t.markdown_rule).opacity(0.5)),
                    PreviewKind::Table => {
                        let mut grid = div()
                            .w_full()
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .border_1()
                            .border_color(color(t.ui_border_subtle).opacity(0.5))
                            .rounded(px(3.))
                            .overflow_hidden()
                            .text_size(px(6.));
                        if let Some(table) = &block.table {
                            for (row_index, cells) in table.rows.iter().enumerate() {
                                grid = grid.child(
                                    div()
                                        .w_full()
                                        .flex()
                                        .when(row_index == 0, |row| {
                                            row.bg(color(t.surface_panel))
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        })
                                        .when(row_index > 0, |row| {
                                            row.border_t_1().border_color(
                                                color(t.ui_border_subtle).opacity(0.4),
                                            )
                                        })
                                        .children(cells.iter().enumerate().map(|(col, cell)| {
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .px(px(3.))
                                                .py(px(2.))
                                                .when(col > 0, |cell| {
                                                    cell.border_l_1().border_color(
                                                        color(t.ui_border_subtle).opacity(0.4),
                                                    )
                                                })
                                                .child(cell.clone())
                                        })),
                                );
                            }
                        }
                        grid
                    }
                    PreviewKind::List => row.pl(px(5.)),
                    PreviewKind::Paragraph => row,
                }
            }))
    }
}

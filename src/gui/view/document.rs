//! The document page: one reading column shared by reading and editing, a
//! line of metadata above it, and the outline with backlinks beside it.
use super::widgets::relative_time;
use super::*;
use crate::gui::presentation::{
    document_layout, BODY_LINE, BODY_SIZE, OUTLINE_WIDTH, RADIUS_MD, SPACE_2XL, SPACE_3XL,
    SPACE_LG, SPACE_MD, SPACE_SM, SPACE_XL, SPACE_XS, TEXT_LABEL, TEXT_META,
};
use gpui_kit::base::TextView;

impl DesktopView {
    pub(super) fn open_daily_document(
        &mut self,
        date: chrono::NaiveDate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .model
            .workspace
            .storage()
            .daily_file_path(&date.to_string())
            .and_then(|path| {
                self.model
                    .workspace
                    .relative_path(&path)
                    .ok_or_else(|| anyhow::anyhow!("日常记录路径超出了知识库范围"))
            });
        match path {
            Ok(path) => {
                self.navigate(Some(path.clone()), window, cx);
                if self.page == Page::Document
                    && self.model.selected.as_ref() == Some(&path)
                    && self.preview
                {
                    self.toggle_preview(window, cx);
                }
            }
            Err(error) => {
                self.status = format!("打开日常记录遇到问题：{error}");
                cx.notify();
            }
        }
    }

    pub(super) fn document_surface(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let layout = document_layout(self.content_width(window));
        let surface = div()
            .id("document-surface")
            .w_full()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .text_size(px(BODY_SIZE))
            .line_height(px(BODY_LINE));
        let body = if self.preview {
            self.document_reading(layout, window, cx)
        } else {
            self.document_editing(layout, cx)
        };
        surface
            .child(body)
            .when_some(layout.outline_left, |surface, left| {
                surface.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(left))
                        .w(px(OUTLINE_WIDTH))
                        .child(self.document_aside(cx)),
                )
            })
    }

    fn document_reading(
        &self,
        layout: crate::gui::presentation::DocumentLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t = self.model.theme.theme;
        let scroll = self.document_scroll.clone();
        let text = crate::gui::rich_text::body(
            self.markdown_view(TextView::new(&self.document_text), cx),
            t,
            window,
            cx,
        )
        .on_reveal(move |line, window, _| {
            let viewport = scroll.bounds();
            let offset = scroll.offset();
            let target = offset.y - (line.top() - viewport.top()) + px(SPACE_XL);
            let max = scroll.max_offset().y;
            scroll.set_offset(gpui_kit::point(offset.x, target.min(px(0.)).max(-max)));
            window.refresh();
        });
        div()
            .debug_selector(|| "document-reading".into())
            .size_full()
            .relative()
            .child(
                div()
                    .id("document-reading-scroll")
                    .debug_selector(|| "document-viewport".into())
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.document_scroll)
                    .pl(px(layout.column_left))
                    .pt(px(SPACE_3XL))
                    .pb(px(SPACE_3XL * 2.))
                    .child(
                        div()
                            .debug_selector(|| "document-column".into())
                            .w(px(layout.column_width))
                            .child(self.document_kicker(cx))
                            .child(crate::gui::rich_text::container(false).child(text))
                            .when(layout.outline_left.is_none(), |column| {
                                column.child(self.document_backlinks(true, cx))
                            }),
                    ),
            )
            .child(Scrollbar::vertical(&self.document_scroll))
            .into_any_element()
    }

    fn document_editing(
        &self,
        layout: crate::gui::presentation::DocumentLayout,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let t = self.model.theme.theme;
        let scrollbar = self.document_editor_scrollbar(cx);
        div()
            .id("document-editor-surface")
            .size_full()
            .pt(px(SPACE_XL))
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div().size_full().pl(px(layout.column_left)).child(
                            div()
                                .debug_selector(|| "document-editor".into())
                                .w(px(layout.column_width))
                                .h_full()
                                .child(Textarea::new(&self.editor).appearance(false).h_full()),
                        ),
                    )
                    .child(scrollbar),
            )
            .child(
                div()
                    .h(px(28.))
                    .pl(px(layout.column_left))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w(px(layout.column_width))
                            .text_right()
                            .text_size(px(TEXT_META))
                            .text_color(color(t.text_muted))
                            .child(format!("{} 字", self.draft_chars)),
                    ),
            )
            .into_any_element()
    }

    /// Date, archive state and tags above the reading column.
    fn document_kicker(&self, _cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let document = self
            .model
            .selected
            .as_ref()
            .and_then(|path| self.model.documents.iter().find(|doc| &doc.path == path));
        let mut row = div()
            .debug_selector(|| "document-kicker".into())
            .mb(px(SPACE_MD))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(SPACE_SM))
            .text_size(px(TEXT_META))
            .line_height(px(20.))
            .text_color(color(t.text_muted));
        if let Some(document) = document {
            row = row.child(format!(
                "更新于 {}",
                relative_time(document.modified, std::time::SystemTime::now())
            ));
            if document.archived {
                row = row.child(
                    div()
                        .px(px(SPACE_SM))
                        .rounded(px(RADIUS_MD))
                        .bg(color(t.surface_sunken))
                        .child("已归档"),
                );
            }
            for tag in document.tags.iter() {
                row = row.child(self.tag_pill(tag));
            }
        }
        row
    }

    fn document_aside(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let base = self
            .outline
            .iter()
            .map(|heading| heading.level)
            .min()
            .unwrap_or(1);
        div()
            .debug_selector(|| "document-outline".into())
            .pt(px(SPACE_3XL + SPACE_XS))
            .flex()
            .flex_col()
            .text_size(px(TEXT_META))
            .line_height(px(18.))
            .when(!self.outline.is_empty(), |aside| {
                aside
                    .child(self.aside_label("大纲"))
                    .children(self.outline.iter().enumerate().map(|(index, heading)| {
                        let hover = color(t.text_primary);
                        div()
                            .id(("outline-heading", index))
                            .when(index == 0, |row| {
                                row.debug_selector(|| "first-outline-heading".into())
                            })
                            .py(px(5.))
                            .pl(px(SPACE_MD + f32::from(heading.level - base) * SPACE_MD))
                            .pr(px(SPACE_XS))
                            .border_l_2()
                            .border_color(color(t.ui_border_subtle))
                            .text_color(color(t.text_muted))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .hover(move |row| {
                                row.text_color(hover).border_color(color(t.ui_action))
                            })
                            .child(heading.text.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reveal_heading(index, window, cx)
                            }))
                    }))
            })
            .child(self.document_backlinks(false, cx))
    }

    fn aside_label(&self, text: impl Into<gpui_kit::SharedString>) -> gpui_kit::Div {
        div()
            .mb(px(SPACE_MD))
            .text_size(px(TEXT_LABEL))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(color(self.model.theme.theme.text_muted))
            .child(text.into())
    }

    /// Notes linking here, as cards. `inline` places them under the column
    /// when the outline has no room beside it.
    fn document_backlinks(&self, inline: bool, cx: &mut Context<Self>) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let links = self
            .model
            .selected
            .as_deref()
            .and_then(|path| self.model.backlinks(path))
            .unwrap_or_default();
        let now = std::time::SystemTime::now();
        let section = div()
            .debug_selector(|| "document-backlinks".into())
            .when(inline, |section| {
                section
                    .mt(px(SPACE_2XL))
                    .pt(px(SPACE_XL))
                    .border_t_1()
                    .border_color(color(t.ui_border_subtle))
            })
            .when(!inline, |section| section.mt(px(SPACE_2XL)));
        if links.is_empty() {
            return section;
        }
        section
            .child(self.aside_label(format!("反向链接 · {}", links.len())))
            .children(links.into_iter().enumerate().map(|(index, path)| {
                let document = self.model.documents.iter().find(|doc| doc.path == path);
                let title = document.map(|doc| doc.title.clone()).unwrap_or_else(|| {
                    std::path::Path::new(&path)
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone())
                });
                let detail = match document {
                    Some(doc) if doc.archived => "归档".to_owned(),
                    Some(doc) => format!("笔记 · {}", relative_time(doc.modified, now)),
                    None => "日常记录".to_owned(),
                };
                let hover = color(t.ui_border);
                let open = path.clone();
                div()
                    .id(("backlink", index))
                    .mt(px(SPACE_SM))
                    .px(px(SPACE_MD))
                    .py(px(SPACE_SM + 2.))
                    .rounded(px(RADIUS_MD))
                    .border_1()
                    .border_color(color(t.ui_border_subtle))
                    .bg(color(t.surface_raised))
                    .cursor_pointer()
                    .hover(move |card| card.border_color(hover))
                    .child(
                        div()
                            .text_size(px(TEXT_META))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(color(t.text_primary))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_LABEL))
                            .text_color(color(t.text_muted))
                            .child(detail),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate(Some(open.clone()), window, cx)
                    }))
            }))
            .when(inline, |section| section.mb(px(SPACE_LG)))
    }

    /// Bring outline entry `index` into view: scroll the reading view to the
    /// heading, or put the editing caret on its line.
    pub(super) fn reveal_heading(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(heading) = self.outline.get(index).cloned() else {
            return;
        };
        if self.preview {
            let headings = self.outline.clone();
            self.document_text.update(cx, |text, cx| {
                let rendered = text.rendered_text();
                let rendered = rendered.as_str();
                let mut from = 0;
                for (position, candidate) in headings.iter().enumerate() {
                    let Some(found) = rendered[from..].find(candidate.text.as_ref()) else {
                        continue;
                    };
                    let start = from + found;
                    from = start + candidate.text.len();
                    if position == index {
                        let _ = text.reveal_range(start..start, cx);
                        break;
                    }
                }
            });
        } else {
            self.editor.update(cx, |editor, cx| {
                editor.set_cursor_position(
                    gpui_kit::component::input::Position::new(heading.line as u32, 0),
                    window,
                    cx,
                );
                editor.focus(window, cx);
            });
        }
        cx.notify();
    }
}

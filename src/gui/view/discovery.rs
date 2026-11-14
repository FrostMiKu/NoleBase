use super::*;
use crate::gui::presentation::{
    RADIUS_MD, RADIUS_PILL, SPACE_MD, SPACE_SM, SPACE_XS, TEXT_META, TEXT_NAV, TEXT_UI,
};

/// The note title and the collection a workspace path belongs to.
fn describe_path(path: &str) -> (String, &'static str) {
    let title = std::path::Path::new(path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned());
    let collection = if path.starts_with("daily/") {
        "日常记录"
    } else if path.starts_with("archives/") {
        "归档"
    } else {
        "笔记"
    };
    (title, collection)
}

impl DesktopView {
    fn open_search_result(
        &mut self,
        path: &str,
        line: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Some(path.to_owned()), window, cx);
        if self.status.is_empty() && self.model.selected.as_deref() == Some(path) {
            self.preview = false;
            self.editor.update(cx, |editor, cx| {
                editor.set_cursor_position(
                    gpui_kit::component::input::Position::new(line.saturating_sub(1) as u32, 0),
                    window,
                    cx,
                )
            });
        }
        cx.notify();
    }

    /// A clickable row with a leading glyph, a title, detail text and an optional trailing count.
    fn discovery_row(
        &self,
        id: impl Into<gpui_kit::ElementId>,
        glyph: &'static str,
        title: String,
        detail: Option<String>,
        trailing: Option<String>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let t = self.model.theme.theme;
        let hover = self.row_hover();
        div()
            .id(id)
            .size_full()
            .px(px(SPACE_MD))
            .flex()
            .items_center()
            .gap(px(SPACE_MD))
            .rounded(px(RADIUS_MD))
            .cursor_pointer()
            .hover(move |row| row.bg(hover))
            .child(
                div()
                    .size(px(32.))
                    .flex_shrink_0()
                    .rounded(px(RADIUS_MD))
                    .bg(color(t.surface_sunken))
                    .text_color(color(t.ui_action))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(glyph).size(px(16.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(TEXT_UI))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(color(t.text_primary))
                            .child(title),
                    )
                    .when_some(detail, |column, detail| {
                        column.child(
                            div()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(TEXT_NAV))
                                .text_color(color(t.text_secondary))
                                .child(detail),
                        )
                    }),
            )
            .when_some(trailing, |row, trailing| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .px(px(SPACE_SM + 2.))
                        .py(px(1.))
                        .rounded(px(RADIUS_PILL))
                        .bg(color(t.surface_sunken))
                        .text_size(px(TEXT_META))
                        .text_color(color(t.text_muted))
                        .child(trailing),
                )
            })
    }

    pub(super) fn search_surface(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let t = self.model.theme.theme;
        let count = self.model.search_results.len();
        let view = cx.entity().downgrade();
        let results = gpui_kit::uniform_list("search-results", count, move |range, _, cx| {
            view.update(cx, |view, cx| {
                range
                    .map(|index| {
                        let result = &view.model.search_results[index];
                        let path = result.path.clone();
                        let line = result.line_no;
                        let (title, collection) = describe_path(&path);
                        div().w_full().h(px(68.)).pb(px(SPACE_XS)).child(
                            view.discovery_row(
                                ("search-result", index),
                                if collection == "日常记录" {
                                    "nole/daily"
                                } else {
                                    "nole/note"
                                },
                                title,
                                Some(result.text.trim().to_owned()),
                                Some(format!("{collection} · 第 {line} 行")),
                            )
                            .when(index == 0, |row| {
                                row.debug_selector(|| "first-search-result".into())
                            })
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_search_result(&path, line, window, cx)
                                },
                            )),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .track_scroll(&self.results_scroll)
        .size_full();
        let summary = if self.model.query.is_empty() {
            "搜索笔记、日常记录与归档的全文，输入 #标签 只查看该标签".to_owned()
        } else if self.search_pending {
            "正在搜索…".to_owned()
        } else {
            format!("找到 {count} 条匹配")
        };
        div()
            .id("search-surface")
            .debug_selector(|| "search-surface".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                Input::new(&self.search)
                    .prefix(icon("nole/search").size(px(16.)))
                    .large()
                    .aria_label("搜索知识库"),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .mt(px(SPACE_MD))
                    .text_size(px(TEXT_META))
                    .text_color(color(t.text_muted))
                    .child(summary),
            )
            .when(
                !self.model.query.is_empty() && !self.search_pending && count == 0,
                |surface| {
                    surface.child(self.empty_state(
                        "search-empty",
                        "nole/search",
                        "没有找到匹配的内容",
                        "换个关键词试试，或检查拼写。",
                    ))
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .pt(px(LIST_TOP))
                    .relative()
                    .child(results)
                    .child(self.main_scrollbar(&self.results_scroll, LIST_TOP)),
            )
    }

    pub(super) fn tags_surface(&self, cx: &mut Context<Self>) -> gpui_kit::Stateful<gpui_kit::Div> {
        let view = cx.entity().downgrade();
        let count = if self.active_tag.is_some() {
            self.model.tag_documents.len()
        } else {
            self.model.tags.len()
        };
        let rows = gpui_kit::uniform_list("tags-content", count, move |range, _, cx| {
            view.update(cx, |view, cx| {
                range
                    .map(|index| {
                        let slot = div().w_full().h(px(56.)).pb(px(SPACE_XS));
                        if view.active_tag.is_some() {
                            let path = view.model.tag_documents[index].clone();
                            let (title, collection) = describe_path(&path);
                            slot.child(
                                view.discovery_row(
                                    ("tag-document", index),
                                    "nole/note",
                                    title,
                                    None,
                                    Some(collection.to_owned()),
                                )
                                .when(index == 0, |row| {
                                    row.debug_selector(|| "first-tag-document".into())
                                })
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.navigate(Some(path.clone()), window, cx)
                                    },
                                )),
                            )
                        } else {
                            let tag = &view.model.tags[index];
                            let name = tag.name.clone();
                            slot.child(
                                view.discovery_row(
                                    gpui_kit::SharedString::from(format!("tag-{name}")),
                                    "nole/tag",
                                    format!("#{}", tag.name),
                                    None,
                                    Some(format!("{} 篇", tag.documents)),
                                )
                                .when(index == 0, |row| row.debug_selector(|| "first-tag".into()))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.model.select_tag(&name);
                                        this.active_tag = Some(name.clone());
                                        cx.notify();
                                    },
                                )),
                            )
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .track_scroll(&self.tags_scroll)
        .size_full();
        div()
            .id("tags-surface")
            .debug_selector(|| "tags-surface".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(self.active_tag.is_some(), |body| {
                body.child(
                    div().flex_shrink_0().mb(px(SPACE_SM)).child(
                        self.button("back-tags", "全部标签")
                            .icon(icon("nole/tag").size(px(14.)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_tag = None;
                                cx.notify();
                            })),
                    ),
                )
            })
            .when(count == 0, |body| {
                body.child(self.empty_state(
                    "tags-empty",
                    "nole/tag",
                    "还没有标签",
                    "在笔记或日常记录中写下 #标签，相关内容会聚合到这里。",
                ))
            })
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .pt(px(LIST_TOP))
                    .child(rows)
                    .child(self.main_scrollbar(&self.tags_scroll, LIST_TOP)),
            )
    }
}

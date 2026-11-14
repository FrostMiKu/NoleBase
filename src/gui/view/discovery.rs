use super::*;

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
                        div().w_full().h(px(88.)).pb(px(8.)).child(
                            div()
                                .id(("search-result", index))
                                .when(index == 0, |row| {
                                    row.debug_selector(|| "first-search-result".into())
                                })
                                .p(px(14.))
                                .h(px(80.))
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .rounded(px(8.))
                                .cursor_pointer()
                                .hover(move |row| row.bg(color(t.surface_panel)))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .text_size(px(12.))
                                        .text_color(color(t.text_muted))
                                        .child(icon("nole/note").size(px(14.)))
                                        .child(format!(
                                            "{} · 第 {} 行",
                                            result.path, result.line_no
                                        )),
                                )
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(result.text.clone()),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_search_result(&path, line, window, cx)
                                })),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .track_scroll(&self.results_scroll)
        .size_full();
        div()
            .id("search-surface")
            .debug_selector(|| "search-surface".into())
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .mb(px(12.))
                    .text_size(px(12.))
                    .text_color(color(t.text_muted))
                    .child(if self.model.query.is_empty() {
                        "搜索笔记、日常记录与归档的全文".to_owned()
                    } else {
                        format!("找到 {count} 条匹配")
                    }),
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
        let t = self.model.theme.theme;
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
                        let row = div().w_full().h(px(48.)).pb(px(2.));
                        if view.active_tag.is_some() {
                            let path = view.model.tag_documents[index].clone();
                            row.child(
                                div()
                                    .id(("tag-document", index))
                                    .when(index == 0, |row| {
                                        row.debug_selector(|| "first-tag-document".into())
                                    })
                                    .w_full()
                                    .h(px(46.))
                                    .px(px(12.))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .rounded(px(5.))
                                    .cursor_pointer()
                                    .hover(move |row| row.bg(color(t.surface_panel)))
                                    .child(icon("nole/note").size(px(16.)))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(path.clone()),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.navigate(Some(path.clone()), window, cx)
                                    })),
                            )
                        } else {
                            let tag = &view.model.tags[index];
                            let name = tag.name.clone();
                            row.child(
                                div()
                                    .id(gpui_kit::SharedString::from(format!("tag-{name}")))
                                    .when(index == 0, |row| {
                                        row.debug_selector(|| "first-tag".into())
                                    })
                                    .w_full()
                                    .h(px(46.))
                                    .px(px(12.))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .rounded(px(5.))
                                    .cursor_pointer()
                                    .hover(move |row| row.bg(color(t.surface_panel)))
                                    .child(icon("nole/tag").size(px(16.)))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(format!("# {}", tag.name)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(color(t.text_muted))
                                            .child(format!("{} 篇笔记", tag.documents)),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.model.select_tag(&name);
                                        this.active_tag = Some(name.clone());
                                        cx.notify();
                                    })),
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
                body.child(self.button("back-tags", "← Tags").justify_start().on_click(
                    cx.listener(|this, _, _, cx| {
                        this.active_tag = None;
                        cx.notify();
                    }),
                ))
            })
            .when(count == 0, |body| {
                body.child(
                    div()
                        .py(px(16.))
                        .text_size(px(13.))
                        .text_color(color(t.text_muted))
                        .child("在笔记中写下 #标签，即可在这里聚合相关内容。"),
                )
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

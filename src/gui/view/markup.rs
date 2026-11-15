//! Bind the shared Markdown semantics to desktop navigation and media.
use super::*;
use crate::core::{attachment::AttachmentUri, markup::Event, model::LinkTarget};
use gpui_kit::base::TextView;

pub(super) fn render_source(source: &str) -> String {
    let source = crate::core::markup::normalize_math_delimiters(source);
    let mut output = source.to_string();
    for item in crate::core::markup::parse(&source).into_iter().rev() {
        let replacement = match &item.event {
            Event::WikiLink(target) => Some(format!(
                "[{}](<{}>)",
                crate::core::attachment::escape_markdown_label(&crate::core::markup::visible_text(
                    std::slice::from_ref(&item)
                )),
                semantic_url("wiki", target)
            )),
            Event::Hashtag(tag) => Some(format!(
                "[\\#{}](<{}>)",
                crate::core::attachment::escape_markdown_label(&tag),
                semantic_url("tag", &tag)
            )),
            Event::Embed(target) => Some(format!(
                "![{}](<{}>)",
                crate::core::attachment::escape_markdown_label(&target),
                semantic_url("embed", &target)
            )),
            _ => None,
        };
        if let Some(replacement) = replacement {
            output.replace_range(item.span, &replacement);
        }
    }
    output
}

fn semantic_url(kind: &str, target: &str) -> String {
    let mut url = reqwest::Url::parse(&format!("nole://{kind}")).expect("semantic URL");
    url.query_pairs_mut().append_pair("target", target);
    url.into()
}

fn semantic_target(url: &str) -> Option<(String, String)> {
    let url = reqwest::Url::parse(url).ok()?;
    if url.scheme() != "nole" {
        return None;
    }
    let kind = url.host_str()?;
    if !matches!(kind, "wiki" | "tag" | "embed") {
        return None;
    }
    Some((
        kind.into(),
        url.query_pairs()
            .find(|(key, _)| key == "target")?
            .1
            .into_owned(),
    ))
}

pub(super) fn image_source(
    workspace: &crate::core::workspace::Workspace,
    uri: &gpui_kit::SharedUri,
) -> gpui_kit::ImageSource {
    let target = semantic_target(uri.as_ref())
        .filter(|(kind, _)| kind == "embed")
        .map(|(_, target)| target);
    let raw = target.as_deref().unwrap_or(uri.as_ref());
    if let Ok(data) = data_url::DataUrl::process(raw) {
        let mime = data.mime_type();
        if let Some(format) =
            gpui_kit::ImageFormat::from_mime_type(&format!("{}/{}", mime.type_, mime.subtype))
        {
            if let Ok((bytes, _)) = data.decode_to_vec() {
                return std::sync::Arc::new(gpui_kit::Image::from_bytes(format, bytes)).into();
            }
        }
    }
    let path = match if target.is_some() {
        crate::core::markup::embed_link_target(raw)
    } else {
        crate::core::markup::link_target(raw)
    } {
        LinkTarget::Attachment(raw) => {
            AttachmentUri::parse(&raw).and_then(|uri| workspace.open_attachment(uri.id()))
        }
        LinkTarget::LocalFile(raw) => workspace.storage().validate_local_file(&raw),
        _ => return uri.clone().into(),
    };
    match path {
        Ok(path) => path.into(),
        Err(error) => {
            let error = gpui_kit::ImageCacheError::Other(std::sync::Arc::new(error));
            gpui_kit::ImageSource::Custom(std::sync::Arc::new(move |_, _| Some(Err(error.clone()))))
        }
    }
}

impl DesktopView {
    pub(super) fn markdown_view(&self, text: TextView, cx: &Context<Self>) -> TextView {
        let view = cx.entity().downgrade();
        let workspace = self.model.workspace.clone();
        text.markdown_extensions(crate::gui::markdown_widgets::extensions(
            self.model.theme.theme,
        ))
        .image_source(move |uri| image_source(&workspace, uri))
        .on_link_click(move |url, _, window, cx| {
            let _ = view.update(cx, |view, cx| view.activate_markdown_link(url, window, cx));
        })
    }

    pub(super) fn markdown(
        &self,
        id: impl Into<gpui_kit::SharedString>,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let text = self.markdown_view(TextView::markdown(id.into(), render_source(source)), cx);
        crate::gui::rich_text::render(text, false, self.model.theme.theme, window, cx)
    }

    pub(super) fn activate_markdown_link(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((kind, target)) = semantic_target(url) {
            match kind.as_str() {
                "wiki" => {
                    let paths = self
                        .model
                        .workspace
                        .wiki_links()
                        .with_index(|index| index.resolve(&target));
                    let paths = match paths.map(Ok).unwrap_or_else(|| {
                        crate::core::document_index::DocumentIndex::build_checked(
                            self.model.workspace.storage(),
                        )
                        .map(|documents| {
                            crate::core::wiki_link_index::WikiLinkIndex::from_documents(&documents)
                                .resolve(&target)
                        })
                    }) {
                        Ok(paths) => paths,
                        Err(error) => {
                            self.status = format!("读取笔记遇到问题：{error}");
                            cx.notify();
                            return;
                        }
                    };
                    if paths.len() > 1 {
                        self.search
                            .update(cx, |input, cx| input.set_value(target, window, cx));
                        self.queue_search(window, cx);
                    } else {
                        let result = paths
                            .into_iter()
                            .next()
                            .map(Ok)
                            .unwrap_or_else(|| self.model.workspace.create_note(&target));
                        match result.and_then(|path| {
                            self.model
                                .workspace
                                .relative_path(&path)
                                .ok_or_else(|| anyhow::anyhow!("笔记路径超出了知识库范围"))
                        }) {
                            Ok(path) => {
                                let _ = self.model.refresh();
                                self.navigate(Some(path), window, cx);
                            }
                            Err(error) => self.status = format!("打开笔记遇到问题：{error}"),
                        }
                    }
                }
                "tag" => {
                    self.open_page(Page::Tags, window, cx);
                    if self.page == Page::Tags {
                        self.model.select_tag(&target);
                        self.active_tag = Some(target);
                    }
                }
                _ => {}
            }
        } else {
            let path = match crate::core::markup::link_target(url) {
                LinkTarget::Attachment(raw) => Some(
                    AttachmentUri::parse(&raw)
                        .and_then(|uri| self.model.workspace.open_attachment(uri.id())),
                ),
                LinkTarget::LocalFile(raw) => {
                    Some(self.model.workspace.storage().validate_local_file(&raw))
                }
                _ => None,
            };
            if let Some(path) = path {
                match path.and_then(|path| {
                    reqwest::Url::from_file_path(path).map_err(|_| anyhow::anyhow!("文件路径无效"))
                }) {
                    Ok(url) => cx.open_url(url.as_str()),
                    Err(error) => self.status = format!("打开附件遇到问题：{error}"),
                }
            } else {
                cx.open_url(url);
            }
        }
        cx.notify();
    }
}

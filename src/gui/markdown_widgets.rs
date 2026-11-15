//! Native Markdown code headers, diagrams and typeset formulas.
mod media;
use super::{icons::icon, presentation::*, theme::color};
use crate::core::theme::Theme;
use gpui_kit::base::{
    markdown_ast::Node, InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode,
    MarkdownParseContext, MarkdownPlugin, SelectableText, TextView,
};
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    ActiveTheme, Sizable,
};
use gpui_kit::{div, prelude::*, px, App, ClipboardItem, IntoElement, SharedString, Window};
use std::hash::{Hash, Hasher};

pub(super) fn extensions(tokens: Theme) -> MarkdownExtensions {
    let mut revision = std::collections::hash_map::DefaultHasher::new();
    for value in [
        tokens.surface_canvas,
        tokens.surface_raised,
        tokens.surface_sunken,
        tokens.text_primary,
        tokens.text_secondary,
        tokens.text_muted,
        tokens.ui_border,
        tokens.ui_border_subtle,
    ] {
        media::hex(value).hash(&mut revision);
    }
    MarkdownExtensions::default()
        .parser_revision(revision.finish())
        .plugin(Code { tokens })
        .plugin(Formula {
            tokens,
            display: true,
        })
        .plugin(Formula {
            tokens,
            display: false,
        })
}

struct Code {
    tokens: Theme,
}
struct CodeData {
    language: String,
    code: SharedString,
    markdown: SharedString,
    diagram: Option<Result<media::Media, String>>,
}
impl MarkdownPlugin for Code {
    fn is_block(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "nole-code"
    }
    fn parse(&self, node: &Node, context: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Code(code) = node else {
            return None;
        };
        let language = code.lang.clone().unwrap_or_default();
        let markdown: SharedString = context.node_source(node)?.to_owned().into();
        let diagram = (language == "mermaid").then(|| media::diagram(&code.value, self.tokens));
        Some(
            MarkdownNode::new(
                self.name(),
                CodeData {
                    language,
                    code: code.value.clone().into(),
                    markdown: markdown.clone(),
                    diagram,
                },
            )
            .text(code.value.clone())
            .markdown(markdown),
        )
    }
    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let code = node.data::<CodeData>().expect("code node");
        let tokens = self.tokens;
        let source = code.code.clone();
        let sans = cx.theme().font_family.clone();
        let body = match &code.diagram {
            Some(Ok(media)) => div()
                .debug_selector(|| "mermaid-diagram".into())
                .w_full()
                .p(px(SPACE_MD))
                .child(
                    gpui_kit::img(media.image.clone())
                        .w(px(media.width))
                        .max_w_full()
                        .h_auto(),
                )
                .into_any_element(),
            Some(Err(error)) => render_error(error, &code.code, tokens).into_any_element(),
            None => {
                let fonts = fonts(cx);
                let style = markdown_style(tokens, &fonts, f32::from(window.rem_size()))
                    .with_code_block(
                        code_block_style(tokens, &fonts)
                            .border_0()
                            .rounded_none()
                            .bg(gpui_kit::transparent_black())
                            .pt_0(),
                    );
                TextView::markdown("code-content", code.markdown.clone())
                    .style(style)
                    .selectable(true)
                    .w_full()
                    .min_w_0()
                    .code_block_actions(|_, _, _| div())
                    .into_any_element()
            }
        };
        div()
            .debug_selector(|| "markdown-code-block".into())
            .w_full()
            .min_w_0()
            .rounded(px(RADIUS_LG))
            .overflow_hidden()
            .border_1()
            .border_color(color(tokens.ui_border_subtle))
            .bg(color(tokens.markdown_code_block_background))
            .child(
                div()
                    .debug_selector(|| "code-header".into())
                    .w_full()
                    .px(px(SPACE_LG))
                    .py(px(SPACE_SM))
                    .flex()
                    .items_center()
                    .justify_between()
                    .font_family(sans)
                    .text_size(px(TEXT_META))
                    .text_color(color(tokens.text_muted))
                    .child(
                        div()
                            .debug_selector(|| "code-language".into())
                            .child(code.language.clone()),
                    )
                    .child(
                        Button::new("copy-code")
                            .ghost()
                            .small()
                            .accessibility_label("复制代码")
                            .debug_selector(|| "copy-code".into())
                            .icon(icon("icons/copy.svg").size(px(14.)))
                            .tooltip("复制代码")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(source.to_string()))
                            }),
                    ),
            )
            .child(body)
    }
}

struct Formula {
    tokens: Theme,
    display: bool,
}
impl MarkdownPlugin for Formula {
    fn is_block(&self) -> bool {
        self.display
    }
    fn name(&self) -> &str {
        if self.display {
            "nole-display-math"
        } else {
            "nole-inline-math"
        }
    }
    fn parse(&self, node: &Node, context: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let value = match (self.display, node) {
            (true, Node::Math(math)) => &math.value,
            (false, Node::InlineMath(math)) => &math.value,
            _ => return None,
        };
        if !self.display && value.trim() != value {
            return None;
        }
        Some(
            MarkdownNode::new(
                self.name(),
                media::formula(value, self.display, self.tokens),
            )
            .text(value.clone())
            .markdown(context.node_source(node)?.to_owned()),
        )
    }
    fn render(&self, node: &MarkdownNode, _: &mut Window, _: &mut App) -> impl IntoElement {
        match node
            .data::<Result<media::Media, String>>()
            .expect("formula node")
        {
            Ok(media) => div()
                .debug_selector(|| "display-math".into())
                .w_full()
                .flex()
                .justify_center()
                .py(px(SPACE_SM))
                .child(
                    gpui_kit::img(media.image.clone())
                        .w(px(media.width))
                        .max_w_full()
                        .h_auto(),
                )
                .into_any_element(),
            Err(error) => render_error(error, node.as_text(), self.tokens).into_any_element(),
        }
    }
    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        _: &mut App,
    ) -> Option<InlineElement> {
        match node
            .data::<Result<media::Media, String>>()
            .expect("formula node")
        {
            Ok(media) => {
                let scale = f32::from(context.font_size()) / 16.;
                Some(
                    InlineElement::new(
                        div()
                            .debug_selector(|| "inline-math".into())
                            .w(px(media.width * scale))
                            .h(px(media.height * scale))
                            .child(gpui_kit::img(media.image.clone()).size_full()),
                    )
                    .with_baseline(px(media.baseline * scale)),
                )
            }
            Err(_) => None,
        }
    }
}

fn render_error(error: &str, source: &str, tokens: Theme) -> gpui_kit::Div {
    div()
        .debug_selector(|| "markdown-render-error".into())
        .p(px(SPACE_MD))
        .text_size(px(TEXT_META))
        .child(
            div()
                .text_color(color(tokens.ui_error))
                .child(format!("暂时无法显示：{error}")),
        )
        .child(
            div()
                .mt(px(SPACE_SM))
                .text_color(color(tokens.text_muted))
                .child(SelectableText::new("render-source", source.to_owned())),
        )
}

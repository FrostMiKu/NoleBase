use super::*;
use crate::core::agent::{ApprovalDecision, ApprovalKind, ApprovalRequest};
use crate::gui::presentation::{
    RADIUS_LG, RADIUS_MD, SPACE_LG, SPACE_MD, SPACE_SM, SPACE_XS, TEXT_LABEL, TEXT_META, TEXT_NAV,
};
use gpui_kit::base::SelectableText;

impl DesktopView {
    pub(super) fn agent_approval(
        &self,
        approval: &ApprovalRequest,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let destructive = matches!(approval.kind, ApprovalKind::DestructiveConfirm);
        let accent = color(if destructive {
            t.ui_error
        } else {
            t.ui_warning
        });
        let mono = crate::gui::presentation::fonts(cx).mono;
        let mut details = div()
            .id("approval-details")
            .debug_selector(|| "approval-details".into())
            .min_h_0()
            .max_h(px(136.))
            .overflow_y_scroll()
            .text_size(px(TEXT_NAV))
            .text_color(color(t.text_primary));
        match &approval.kind {
            ApprovalKind::Command(command) => {
                details = details
                    .child(div().mb(px(SPACE_SM + 2.)).child(SelectableText::new(
                        "approval-purpose",
                        command.purpose.clone(),
                    )))
                    .when(!approval.message.is_empty(), |details| {
                        details.child(SelectableText::new(
                            "approval-message",
                            approval.message.clone(),
                        ))
                    })
                    .child(
                        div()
                            .rounded(px(RADIUS_MD))
                            .border_1()
                            .border_color(color(t.ui_border_subtle))
                            .bg(color(t.markdown_code_block_background))
                            .p(px(SPACE_SM + 2.))
                            .child(
                                div()
                                    .mb(px(SPACE_XS))
                                    .text_size(px(TEXT_LABEL))
                                    .text_color(color(t.text_muted))
                                    .child(command.label.clone()),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "approval-command".into())
                                    .font_family(mono.clone())
                                    .text_color(color(t.markdown_code_block_text))
                                    .child(SelectableText::new(
                                        "approval-code",
                                        command.code.clone(),
                                    )),
                            ),
                    );
            }
            _ => {
                details = details
                    .when(matches!(approval.kind, ApprovalKind::Diff), |details| {
                        details.font_family(mono.clone())
                    })
                    .child(SelectableText::new(
                        "approval-message",
                        approval.message.clone(),
                    ));
            }
        }
        div()
            .debug_selector(|| "agent-approval".into())
            .w_full()
            .rounded(px(RADIUS_LG))
            .border_1()
            .border_color(accent.opacity(0.45))
            .bg(color(t.surface_raised))
            .shadow_sm()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(SPACE_LG))
                    .py(px(SPACE_SM + 2.))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_SM))
                    .bg(accent.opacity(0.08))
                    .border_b_1()
                    .border_color(accent.opacity(0.25))
                    .text_size(px(TEXT_NAV))
                    .child(
                        div()
                            .text_color(accent)
                            .child(icon("nole/alert").size(px(15.))),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_META))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(accent)
                            .child(if destructive {
                                "不可撤销的操作"
                            } else {
                                "等待批准"
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(approval.title.clone()),
                    ),
            )
            .child(div().px(px(SPACE_LG)).py(px(SPACE_MD)).child(details))
            .child(
                div()
                    .px(px(SPACE_LG))
                    .pb(px(SPACE_MD))
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_SM))
                    .child(self.button("deny-agent", "拒绝").on_click(cx.listener(
                        |this, _, _, cx| this.agent_decision(ApprovalDecision::Deny, cx),
                    )))
                    .child(
                        self.primary_button("approve-agent", "允许")
                            .when(destructive, |button| button.danger())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.agent_decision(ApprovalDecision::Approve, cx)
                            })),
                    ),
            )
    }
}

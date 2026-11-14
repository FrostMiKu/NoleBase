use super::*;
use crate::core::agent::{ApprovalDecision, ApprovalKind, ApprovalRequest};
use gpui_kit::base::SelectableText;

impl DesktopView {
    pub(super) fn agent_approval(
        &self,
        approval: &ApprovalRequest,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let t = self.model.theme.theme;
        let accent = color(
            if matches!(approval.kind, ApprovalKind::DestructiveConfirm) {
                t.ui_error
            } else {
                t.ui_warning
            },
        );
        let mut details = div()
            .id("approval-details")
            .debug_selector(|| "approval-details".into())
            .min_h_0()
            .max_h(px(136.))
            .overflow_y_scroll()
            .text_size(px(13.))
            .text_color(color(t.text_primary));
        match &approval.kind {
            ApprovalKind::Command(command) => {
                details = details
                    .child(div().mb(px(10.)).child(SelectableText::new(
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
                            .rounded(px(6.))
                            .bg(color(t.surface_panel))
                            .p(px(10.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .text_size(px(11.))
                                    .text_color(color(t.text_muted))
                                    .child(command.label.clone()),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "approval-command".into())
                                    .font_family("monospace")
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
                        details.font_family("monospace")
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
            .p(px(14.))
            .rounded(px(10.))
            .border_1()
            .border_color(accent.opacity(0.35))
            .bg(color(t.surface_message_agent))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(13.))
                    .child(div().text_color(accent).child("等待批准"))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(approval.title.clone()),
                    ),
            )
            .child(details)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(self.button("deny-agent", "拒绝").on_click(cx.listener(
                        |this, _, _, cx| this.agent_decision(ApprovalDecision::Deny, cx),
                    )))
                    .child(
                        self.button("approve-agent", "允许")
                            .bg(color(t.ui_action))
                            .text_color(color(t.text_on_accent))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.agent_decision(ApprovalDecision::Approve, cx)
                            })),
                    ),
            )
    }
}

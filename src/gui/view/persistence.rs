use super::*;

#[derive(Clone, Copy)]
pub(super) enum Compose {
    Daily,
    Agent,
}

impl DesktopView {
    pub(super) fn queue_autosave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.model.selected.clone() else {
            return;
        };
        self.save_pending = true;
        self.status.clear();
        self.autosave_task = Some(cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let _ = cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    if view.model.selected.as_ref() == Some(&path) {
                        use gpui_kit::EntityInputHandler;
                        let composing = view.editor.update(cx, |input, cx| {
                            input.marked_text_range(window, cx).is_some()
                        });
                        if !composing {
                            view.save(window, cx);
                        }
                    }
                })
            });
        }));
        cx.notify();
    }

    pub(super) fn compose_input(
        &self,
        kind: Compose,
        height: f32,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let (id, input) = match kind {
            Compose::Daily => ("daily-compose", &self.editor),
            Compose::Agent => ("agent-compose", &self.agent_input),
        };
        div()
            .id(id)
            .w_full()
            .capture_action(cx.listener(
                move |view, action: &gpui_kit::component::input::Enter, window, cx| {
                    if !action.secondary
                        && view.handle_compose_enter(kind, action.shift, window, cx)
                    {
                        cx.stop_propagation();
                    } else {
                        cx.propagate();
                    }
                },
            ))
            .child(Textarea::new(input).appearance(false).h(px(height)))
    }

    fn handle_compose_enter(
        &mut self,
        kind: Compose,
        newline: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use gpui_kit::EntityInputHandler;
        let input = match kind {
            Compose::Daily => self.editor.clone(),
            Compose::Agent => self.agent_input.clone(),
        };
        if input.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            return false;
        }
        if newline {
            input.update(cx, |input, cx| input.replace("\n", window, cx));
        } else {
            match kind {
                Compose::Daily => {
                    self.save(window, cx);
                }
                Compose::Agent => self.send_agent(window, cx),
            }
        }
        true
    }

    pub(super) fn flush_save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.autosave_task.take();
        self.save(window, cx)
    }

    pub(super) fn content_width(&self, window: &Window) -> f32 {
        f32::from(window.viewport_size().width)
            - self.sidebar_motion.value(std::time::Instant::now())
    }
}

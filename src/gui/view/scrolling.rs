use super::*;
use gpui_kit::{base::input::EditorScrollbarContext, component::scroll::ScrollbarHandle};
use std::{cell::RefCell, rc::Rc};

impl DesktopView {
    /// Present the editor's own scroll state outside its clipped text column.
    /// The callback supplies the current frame after caret scrolling; the
    /// sibling overlay keeps the track at the workspace edge.
    pub(super) fn document_editor_scrollbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = Rc::new(RefCell::new(None::<EditorScrollbarContext>));
        let editor_frame = frame.clone();
        self.editor.update(cx, |editor, _| {
            editor.set_scrollbar_renderer(Some(Rc::new(move |context, _, _| {
                *editor_frame.borrow_mut() = Some(context);
                div().into_any_element()
            })));
        });
        gpui_kit::canvas(
            move |bounds, window, cx| {
                let context = frame.borrow().clone()?;
                let mut viewport = context.viewport_bounds();
                viewport.origin.x = bounds.left();
                viewport.size.width = bounds.size.width;
                let mut scrollbar = Scrollbar::vertical(&context)
                    .viewport_bounds(viewport)
                    .into_any_element();
                scrollbar.prepaint_as_root(viewport.origin, viewport.size.into(), window, cx);
                Some(scrollbar)
            },
            |_, scrollbar, window, cx| {
                if let Some(mut scrollbar) = scrollbar {
                    scrollbar.paint(window, cx);
                }
            },
        )
        .absolute()
        .size_full()
    }
}

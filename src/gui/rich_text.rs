//! GPUI Kit owns Markdown layout, selection, table wrapping and streaming motion.
use gpui_kit::component::text::TextView;
use gpui_kit::{div, prelude::*, px, App, SharedString, Window};

pub fn markdown(
    id: impl Into<SharedString>,
    source: SharedString,
    scrollable: bool,
    tokens: crate::core::theme::Theme,
    window: &mut Window,
    _cx: &mut App,
) -> gpui_kit::AnyElement {
    let style = super::presentation::markdown_style(tokens, f32::from(window.rem_size()));
    div()
        .debug_selector(|| "markdown-view".into())
        .w_full()
        .min_w_0()
        .text_size(px(super::presentation::BODY_SIZE))
        .line_height(gpui_kit::relative(
            super::presentation::BODY_LINE / super::presentation::BODY_SIZE,
        ))
        .when(scrollable, |view| view.size_full())
        .child(
            TextView::markdown(id.into(), source)
                .style(style)
                .selectable(true)
                .scrollable(scrollable)
                .stream_fade(!scrollable)
                .w_full()
                .min_w_0()
                .when(scrollable, |text| text.size_full()),
        )
        .into_any_element()
}

#[cfg(all(test, feature = "gui-test"))]
mod tests {
    use super::*;
    use gpui_kit::{size, Context, Render, TestAppContext};

    struct Fixture {
        source: SharedString,
    }
    impl Render for Fixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().w_full().child(markdown(
                "spacing-fixture",
                self.source.clone(),
                false,
                crate::core::theme::Theme::default(),
                window,
                cx,
            ))
        }
    }

    #[gpui_kit::test]
    fn markdown_keeps_paragraph_spacing_and_wraps_at_the_container(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let fixture = cx.new(|_| Fixture { source: "First paragraph.\n\nSecond paragraph.\n\nThird paragraph with enough words to wrap when the container becomes narrow.".into() });
            gpui_kit::component::Root::new(fixture, window, cx)
        });
        cx.simulate_resize(size(px(800.), px(1200.)));
        cx.run_until_parked();
        let wide = cx.debug_bounds("markdown-view").unwrap();
        assert!(wide.size.height >= px(98.));
        cx.simulate_resize(size(px(220.), px(1200.)));
        cx.run_until_parked();
        let narrow = cx.debug_bounds("markdown-view").unwrap();
        assert!(narrow.size.height > wide.size.height);
        assert!(narrow.size.width <= px(220.));
    }
}

#[cfg(all(test, feature = "gui-test"))]
mod table_tests {
    use super::*;
    use gpui_kit::{size, Context, Render, TestAppContext};
    struct TableFixture {
        source: SharedString,
    }
    impl Render for TableFixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            markdown(
                "native-table",
                self.source.clone(),
                false,
                crate::core::theme::Theme::default(),
                window,
                cx,
            )
        }
    }
    #[gpui_kit::test]
    fn native_table_wraps_its_cells_with_the_reading_width(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let source = format!(
            "| Name | Description |\n| --- | --- |\n| Example | {} |\n",
            "A description with words that wrap in the available space. ".repeat(6)
        );
        let (_, cx) = cx.add_window_view(|window, cx| {
            let fixture = cx.new(|_| TableFixture {
                source: source.into(),
            });
            gpui_kit::component::Root::new(fixture, window, cx)
        });
        cx.simulate_resize(size(px(800.), px(1200.)));
        cx.run_until_parked();
        let wide = cx.debug_bounds("markdown-view").unwrap();
        cx.simulate_resize(size(px(320.), px(1200.)));
        cx.run_until_parked();
        let narrow = cx.debug_bounds("markdown-view").unwrap();
        assert!(narrow.size.width <= px(320.));
        assert!(narrow.size.height > wide.size.height);
    }
}

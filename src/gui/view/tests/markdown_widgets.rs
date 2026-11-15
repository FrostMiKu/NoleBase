use super::*;

#[gpui_kit::test]
fn code_header_places_language_left_and_copies_only_the_code(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Code").unwrap();
    model
        .workspace
        .write_document(
            "data/Code.md",
            "```rust\nfn main() {\n    println!(\"你好\");\n}\n```\n",
        )
        .unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Code.md".into()), window, cx)
        })
    });
    cx.run_until_parked();
    let header = cx.debug_bounds("code-header").unwrap();
    let language = cx.debug_bounds("code-language").unwrap();
    let copy = cx.debug_bounds("copy-code").unwrap();
    assert!(language.left() < header.center().x);
    assert!(copy.left() > header.center().x);
    assert!(language.right() < copy.left());
    assert!(copy.bottom() <= header.bottom());
    cx.simulate_click(copy.center(), Default::default());
    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "fn main() {\n    println!(\"你好\");\n}"
        );
        assert!(view.read(cx).preview);
    });
}

#[gpui_kit::test]
fn markdown_renders_mermaid_and_inline_and_display_formulas_across_window_sizes(
    cx: &mut TestAppContext,
) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Figures").unwrap();
    model.workspace.write_document("data/Figures.md", "Inline $\\frac{a^2+b^2}{c}$ formula.\n\n$$\n\\int_0^1 x^2\\,dx = \\frac{1}{3}\n$$\n\n```mermaid\nflowchart LR\n A[开始] --> B[完成]\n```\n").unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Figures.md".into()), window, cx)
        })
    });
    for width in [1360., 820.] {
        cx.simulate_resize(size(px(width), px(1000.)));
        cx.run_until_parked();
        let inline = cx.debug_bounds("inline-math").unwrap();
        assert!(inline.size.height > px(20.));
        let block = cx.debug_bounds("display-math").unwrap();
        assert!(block.size.height > px(30.));
        let diagram = cx.debug_bounds("mermaid-diagram").unwrap();
        assert!(diagram.size.height > px(30.));
        let column = cx.debug_bounds("document-column").unwrap();
        assert!(diagram.right() <= column.right());
        assert!(block.right() <= column.right());
        assert!(cx.debug_bounds("markdown-render-error").is_none());
    }
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.model
                .workspace
                .write_document(
                    "data/Figures.md",
                    r"Inline \(\frac{a}{b}\).

\[\int_0^1 x^2\,dx = \frac{1}{3}\]",
                )
                .unwrap();
            view.navigate(Some("data/Figures.md".into()), window, cx);
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("inline-math").is_some());
    assert!(cx.debug_bounds("display-math").is_some());
}

#[gpui_kit::test]
fn malformed_diagrams_keep_their_source_available_and_copyable(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Invalid").unwrap();
    model
        .workspace
        .write_document("data/Invalid.md", "```mermaid\nnot a diagram\n```\n")
        .unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Invalid.md".into()), window, cx)
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("markdown-render-error").is_some());
    let copy = cx.debug_bounds("copy-code").unwrap().center();
    cx.simulate_click(copy, Default::default());
    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "not a diagram"
        )
    });
}

#[gpui_kit::test]
fn dollar_amounts_stay_as_prose(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("Prices").unwrap();
    model
        .workspace
        .write_document("data/Prices.md", "Price: $5 and $10.")
        .unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Prices.md".into()), window, cx)
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("inline-math").is_none());
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(
            view.document_text
                .read(cx)
                .rendered_text()
                .as_str()
                .trim_end(),
            "Price: $5 and $10."
        );
    });
}

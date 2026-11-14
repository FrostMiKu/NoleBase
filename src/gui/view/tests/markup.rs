use super::*;
use gpui_kit::test::TestWindowExt;

#[test]
fn semantic_markup_preserves_code_and_escaped_syntax() {
    let source = "[[研究 A]] #项目 ![[nole://attachment/00000000-0000-4000-8000-000000000001]]\n\n`[[code]] #code`\n\n```text\n[[fenced]] #fenced\n```\n\n\\[[escaped]]";
    let rendered = super::super::markup::render_source(source);
    assert!(rendered.contains("nole://wiki?target="));
    assert!(rendered.contains("nole://tag?target="));
    assert!(rendered.contains("nole://embed?target="));
    assert!(rendered.contains("`[[code]] #code`"));
    assert!(rendered.contains("```text\n[[fenced]] #fenced\n```"));
    assert!(rendered.contains("\\[[escaped]]"));
}

#[gpui_kit::test]
fn rendered_wiki_links_and_tags_navigate_on_click(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    model.workspace.create_note("研究 A").unwrap();
    model
        .workspace
        .write_document("data/研究 A.md", "# 项目\n\n#research")
        .unwrap();
    model.workspace.create_note("Links").unwrap();
    model
        .workspace
        .write_document("data/Links.md", "[[研究 A]]")
        .unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Links.md".into()), window, cx)
        })
    });
    cx.run_until_parked();
    let body = cx.debug_bounds("markdown-view").unwrap();
    cx.simulate_click(
        point(body.left() + px(15.), body.top() + px(14.)),
        Default::default(),
    );
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert_eq!(view.model.selected.as_deref(), Some("data/研究 A.md"));
            view.model
                .workspace
                .write_document("data/Links.md", "#research")
                .unwrap();
            view.navigate(Some("data/Links.md".into()), window, cx);
        });
    });
    cx.run_until_parked();
    let body = cx.debug_bounds("markdown-view").unwrap();
    cx.simulate_click(
        point(body.left() + px(15.), body.top() + px(14.)),
        Default::default(),
    );
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert!(view.read(cx).page == Page::Tags);
        assert_eq!(view.read(cx).active_tag.as_deref(), Some("research"));
    });
}

#[gpui_kit::test]
fn attachment_images_use_the_managed_file_and_display_at_their_image_height(
    cx: &mut TestAppContext,
) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(240, 160)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let metadata = model
        .workspace
        .attachments()
        .import_bytes(bytes.get_ref(), Some("picture.png"))
        .unwrap();
    let uri = metadata.uri().to_string();
    let source = super::super::markup::image_source(&model.workspace, &uri.clone().into());
    assert!(matches!(
        source,
        gpui_kit::ImageSource::Resource(gpui_kit::Resource::Path(_))
    ));
    model.workspace.create_note("Picture").unwrap();
    model
        .workspace
        .write_document("data/Picture.md", &format!("![picture]({uri})"))
        .unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Picture.md".into()), window, cx)
        })
    });
    cx.run_until_parked();
    let body = cx.debug_bounds("markdown-view").unwrap();
    assert!(body.size.height >= px(160.), "{body:?}");
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.model
                .workspace
                .write_document("data/Picture.md", &format!("![[{uri}]]"))
                .unwrap();
            view.navigate(Some("data/Picture.md".into()), window, cx);
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("markdown-view").unwrap().size.height >= px(160.));
    use base64::Engine;
    let embedded = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.get_ref())
    );
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.model
                .workspace
                .write_document("data/Picture.md", &format!("![picture]({embedded})"))
                .unwrap();
            view.navigate(Some("data/Picture.md".into()), window, cx);
        })
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("markdown-view").unwrap().size.height >= px(160.));
}

#[gpui_kit::test]
fn daily_navigation_and_capture_show_the_latest_content(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let storage = Storage::new(root.path()).unwrap();
    storage.ensure_files().unwrap();
    let today = chrono::Local::now().date_naive();
    for days in (0..20).rev() {
        let date = today - chrono::Duration::days(days);
        let path = storage.daily_file_path(&date.to_string()).unwrap();
        std::fs::write(
            path,
            format!(
                "## {date}\n\n{}",
                "A daily entry with readable text.\n\n".repeat(6)
            ),
        )
        .unwrap();
    }
    let model = Desktop::open(storage).unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| window.click("daily-nav", cx));
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert!(view
                .daily_list
                .bounds_for_item(view.model.daily_blocks.len() - 1)
                .is_some());
            view.daily_list.scroll_to(Default::default());
            cx.notify();
            view.open_page(Page::Home, window, cx);
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.click("daily-nav", cx));
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            assert!(view
                .daily_list
                .bounds_for_item(view.model.daily_blocks.len() - 1)
                .is_some());
            view.editor.update(cx, |input, cx| {
                input.set_value("Newest daily entry", window, cx)
            });
        })
    });
    cx.update(|window, cx| window.click("capture", cx));
    cx.run_until_parked();
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert!(view
            .daily_list
            .bounds_for_item(view.model.daily_blocks.len() - 1)
            .is_some());
    });
}

#[gpui_kit::test]
fn attachment_links_open_the_managed_file_and_report_invalid_uris(cx: &mut TestAppContext) {
    cx.update(crate::gui::init);
    let root = tempfile::tempdir().unwrap();
    let mut model = Desktop::open(Storage::new(root.path()).unwrap()).unwrap();
    let attachment = model
        .workspace
        .attachments()
        .import_bytes(b"hello", Some("notes.txt"))
        .unwrap();
    let path = model.workspace.open_attachment(attachment.id).unwrap();
    model.workspace.create_note("Attachment").unwrap();
    model
        .workspace
        .write_document(
            "data/Attachment.md",
            &format!("[notes]({})", attachment.uri()),
        )
        .unwrap();
    model.refresh().unwrap();
    let (view, cx) = desktop_window(cx, model);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.navigate(Some("data/Attachment.md".into()), window, cx)
        })
    });
    cx.run_until_parked();
    let body = cx.debug_bounds("markdown-view").unwrap();
    cx.simulate_click(
        point(body.left() + px(15.), body.top() + px(14.)),
        Default::default(),
    );
    let opened = cx.opened_url().unwrap();
    assert_eq!(
        reqwest::Url::parse(&opened)
            .unwrap()
            .to_file_path()
            .unwrap(),
        path
    );
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.activate_markdown_link("nole://attachment/invalid", window, cx);
            assert!(view.status.starts_with("打开附件遇到问题"));
        })
    });
    assert_eq!(cx.opened_url().as_deref(), Some(opened.as_str()));
}

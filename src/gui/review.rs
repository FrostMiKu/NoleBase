//! Native pixel capture of the production desktop against an isolated workspace.
use super::{icons, model::Desktop, theme, view::DesktopView};
use crate::core::storage::Storage;
use anyhow::Result;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{prelude::*, px, size, Bounds, HeadlessAppContext, WindowBounds, WindowOptions};
use std::{path::Path, sync::Arc};

pub fn capture(storage: Storage, output: &Path) -> Result<()> {
    let mut model = Desktop::open(storage)?;
    for (name, source) in [
        ("Reading", "# 阅读与写作\n\n为知识留出呼吸的空间。Nole 将每日记录、长篇笔记与 Agent 放在同一个工作区，让阅读与整理保持连贯。\n\n## 清晰的正文层级\n\n正文保持舒适的行宽，**重点内容**与 *补充说明* 形成层次。这里包含一个 [参考链接](https://example.com) 和 `inline code`。\n\n> 记录观察，建立联系，让想法逐渐清晰。\n\n### 下一步\n\n- [x] 统一阅读与编辑布局\n- [ ] 整理今天的笔记\n- [ ] 回顾本周的重要想法\n\n```rust\nfn main() {\n    println!(\"Hello, Nole\");\n}\n```\n\n| 内容 | 状态 | 说明 |\n| :--- | :---: | ---: |\n| 正文排版 | 完成 | 舒适行宽与清晰层级 |\n| 动画 | 完成 | 平滑衔接 |\n"),
        ("Weekend in Lisbon", "# Weekend in Lisbon\n\nTwo slow days, no laptop.\n\n## Sunday\n\n| Time | Where | Notes |\n| --- | --- | --- |\n| 09:00 | Miradouro da Graça | Best light before ten |\n| 11:00 | Tram 28 to Estrela | Avoid the crowded stop |\n| 13:00 | Time Out Market | Share small plates |\n\n> Take the long way home.\n\n## Small discoveries\n\n- A bookshop by the water\n- A quiet café for the afternoon\n- The view from the park\n"),
        ("Design review", "# Design review · week 6\n\n## What changed\n\nThis release rewrote Markdown export, and the numbers held up. Onboarding recovered image captions. The sync layer received a smaller, more consistent interface.\n\n| Metric | Last week | This week |\n| --- | --- | --- |\n| Avg. session | 9,058 | 5,066 |\n| Daily actives | 5,240 | 7,918 |\n\n## Next steps\n\n- [x] Review the reading view\n- [ ] Simplify the next interaction\n"),
        ("Reading notes", "# Reading notes\n\n## The Design of Everyday Things\n\nGood design helps us understand what we can do and what happened after we did it.\n\n> Make the next action clear.\n\n## Big ideas\n\n1. Visibility guides attention.\n2. Feedback closes the loop.\n3. Constraints make choices easier.\n"),
    ] {
        model.workspace.create_note(name)?;
        model.workspace.write_document(&format!("data/{name}.md"), source)?;
    }
    model.refresh()?;
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(icons::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        super::init(cx);
        theme::apply(model.theme.theme, cx);
    });
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Default::default(),
                    size: size(px(1360.), px(900.)),
                })),
                focus: false,
                show: false,
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| DesktopView::new(model, window, cx)),
        )
    })?;
    std::fs::create_dir_all(output)?;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover("new-note", cx);
    })?;
    cx.advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))?;
    let home = cx.capture_screenshot(handle)?;
    assert!(home.width() >= 1360 && home.height() >= 900);
    home.save(output.join("home.png"))?;
    cx.update_window(handle, |_, window, cx| window.click("new-note", cx))?;
    cx.advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))?;
    cx.capture_screenshot(handle)?
        .save(output.join("new-note.png"))?;
    cx.update_window(handle, |_, window, cx| window.press("escape", cx))?;
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.review_document("data/Reading.md".into(), window, cx)
        });
        window.render_frame(cx);
    })?;
    cx.advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))?;
    cx.capture_screenshot(handle)?
        .save(output.join("reading.png"))?;
    Ok(())
}

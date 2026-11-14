//! Lucide icons, embedded locally with their ISC license.
use gpui_kit::component::Icon;
use gpui_kit::{prelude::*, AssetSource, SharedString};
use std::borrow::Cow;

pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "nole/folder" => include_bytes!("../../assets/icons/lucide/folder.svg"),
            "nole/folder-open" => include_bytes!("../../assets/icons/lucide/folder-open.svg"),
            "nole/archive" => include_bytes!("../../assets/icons/lucide/archive.svg"),
            "nole/chevron-right" => include_bytes!("../../assets/icons/lucide/chevron-right.svg"),
            "nole/chevron-down" => include_bytes!("../../assets/icons/lucide/chevron-down.svg"),
            "nole/todo" => include_bytes!("../../assets/icons/lucide/list-todo.svg"),
            "nole/home" => include_bytes!("../../assets/icons/lucide/house.svg"),
            "nole/daily" => include_bytes!("../../assets/icons/lucide/calendar-days.svg"),
            "nole/note" => include_bytes!("../../assets/icons/lucide/file-text.svg"),
            "nole/plus" => include_bytes!("../../assets/icons/lucide/plus.svg"),
            "nole/tag" => include_bytes!("../../assets/icons/lucide/tag.svg"),
            "nole/agent" => include_bytes!("../../assets/icons/lucide/bot.svg"),
            "nole/search" => include_bytes!("../../assets/icons/lucide/search.svg"),
            "nole/sidebar" => include_bytes!("../../assets/icons/lucide/panel-left.svg"),
            "nole/appearance" => include_bytes!("../../assets/icons/lucide/sun-moon.svg"),
            "nole/arrow" => include_bytes!("../../assets/icons/lucide/arrow-up.svg"),
            "nole/edit" => include_bytes!("../../assets/icons/lucide/square-pen.svg"),
            "nole/read" => include_bytes!("../../assets/icons/lucide/book-open.svg"),
            "nole/close" => include_bytes!("../../assets/icons/lucide/x.svg"),
            "nole/check" => include_bytes!("../../assets/icons/lucide/check.svg"),
            "nole/spinner" => include_bytes!("../../assets/icons/lucide/loader-circle.svg"),
            "nole/thinking" => include_bytes!("../../assets/icons/lucide/brain.svg"),
            "nole/tool" => include_bytes!("../../assets/icons/lucide/wrench.svg"),
            "nole/alert" => include_bytes!("../../assets/icons/lucide/circle-alert.svg"),
            _ => return gpui_kit::assets::Assets.load(path),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }
    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}

pub fn icon(name: &'static str) -> Icon {
    // Icon resolves inherited text color before painting the underlying SVG.
    Icon::default()
        .path(name)
        .size(gpui_kit::px(18.))
        .flex_shrink_0()
}

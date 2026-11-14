//! Browser entry point and helpers for the web build.

pub mod api;

use std::borrow::Cow;

use anyhow::Result;
use gpui_kit::*;
use wasm_bindgen::prelude::*;

use crate::{app, model, theme};

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn storage_get(key: &str) -> Option<String> {
    local_storage()?.get_item(key).ok()?
}

pub fn storage_set(key: &str, value: &str) -> Result<()> {
    local_storage()
        .ok_or_else(|| anyhow::anyhow!("browser storage is unavailable"))?
        .set_item(key, value)
        .map_err(|_| anyhow::anyhow!("couldn't save to browser storage (is it full?)"))
}

pub fn storage_remove(key: &str) {
    if let Some(s) = local_storage() {
        let _ = s.remove_item(key);
    }
}

/// Origin (scheme://host) of the server the UI was loaded from.
fn origin() -> String {
    web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default()
}

/// Host of the server the UI was loaded from.
pub fn server_host() -> String {
    web_sys::window().and_then(|w| w.location().host().ok()).unwrap_or_default()
}

const FONTS: [&[u8]; 3] = [
    include_bytes!("../../assets/fonts/NotoSans-Regular.ttf"),
    include_bytes!("../../assets/fonts/NotoSans-Medium.ttf"),
    include_bytes!("../../assets/fonts/NotoSans-SemiBold.ttf"),
];

#[wasm_bindgen(start)]
pub fn start() {
    gpui_kit::platform::web_init();
    // Icons are fetched from the server that served the page (/assets/icons/…).
    gpui_kit::platform::single_threaded_web().with_assets(gpui_kit::assets::Assets::new(origin())).run(|cx| {
        // Browsers expose no system fonts to the canvas renderer: bundle one.
        if let Err(e) = cx.text_system().add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect()) {
            web_sys::console::error_1(&format!("font: {e:#}").into());
        }
        gpui_kit::init(cx);
        theme::init(cx);
        gpui_kit::component::Theme::update(cx, |t| t.font_family = "Noto Sans".into());
        let appearance =
            model::Library::load(std::path::Path::new("/")).map(|l| l.settings.appearance).unwrap_or_default();
        theme::apply(appearance, None, cx);
        gpui_kit::open_window(crate::window_options(cx), cx, |window, cx| {
            cx.new(|cx| app::LocalPlaylist::new(model::Paths::web(), window, cx))
        })
        .expect("failed to open window");
    });
}

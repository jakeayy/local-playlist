//! Material 3–inspired light/dark themes and the System/Light/Dark choice.

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Window};
use serde::{Deserialize, Serialize};

const THEMES: &str = include_str!("../assets/material-theme.json");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// Register the Material themes as the app's light and dark themes.
pub fn init(cx: &mut App) {
    if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(THEMES) {
        eprintln!("failed to load theme: {e:#}");
        return;
    }
    let themes = ThemeRegistry::global(cx).themes().clone();
    let (Some(light), Some(dark)) = (themes.get("Material Light"), themes.get("Material Dark")) else { return };
    let (light, dark) = (light.clone(), dark.clone());
    Theme::update(cx, |t| {
        t.light_theme = light;
        t.dark_theme = dark;
    });
}

/// Apply the user's choice (System follows the window/OS appearance).
pub fn apply(choice: Appearance, window: Option<&mut Window>, cx: &mut App) {
    match choice {
        Appearance::System => Theme::sync_system_appearance(window, cx),
        Appearance::Light => Theme::change(ThemeMode::Light, window, cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, window, cx),
    }
}

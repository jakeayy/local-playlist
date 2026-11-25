//! Material 3–inspired light/dark themes and the System/Light/Dark choice.

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry, ThemeSet};
use gpui_kit::{App, Window};

const THEMES: &str = include_str!("../assets/material-theme.json");

pub use crate::model::Appearance;

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

/// Hue (0..1) of the purple the Material themes are built from.
const SEED_HUE: f32 = 0.72;
/// Colours within this hue distance of the seed belong to the accent palette
/// (primary, selection, sliders, tinted surfaces…); others (status colours,
/// greys) are left alone.
const SEED_RANGE: f32 = 0.09;
/// Typical saturation of a cover's dominant colour, mapped to the theme's own.
pub const REFERENCE_SATURATION: f32 = 0.55;

/// An accent-palette colour turned to an accent's hue (hue, saturation from a
/// cover), as the cover-tinted themes show it.
pub fn tint((r, g, b): (u8, u8, u8), accent: (f32, f32)) -> (u8, u8, u8) {
    let (_, s, l) = crate::cover::rgb_to_hsl(r, g, b);
    tinted(s, l, accent)
}

/// Saturation `s` scaled by how vivid the cover is, lightness `l` kept (so
/// contrast stays as the theme designed it), at the accent's hue.
fn tinted(s: f32, l: f32, (hue, sat): (f32, f32)) -> (u8, u8, u8) {
    crate::cover::hsl_to_rgb(hue, (s * (sat / REFERENCE_SATURATION).clamp(0.45, 1.3)).min(1.0), l)
}

/// Recolour the light/dark themes around an accent (hue, saturation from a
/// cover; `None` = the stock purple), `t` (0..1) of the way from `from` to
/// `to`, then re-apply `choice`. Step `t` over a few frames for a fade.
pub fn set_accent(from: Option<(f32, f32)>, to: Option<(f32, f32)>, t: f32, choice: Appearance, cx: &mut App) {
    let json = if from.is_none() && to.is_none() { THEMES.to_string() } else { recolor(THEMES, from, to, t) };
    let set = match serde_json::from_str::<ThemeSet>(&json) {
        Ok(set) => set,
        Err(e) => return eprintln!("failed to build cover theme: {e:#}"),
    };
    let (mut light, mut dark) = (None, None);
    for t in set.themes {
        if t.mode.is_dark() { dark = Some(t) } else { light = Some(t) }
    }
    let (Some(light), Some(dark)) = (light, dark) else { return };
    Theme::update(cx, |t| {
        t.light_theme = Rc::new(light);
        t.dark_theme = Rc::new(dark);
    });
    apply(choice, None, cx);
}

/// Theme JSON with every accent-palette colour (`#RRGGBB[AA]` near the seed
/// hue) turned to an accent's hue, its saturation scaled by how vivid the cover
/// is; lightness is kept, so contrast stays as the theme designed it. The
/// result is blended in RGB `t` of the way from the `from` to the `to` accent
/// (so a fade doesn't sweep through the hues in between).
fn recolor(json: &str, from: Option<(f32, f32)>, to: Option<(f32, f32)>, t: f32) -> String {
    use crate::cover::rgb_to_hsl;
    let tint = |accent: Option<(f32, f32)>, (_, s, l): (f32, f32, f32), orig: (u8, u8, u8)| match accent {
        Some(accent) => tinted(s, l, accent),
        None => orig,
    };
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    let mut out = String::with_capacity(json.len());
    let mut rest = json;
    while let Some(i) = rest.find("\"#") {
        out.push_str(&rest[..i + 1]);
        rest = &rest[i + 1..];
        let hex_len = rest[1..].bytes().take_while(u8::is_ascii_hexdigit).count();
        let hex = &rest[1..1 + hex_len];
        let parsed = (hex_len == 6 || hex_len == 8).then(|| u32::from_str_radix(&hex[..6], 16).ok()).flatten();
        match parsed {
            Some(rgb) => {
                let (r, g, b) = ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);
                let hsl = rgb_to_hsl(r, g, b);
                let d = (hsl.0 - SEED_HUE).abs();
                if hsl.1 > 0.02 && d.min(1.0 - d) < SEED_RANGE {
                    let (a, b2) = (tint(from, hsl, (r, g, b)), tint(to, hsl, (r, g, b)));
                    let (r, g, b) = (mix(a.0, b2.0), mix(a.1, b2.1), mix(a.2, b2.2));
                    out.push_str(&format!("#{r:02X}{g:02X}{b:02X}{}", &hex[6..]));
                } else {
                    out.push('#');
                    out.push_str(hex);
                }
                rest = &rest[1 + hex_len..];
            }
            None => {
                out.push('#');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{THEMES, recolor};

    #[test]
    fn recolours_only_the_accent_palette() {
        // Teal.
        let json = recolor(THEMES, None, Some((0.49, 0.6)), 1.0);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let dark = v["themes"].as_array().unwrap().iter().find(|t| t["mode"] == "dark").unwrap();
        let c = |k: &str| dark["colors"][k].as_str().unwrap().to_string();
        let hue = |hex: &str| {
            let n = u32::from_str_radix(&hex[1..7], 16).unwrap();
            crate::cover::rgb_to_hsl((n >> 16) as u8, (n >> 8) as u8, n as u8)
        };
        // Primary turned teal, same lightness; alpha suffixes survive.
        let (h, _, l) = hue(&c("primary.background"));
        assert!((h - 0.49).abs() < 0.02 && (l - 0.87).abs() < 0.02, "{h} {l}");
        assert!(c("selection.background").ends_with("59"));
        // Halfway from the stock purple to teal is in between, in RGB.
        let half: serde_json::Value = serde_json::from_str(&recolor(THEMES, None, Some((0.49, 0.6)), 0.5)).unwrap();
        let half_dark = half["themes"].as_array().unwrap().iter().find(|t| t["mode"] == "dark").unwrap();
        let mid = half_dark["colors"]["primary.background"].as_str().unwrap();
        assert!(mid != c("primary.background") && mid != "#D0BCFF", "{mid}");
        // t = 0 from no accent is the stock theme.
        assert_eq!(recolor(THEMES, None, Some((0.49, 0.6)), 0.0), THEMES);
        // Status colours are untouched (neutrals only pick up a faint tint).
        let orig: serde_json::Value = serde_json::from_str(THEMES).unwrap();
        let orig_dark = orig["themes"].as_array().unwrap().iter().find(|t| t["mode"] == "dark").unwrap();
        for k in ["danger.background", "success.background", "warning.background", "info.background"] {
            if let Some(o) = orig_dark["colors"][k].as_str() {
                assert_eq!(dark["colors"][k].as_str().unwrap(), o, "{k}");
            }
        }
    }
}

//! Tray icon for playing in the background (desktop): with it, closing the
//! window leaves the app playing, and the icon brings the window back,
//! controls playback, or quits.
//!
//! * Linux: a StatusNotifierItem over D-Bus (ksni, no GTK), shown by KDE,
//!   most other desktops, and GNOME with the AppIndicator extension.
//! * Windows: a notification-area icon (Shell_NotifyIcon) run by a small
//!   window on its own thread.
//!
//! Where there is no tray, [`Tray::start`] returns `None` and closing the
//! window quits, as without this feature. GPUI can't hide a window, so the
//! window really closes and the tray opens a new one.

use futures::channel::mpsc::UnboundedSender;

#[cfg(target_os = "linux")]
#[path = "tray/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "tray/windows.rs"]
mod imp;
#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub struct Tray;

    impl Tray {
        pub fn start(_: super::UnboundedSender<super::Command>) -> Option<Self> {
            None
        }

        pub fn set_status(&self, _: &super::Status) {}
    }
}

/// What the tray asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// The icon was clicked: show the window, or close it if it's in front.
    ToggleWindow,
    ShowWindow,
    HideWindow,
    PlayPause,
    Next,
    Previous,
    Quit,
    /// The desktop's tray went away: nothing could bring a closed window back.
    TrayGone,
    TrayBack,
}

/// What the tray shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// "Title — Artist" of the current track.
    pub track: Option<String>,
    pub playing: bool,
    pub window_open: bool,
}

pub struct Tray(imp::Tray);

impl Tray {
    /// Put the icon in the tray; `None` if this desktop has no tray.
    /// Clicks and menu choices arrive on `commands`.
    pub fn start(commands: UnboundedSender<Command>) -> Option<Self> {
        imp::Tray::start(commands).map(Self)
    }

    pub fn set_status(&self, status: &Status) {
        self.0.set_status(status);
    }
}

/// Labels of the tray menu, shared by both platforms.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
fn menu_labels(status: &Status) -> [(Command, &'static str); 5] {
    [
        (Command::PlayPause, if status.playing { "Pause" } else { "Play" }),
        (Command::Previous, "Previous"),
        (Command::Next, "Next"),
        if status.window_open {
            (Command::HideWindow, "Close window (keep playing)")
        } else {
            (Command::ShowWindow, "Show Local Playlist")
        },
        (Command::Quit, "Quit"),
    ]
}

/// The app icon (`docs/icon.svg`: a rounded square with the "list music"
/// glyph) drawn at `size`×`size` pixels, as straight-alpha RGBA. Drawn from
/// its shapes, which saves bundling a PNG or an SVG renderer.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    const BG: [f32; 3] = [208.0, 188.0, 255.0]; // #D0BCFF
    const FG: [f32; 3] = [56.0, 30.0, 114.0]; // #381E72
    // The glyph is a 24-unit Lucide icon placed at (28, 28), scaled by 3, on
    // the 128-unit square; strokes are 2 units (6 on the square), round caps.
    let g = |x: f32, y: f32| (28.0 + 3.0 * x, 28.0 + 3.0 * y);
    let lines =
        [(g(3., 5.), g(16., 5.)), (g(3., 12.), g(11., 12.)), (g(3., 19.), g(11., 19.)), (g(21., 5.), g(21., 16.))];
    let (centre, radius) = (g(18., 16.), 9.0);
    let half_stroke = 3.0;

    let scale = size as f32 / 128.0;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            // Pixel centre in square units.
            let (x, y) = ((px as f32 + 0.5) / scale, (py as f32 + 0.5) / scale);
            let square = rounded_square(x, y, 128.0, 36.0);
            let glyph = lines
                .iter()
                .map(|&(a, b)| segment(x, y, a, b))
                .fold(((x - centre.0).hypot(y - centre.1) - radius).abs(), f32::min)
                - half_stroke;
            // Distances to coverage, anti-aliased over one pixel.
            let cover = |d: f32| (0.5 - d * scale).clamp(0.0, 1.0);
            let (a, ink) = (cover(square), cover(glyph));
            out.extend(BG.iter().zip(FG).map(|(bg, fg)| (bg + (fg - bg) * ink).round() as u8));
            out.push((a * 255.0).round() as u8);
        }
    }
    out
}

/// Signed distance to a square of side `side` at the origin with rounded corners.
fn rounded_square(x: f32, y: f32, side: f32, r: f32) -> f32 {
    let h = side / 2.0;
    let (qx, qy) = ((x - h).abs() - (h - r), (y - h).abs() - (h - r));
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// Distance from (x, y) to the segment a–b.
pub(crate) fn segment(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (x - a.0 - t * dx).hypot(y - a.1 - t * dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_the_icon() {
        let px = icon_rgba(64);
        assert_eq!(px.len(), 64 * 64 * 4);
        let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..][..4];
        // Corners are outside the rounded square, the middle of the left
        // edge inside it, in the background colour.
        assert_eq!(at(0, 0)[3], 0);
        assert_eq!(at(1, 32), &[208, 188, 255, 255]);
        // On the top stroke of the glyph: (3..16, 5) → square (37..76, 43).
        assert_eq!(at(28, 21), &[56, 30, 114, 255]);
    }

    #[test]
    fn menu_follows_the_state() {
        let playing = Status { track: None, playing: true, window_open: false };
        let labels = menu_labels(&playing);
        assert_eq!(labels[0], (Command::PlayPause, "Pause"));
        assert_eq!(labels[3].0, Command::ShowWindow);
    }
}

//! Night-sky backdrop for the circle visualizer: a dark sky, twinkling stars, an
//! aurora curtain that brightens with the bass, and mountain ridges. Drawn
//! from shapes (no image assets), tinted with the theme's accent colour.

use std::f32::consts::PI;

use gpui_kit::*;

const STARS: usize = 170;
/// Vertical aurora rays across the width.
const RAYS: usize = 110;

struct Star {
    /// Position as fractions of the area.
    x: f32,
    y: f32,
    size: f32,
    twinkle_speed: f32,
    phase: f32,
}

pub struct Backdrop {
    stars: Vec<Star>,
    /// Per-ray random offsets (length, brightness, flicker phase).
    rays: Vec<(f32, f32, f32)>,
}

/// Colours for one frame.
#[derive(Clone, Copy)]
pub struct Palette {
    pub sky_top: Hsla,
    pub sky_horizon: Hsla,
    pub aurora: Hsla,
    pub star: Hsla,
    pub ridge_far: Hsla,
    pub ridge_near: Hsla,
}

impl Palette {
    /// From the theme: a night sky in dark themes, a dusk sky in light ones.
    pub fn new(background: Hsla, accent: Hsla) -> Self {
        let dark = background.l < 0.5;
        let with = |c: Hsla, s: f32, l: f32, a: f32| Hsla { h: c.h, s, l, a };
        let sky_top = if dark { with(background, background.s, background.l * 0.7, 1.0) } else { background };
        let sky_horizon = with(accent, accent.s * 0.6, if dark { 0.16 } else { 0.86 }, 1.0);
        Self {
            sky_top,
            sky_horizon,
            aurora: with(accent, (accent.s * 1.1).min(1.0), if dark { 0.72 } else { 0.6 }, 1.0),
            star: if dark { with(accent, 0.3, 0.95, 1.0) } else { with(accent, 0.4, 0.55, 1.0) },
            ridge_far: with(accent, accent.s * 0.35, if dark { 0.1 } else { 0.7 }, 1.0),
            ridge_near: with(accent, accent.s * 0.25, if dark { 0.05 } else { 0.55 }, 1.0),
        }
    }
}

impl Backdrop {
    /// `random` yields uniform numbers in 0..1.
    pub fn new(mut random: impl FnMut() -> f32) -> Self {
        let stars = (0..STARS)
            .map(|_| Star {
                x: random(),
                // Mostly high up; the ridges hide the bottom anyway.
                y: random().powf(1.3) * 0.8,
                size: 0.8 + 1.6 * random() * random(),
                twinkle_speed: 0.6 + 2.0 * random(),
                phase: random() * 2.0 * PI,
            })
            .collect();
        let rays = (0..RAYS).map(|_| (random(), random(), random() * 2.0 * PI)).collect();
        Self { stars, rays }
    }

    /// Paint into `bounds`. `time` in seconds drives the slow drift; `glow`
    /// (0..1, the bass) brightens the aurora.
    pub fn paint(&self, bounds: Bounds<Pixels>, time: f32, glow: f32, colors: Palette, window: &mut Window) {
        let (w, h) = (bounds.size.width, bounds.size.height);
        let at = |x: f32, y: f32| bounds.origin + point(w * x, h * y);

        // Sky.
        window.paint_quad(fill(
            bounds,
            linear_gradient(180., linear_color_stop(colors.sky_top, 0.25), linear_color_stop(colors.sky_horizon, 1.)),
        ));

        // Stars.
        for s in &self.stars {
            let twinkle = 0.55 + 0.45 * (time * s.twinkle_speed + s.phase).sin();
            let size = px(s.size);
            let pos = at(s.x, s.y) - point(size / 2.0, size / 2.0);
            window.paint_quad(
                fill(Bounds::new(pos, size2(size)), colors.star.opacity(0.75 * twinkle)).corner_radii(size / 2.0),
            );
        }

        // Aurora: rays hanging from a drifting arc, fading in from the top and
        // out towards their base. Overlapping translucent rays read as a soft glow.
        let slot = w / RAYS as f32;
        let ray_w = slot * 2.2;
        let brightness = 0.55 + 0.6 * glow;
        for (i, &(len_r, bright_r, phase)) in self.rays.iter().enumerate() {
            let x = (i as f32 + 0.5) / RAYS as f32;
            // Arc high in the middle, lower at the sides, slowly waving.
            let base = 0.5 - 0.16 * (PI * x).sin()
                + 0.05 * (x * 7.0 + time * 0.13).sin()
                + 0.025 * (x * 17.0 - time * 0.21).sin();
            let length = 0.16 + 0.22 * len_r + 0.05 * (x * 5.0 + time * 0.1).sin();
            let flicker = 0.75 + 0.25 * (time * 0.7 + phase).sin();
            // Fainter towards the edges of the screen.
            let edge = (PI * x).sin().powf(0.6);
            let alpha = (0.05 + 0.1 * bright_r) * flicker * edge * brightness;
            let top = base - length;
            let fade = length * 0.25;
            let left = bounds.left() + w * x - ray_w / 2.0;
            let color =
                Hsla { h: (colors.aurora.h + 0.04 * (x - 0.5) + 0.02 * bright_r).rem_euclid(1.0), ..colors.aurora };
            // Body: transparent at the top → bright just above the base.
            window.paint_quad(fill(
                Bounds::new(point(left, bounds.top() + h * top), size(ray_w, h * (length - fade))),
                linear_gradient(
                    180.,
                    linear_color_stop(color.opacity(0.0), 0.),
                    linear_color_stop(color.opacity(alpha), 1.),
                ),
            ));
            // Base: bright → transparent.
            window.paint_quad(fill(
                Bounds::new(point(left, bounds.top() + h * (base - fade)), size(ray_w, h * fade)),
                linear_gradient(
                    180.,
                    linear_color_stop(color.opacity(alpha), 0.),
                    linear_color_stop(color.opacity(0.0), 1.),
                ),
            ));
        }

        // Mountains: a lighter, softer ridge behind a darker one.
        let far =
            |x: f32| 0.2 + 0.05 * (x * 6.0 + 1.3).sin() + 0.03 * (x * 15.0 + 0.4).sin() + peak(x, 0.7, 0.07, 0.08);
        let near = |x: f32| {
            0.11 + 0.035 * (x * 9.0 + 2.0).sin()
                + 0.02 * (x * 27.0).sin()
                + 0.012 * (x * 61.0 + 1.0).sin()
                + peak(x, 0.42, 0.09, 0.07)
                + peak(x, 0.12, 0.06, 0.05)
        };
        for (height, color) in [(&far as &dyn Fn(f32) -> f32, colors.ridge_far), (&near, colors.ridge_near)] {
            let mut path = PathBuilder::fill();
            path.move_to(at(0.0, 1.0));
            const STEPS: usize = 120;
            for i in 0..=STEPS {
                let x = i as f32 / STEPS as f32;
                path.line_to(at(x, 1.0 - height(x)));
            }
            path.line_to(at(1.0, 1.0));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
    }
}

/// A pointed peak of `height` at `center` with half-width `width` (fractions).
fn peak(x: f32, center: f32, width: f32, height: f32) -> f32 {
    let d = ((x - center).abs() / width).min(1.0);
    height * (1.0 - d).powf(1.6)
}

fn size2(s: Pixels) -> Size<Pixels> {
    size(s, s)
}

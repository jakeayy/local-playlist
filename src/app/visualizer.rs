//! Full-size music visualizer shown in place of the track list.

use std::cell::Cell;
use std::collections::VecDeque;
use std::f32::consts::PI;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::backdrop::{Backdrop, Palette};
use crate::audio::Tap;
use crate::spectrum::{FFT_LEN, Spectrum, Tuning};

const BANDS: usize = 64;

// Bars.
/// Gap between bars, as a fraction of a bar's slot.
const GAP: f32 = 0.22;
const PEAK_HEIGHT: f32 = 3.0;

// Circle (sizes are fractions of the smaller side of the area).
/// Bands around each half of the circle (see [`Tuning::CIRCLE`]).
const RING_BANDS: usize = 40;
const RADIUS: f32 = 0.2;
/// Extra radius at full bass.
const PULSE: f32 = 0.16;
/// Spike length at full level.
const SPIKE: f32 = 0.24;
/// Rings drawn behind the front one, each showing an older frame, a bit larger
/// and in a shifted hue.
const LAYERS: usize = 4;
const FRAMES_PER_LAYER: usize = 2;
const LAYER_SCALE: f32 = 0.06;
/// Where the bass sits on each side, measured from the top (radians).
/// Above it the frequencies rise towards the top centre; below it the outline
/// tapers back to the disc.
const BASS_ANGLE: f32 = 0.39 * PI;
/// Weight of the highest band at the top centre (bass = 1).
const TREBLE_WEIGHT: f32 = 0.45;
/// Below the bass, how far up the spectrum the taper reaches (fraction of bands).
const TAPER_BANDS: f32 = 0.3;
/// Outline points per side.
const RING_POINTS: usize = 72;
const MAX_PARTICLES: usize = 260;
/// A kick: the ring's bass level (gated, so ~0 when the bass is quiet) above
/// `KICK_MIN` and rising at least `KICK_RISE` over its recent average.
const KICK_MIN: f32 = 0.4;
const KICK_RISE: f32 = 0.18;
const SHAKE_PX: f32 = 7.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Bars,
    Circle,
}

struct Particle {
    /// Direction (radians) and distance from the centre (fraction of the smaller side).
    angle: f32,
    dist: f32,
    speed: f32,
    size: f32,
}

pub struct Visualizer {
    pub style: Style,
    cover: Option<Arc<RenderImage>>,
    tap: Option<Tap>,
    spectrum: Spectrum,
    /// Calmer, bass-led analysis for the circle.
    ring: Spectrum,
    samples: Vec<f32>,
    /// Tap counter at the previous frame (unchanged = nothing playing).
    last_written: u64,
    last_frame_ms: f64,
    /// Recent band frames, newest first (for the trailing rings).
    history: VecDeque<Vec<f32>>,
    kicks: KickDetector,
    particles: Vec<Particle>,
    /// Fractional particles owed to the spawn rate.
    spawn_debt: f32,
    rng: u32,
    /// Size of the area at the last paint (the layout of the next frame).
    bounds: Rc<Cell<Bounds<Pixels>>>,
    backdrop: Rc<Backdrop>,
    /// Seconds of animation so far (drives the backdrop's drift).
    time: f32,
}

impl Visualizer {
    pub fn new(tap: Option<Tap>) -> Self {
        let mut rng = (crate::now_ms() as u32) | 1;
        let backdrop = Rc::new(Backdrop::new(|| xorshift(&mut rng)));
        Self {
            style: Style::Circle,
            cover: None,
            tap,
            spectrum: Spectrum::new(BANDS, Tuning::BARS),
            ring: Spectrum::new(RING_BANDS, Tuning::CIRCLE),
            samples: vec![0.0; FFT_LEN],
            last_written: 0,
            last_frame_ms: crate::now_ms(),
            history: VecDeque::new(),
            kicks: KickDetector::default(),
            particles: Vec::new(),
            spawn_debt: 0.0,
            rng,
            bounds: Rc::default(),
            backdrop,
            time: 0.0,
        }
    }

    pub fn set_cover(&mut self, cover: Option<Arc<RenderImage>>) {
        self.cover = cover;
    }

    /// Uniform random number in 0..1 (xorshift; only for looks).
    fn random(&mut self) -> f32 {
        xorshift(&mut self.rng)
    }

    /// Feed the latest audio into the analyser and move everything along;
    /// returns whether anything is still moving (so another frame is needed).
    fn advance(&mut self) -> bool {
        let now = crate::now_ms();
        let dt = (((now - self.last_frame_ms) / 1000.0) as f32).clamp(0.0, 0.1);
        self.time += dt;
        self.last_frame_ms = now;
        let mut playing = false;
        let mut rate = 0;
        if let Some(tap) = &self.tap {
            let (r, written) = tap.latest(&mut self.samples);
            playing = written != self.last_written;
            self.last_written = written;
            rate = r;
        }
        let input = playing.then_some((&self.samples[..], rate));
        self.spectrum.update(input, dt);
        self.ring.update(input, dt);

        self.history.push_front(self.ring.bands().to_vec());
        self.history.truncate(LAYERS * FRAMES_PER_LAYER + 1);

        self.kicks.update(self.ring.bass_raw(), playing, dt);

        if self.style == Style::Circle {
            self.move_particles(playing, dt);
        }
        playing
            || !self.spectrum.is_silent()
            || !self.ring.is_silent()
            || !self.particles.is_empty()
            || self.kicks.kick > 0.01
    }

    fn move_particles(&mut self, playing: bool, dt: f32) {
        let boost = 1.0 + 5.0 * self.kicks.kick + 2.0 * self.spectrum.bass();
        for p in &mut self.particles {
            p.dist += p.speed * boost * dt;
        }
        self.particles.retain(|p| p.dist < 0.9);
        if !playing {
            return;
        }
        self.spawn_debt += dt * (25.0 + 140.0 * self.spectrum.bass());
        while self.spawn_debt >= 1.0 {
            self.spawn_debt -= 1.0;
            if self.particles.len() >= MAX_PARTICLES {
                continue;
            }
            let p = Particle {
                angle: self.random() * 2.0 * PI,
                dist: RADIUS * (0.6 + 0.6 * self.random()),
                speed: 0.02 + 0.05 * self.random(),
                size: 1.0 + 2.5 * self.random() * self.random(),
            };
            self.particles.push(p);
        }
    }
}

impl Render for Visualizer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.advance() {
            window.request_animation_frame();
        } else {
            // Idle: check now and then whether playback started.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        let bounds_cell = self.bounds.clone();
        let store_bounds = move |bounds: Bounds<Pixels>, _: &mut Window, _: &mut App| bounds_cell.set(bounds);
        match self.style {
            // Leave room for the page's title above the bars.
            Style::Bars => {
                div().size_full().pt(px(96.)).px_6().pb_6().child(canvas(store_bounds, self.paint_bars(cx)).size_full())
            }
            Style::Circle => self.render_circle(store_bounds, cx),
        }
    }
}

impl Visualizer {
    fn paint_bars(&self, cx: &App) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) + 'static {
        let theme = cx.theme();
        let (bar_lo, bar_hi, peak) = (theme.primary.opacity(0.55), theme.primary, theme.foreground.opacity(0.7));
        let bands = self.spectrum.bands().to_vec();
        let peaks = self.spectrum.peaks().to_vec();
        move |bounds, _, window, _| {
            let n = bands.len() as f32;
            let slot = bounds.size.width / n;
            let bar_w = slot * (1.0 - GAP);
            let height = bounds.size.height;
            let bottom = bounds.bottom();
            for (i, (&v, &p)) in bands.iter().zip(&peaks).enumerate() {
                let x = bounds.left() + slot * i as f32 + (slot - bar_w) / 2.0;
                let h = (height * v).max(px(2.0));
                let bar = Bounds::new(point(x, bottom - h), size(bar_w, h));
                let radius = (bar_w / 2.0).min(px(3.0));
                window.paint_quad(
                    fill(bar, linear_gradient(0., linear_color_stop(bar_lo, 0.), linear_color_stop(bar_hi, 1.)))
                        .corner_radii(Corners { top_left: radius, top_right: radius, ..Default::default() }),
                );
                if p > 0.01 {
                    let y = bottom - height * p - px(PEAK_HEIGHT + 2.0);
                    let mark = Bounds::new(point(x, y), size(bar_w, px(PEAK_HEIGHT)));
                    window.paint_quad(fill(mark, peak).corner_radii(px(1.5)));
                }
            }
        }
    }

    fn render_circle(
        &mut self,
        store_bounds: impl FnOnce(Bounds<Pixels>, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        // Geometry from the last paint, relative to the area's origin.
        let area = self.bounds.get().size;
        let unit = area.width.min(area.height);
        let shake = if cx.reduce_motion() {
            point(px(0.), px(0.))
        } else {
            let k = self.kicks.kick * self.kicks.kick * SHAKE_PX;
            point(px((self.random() - 0.5) * 2.0 * k), px((self.random() - 0.5) * 2.0 * k))
        };
        let center = point(area.width / 2.0, area.height / 2.0) + shake;
        let radius = unit * (RADIUS + PULSE * self.ring.bass().powf(1.5) * 0.5 + 0.02 * self.kicks.kick);

        // Rings back to front: (levels, scale, color).
        let front = theme.foreground;
        let mut rings = Vec::new();
        for layer in (0..=LAYERS).rev() {
            let Some(levels) = self.history.get(layer * FRAMES_PER_LAYER).or(self.history.back()) else {
                break;
            };
            let color = if layer == 0 {
                front
            } else {
                let mut c = theme.primary;
                c.h = (c.h + 0.07 * (layer - 1) as f32).fract();
                c.a = 0.85 - 0.12 * (layer - 1) as f32;
                c
            };
            rings.push((ring_profile(levels), 1.0 + LAYER_SCALE * layer as f32, color));
        }
        let particles: Vec<(f32, f32, f32)> = self.particles.iter().map(|p| (p.angle, p.dist, p.size)).collect();
        let particle_color = theme.foreground;
        let disc = theme.background;
        let backdrop = self.backdrop.clone();
        let palette = Palette::new(theme.background, theme.primary);
        let (time, glow) = (self.time, self.ring.bass());

        let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut Window, _: &mut App| {
            backdrop.paint(bounds, time, glow, palette, window);
            let c = bounds.origin + center;
            for &(angle, dist, size) in &particles {
                // Fade in near the circle, out towards the edges.
                let alpha = ((dist - RADIUS * 0.8) * 8.0).clamp(0.0, 1.0) * (1.0 - dist / 0.9).clamp(0.0, 1.0);
                let pos = c + point(unit * dist * angle.cos(), unit * dist * angle.sin());
                let s = px(size);
                window.paint_quad(
                    fill(Bounds::new(pos - point(s / 2.0, s / 2.0), size2(s)), particle_color.opacity(0.7 * alpha))
                        .corner_radii(s / 2.0),
                );
            }
            for (levels, scale, color) in &rings {
                if let Some(path) = spectrum_ring(c, radius, unit * SPIKE * *scale, levels) {
                    window.paint_path(path, *color);
                }
            }
            // The disc in the middle (the cover, if any, is drawn over it).
            window.paint_quad(
                fill(Bounds::new(c - point(radius, radius), size2(radius * 2.0)), disc).corner_radii(radius),
            );
        };

        let cover = self.cover.clone();
        div().size_full().relative().child(canvas(store_bounds, paint).size_full()).when_some(cover, |d, cover| {
            // Slightly inside the disc so the ring edge stays visible.
            let r = radius * 0.94;
            d.child(
                img(cover)
                    .absolute()
                    .left(center.x - r)
                    .top(center.y - r)
                    .size(r * 2.0)
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            )
        })
    }
}

/// Spots kicks in a bass level; `kick` jumps to the hit's strength (0..1) and
/// decays, driving the shake and the particle burst.
#[derive(Default)]
struct KickDetector {
    /// Slow average of the bass.
    avg: f32,
    kick: f32,
}

impl KickDetector {
    fn update(&mut self, bass: f32, playing: bool, dt: f32) {
        if playing && bass > KICK_MIN && bass - self.avg > KICK_RISE {
            let strength = ((bass - KICK_MIN) / (1.0 - KICK_MIN)).clamp(0.0, 1.0).sqrt();
            self.kick = self.kick.max(strength);
        }
        self.avg += (bass - self.avg) * (1.0 - (-dt / 0.4).exp());
        self.kick *= (-dt / 0.18).exp();
    }
}

/// Uniform random number in 0..1 (xorshift; only for looks).
fn xorshift(state: &mut u32) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state >> 8) as f32 / (1 << 24) as f32
}

fn size2(s: Pixels) -> Size<Pixels> {
    size(s, s)
}

/// Spike height (0..1) at [`RING_POINTS`] angles from the top to the bottom
/// of one side, from band levels (low to high): treble at the top centre,
/// toned down, falling in pitch to the bass at [`BASS_ANGLE`], then the low
/// bands again, tapering to nothing at the bottom.
fn ring_profile(levels: &[f32]) -> Vec<f32> {
    let last = (levels.len() - 1) as f32;
    let at = |idx: f32| {
        let i = (idx.clamp(0.0, last)).floor() as usize;
        let f = idx - i as f32;
        levels[i] * (1.0 - f) + levels[(i + 1).min(levels.len() - 1)] * f
    };
    (0..RING_POINTS)
        .map(|i| {
            let theta = PI * (i as f32 + 0.5) / RING_POINTS as f32;
            if theta <= BASS_ANGLE {
                let f = theta / BASS_ANGLE;
                at((1.0 - f) * last) * (TREBLE_WEIGHT + (1.0 - TREBLE_WEIGHT) * f)
            } else {
                let t = (theta - BASS_ANGLE) / (PI - BASS_ANGLE);
                let taper = (1.0 - t) * (1.0 - t) * (3.0 - 2.0 * (1.0 - t)); // smoothstep
                at(t * TAPER_BANDS * last) * taper
            }
        })
        .collect()
}

/// Closed smooth outline around `center`: a side's [`ring_profile`] mirrored
/// left/right, as spikes of up to `spike` beyond `radius`.
fn spectrum_ring(center: Point<Pixels>, radius: Pixels, spike: Pixels, profile: &[f32]) -> Option<Path<Pixels>> {
    let n = profile.len();
    // Right side top → bottom, then left side bottom → top.
    let points: Vec<Point<Pixels>> = (0..2 * n)
        .map(|i| {
            let (side, b) = if i < n { (1.0, i) } else { (-1.0, 2 * n - 1 - i) };
            let theta = PI * (b as f32 + 0.5) / n as f32;
            let r = radius + spike * profile[b];
            center + point(r * side * theta.sin(), -r * theta.cos())
        })
        .collect();
    // Catmull-Rom through the points, as cubic Béziers.
    let m = points.len();
    let at = |i: isize| points[i.rem_euclid(m as isize) as usize];
    let mut path = PathBuilder::fill();
    path.move_to(points[0]);
    for i in 0..m as isize {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let c1 = p1 + (p2 - p0) / 6.0;
        let c2 = p2 - (p3 - p1) / 6.0;
        path.cubic_bezier_to(p2, c1, c2);
    }
    path.close();
    path.build().ok()
}

#[cfg(test)]
mod tests {
    use super::{BASS_ANGLE, PI, RING_BANDS, RING_POINTS, TREBLE_WEIGHT, ring_profile};

    #[test]
    fn bass_peaks_at_the_side_and_tapers() {
        // Strong bass, a little treble.
        let levels: Vec<f32> = (0..RING_BANDS).map(|b| if b < 6 { 1.0 } else { 0.2 }).collect();
        let p = ring_profile(&levels);
        let peak = p.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let peak_angle = PI * (peak as f32 + 0.5) / RING_POINTS as f32;
        assert!((peak_angle - BASS_ANGLE).abs() < 0.15 * PI, "peak at {:.0}°", peak_angle.to_degrees());
        // Treble at the top is toned down, the bottom is flat.
        assert!(p[0] < 0.2 * TREBLE_WEIGHT + 0.01);
        assert!(p[RING_POINTS - 1] < 0.01);
    }
}

//! Full-size music visualizer shown in place of the track list: bars, or a
//! Trap Nation-style circle: a disc that jumps and shakes with the bass,
//! ringed by the bass spectrum with coloured trails that flare out on hits.

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
use crate::spectrum::{MAX_FFT_LEN, Spectrum, Tuning};

const BANDS: usize = 64;

// Bars.
/// Gap between bars, as a fraction of a bar's slot.
const GAP: f32 = 0.22;
const PEAK_HEIGHT: f32 = 3.0;

/// The tap gets audio in bursts (the output device's buffer, often 20 ms or
/// more), so many frames see nothing new; playback counts as stopped only
/// after this long without samples. Before that, frames reuse the last audio
/// instead of falling for a frame and jumping back, which made everything
/// flutter (most at high refresh rates).
const STALL_MS: f64 = 150.0;

// Circle (sizes are fractions of the smaller side of the area).
/// Bands around each half of the circle (see [`Tuning::CIRCLE`]).
const RING_BANDS: usize = 40;
const RADIUS: f32 = 0.2;
/// How much larger the disc, its cover and the ring get at full bass
/// ([`Spectrum::bass`]): the whole circle jumps with each kick and stays
/// pumped while the bass keeps going, like Trap Nation's logo.
const JUMP: f32 = 0.33;
/// Spike length of the front (white) ring at full level.
const SPIKE: f32 = 0.1;
/// Coloured rings behind the front one, each `LAYER_DELAY_SECS` further
/// behind, smoother, and in a shifted hue: a trail of each hit.
const LAYERS: usize = 6;
const LAYER_DELAY_SECS: f32 = 0.03;
/// How the rings behind flare out (see [`flare`]): layer `n` draws a level
/// `v` as `(1 + n·FLARE_GAIN) · v^(1 + n·FLARE_EXP)`. Quiet, they stay
/// inside the front ring; on loud hits they shoot out past it, up to
/// `1 + LAYERS·FLARE_GAIN` times as far.
const FLARE_GAIN: f32 = 0.15;
const FLARE_EXP: f32 = 0.12;
/// Frames of ring history kept at most (the trails need 180 ms of them).
const MAX_HISTORY: usize = 96;
/// Outline points per side.
const RING_POINTS: usize = 72;
const MAX_PARTICLES: usize = 260;
/// The circle vibrates with the bass, smoothly: up to `SHAKE_PX` away at
/// full bass (less on quieter bass, still when there's none), cycling up to
/// `SHAKE_HZ` times a second.
const SHAKE_PX: f32 = 7.0;
const SHAKE_HZ: f32 = 9.0;

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
    /// The circle's spikes: only the bass, in fine steps (see [`Tuning::CIRCLE`]).
    ring: Spectrum,
    samples: Vec<f32>,
    /// Tap counter at the previous frame, and when it last moved.
    last_written: u64,
    last_audio_ms: f64,
    last_frame_ms: f64,
    /// Recent ring bands with their frame's time (`last_frame_ms`), newest
    /// first (for the trailing rings), back to [`LAYERS`] × [`LAYER_DELAY_SECS`].
    history: VecDeque<(f64, Vec<f32>)>,
    /// Advances with the bass; drives the shake (see [`shake_offset`]).
    shake_phase: f32,
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
            samples: vec![0.0; MAX_FFT_LEN],
            last_written: 0,
            last_audio_ms: f64::NEG_INFINITY,
            last_frame_ms: crate::now_ms(),
            history: VecDeque::new(),
            shake_phase: 0.0,
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
            if written != self.last_written {
                self.last_written = written;
                self.last_audio_ms = now;
            }
            playing = now - self.last_audio_ms < STALL_MS;
            rate = r;
        }
        let input = playing.then_some((&self.samples[..], rate));
        // The bars' analysis also gives the bass level, so it always runs; the
        // ring's long FFT only while the circle shows (it settles within a few
        // frames of switching).
        self.spectrum.update(input, dt);
        self.ring.update(input.filter(|_| self.style == Style::Circle), dt);

        self.history.push_front((now, self.ring.bands().to_vec()));
        // Keep one frame at least as old as the last layer needs, and never
        // more than a few hundred frames a second would (if the clock jumps).
        let keep_ms = (LAYERS as f32 * LAYER_DELAY_SECS * 1000.0) as f64;
        while self.history.len() > 2 && now - self.history[self.history.len() - 2].0 >= keep_ms {
            self.history.pop_back();
        }
        self.history.truncate(MAX_HISTORY);

        // From the bars' analysis: its shorter FFT sees a kick sooner.
        let bass = self.spectrum.bass();
        // Wrapped where every frequency in `shake_offset` completes whole cycles.
        self.shake_phase = (self.shake_phase + 2.0 * PI * SHAKE_HZ * bass * dt) % (200.0 * PI);

        if self.style == Style::Circle {
            self.move_particles(playing, dt);
        }
        playing || !self.spectrum.is_silent() || !self.ring.is_silent() || !self.particles.is_empty()
    }

    fn move_particles(&mut self, playing: bool, dt: f32) {
        let bass = self.spectrum.bass();
        // Bursts outwards on hits.
        let boost = 1.0 + 6.0 * bass * bass;
        for p in &mut self.particles {
            p.dist += p.speed * boost * dt;
        }
        self.particles.retain(|p| p.dist < 0.9);
        if !playing {
            return;
        }
        self.spawn_debt += dt * (25.0 + 140.0 * bass);
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
        let bass = self.spectrum.bass();
        let shake = if cx.reduce_motion() {
            point(px(0.), px(0.))
        } else {
            let (x, y) = shake_offset(self.shake_phase);
            let k = bass * bass * SHAKE_PX;
            point(px(x * k), px(y * k))
        };
        let center = point(area.width / 2.0, area.height / 2.0) + shake;
        let radius = unit * RADIUS * (1.0 + JUMP * bass);

        // Rings back to front: (profile, color).
        let front = theme.foreground;
        let mut rings = Vec::new();
        for layer in (0..=LAYERS).rev() {
            // The newest frame at least this layer's delay old.
            let age_ms = (layer as f32 * LAYER_DELAY_SECS * 1000.0) as f64 - 0.1;
            let now = self.last_frame_ms;
            let Some((_, levels)) = self.history.iter().find(|(t, _)| now - t >= age_ms).or(self.history.back()) else {
                break;
            };
            let color = if layer == 0 {
                front
            } else {
                let mut c = theme.primary;
                c.h = (c.h + 0.07 * (layer - 1) as f32).fract();
                c.a = 0.85 - 0.1 * (layer - 1) as f32;
                c
            };
            rings.push((layer_profile(levels, layer), color));
        }
        let particles: Vec<(f32, f32, f32)> = self.particles.iter().map(|p| (p.angle, p.dist, p.size)).collect();
        let particle_color = theme.foreground;
        let disc = theme.background;
        let backdrop = self.backdrop.clone();
        let palette = Palette::new(theme.background, theme.primary);
        let (time, glow) = (self.time, bass);

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
            for (profile, color) in &rings {
                if let Some(path) = spectrum_ring(c, radius, unit * SPIKE, profile) {
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

/// Where the shake puts the circle (each axis about -1..1) at `phase`: a few
/// sines at unrelated rates, so it wanders like a shaking speaker instead of
/// circling. Every rate is a multiple of 1/100, so the phase can wrap at 200π.
fn shake_offset(phase: f32) -> (f32, f32) {
    let x = (phase.sin() + 0.5 * (2.37 * phase + 1.7).sin()) / 1.5;
    let y = ((1.31 * phase + 0.5).sin() + 0.5 * (2.83 * phase + 4.1).sin()) / 1.5;
    (x, y)
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
/// of one side, from band levels (low to high), as Trap Nation lays it out:
/// the deepest notes at the top centre, rising in pitch down the side.
fn ring_profile(levels: &[f32]) -> Vec<f32> {
    let last = (levels.len() - 1) as f32;
    (0..RING_POINTS)
        .map(|i| {
            let idx = (i as f32 + 0.5) / RING_POINTS as f32 * last;
            let b = idx.floor() as usize;
            let f = idx - b as f32;
            levels[b] * (1.0 - f) + levels[(b + 1).min(levels.len() - 1)] * f
        })
        .collect()
}

/// The profile of ring `layer` (0 = front): smoother the further back, and
/// flared (see [`flare`]).
fn layer_profile(levels: &[f32], layer: usize) -> Vec<f32> {
    let n = levels.len();
    let mut levels = levels.to_vec();
    for _ in 0..layer {
        levels = (0..n)
            .map(|b| 0.25 * levels[b.saturating_sub(1)] + 0.5 * levels[b] + 0.25 * levels[(b + 1).min(n - 1)])
            .collect();
    }
    ring_profile(&levels).into_iter().map(|v| flare(v, layer)).collect()
}

/// Spike height of `level` in ring `layer`, relative to the front's at full
/// level (see [`FLARE_GAIN`]).
fn flare(level: f32, layer: usize) -> f32 {
    let n = layer as f32;
    (1.0 + n * FLARE_GAIN) * level.max(0.0).powf(1.0 + n * FLARE_EXP)
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
    use super::{LAYERS, RING_BANDS, RING_POINTS, flare, layer_profile, ring_profile};

    #[test]
    fn deepest_notes_at_the_top() {
        let peak = |levels: &[f32]| {
            let p = ring_profile(levels);
            assert_eq!(p.len(), RING_POINTS);
            p.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0
        };
        let low: Vec<f32> = (0..RING_BANDS).map(|b| if b < 4 { 1.0 } else { 0.1 }).collect();
        assert!(peak(&low) < RING_POINTS / 8, "{}", peak(&low));
        let high: Vec<f32> = (0..RING_BANDS).map(|b| if b >= RING_BANDS - 4 { 1.0 } else { 0.1 }).collect();
        assert!(peak(&high) > RING_POINTS * 7 / 8, "{}", peak(&high));
    }

    #[test]
    fn trails_flare_out_only_on_loud_hits() {
        // The front ring draws levels as they are.
        assert_eq!(flare(0.6, 0), 0.6);
        // The last trail stays inside it when quiet and shoots past it when loud.
        assert!(flare(0.3, LAYERS) < 0.3);
        assert!(flare(1.0, LAYERS) > 1.5);
        // Further back, smoother: a lone loud band spreads out and drops.
        let mut levels = vec![0.0; RING_BANDS];
        levels[RING_BANDS / 2] = 1.0;
        let front = layer_profile(&levels, 0).into_iter().fold(0.0, f32::max);
        let back = layer_profile(&levels, LAYERS).into_iter().fold(0.0, f32::max);
        assert!(back < front, "{back} vs {front}");
    }
}

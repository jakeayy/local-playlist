//! The player bar's seek row: elapsed time, the slider (with skipped
//! SponsorBlock segments drawn over it) and the length.
//!
//! It is its own view because it's the only thing that changes while a track
//! simply plays: the clock ticks 5×/s, and redrawing just this row then
//! leaves the rest of the window (sidebar, track list) drawn from last frame.
//! It only reads the app's state; the slider reports to the app itself.

use super::*;

pub(super) struct Progress {
    app: WeakEntity<LocalPlaylist>,
}

impl Progress {
    pub(super) fn new(app: WeakEntity<LocalPlaylist>) -> Self {
        Self { app }
    }
}

impl Render for Progress {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.app.upgrade() {
            Some(app) => app.read(cx).render_progress(cx),
            None => h_flex(),
        }
    }
}

impl LocalPlaylist {
    fn render_progress(&self, cx: &App) -> Div {
        let theme = cx.theme();
        let total = self.total_duration();
        // SponsorBlock segments of the current track as (start, length) fractions.
        let skipped: Vec<(f32, f32)> = match (&self.now, total) {
            (Some(now), Some(total)) if self.library.settings.sponsorblock && !total.is_zero() => {
                let total = total.as_secs_f64();
                self.segments
                    .get(&now.track.source_id)
                    .filter(|_| now.track.platform == Platform::YouTube)
                    .map(|(_, segs)| {
                        crate::sponsorblock::skipped_ranges(
                            segs,
                            &self.library.settings.sponsorblock_categories,
                        )
                        .into_iter()
                        .filter(|&(start, _)| start < total)
                        .map(|(start, end)| {
                            (
                                (start / total) as f32,
                                ((end.min(total) - start) / total) as f32,
                            )
                        })
                        .collect()
                    })
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let pos = match (self.seeking, total) {
            (Some(frac), Some(t)) => t.mul_f32(frac),
            _ => self.audio.as_ref().map(|a| a.position()).unwrap_or_default(),
        };
        let fmt = |d: Duration| format_duration(d.as_secs() as u32);
        h_flex()
            .w_full()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().w(px(44.)).text_right().child(fmt(pos)))
            .child(
                div()
                    .flex_1()
                    .relative()
                    .child(Slider::new(&self.seek).disabled(!matches!(self.state, PlayState::Playing)))
                    .when(!skipped.is_empty(), |d| {
                        // Skipped parts, drawn over the slider track (24 px row,
                        // 6 px track). No listeners, so clicks reach the slider.
                        d.child(
                            div().absolute().inset_0().flex().items_center().child(
                                div().relative().w_full().h(px(6.)).children(skipped.iter().map(
                                    |&(start, len)| {
                                        div()
                                            .absolute()
                                            .top_0()
                                            .bottom_0()
                                            .left(relative(start))
                                            .w(relative(len))
                                            .min_w(px(2.))
                                            .bg(theme.danger.opacity(0.85))
                                    },
                                )),
                            ),
                        )
                    }),
            )
            .child(div().w(px(44.)).child(total.map(fmt).unwrap_or_else(|| "--:--".into())))
    }
}

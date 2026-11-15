//! The lyrics page, shown in place of the track list: the playing track's
//! lyrics with the line being sung highlighted and kept in the middle, plain
//! text when they aren't timed. Clicking a timed line plays from there.
//!
//! The line being sung fills with the accent colour like karaoke, left to
//! right with a soft edge: through each word in its own time when the lyrics
//! time words, otherwise at a steady pace until the next line
//! (`Lyrics::line_progress`).
//! Sparks drift off the edge of the fill while it moves ([`Sparks`]).
//!
//! Timings can be shifted per song (saved in the library), for music videos
//! whose intro the lyrics weren't timed with: − / + in the header, or
//! right-click a line as it's sung. Lyrics timed for another cut of the song
//! say so in the header, as a hint that they may need shifting.
//!
//! It is its own view, like the seek row: while a song plays only the
//! highlight moves, so it checks the position every 50 ms while shown and
//! redraws (just itself) when the line or word changes. Lyrics are looked up
//! only for tracks played while the page is open (see `crate::lyrics`).

use std::cell::Cell;
use std::sync::Arc;

use super::*;
use crate::lyrics::Lyrics;

/// Highlight this much ahead of the audio clock: a poll can be up to 50 ms
/// late, and a word lit slightly early reads better than one lit late.
const LEAD_MS: u32 = 60;
/// After the user scrolls, the page stops following the song for a while.
const FOLLOW_PAUSE_MS: f64 = 4000.0;
const POLL_MS: u64 = 50;
/// Offset steps (− / +; with Shift the larger one).
const NUDGE_MS: i32 = 500;
const BIG_NUDGE_MS: i32 = 5000;
const MAX_OFFSET_MS: i32 = 10 * 60 * 1000;
/// Steps of a line's fill that are drawn (it moves on at most every poll).
const FILL_STEPS: f32 = 256.0;
/// Characters the edge of a line's fill fades over.
const FILL_EDGE: f32 = 4.0;

/// The highlight as drawn.
#[derive(Clone, Copy, Default, PartialEq)]
struct Highlight {
    line: Option<usize>,
    /// Fill of the line, in `FILL_STEPS`.
    fill: u16,
}

enum Status {
    /// Nothing playing.
    Idle,
    Loading,
    Ready(Option<Arc<Lyrics>>),
    Failed(String),
}

pub(super) struct LyricsView {
    app: WeakEntity<LocalPlaylist>,
    /// Track (platform, id) the lyrics are for, and its file key (which
    /// its offset is saved under).
    track: Option<(Platform, String)>,
    key: String,
    status: Status,
    scroll: ScrollHandle,
    shown: Highlight,
    /// Line last scrolled to the middle, and the scroll offset being eased to.
    centred: Option<usize>,
    target: Option<Pixels>,
    user_scrolled_ms: f64,
    polling: bool,
    /// Where the fill's edge is (set while painting the line), for `sparks`.
    edge: Rc<Cell<Option<Edge>>>,
    sparks: Entity<Sparks>,
}

impl LyricsView {
    pub(super) fn new(app: WeakEntity<LocalPlaylist>, cx: &mut App) -> Self {
        let edge = Rc::new(Cell::new(None));
        let sparks = {
            let edge = edge.clone();
            cx.new(|_| Sparks::new(edge))
        };
        Self {
            edge,
            sparks,
            key: String::new(),
            app,
            track: None,
            status: Status::Idle,
            scroll: ScrollHandle::new(),
            shown: Highlight::default(),
            centred: None,
            target: None,
            user_scrolled_ms: 0.0,
            polling: false,
        }
    }

    /// Show `track`'s lyrics (`None`: nothing playing), looking them up unless
    /// already shown or being looked up. `retry` asks again after a failure.
    pub(super) fn show(&mut self, track: Option<&Track>, paths: &Paths, retry: bool, cx: &mut Context<Self>) {
        let Some(track) = track else {
            self.track = None;
            self.status = Status::Idle;
            cx.notify();
            return;
        };
        let key = (track.platform, track.source_id.clone());
        let failed = matches!(self.status, Status::Failed(_));
        if self.track.as_ref() == Some(&key) && !(failed && retry) {
            return;
        }
        self.track = Some(key.clone());
        self.key = track.file_key();
        self.status = Status::Loading;
        self.shown = Highlight::default();
        self.centred = None;
        self.target = None;
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
        let (track, paths) = (track.clone(), paths.clone());
        cx.spawn(async move |this, cx| {
            #[cfg(not(target_family = "wasm"))]
            let result = cx.background_spawn(async move { crate::lyrics::for_track(&paths, &track) }).await;
            #[cfg(target_family = "wasm")]
            let result = {
                let _ = paths;
                crate::web::api::lyrics(&track).await
            };
            let _ = this.update(cx, |this, cx| {
                if this.track.as_ref() != Some(&key) {
                    return; // another track started meanwhile
                }
                this.status = match result {
                    Ok(l) => Status::Ready(l.map(Arc::new)),
                    Err(e) => {
                        eprintln!("lyrics lookup failed for {}: {e:#}", key.1);
                        Status::Failed(format!("{e:#}"))
                    }
                };
                crate::mem::trim(); // the responses were parsed JSON pages
                cx.notify();
            });
        })
        .detach();
    }

    /// The lyrics, if they're timed (and so follow the song).
    fn timed(&self) -> Option<&Arc<Lyrics>> {
        match &self.status {
            Status::Ready(Some(l)) if l.synced => Some(l),
            _ => None,
        }
    }

    /// The user's offset for this song.
    fn offset(&self, cx: &App) -> Option<i32> {
        let app = self.app.upgrade()?;
        app.read(cx).library.lyrics_offsets.get(&self.key).copied()
    }

    fn set_offset(&mut self, offset: Option<i32>, cx: &mut Context<Self>) {
        let Some(app) = self.app.upgrade() else { return };
        let offset = offset.map(|o| o.clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS));
        let key = self.key.clone();
        app.update(cx, |app, cx| app.set_lyrics_offset(&key, offset, cx));
        // Back to following the song, at its new place.
        self.user_scrolled_ms = 0.0;
        self.centred = None;
        cx.notify();
    }

    /// Shift the lyrics by one step later (`dir` 1) or earlier (-1).
    fn nudge(&mut self, dir: i32, big: bool, cx: &mut Context<Self>) {
        let step = if big { BIG_NUDGE_MS } else { NUDGE_MS };
        let offset = self.offset(cx).unwrap_or(0) + dir * step;
        self.set_offset(Some(offset), cx);
    }

    /// The line starting at `start_ms` is being sung now: shift the lyrics so it is.
    fn sync_line(&mut self, start_ms: u32, cx: &mut Context<Self>) {
        let Some(app) = self.app.upgrade() else { return };
        let position = app.read(cx).position_ms() as i64;
        self.set_offset(Some((position - start_ms as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32), cx);
    }

    /// Check the position while the page is shown with timed lyrics.
    fn ensure_polling(&mut self, cx: &mut Context<Self>) {
        if self.polling || self.timed().is_none() {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(POLL_MS)).await;
                if !matches!(this.update(cx, |this, cx| this.poll(cx)), Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// One poll: redraw if the highlight moved, keep the line in the middle.
    /// `false` stops polling (page hidden, window closed, untimed lyrics).
    fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        let visible = self.app.upgrade().map(|app| {
            let app = app.read(cx);
            (app.panel == Panel::Lyrics && app.view == View::Playlist && app.window.is_some(), app.position_ms())
        });
        let offset = self.offset(cx);
        let (Some((true, ms)), Some(lyrics)) = (visible, self.timed().cloned()) else {
            self.polling = false;
            return false;
        };
        let now = highlight(&lyrics, ms, offset.unwrap_or(0));
        let mut redraw = now != self.shown;
        if let Some(i) = now.line
            && self.centred != Some(i)
            && crate::now_ms() - self.user_scrolled_ms > FOLLOW_PAUSE_MS
            && let Some(y) = self.centre_offset(i)
        {
            self.centred = Some(i);
            self.target = Some(y);
        }
        if let Some(target) = self.target {
            let current = self.scroll.offset();
            let eased = if cx.reduce_motion() { target } else { current.y + (target - current.y) * 0.3 };
            let done = (target - eased).abs() < px(0.5);
            self.scroll.set_offset(point(current.x, if done { target } else { eased }));
            if done {
                self.target = None;
            }
            redraw = true;
        }
        if redraw {
            cx.notify();
            // The edge moved: sparks need frames again if they had stopped.
            if !self.sparks.read(cx).animating {
                self.sparks.update(cx, |_, cx| cx.notify());
            }
        }
        true
    }

    /// Scroll offset that puts line `i` in the middle (from the last layout).
    fn centre_offset(&self, i: usize) -> Option<Pixels> {
        // Child 0 is the spacer above the lines.
        let line = self.scroll.bounds_for_item(i + 1)?;
        let view = self.scroll.bounds();
        if view.size.height <= px(1.) {
            return None;
        }
        let y = (view.top() + view.size.height / 2.) - (line.top() + line.size.height / 2.);
        let max = self.scroll.max_offset().y.abs();
        Some(y.min(px(0.)).max(-max))
    }

    /// Play from where the lyrics are at `ms` (shifted by the offset).
    fn seek_to(&mut self, ms: u32, cx: &mut Context<Self>) {
        // Follow the song again straight away.
        self.user_scrolled_ms = 0.0;
        self.centred = None;
        let to = (ms as i64 + self.offset(cx).unwrap_or(0) as i64).max(0) as u64;
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |app, cx| app.seek_to(Duration::from_millis(to), cx));
        }
    }
}

/// The highlight at `ms` into the track, with the lyrics `offset` ms later.
fn highlight(lyrics: &Lyrics, ms: u32, offset: i32) -> Highlight {
    let at = ms as i64 + LEAD_MS as i64 - offset as i64;
    let Ok(ms) = u32::try_from(at) else { return Highlight::default() };
    let Some(i) = lyrics.line_at(ms) else { return Highlight::default() };
    let fill = (lyrics.line_progress(i, ms) * FILL_STEPS).round() as u16;
    Highlight { line: Some(i), fill }
}

/// Colours for a line `fill` (0..1) of the way through, left to right: `to`
/// behind the fill, `from` ahead of it, blended across a few characters at
/// its edge. Only the parts not in `from` (the text's own colour).
fn fill_runs(text: &str, fill: f32, from: Hsla, to: Hsla) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    /// Blend levels across the edge (runs of the same level are merged).
    const LEVELS: f32 = 12.0;
    let chars = text.chars().count() as f32;
    // The edge starts before the first character and ends past the last, so
    // 0 and 1 are fully before and fully after.
    let front = fill * (chars + FILL_EDGE);
    let mut runs: Vec<(std::ops::Range<usize>, u8)> = Vec::new();
    for (k, (at, c)) in text.char_indices().enumerate() {
        let level = (((front - k as f32) / FILL_EDGE).clamp(0.0, 1.0) * LEVELS).round() as u8;
        let end = at + c.len_utf8();
        match runs.last_mut() {
            Some((range, l)) if *l == level => range.end = end,
            _ => runs.push((at..end, level)),
        }
    }
    let (from, to) = (Rgba::from(from), Rgba::from(to));
    let mix = |t: f32| Rgba {
        r: from.r + (to.r - from.r) * t,
        g: from.g + (to.g - from.g) * t,
        b: from.b + (to.b - from.b) * t,
        a: from.a + (to.a - from.a) * t,
    };
    runs.into_iter()
        .filter(|(_, level)| *level > 0)
        .map(|(range, level)| {
            (range, HighlightStyle { color: Some(Hsla::from(mix(level as f32 / LEVELS))), ..Default::default() })
        })
        .collect()
}

impl Render for LyricsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(app) = self.app.upgrade() else { return div() };
        let (now, ms, cover, playing) = {
            let app = app.read(cx);
            let playing = app.state == PlayState::Playing && app.audio.as_ref().is_some_and(|a| !a.is_paused());
            (app.now.as_ref().map(|n| n.track.clone()), app.position_ms(), app.now_cover.clone(), playing)
        };
        let offset = self.offset(cx);
        // Set again by the line being sung, if it's drawn.
        self.edge.set(None);
        self.ensure_polling(cx);

        let header = h_flex()
            .flex_none()
            .gap_4()
            .px_6()
            .pt_6()
            .pb_2()
            .when_some(now.as_ref(), |d, _| d.child(thumbs::cover_tile(cover.clone(), px(56.), &theme)))
            .child(match &now {
                Some(t) => v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_xl().font_semibold().truncate().child(t.title.clone()))
                    .child(div().truncate().text_color(theme.muted_foreground).child(t.artist.clone())),
                None => v_flex().flex_1(),
            })
            .when_some(self.source_note(), |d, note| {
                d.child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(note))
            })
            .when_some(self.render_offset(offset, &theme, cx), |d, controls| d.child(controls));

        let message = |icon: IconName, title: SharedString, detail: Option<SharedString>| {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .px_8()
                .text_center()
                .child(Icon::new(icon).large().text_color(theme.muted_foreground))
                .child(div().text_lg().font_medium().child(title))
                .when_some(detail, |d, s| d.child(div().text_sm().text_color(theme.muted_foreground).child(s)))
        };
        let body = match &self.status {
            Status::Idle => message(IconName::MicVocal, "Nothing playing".into(), None).into_any_element(),
            Status::Loading => message(IconName::LoaderCircle, "Looking up lyrics…".into(), None).into_any_element(),
            Status::Failed(e) => {
                message(IconName::CircleAlert, "Couldn't look up lyrics".into(), Some(e.clone().into()))
                    .child(Button::new("lyrics-retry").small().icon(IconName::RefreshCw).label("Try again").on_click(
                        cx.listener(|this, _, _, cx| {
                            let Some(app) = this.app.upgrade() else { return };
                            let (track, paths) = {
                                let app = app.read(cx);
                                (app.now.as_ref().map(|n| n.track.clone()), app.paths.clone())
                            };
                            this.show(track.as_ref(), &paths, true, cx);
                        }),
                    ))
                    .into_any_element()
            }
            Status::Ready(None) => {
                // Say what was asked for: a wrong guess explains a miss.
                let asked =
                    now.as_ref().and_then(|t| crate::lyrics::query::guesses(&t.title, &t.artist).into_iter().next());
                let detail =
                    asked.map(|q| format!("Looked up as \"{}\" by {} on LRCLIB and lrc.red.", q.title, q.artist));
                message(IconName::MicVocal, "No lyrics found for this track".into(), detail.map(Into::into))
                    .into_any_element()
            }
            Status::Ready(Some(l)) if l.instrumental => {
                message(IconName::AudioLines, "Instrumental".into(), None).into_any_element()
            }
            Status::Ready(Some(l)) => self.render_lines(l.clone(), ms, offset, playing, &theme, cx).into_any_element(),
        };
        div().size_full().child(v_flex().size_full().child(header).child(body))
    }
}

impl LyricsView {
    /// "From LRCLIB", noting untimed lyrics and timings for another cut.
    fn source_note(&self) -> Option<String> {
        let Status::Ready(Some(l)) = &self.status else { return None };
        let source = l.source.label();
        match (l.synced, l.other_cut) {
            _ if l.instrumental => None,
            (false, _) => Some(format!("Lyrics from {source} · not timed")),
            (true, None) => Some(format!("Lyrics from {source}")),
            (true, Some(d)) => Some(format!(
                "Lyrics from {source} · timed for a version {} s {}",
                d.abs(),
                if d > 0 { "longer" } else { "shorter" }
            )),
        }
    }

    /// The timing offset: − / +, how much, and a reset (timed lyrics only).
    fn render_offset(
        &self,
        offset: Option<i32>,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let Status::Ready(Some(l)) = &self.status else { return None };
        if !l.synced || l.instrumental {
            return None;
        }
        let label = match offset {
            None => "0.0 s".to_string(),
            Some(o) => format!(
                "{}{:.1} s",
                if o > 0 {
                    "+"
                } else if o < 0 {
                    "−"
                } else {
                    ""
                },
                o.abs() as f32 / 1000.0
            ),
        };
        let step =
            |id: &'static str, icon: IconName, dir: i32, tip: &'static str| {
                Button::new(id).ghost().xsmall().icon(icon).tooltip(tip).on_click(
                    cx.listener(move |this, ev: &ClickEvent, _, cx| this.nudge(dir, ev.modifiers().shift, cx)),
                )
            };
        Some(
            h_flex()
                .flex_none()
                .gap_0p5()
                .child(step("lyrics-earlier", IconName::Minus, -1, "Lyrics earlier (Shift: 5 s)"))
                .child(
                    div()
                        .id("lyrics-offset")
                        .min_w(px(64.))
                        .text_center()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label)
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(
                                "Lyrics timing for this song. Right-click a line as it's sung to sync to it.",
                            )
                            .build(window, cx)
                        }),
                )
                .child(step("lyrics-later", IconName::Plus, 1, "Lyrics later (Shift: 5 s)"))
                .when(offset.is_some(), |d| {
                    d.child(
                        Button::new("lyrics-offset-reset")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Reset timing")
                            .on_click(cx.listener(|this, _, _, cx| this.set_offset(None, cx))),
                    )
                }),
        )
    }

    fn render_lines(
        &mut self,
        lyrics: Arc<Lyrics>,
        ms: u32,
        offset: Option<i32>,
        playing: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let now = if lyrics.synced { highlight(&lyrics, ms, offset.unwrap_or(0)) } else { Highlight::default() };
        self.shown = now;
        let current = now.line;
        // Room above the first line and below the last, so both can sit in
        // the middle (the view's height from the last layout).
        let spacer = || div().flex_none().h((self.scroll.bounds().size.height * 0.45).max(px(24.)));
        let (fg, muted, accent) = (theme.foreground, theme.muted_foreground, theme.primary);
        let lines = lyrics.lines.iter().enumerate().map(|(i, line)| {
            let start = line.start_ms;
            let mut base = div().id(("lyric", i)).flex_none().w_full().py_1();
            if lyrics.synced {
                // Right-click a line as it's sung: the lyrics follow from there.
                base = base.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| this.sync_line(start, cx)),
                );
            }
            if !lyrics.synced {
                return if line.is_label() {
                    base.pt_3().text_sm().font_medium().text_color(muted).child(line.text.clone())
                } else {
                    base.py_0p5().text_lg().text_color(fg).child(if line.text.is_empty() {
                        " ".to_string()
                    } else {
                        line.text.clone()
                    })
                };
            }
            // The line being sung stands out: what's still to come in it is
            // dimmed, so the accent fill reads clearly.
            let unsung = fg.opacity(0.6);
            let colour = match current {
                Some(c) if i == c => unsung,
                Some(c) if i < c => muted.opacity(0.5),
                _ => muted.opacity(0.85),
            };
            // An empty timed line is an instrumental break.
            let text = if line.text.trim().is_empty() { "♪".to_string() } else { line.text.clone() };
            let base = base
                .relative()
                .text_2xl()
                .font_semibold()
                .text_color(colour)
                .cursor_pointer()
                .hover(|s| s.text_color(fg))
                .on_click(cx.listener(move |this, _, _, cx| this.seek_to(start, cx)));
            if current != Some(i) {
                return base.child(StyledText::new(text));
            }
            let fill = now.fill as f32 / FILL_STEPS;
            let edge_at = fill_edge(&text, fill);
            let styled = StyledText::new(text.clone()).with_highlights(fill_runs(&text, fill, unsung, accent));
            // Record where the edge is drawn, once the text is laid out (a
            // sibling after it is prepainted after it).
            let layout = styled.layout().clone();
            let edge = self.edge.clone();
            let sparks = edge_at.filter(|_| playing && !cx.reduce_motion());
            base.child(styled).child(
                canvas(
                    move |_, _, _| {
                        let Some((ix, next_ix, t)) = sparks else { return };
                        let (Some(a), Some(b)) = (layout.position_for_index(ix), layout.position_for_index(next_ix))
                        else {
                            return;
                        };
                        // Between two characters on the same row; else at the first.
                        let x = if a.y == b.y { a.x + (b.x - a.x) * t } else { a.x };
                        let (height, drawn_ms) = (layout.line_height(), crate::now_ms());
                        edge.set(Some(Edge { at: point(x, a.y), height, colour: accent, drawn_ms }));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_0(),
            )
        });
        let sparks = self.sparks.clone();
        let list = div()
            .id("lyrics")
            .size_full()
            .flex()
            .flex_col()
            .px_8()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_scroll(cx.listener(|this, _: &ScrollEvent, _, _| {
                this.user_scrolled_ms = crate::now_ms();
                this.target = None;
            }))
            .child(spacer())
            .children(lines)
            .child(spacer());
        // The sparks are drawn over the lines; with no listeners of their
        // own, clicks and scrolling reach the lines.
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .child(list)
            .child(div().absolute().inset_0().child(sparks))
    }
}

/// The edge of the fill as last drawn, in window coordinates.
#[derive(Clone, Copy)]
struct Edge {
    at: Point<Pixels>,
    height: Pixels,
    colour: Hsla,
    drawn_ms: f64,
}

/// Where the edge of a fill drawn by [`fill_runs`] is: between the characters
/// at bytes `a` and `b`, `t` of the way. `None` before it starts or once done.
fn fill_edge(text: &str, fill: f32) -> Option<(usize, usize, f32)> {
    if fill <= 0.0 || fill >= 1.0 {
        return None;
    }
    let chars = text.chars().count();
    // The middle of the blended edge, in characters.
    let mid = (fill * (chars as f32 + FILL_EDGE) - FILL_EDGE / 2.0).clamp(0.0, chars as f32);
    let k = mid.floor() as usize;
    let byte = |k: usize| text.char_indices().nth(k).map_or(text.len(), |(i, _)| i);
    Some((byte(k), byte((k + 1).min(chars)), mid - k as f32))
}

/// Sparks per second while the edge moves.
const SPARK_RATE: f32 = 45.0;
const MAX_SPARKS: usize = 90;
/// An edge not redrawn for this long has stopped (paused, or a held note).
const EDGE_STALE_MS: f64 = 300.0;

struct Spark {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    age: f32,
    life: f32,
    size: f32,
}

/// Sparks drifting off the edge of the fill. Its own view, drawn every frame
/// only while sparks are alive, so the lyrics under it aren't redrawn for them.
pub(super) struct Sparks {
    edge: Rc<Cell<Option<Edge>>>,
    sparks: Vec<Spark>,
    colour: Hsla,
    last_frame_ms: f64,
    /// Fractional sparks owed to the spawn rate.
    debt: f32,
    rng: u32,
    animating: bool,
}

impl Sparks {
    fn new(edge: Rc<Cell<Option<Edge>>>) -> Self {
        Self {
            edge,
            sparks: Vec::new(),
            colour: hsla(0., 0., 1., 1.),
            last_frame_ms: crate::now_ms(),
            debt: 0.0,
            rng: (crate::now_ms() as u32) | 1,
            animating: false,
        }
    }

    fn random(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Move the sparks on and add new ones at a moving edge. Whether anything
    /// is left to animate.
    fn advance(&mut self) -> bool {
        let now = crate::now_ms();
        let dt = (((now - self.last_frame_ms) / 1000.0) as f32).clamp(0.0, 0.1);
        self.last_frame_ms = now;
        for s in &mut self.sparks {
            s.age += dt;
            s.x += s.vx * dt;
            s.y += s.vy * dt;
            // They rise, slowing down.
            s.vy *= 1.0 - 1.8 * dt;
        }
        self.sparks.retain(|s| s.age < s.life);
        match self.edge.get().filter(|e| now - e.drawn_ms < EDGE_STALE_MS) {
            Some(edge) => {
                self.colour = edge.colour;
                self.debt = (self.debt + dt * SPARK_RATE).min(4.0);
                let (x, y, h) = (f32::from(edge.at.x), f32::from(edge.at.y), f32::from(edge.height));
                while self.debt >= 1.0 && self.sparks.len() < MAX_SPARKS {
                    self.debt -= 1.0;
                    let r: [f32; 6] = std::array::from_fn(|_| self.random());
                    self.sparks.push(Spark {
                        x: x + (r[0] - 0.5) * 6.0,
                        y: y + h * (0.2 + 0.6 * r[1]),
                        vx: -12.0 + 30.0 * r[2],
                        vy: -(18.0 + 40.0 * r[3]),
                        age: 0.0,
                        life: 0.45 + 0.45 * r[4],
                        size: 1.5 + 2.0 * r[5],
                    });
                }
            }
            None => self.debt = 0.0,
        }
        !self.sparks.is_empty() || self.edge.get().is_some_and(|e| now - e.drawn_ms < EDGE_STALE_MS)
    }
}

impl Render for Sparks {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.animating = self.advance();
        if self.animating {
            window.request_animation_frame();
        }
        let colour = self.colour;
        let sparks: Vec<(Point<Pixels>, f32, f32)> =
            self.sparks.iter().map(|s| (point(px(s.x), px(s.y)), s.size, 1.0 - s.age / s.life)).collect();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                for (at, size, left) in sparks {
                    let s = px(size * (0.5 + 0.5 * left));
                    let bounds = Bounds::new(at - point(s / 2., s / 2.), gpui_kit::size(s, s));
                    window.paint_quad(fill(bounds, colour.opacity(0.85 * left * left)).corner_radii(s / 2.));
                }
            },
        )
        .size_full()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` macro would replace the built-in one.
    use super::{LEAD_MS, fill_edge, fill_runs, highlight};
    use crate::lyrics::{Line, Lyrics, Source};
    use gpui_kit::hsla;

    #[test]
    fn fill_moves_left_to_right() {
        let (from, to) = (hsla(0., 0., 1., 1.), hsla(0., 0., 0., 1.));
        let text = "Héllo world";
        assert!(fill_runs(text, 0.0, from, to).is_empty());
        let full = fill_runs(text, 1.0, from, to);
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].0, 0..text.len());
        // Halfway: the start is done, the end not reached, the edge blended.
        let half = fill_runs(text, 0.5, from, to);
        assert_eq!(half.first().unwrap().0.start, 0);
        assert!(half.last().unwrap().0.end < text.len());
        let lightness: Vec<f32> = half.iter().map(|(_, s)| s.color.unwrap().l).collect();
        assert!(lightness.windows(2).all(|w| w[0] <= w[1]), "darkest (most filled) first: {lightness:?}");
        assert!(half.iter().all(|(r, _)| text.is_char_boundary(r.start) && text.is_char_boundary(r.end)));
    }

    #[test]
    fn fill_edge_is_mid_blend() {
        assert_eq!(fill_edge("abcd", 0.0), None);
        assert_eq!(fill_edge("abcd", 1.0), None);
        // 4 characters and a 4-character blend: halfway is at the third.
        assert_eq!(fill_edge("abcd", 0.5), Some((2, 3, 0.0)));
        // Byte offsets around a two-byte character, halfway between them.
        let (a, b, t) = fill_edge("héllo", 3.5 / 9.0).unwrap();
        assert_eq!((a, b), (1, 3));
        assert!((t - 0.5).abs() < 1e-4);
    }

    #[test]
    fn offset_shifts_the_highlight() {
        let line = |start_ms: u32| Line { start_ms, end_ms: None, text: "la".into(), words: Vec::new() };
        let l = Lyrics::timed(Source::Lrclib, vec![line(1000), line(5000)], None);
        let at = |ms: u32, offset: i32| highlight(&l, ms - LEAD_MS, offset).line;
        assert_eq!(at(1000, 0), Some(0));
        // A 20 s intro: the first line comes at 21 s.
        assert_eq!(at(1000, 20_000), None);
        assert_eq!(at(20_999, 20_000), None);
        assert_eq!(at(21_000, 20_000), Some(0));
        assert_eq!(at(25_000, 20_000), Some(1));
        // Earlier: the lyrics are ahead of the track.
        assert_eq!(at(100, -900), Some(0));
        assert_eq!(at(4100, -900), Some(1));
    }
}

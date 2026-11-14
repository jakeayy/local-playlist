//! Track list thumbnails: small covers loaded when their row is shown, a few at
//! a time, cached on disk (see `crate::cover`).
//!
//! Only a bounded number stay decoded in memory: GPUI also keeps a copy of
//! every image it has drawn in its texture atlas until told to drop it, so a
//! long scroll through a big playlist would otherwise grow without limit.
//! Past [`MAX_READY`], the least recently shown thumbnails that aren't in view
//! are dropped (from both); they're decoded again from the disk cache when
//! their row comes back. Rows only scrolled past aren't loaded at all.
//!
//! "In view" comes from the list's scroll position, not from which rows were
//! rendered: GPUI can keep drawing rows from earlier frames without rendering
//! them again.

use std::collections::HashMap;
use std::ops::Range;

use super::*;

/// Downloads at once.
const PARALLEL: usize = 4;
/// Decoded size (shown at 36 px; sharp on 2× screens). ~20 KB each.
const THUMB_PX: u32 = 72;
/// Decoded thumbnails kept: a few screens' worth.
const MAX_READY: usize = 300;
/// Evicting goes down to this many, so it doesn't run for every new thumbnail.
const EVICT_TO: usize = MAX_READY * 3 / 4;
/// Wait before retrying a failed thumbnail (e.g. while offline).
const RETRY_MS: f64 = 5.0 * 60.0 * 1000.0;

type Key = (Platform, String);

enum State {
    Queued,
    Loading,
    Ready(Arc<RenderImage>),
    Failed { at: f64 },
}

struct Entry {
    state: State,
    /// When the thumbnail was last asked for (a [`Thumbs::clock`] value).
    used: u64,
}

#[derive(Default)]
pub struct Thumbs {
    /// Per platform (see [`slot`]), by source id: looked up by `&str`, so
    /// without allocating, for every row drawn.
    entries: [HashMap<String, Entry>; 2],
    /// Waiting to load, newest last (loaded first).
    queue: Vec<Key>,
    running: usize,
    ready: usize,
    /// Counts lookups, to order entries by last use.
    clock: u64,
}

impl Thumbs {
    fn get_mut(&mut self, (platform, id): &Key) -> Option<&mut Entry> {
        self.entries[slot(*platform)].get_mut(id.as_str())
    }

    fn remove(&mut self, (platform, id): &Key) -> Option<Entry> {
        self.entries[slot(*platform)].remove(id.as_str())
    }
}

fn slot(platform: Platform) -> usize {
    match platform {
        Platform::YouTube => 0,
        Platform::SoundCloud => 1,
    }
}

impl LocalPlaylist {
    /// The track's thumbnail if loaded; otherwise starts loading it.
    pub(super) fn thumb(&mut self, track: &Track, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        self.thumbs.clock += 1;
        let used = self.thumbs.clock;
        let map = &mut self.thumbs.entries[slot(track.platform)];
        match map.get_mut(track.source_id.as_str()) {
            Some(entry) => {
                entry.used = used;
                match &entry.state {
                    State::Ready(img) => return Some(img.clone()),
                    State::Queued | State::Loading => return None,
                    State::Failed { at } if crate::now_ms() - at < RETRY_MS => return None,
                    State::Failed { .. } => entry.state = State::Queued,
                }
            }
            None => {
                map.insert(track.source_id.clone(), Entry { state: State::Queued, used });
            }
        }
        self.thumbs.queue.push((track.platform, track.source_id.clone()));
        self.pump_thumbs(cx);
        None
    }

    /// Track list rows in view, with a screen's worth either side.
    fn rows_in_view(&self) -> Range<usize> {
        let state = self.list_scroll.0.borrow();
        let row = state.last_item_size.map(|s| s.item.height).filter(|h| *h > px(1.)).unwrap_or(px(48.));
        let top = (-state.base_handle.offset().y).max(px(0.));
        // Before the list's first layout its height is unknown (0): assume a tall one.
        let height = state.base_handle.bounds().size.height.max(px(1440.));
        let first = (top / row).floor() as usize;
        let screen = (height / row).ceil() as usize + 1;
        first.saturating_sub(screen)..first + 2 * screen
    }

    /// Whether `key`'s thumbnail may be on screen: in or near the track list's
    /// view, or the playing track (the player bar shows it until the full
    /// cover has loaded).
    fn thumb_in_view(&self, (platform, id): &Key, rows: &Range<usize>) -> bool {
        let is = |t: &Track| t.platform == *platform && t.source_id == *id;
        if self.now.as_ref().is_some_and(|n| is(&n.track)) {
            return true;
        }
        let Some(p) = self.selected_playlist() else { return false };
        rows.clone()
            .filter_map(|row| match &self.list_rows {
                Some(rows) => rows.get(row).copied(),
                None => Some(row),
            })
            .filter_map(|ix| p.tracks.get(ix))
            .any(is)
    }

    fn pump_thumbs(&mut self, cx: &mut Context<Self>) {
        if self.thumbs.queue.is_empty() {
            return;
        }
        let rows = self.rows_in_view();
        while self.thumbs.running < PARALLEL {
            let Some(key) = self.thumbs.queue.pop() else { return };
            if !self.thumbs.get_mut(&key).is_some_and(|e| matches!(e.state, State::Queued)) {
                continue;
            }
            if !self.thumb_in_view(&key, &rows) {
                // Scrolled away before its turn: forget it; it's queued again if
                // its row comes back.
                self.thumbs.remove(&key);
                continue;
            }
            if let Some(entry) = self.thumbs.get_mut(&key) {
                entry.state = State::Loading;
            }
            self.thumbs.running += 1;
            let paths = self.paths.clone();
            cx.spawn(async move |this, cx| {
                let (platform, id) = key.clone();
                let res = fetch_cover(cx, paths, platform, id, crate::cover::Size::Small, THUMB_PX, false).await;
                let _ = this.update(cx, |this, cx| {
                    this.thumbs.running -= 1;
                    let state = match res {
                        Ok((img, _)) => {
                            this.thumbs.ready += 1;
                            State::Ready(img)
                        }
                        Err(_) => State::Failed { at: crate::now_ms() },
                    };
                    match this.thumbs.get_mut(&key) {
                        Some(entry) => entry.state = state,
                        None => {
                            let used = this.thumbs.clock;
                            this.thumbs.entries[slot(key.0)].insert(key.1, Entry { state, used });
                        }
                    }
                    this.evict_thumbs(cx);
                    this.pump_thumbs(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Past [`MAX_READY`], drop the least recently used thumbnails that aren't
    /// in view, from memory and from GPUI's atlas. Runs from a task, never
    /// while a window is being drawn, so `drop_image` reaches every window.
    fn evict_thumbs(&mut self, cx: &mut Context<Self>) {
        if self.thumbs.ready <= MAX_READY {
            return;
        }
        let rows = self.rows_in_view();
        let mut old: Vec<(u64, Key)> = [Platform::YouTube, Platform::SoundCloud]
            .into_iter()
            .flat_map(|p| self.thumbs.entries[slot(p)].iter().map(move |(id, e)| (p, id, e)))
            .filter(|(_, _, e)| matches!(e.state, State::Ready(_)))
            .map(|(p, id, e)| (e.used, (p, id.clone())))
            .collect();
        old.retain(|(_, key)| !self.thumb_in_view(key, &rows));
        old.sort_unstable_by_key(|(used, _)| *used);
        let excess = self.thumbs.ready - EVICT_TO;
        for (_, key) in old.into_iter().take(excess) {
            if let Some(Entry { state: State::Ready(img), .. }) = self.thumbs.remove(&key) {
                self.thumbs.ready -= 1;
                cx.drop_image(img, None);
            }
        }
        crate::mem::trim();
    }
}

/// A square cover (or a placeholder while there's none) with rounded corners.
pub(super) fn cover_tile(image: Option<Arc<RenderImage>>, size: Pixels, theme: &gpui_kit::component::Theme) -> Div {
    let tile = div().size(size).flex_none().rounded_md().overflow_hidden().bg(theme.muted);
    match image {
        Some(image) => tile.child(img(image).size_full()),
        None => tile
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(IconName::Disc3).small().text_color(theme.muted_foreground.opacity(0.6))),
    }
}

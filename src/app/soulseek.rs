//! "Find on Soulseek": search the Soulseek network for a track by its
//! artist and title, pick a file, and play that instead of the track's own
//! audio (desktop). The track's own file is kept, so "Restore original"
//! puts it back (`storage::replace_audio`).
//!
//! Soulseek is peer to peer and expects users to share, which this app
//! doesn't, so the first Soulseek screen, and logging in, ask the user to
//! accept a warning first (`Settings::soulseek_accepted`). The connection
//! is opened only for a search and closed again once the page is left and no
//! download is running.

use super::*;
use crate::model::SoulseekFile;
use crate::secrets::SoulseekLogin;
use crate::soulseek::{Client, SearchResult, Transfer, TransferState};

/// Text inputs of the Soulseek page and its Settings card (made per window).
pub(super) struct SoulseekInputs {
    query: Entity<InputState>,
    user: Entity<InputState>,
    pass: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
pub(super) struct SoulseekUi {
    client: Option<Arc<Client>>,
    /// Logging in or starting a search.
    busy: bool,
    error: Option<String>,
    /// The track the page looks for: (playlist id, track id).
    page: Option<(String, String)>,
    results: Rc<Vec<SearchResult>>,
    /// (results, users) when `results` was last copied.
    counts: (usize, usize),
    /// When the current search started (ms).
    searched_at: Option<f64>,
    picks: Vec<Pick>,
    scroll: UniformListScrollHandle,
    pub(super) inputs: Option<SoulseekInputs>,
}

/// A file being downloaded to replace a track's audio.
struct Pick {
    playlist_id: String,
    track_id: String,
    file: SoulseekFile,
    transfer: Arc<Transfer>,
    last: TransferState,
}

/// What to do once the warning is accepted.
#[derive(Clone)]
enum Next {
    Login,
    Search { playlist_id: String, track_id: String },
}

/// How long the page says "Searching…" (results keep arriving for a while).
const SEARCH_SHOWN_MS: f64 = 20_000.0;
/// A file this much longer or shorter than the track is probably another version.
const LENGTH_SLACK_SECS: u32 = 8;

impl LocalPlaylist {
    // ------------------------------------------------------------ warning

    /// Run `next`, after the user accepted the Soulseek warning (asked once).
    fn soulseek_gate(&mut self, next: Next, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.settings.soulseek_accepted {
            self.soulseek_go(next, window, cx);
            return;
        }
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let theme = cx.theme();
            let point = |text: &'static str| {
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_start()
                    .child(div().flex_none().text_color(theme.muted_foreground).child("•"))
                    .child(div().flex_1().min_w_0().child(text))
            };
            let (this, next) = (this.clone(), next.clone());
            alert
                .icon(Icon::new(IconName::TriangleAlert).text_color(theme.warning))
                .title("Soulseek is a peer-to-peer network")
                .description(
                    v_flex()
                        .w_full()
                        .mt_1()
                        .gap_2()
                        .text_sm()
                        .text_left()
                        .child(point(
                            "Files come straight from other people's computers. They, and the Soulseek server, see your IP address.",
                        ))
                        .child(point(
                            "Soulseek expects everyone to share their music back. Local Playlist shares nothing, so some users won't send you files.",
                        ))
                        .child(point(
                            "Users, or Soulseek's operators, can ban you for not sharing (\"leeching\"). A banned account can't download from them.",
                        ))
                        .child(point(
                            "Much of what's shared there is copyrighted. Downloading it may be illegal where you live.",
                        )),
                )
                .width(px(520.))
                .confirm()
                .ok_text("I understand")
                .cancel_text("Not now")
                .on_ok(move |_, window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.library.settings.soulseek_accepted = true;
                        this.save();
                        this.soulseek_go(next.clone(), window, cx);
                    });
                    true
                })
        });
    }

    fn soulseek_go(&mut self, next: Next, window: &mut Window, cx: &mut Context<Self>) {
        match next {
            Next::Login => self.soulseek_login(window, cx),
            Next::Search { playlist_id, track_id } => self.show_soulseek_page(playlist_id, track_id, window, cx),
        }
    }

    // ------------------------------------------------------------- inputs

    /// The page's inputs, made for this window on first use.
    fn soulseek_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> &SoulseekInputs {
        if self.slsk.inputs.is_none() {
            let query = cx.new(|cx| InputState::new(window, cx).placeholder("Artist - Title"));
            let user = cx.new(|cx| InputState::new(window, cx).placeholder("Soulseek username"));
            let pass = cx.new(|cx| InputState::new(window, cx).placeholder("Password").masked(true));
            let subscriptions = vec![
                cx.subscribe_in(&query, window, |this, _, ev: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = ev {
                        this.soulseek_search(window, cx);
                    }
                }),
                cx.subscribe_in(&pass, window, |this, _, ev: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = ev {
                        this.soulseek_gate(Next::Login, window, cx);
                    }
                }),
            ];
            self.slsk.inputs = Some(SoulseekInputs { query, user, pass, _subscriptions: subscriptions });
        }
        self.slsk.inputs.as_ref().unwrap()
    }

    // -------------------------------------------------------------- login

    fn soulseek_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.slsk.busy {
            return;
        }
        let inputs = self.soulseek_inputs(window, cx);
        let username = inputs.user.read(cx).value().trim().to_string();
        let password = inputs.pass.read(cx).value().to_string();
        if username.is_empty() || password.is_empty() {
            window.push_notification(Notification::warning("Enter a Soulseek username and password."), cx);
            return;
        }
        self.slsk.busy = true;
        self.slsk.error = None;
        cx.notify();
        let (u, p) = (username.clone(), password.clone());
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { Client::login(&u, &p) }).await;
            Self::finish(&this, cx, move |this, mut window, cx| {
                this.slsk.busy = false;
                match result {
                    Ok(client) => {
                        this.slsk.client = Some(Arc::new(client));
                        this.creds.soulseek = Some(SoulseekLogin { username: username.clone(), password });
                        this.persist_creds();
                        if let (Some(window), Some(inputs)) = (window.as_deref_mut(), this.slsk.inputs.as_ref()) {
                            inputs.pass.update(cx, |i, cx| i.set_value("", window, cx));
                        }
                        this.note(window, Notification::success(format!("Logged in to Soulseek as {username}")), cx);
                    }
                    Err(e) => this.note(window, Notification::warning(format!("{e:#}")), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn soulseek_logout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for pick in self.slsk.picks.drain(..) {
            pick.transfer.cancel();
        }
        self.slsk.client = None;
        self.slsk.results = Rc::default();
        self.slsk.counts = (0, 0);
        self.creds.soulseek = None;
        self.persist_creds();
        window.push_notification(Notification::info("Logged out of Soulseek (login removed from this device)"), cx);
        cx.notify();
    }

    // ------------------------------------------------------------- search

    /// "Find on Soulseek" for a track (asks to accept the warning first).
    pub(super) fn open_soulseek(
        &mut self,
        playlist_id: &str,
        track_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = Next::Search { playlist_id: playlist_id.into(), track_id: track_id.into() };
        self.soulseek_gate(next, window, cx);
    }

    fn show_soulseek_page(
        &mut self,
        playlist_id: String,
        track_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(track) = self.library.playlist(&playlist_id).and_then(|p| p.tracks.iter().find(|t| t.id == track_id))
        else {
            return;
        };
        let query = crate::soulseek::query_for(&track.artist, &track.title);
        let same = self.slsk.page.as_ref() == Some(&(playlist_id.clone(), track_id.clone()));
        self.view = View::Soulseek;
        if same && !self.slsk.results.is_empty() {
            cx.notify();
            return;
        }
        let inputs = self.soulseek_inputs(window, cx);
        inputs.query.update(cx, |i, cx| i.set_value(query, window, cx));
        self.slsk.page = Some((playlist_id, track_id));
        self.slsk.results = Rc::default();
        self.slsk.counts = (0, 0);
        self.slsk.error = None;
        self.slsk.searched_at = None;
        if self.creds.soulseek.is_some() {
            self.soulseek_search(window, cx);
        }
        cx.notify();
    }

    fn soulseek_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.slsk.busy {
            return;
        }
        let Some(login) = self.creds.soulseek.clone() else {
            window.push_notification(Notification::warning("Log in to Soulseek first (Settings → Accounts)."), cx);
            return;
        };
        let text = self.soulseek_inputs(window, cx).query.read(cx).value().to_string();
        let client = self.slsk.client.clone().filter(|c| !c.is_closed());
        self.slsk.busy = true;
        self.slsk.error = None;
        self.slsk.results = Rc::default();
        self.slsk.counts = (0, 0);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let client = match client {
                        Some(c) => c,
                        None => Arc::new(Client::login(&login.username, &login.password)?),
                    };
                    client.search(&text)?;
                    anyhow::Ok(client)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.slsk.busy = false;
                match result {
                    Ok(client) => {
                        this.slsk.client = Some(client);
                        this.slsk.searched_at = Some(crate::now_ms());
                    }
                    Err(e) => this.slsk.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -------------------------------------------------------------- picks

    fn soulseek_pick(&mut self, result: &SearchResult, window: &mut Window, cx: &mut Context<Self>) {
        let Some((playlist_id, track_id)) = self.slsk.page.clone() else { return };
        let Some(client) = self.slsk.client.clone() else { return };
        let Some(playlist) = self.library.playlist(&playlist_id) else { return };
        let Some(track) = playlist.tracks.iter().find(|t| t.id == track_id) else { return };
        let dir = storage::playlist_dir(&self.music_root(), playlist);
        let key = track.file_key();
        if self.downloads.contains_key(&dir.join(&key)) {
            window.push_notification(
                Notification::warning("This track is downloading from its own site; try again once it's done."),
                cx,
            );
            return;
        }
        if self.slsk.picks.iter().any(|p| p.track_id == track_id) {
            return;
        }
        let tmp = dir.join(storage::DOWNLOADING_DIR);
        if let Err(e) = std::fs::create_dir_all(&tmp) {
            window.push_notification(Notification::error(format!("Couldn't create {}: {e}", tmp.display())), cx);
            return;
        }
        let dest = tmp.join(format!("{key}.soulseek.{}", result.ext));
        let transfer = client.download(result, dest);
        let file = SoulseekFile { user: result.user.clone(), path: result.path(), quality: result.quality_label() };
        self.slsk.picks.push(Pick { playlist_id, track_id, file, transfer, last: TransferState::Connecting });
        cx.notify();
    }

    /// Follow searches and downloads (from the app's tick).
    pub(super) fn soulseek_tick(&mut self, cx: &mut Context<Self>) {
        if let Some(client) = &self.slsk.client {
            if self.view == View::Soulseek {
                let counts = client.result_count();
                if counts != self.slsk.counts {
                    self.slsk.counts = counts;
                    self.slsk.results = Rc::new(client.results());
                    cx.notify();
                }
                // The "Searching…" label ends.
                if self.slsk.searched_at.is_some_and(|at| crate::now_ms() - at > SEARCH_SHOWN_MS) {
                    self.slsk.searched_at = None;
                    cx.notify();
                }
            } else if self.slsk.picks.is_empty() {
                // Nothing left to do on the network: leave it.
                self.slsk.client = None;
            }
        }
        if self.slsk.client.as_ref().is_some_and(|c| c.is_closed()) && self.slsk.picks.is_empty() {
            self.slsk.client = None;
        }

        let mut i = 0;
        while i < self.slsk.picks.len() {
            let state = self.slsk.picks[i].transfer.state();
            if state != self.slsk.picks[i].last {
                self.slsk.picks[i].last = state.clone();
                cx.notify();
            }
            match state {
                TransferState::Done => {
                    let pick = self.slsk.picks.remove(i);
                    self.apply_pick(pick, cx);
                }
                TransferState::Failed(why) => {
                    let pick = self.slsk.picks.remove(i);
                    let _ = std::fs::remove_file(&pick.transfer.dest);
                    if why != "Cancelled" {
                        self.note(None, Notification::warning(format!("Soulseek download failed: {why}")), cx);
                    }
                }
                _ => i += 1,
            }
        }
    }

    /// A picked file arrived: play it instead of the track's own audio.
    fn apply_pick(&mut self, pick: Pick, cx: &mut Context<Self>) {
        let root = self.music_root();
        let Some(playlist) = self.library.playlist(&pick.playlist_id) else { return };
        let Some(track) = playlist.tracks.iter().find(|t| t.id == pick.track_id) else {
            let _ = std::fs::remove_file(&pick.transfer.dest);
            return;
        };
        let dir = storage::playlist_dir(&root, playlist);
        let key = track.file_key();
        let keep_current = track.soulseek.is_none();
        let playing = self.release_if_playing(&pick.track_id);
        let result = storage::replace_audio(&dir, &key, &pick.transfer.dest, keep_current);
        let _ = std::fs::remove_dir(dir.join(storage::DOWNLOADING_DIR));
        match result {
            Ok(_) => {
                if let Some(t) = self.library.playlist_mut(&pick.playlist_id).and_then(|p| p.track_mut(&pick.track_id))
                {
                    t.soulseek = Some(pick.file.clone());
                }
                self.save();
                let msg = format!("Now using {} from Soulseek ({})", file_name(&pick.file.path), pick.file.quality);
                self.note(None, Notification::success(msg), cx);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&pick.transfer.dest);
                self.note(None, Notification::error(format!("Couldn't use the Soulseek file: {e:#}")), cx);
            }
        }
        self.on_disk.clear();
        self.download_errors.remove(&dir.join(&key));
        if playing {
            self.play(&pick.playlist_id, &pick.track_id, cx);
        }
        cx.notify();
    }

    /// Stop playing `track_id` if it's playing, so its file can be swapped
    /// (Windows can't move an open file). Returns whether it was playing.
    fn release_if_playing(&mut self, track_id: &str) -> bool {
        let playing = self.state == PlayState::Playing && self.now.as_ref().is_some_and(|n| n.track.id == track_id);
        if playing && let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        playing
    }

    /// Put a replaced track's own audio back.
    pub(super) fn restore_original_audio(
        &mut self,
        playlist_id: &str,
        track_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.music_root();
        let Some(playlist) = self.library.playlist(playlist_id) else { return };
        let Some(track) = playlist.tracks.iter().find(|t| t.id == track_id) else { return };
        if track.soulseek.is_none() {
            return;
        }
        let (dir, key, platform) = (storage::playlist_dir(&root, playlist), track.file_key(), track.platform);
        let playing = self.release_if_playing(track_id);
        match storage::restore_original(&dir, &key) {
            Ok(kept) => {
                if let Some(t) = self.library.playlist_mut(playlist_id).and_then(|p| p.track_mut(track_id)) {
                    t.soulseek = None;
                }
                self.save();
                let msg = if kept {
                    format!("Restored the original from {}", platform.label())
                } else {
                    format!("Removed the Soulseek file; the original downloads from {} when played", platform.label())
                };
                window.push_notification(Notification::success(msg), cx);
            }
            Err(e) => window.push_notification(Notification::error(format!("Couldn't restore: {e:#}")), cx),
        }
        self.on_disk.clear();
        if playing {
            self.play(playlist_id, track_id, cx);
        }
        cx.notify();
    }

    // ---------------------------------------------------------- rendering

    /// The Soulseek account in Settings → Accounts.
    pub(super) fn render_soulseek_account(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let inputs = self.soulseek_inputs(window, cx);
        let (user, pass) = (inputs.user.clone(), inputs.pass.clone());
        let login = self.creds.soulseek.as_ref().map(|l| l.username.clone());
        let busy = self.slsk.busy;
        v_flex()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_3()
                    .child(soulseek_badge(&theme))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(if login.is_some() { theme.success } else { theme.muted_foreground })
                            .child(match &login {
                                Some(name) => format!("Logged in as {name}"),
                                None => "Not logged in".into(),
                            }),
                    )
                    .when(login.is_some(), |d| {
                        d.child(
                            Button::new("logout-slsk")
                                .small()
                                .icon(IconName::LogOut)
                                .label("Log out")
                                .on_click(cx.listener(|this, _, window, cx| this.soulseek_logout(window, cx))),
                        )
                    }),
            )
            .when(login.is_none(), |d| {
                d.child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&user).small()))
                        .child(div().flex_1().child(Input::new(&pass).small()))
                        .child(
                            Button::new("login-slsk")
                                .primary()
                                .small()
                                .icon(IconName::LogIn)
                                .label(if busy { "Logging in…" } else { "Log in" })
                                .loading(busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.soulseek_gate(Next::Login, window, cx)),
                                ),
                        ),
                )
            })
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                "For \"Find on Soulseek\" on a track: a peer-to-peer network where files come from other users. \
                 A username nobody has taken yet creates a new account. Soulseek logs in with the password itself, \
                 so it's stored encrypted like the other logins.",
            ))
            .into_any_element()
    }

    /// The "Find on Soulseek" page.
    pub(super) fn render_soulseek(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let query_input = self.soulseek_inputs(window, cx).query.clone();
        let Some((playlist_id, track_id)) = self.slsk.page.clone() else { return div().into_any_element() };
        let Some(track) =
            self.library.playlist(&playlist_id).and_then(|p| p.tracks.iter().find(|t| t.id == track_id)).cloned()
        else {
            return div().into_any_element();
        };
        let logged_in = self.creds.soulseek.is_some();
        let pick = self.slsk.picks.iter().find(|p| p.track_id == track_id);
        let picking = pick.is_some();
        let (results, counts) = (self.slsk.results.clone(), self.slsk.counts);
        let searching = self.slsk.busy || self.slsk.searched_at.is_some();

        let status: (SharedString, Hsla) = if let Some(e) = &self.slsk.error {
            (e.clone().into(), theme.danger)
        } else if self.slsk.busy {
            ("Connecting to Soulseek…".into(), theme.muted_foreground)
        } else if self.slsk.searched_at.is_some() {
            (format!("Searching… {} files from {} users so far", counts.0, counts.1).into(), theme.muted_foreground)
        } else if counts.0 > 0 {
            (format!("{} files from {} users, best quality first", counts.0, counts.1).into(), theme.muted_foreground)
        } else if self.slsk.client.is_some() {
            ("Nothing found. Try fewer words, or just the title.".into(), theme.muted_foreground)
        } else {
            ("".into(), theme.muted_foreground)
        };

        let pick_line = pick.map(|p| {
            let who = &p.file.user;
            let text = match &p.last {
                TransferState::Connecting => format!("Connecting to {who}…"),
                TransferState::Queued(Some(n)) => format!("Waiting in {who}'s queue (place {n})…"),
                TransferState::Queued(None) => format!("Waiting for {who} to send it…"),
                TransferState::Downloading(done, total) => format!(
                    "Downloading from {who}: {}% ({:.1} of {:.1} MB)",
                    done * 100 / (*total).max(1),
                    *done as f64 / 1e6,
                    *total as f64 / 1e6
                ),
                TransferState::Done => "Done".into(),
                TransferState::Failed(why) => why.clone(),
            };
            let transfer = p.transfer.clone();
            h_flex()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(theme.primary.alpha(0.1))
                .child(Icon::new(IconName::LoaderCircle).small().text_color(theme.primary))
                .child(v_flex().flex_1().min_w_0().child(div().text_sm().truncate().child(text)).child(
                    div().text_xs().truncate().text_color(theme.muted_foreground).child(file_name(&p.file.path)),
                ))
                .child(
                    Button::new("slsk-cancel")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .on_click(move |_, _, _| transfer.cancel()),
                )
        });

        let track_len = track.duration_secs;
        let rows = results.clone();
        let list = uniform_list(
            "slsk-results",
            results.len(),
            cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                let theme = cx.theme().clone();
                range
                    .filter_map(|ix| rows.get(ix).map(|r| (ix, r.clone())))
                    .map(|(ix, r)| {
                        let length_off = matches!((r.duration_secs, track_len), (Some(a), Some(b)) if a.abs_diff(b) > LENGTH_SLACK_SECS);
                        let quality_color = if r.lossless() {
                            theme.success
                        } else if r.kbps().is_some_and(|k| k >= 256) {
                            theme.primary
                        } else {
                            theme.muted_foreground
                        };
                        let picked = r.clone();
                        h_flex()
                            .id(("slsk-row", ix))
                            .w_full()
                            .h(px(52.))
                            .px_3()
                            .gap_3()
                            .rounded_lg()
                            .hover(|d| d.bg(theme.list_hover))
                            .child(
                                div()
                                    .w(px(104.))
                                    .flex_none()
                                    .text_sm()
                                    .font_semibold()
                                    .text_color(quality_color)
                                    .child(r.quality_label()),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().child(r.file_name()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .truncate()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{} · {}", r.folder(), r.user)),
                                    ),
                            )
                            .child(
                                div()
                                    .id(("slsk-len", ix))
                                    .w(px(52.))
                                    .flex_none()
                                    .text_right()
                                    .text_sm()
                                    .text_color(if length_off { theme.warning } else { theme.muted_foreground })
                                    .child(r.duration_secs.map(format_duration).unwrap_or_default())
                                    .when(length_off, |d| {
                                        d.tooltip(|window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(
                                                "A different length from the track: maybe another version",
                                            )
                                            .build(window, cx)
                                        })
                                    }),
                            )
                            .child(
                                div()
                                    .w(px(64.))
                                    .flex_none()
                                    .text_right()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{:.1} MB", r.size as f64 / 1e6)),
                            )
                            .child(
                                div()
                                    .w(px(76.))
                                    .flex_none()
                                    .text_right()
                                    .text_xs()
                                    .text_color(if r.free_slot { theme.success } else { theme.muted_foreground })
                                    .child(if r.free_slot { "Free slot".to_string() } else { format!("Queue {}", r.queue) }),
                            )
                            .child(
                                Button::new(("slsk-use", ix))
                                    .small()
                                    .label("Use this")
                                    .disabled(picking)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.soulseek_pick(&picked, window, cx)
                                    })),
                            )
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.slsk.scroll)
        .size_full();

        let pid = playlist_id.clone();
        let tid = track_id.clone();
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .p_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("slsk-back")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ArrowLeft)
                                    .tooltip("Back to the playlist")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.view = View::Playlist;
                                        cx.notify();
                                    })),
                            )
                            .child(div().text_xl().child("Find on Soulseek"))
                            .child(div().flex_1())
                            .child(soulseek_badge(&theme)),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .px_3()
                            .py_2()
                            .rounded_lg()
                            .bg(theme.muted)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_medium().truncate().child(track.title.clone()))
                                    .child(div().text_xs().truncate().text_color(theme.muted_foreground).child(
                                        match &track.soulseek {
                                            Some(f) => format!(
                                                "Now plays {} from {} on Soulseek ({}) instead of the {} audio",
                                                file_name(&f.path),
                                                f.user,
                                                f.quality,
                                                track.platform.label()
                                            ),
                                            None => format!(
                                                    "{} · {} audio{}",
                                                    if track.artist.is_empty() { "—" } else { track.artist.as_str() },
                                                    track.platform.label(),
                                                    track_len
                                                        .map(|s| format!(" · {}", format_duration(s)))
                                                        .unwrap_or_default()
                                                ),
                                        },
                                    )),
                            )
                            .when(track.soulseek.is_some(), |d| {
                                d.child(
                                    Button::new("slsk-restore")
                                        .small()
                                        .icon(IconName::Undo2)
                                        .label("Restore original")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.restore_original_audio(&pid, &tid, window, cx)
                                        })),
                                )
                            }),
                    )
                    .when(!logged_in, |d| {
                        d.child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("Log in to Soulseek to search (Settings → Accounts)."),
                                )
                                .child(
                                    Button::new("slsk-settings")
                                        .small()
                                        .primary()
                                        .icon(IconName::Settings)
                                        .label("Open Settings")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.view = View::Settings;
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .when(logged_in, |d| {
                        d.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div().flex_1().child(
                                        Input::new(&query_input)
                                            .prefix(
                                                Icon::new(IconName::Search).small().text_color(theme.muted_foreground),
                                            )
                                            .cleanable(true),
                                    ),
                                )
                                .child(
                                    Button::new("slsk-search")
                                        .icon(IconName::Search)
                                        .label("Search")
                                        .loading(searching)
                                        .on_click(cx.listener(|this, _, window, cx| this.soulseek_search(window, cx))),
                                ),
                        )
                        .child(div().text_sm().text_color(status.1).child(status.0))
                    })
                    .children(pick_line),
            )
            .child(div().flex_1().min_h_0().relative().px_2().py_1().when(!results.is_empty(), |d| {
                d.child(list).child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(Scrollbar::width())
                        .child(Scrollbar::vertical(&self.slsk.scroll)),
                )
            }))
            .into_any_element()
    }
}

/// The file name in a Soulseek path (`Music\Artist\01 Song.flac`).
fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

pub(super) fn soulseek_badge(theme: &gpui_kit::component::Theme) -> Div {
    badge("Soulseek", 205. / 360., theme)
}

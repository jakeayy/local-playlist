//! Settings page: accounts (webview login / pasted cookies) and components.

use super::*;
use crate::login;
use crate::secrets::parse_cookies;
use crate::theme::Appearance;
use crate::ytdlp::Source;
use gpui_kit::component::input::Textarea;

const PLATFORMS: [Platform; 2] = [Platform::YouTube, Platform::SoundCloud];
const COOKIES_HELP_URL: &str = "https://github.com/jakeayy/local-playlist/blob/main/ADDING-COOKIES.md";

impl LocalPlaylist {
    // ------------------------------------------------------------ components

    pub(super) fn refresh_components(&mut self, cx: &mut Context<Self>) {
        let ytdlp = self.ytdlp.clone();
        cx.spawn(async move |this, cx| {
            let status = cx.background_spawn(async move { ytdlp.status() }).await;
            let _ = this.update(cx, |this, cx| {
                this.components = Some(status);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn setup_needed(&self) -> bool {
        self.components.as_ref().is_some_and(|s| matches!(s.ytdlp, State::Missing) || matches!(s.js, State::Missing))
    }

    pub(super) fn components_label(&self) -> SharedString {
        if let Some(st) = &self.installing {
            return st.lock().unwrap().0.clone().into();
        }
        match &self.components {
            None => "Checking components…".into(),
            Some(s) => match (&s.ytdlp, &s.js) {
                (State::Missing, _) => "yt-dlp is missing".into(),
                (_, State::Missing) => "JavaScript runtime is missing".into(),
                (State::Managed(i), _) => format!("yt-dlp {}", i.short()).into(),
                (State::System(v), _) => format!("{v} (system)").into(),
            },
        }
    }

    fn state_label(state: Option<&State>) -> String {
        match state {
            None => "Checking…".into(),
            Some(State::Managed(i)) => format!("Bundled · {}", i.short()),
            Some(State::System(v)) => format!("System · {v}"),
            Some(State::Missing) => "Not installed".into(),
        }
    }

    /// Download/update managed components (all managed ones when `only` is empty).
    pub(super) fn install_components(&mut self, only: Vec<Component>, window: &mut Window, cx: &mut Context<Self>) {
        if self.installing.is_some() {
            return;
        }
        let list: Vec<Component> = if only.is_empty() {
            let mut v = Vec::new();
            if self.ytdlp.ytdlp_source == Source::Managed {
                v.push(Component::YtDlp);
            }
            if self.ytdlp.js_source == Source::Managed {
                v.push(Component::JsRuntime);
            }
            v
        } else {
            only
        };
        if list.is_empty() {
            window.push_notification(Notification::info("All components come from the system; nothing to update."), cx);
            return;
        }
        let status = Arc::new(Mutex::new(("Starting download…".to_string(), None)));
        self.installing = Some(status.clone());
        cx.notify();
        let ytdlp = self.ytdlp.clone();
        cx.spawn_in(window, async move |this, cx| {
            let results = cx
                .background_spawn(async move {
                    list.into_iter()
                        .map(|c| {
                            let r = ytdlp.install(c, &|s, f| *status.lock().unwrap() = (s, f));
                            (c, r)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.installing = None;
                for (c, r) in results {
                    let note = match r {
                        Ok(Some(i)) => Notification::success(format!("{} {} installed", c.label(), i.short())),
                        Ok(None) => Notification::info(format!("{} is already up to date", c.label())),
                        Err(e) => Notification::error(format!("{} download failed: {e:#}", c.label())).autohide(false),
                    };
                    window.push_notification(note, cx);
                }
                this.refresh_components(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_source(&mut self, c: Component, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        match c {
            Component::YtDlp => self.library.settings.ytdlp_source = source,
            Component::JsRuntime => self.library.settings.js_source = source,
        }
        self.ytdlp.ytdlp_source = self.library.settings.ytdlp_source;
        self.ytdlp.js_source = self.library.settings.js_source;
        self.save();
        self.components = None;
        self.refresh_components(cx);
        if source == Source::Managed && self.ytdlp.installed(c).is_none() {
            self.install_components(vec![c], window, cx);
        }
        cx.notify();
    }

    // -------------------------------------------------------------- accounts

    fn persist_creds(&mut self) {
        match SecretStore::new(&self.paths).save(&self.creds) {
            Ok(storage) => self.key_storage = Some(storage),
            Err(e) => self.notify_error(format!("Could not save login: {e:#}")),
        }
    }

    fn login(&mut self, p: Platform, window: &mut Window, cx: &mut Context<Self>) {
        if self.logging_in.is_some() {
            return;
        }
        self.logging_in = Some(p);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { login::login(p) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.logging_in = None;
                match result {
                    Ok(cookies) => {
                        this.creds.set(p, cookies);
                        this.persist_creds();
                        window.push_notification(Notification::success(format!("Logged in to {}", p.label())), cx);
                    }
                    Err(e) => window.push_notification(Notification::warning(format!("{e:#}")), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_channel_picker(&mut self, cx: &mut Context<Self>) {
        self.channel_picker_open = !self.channel_picker_open;
        if self.channel_picker_open && !matches!(self.yt_channels, Some(Ok(_))) {
            self.yt_channels = None;
            let cookies = self.creds.get(Platform::YouTube).map(<[_]>::to_vec);
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_spawn(async move {
                        let cookies = cookies.ok_or_else(|| "Not logged in".to_string())?;
                        sources::youtube::channels(&cookies).map_err(|e| format!("{e:#}"))
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.yt_channels = Some(result);
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn select_channel(&mut self, channel: sources::youtube::Channel, window: &mut Window, cx: &mut Context<Self>) {
        let name = channel.name.clone();
        self.creds.youtube_channel = Some(channel);
        self.persist_creds();
        self.channel_picker_open = false;
        window.push_notification(Notification::success(format!("Using YouTube channel \"{name}\"")), cx);
        cx.notify();
    }

    fn logout(&mut self, p: Platform, window: &mut Window, cx: &mut Context<Self>) {
        if p == Platform::YouTube {
            self.yt_channels = None;
            self.channel_picker_open = false;
        }
        self.creds.remove(p);
        self.persist_creds();
        window.push_notification(
            Notification::info(format!("Logged out of {} (session removed from this device)", p.label())),
            cx,
        );
        cx.notify();
    }

    fn save_pasted_cookies(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.cookie_target else { return };
        let text = self.cookie_input.read(cx).value().to_string();
        match parse_cookies(&text, p) {
            Ok(cookies) => {
                let n = cookies.len();
                self.creds.set(p, cookies);
                self.persist_creds();
                self.cookie_target = None;
                self.cookie_input.update(cx, |i, cx| i.set_value("", window, cx));
                window.push_notification(
                    Notification::success(format!("Saved {n} {} cookies", p.label())),
                    cx,
                );
            }
            Err(e) => window.push_notification(Notification::warning(format!("{e:#}")), cx),
        }
        cx.notify();
    }

    // ------------------------------------------------------------- rendering

    pub(super) fn render_setup_banner(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.setup_needed() && self.installing.is_none() {
            return None;
        }
        // Only show the install progress here while setup is still needed.
        if !self.setup_needed() {
            return None;
        }
        let theme = cx.theme().clone();
        let missing = self.ytdlp.missing_managed();
        let mb: u32 = missing.iter().map(|c| c.approx_mb()).sum();
        let names = missing.iter().map(|c| c.label()).collect::<Vec<_>>().join(" and ");
        let installing = self.installing.as_ref().map(|s| s.lock().unwrap().clone());

        let text = match (&installing, missing.is_empty()) {
            (Some((status, _)), _) => status.clone(),
            (None, false) => format!(
                "To play music, Local Playlist needs {names} (about {mb} MB, downloaded once from GitHub)."
            ),
            (None, true) => {
                "A component selected as \"System\" wasn't found on PATH. Install it or switch back to the bundled version in Settings.".into()
            }
        };

        Some(
            h_flex()
                .flex_none()
                .gap_3()
                .px_4()
                .py_3()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.warning.alpha(0.12))
                .child(Icon::new(IconName::Download).text_color(theme.warning))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().child(text))
                        .when_some(installing.and_then(|(_, f)| f), |d, f| {
                            d.child(
                                div().h(px(4.)).w_full().rounded_full().bg(theme.muted).child(
                                    div().h_full().rounded_full().bg(theme.primary).w(relative(f.clamp(0., 1.))),
                                ),
                            )
                        }),
                )
                .when(self.installing.is_none() && !missing.is_empty(), |d| {
                    d.child(
                        Button::new("setup-download")
                            .primary()
                            .small()
                            .icon(IconName::Download)
                            .label("Download")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let missing = this.ytdlp.missing_managed();
                                this.install_components(missing, window, cx);
                            })),
                    )
                    .child(
                        Button::new("setup-system")
                            .ghost()
                            .small()
                            .label("Use system tools")
                            .tooltip("Use yt-dlp and a JavaScript runtime (deno/node/bun/qjs) installed on this computer")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_source(Component::YtDlp, Source::System, window, cx);
                                this.set_source(Component::JsRuntime, Source::System, window, cx);
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    pub(super) fn render_settings(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let section = |title: &'static str, desc: SharedString| {
            v_flex()
                .gap_1()
                .child(div().text_lg().font_semibold().child(title))
                .child(div().text_sm().text_color(theme.muted_foreground).child(desc))
        };
        let card = || v_flex().gap_3().p_4().rounded_lg().border_1().border_color(theme.border);

        let storage_note: SharedString = match self.key_storage {
            Some(KeyStorage::File) => "Logins are encrypted, but no system keyring was available, so the key is kept in a private file in the app's data folder.".into(),
            _ => "Your password goes only to the platform's own login page. The app keeps the resulting session cookies, encrypted with a key held in your system keyring, and sends them only to that platform when needed.".into(),
        };

        // ---- accounts
        let accounts = PLATFORMS.map(|p| {
            let logged_in = self.creds.get(p).is_some();
            let busy = self.logging_in == Some(p);
            let editing = self.cookie_target == Some(p);
            let quick: Vec<(&'static str, &'static str, Link)> = match p {
                Platform::YouTube => vec![
                    ("yt-ll", "Liked videos", Link::YouTubePlaylist { list: "LL".into() }),
                    ("yt-lm", "Liked music", Link::YouTubePlaylist { list: "LM".into() }),
                    ("yt-wl", "Watch later", Link::YouTubePlaylist { list: "WL".into() }),
                ],
                Platform::SoundCloud => vec![("sc-likes", "My likes", Link::SoundCloudMyLikes)],
            };
            card()
                .child(
                    h_flex()
                        .gap_3()
                        .child(platform_badge(p, &theme))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(if logged_in { theme.success } else { theme.muted_foreground })
                                .child(if logged_in { "Logged in" } else { "Not logged in" }),
                        )
                        .when(!logged_in, |d| {
                            d.child(
                                Button::new(SharedString::from(format!("paste-{}", p.short())))
                                    .ghost()
                                    .small()
                                    .label("Paste cookies…")
                                    .tooltip("Provide cookies exported from your browser instead of logging in here")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.cookie_target = if this.cookie_target == Some(p) { None } else { Some(p) };
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("login-{}", p.short())))
                                    .primary()
                                    .small()
                                    .icon(IconName::LogIn)
                                    .label(if busy { "Waiting for login…" } else { "Log in" })
                                    .loading(busy)
                                    .on_click(cx.listener(move |this, _, window, cx| this.login(p, window, cx))),
                            )
                        })
                        .when(logged_in, |d| {
                            d.child(
                                Button::new(SharedString::from(format!("logout-{}", p.short())))
                                    .small()
                                    .icon(IconName::LogOut)
                                    .label("Log out")
                                    .on_click(cx.listener(move |this, _, window, cx| this.logout(p, window, cx))),
                            )
                        }),
                )
                .when(logged_in && p == Platform::YouTube, |d| {
                    let current = self.creds.youtube_channel.as_ref();
                    let label = current.map(|c| c.name.clone()).unwrap_or_else(|| "Default channel".into());
                    d.child(
                        h_flex()
                            .gap_2()
                            .child(div().text_sm().text_color(theme.muted_foreground).child("Channel:"))
                            .child(div().text_sm().font_medium().truncate().child(label))
                            .child(
                                Button::new("yt-channel")
                                    .ghost()
                                    .small()
                                    .label(if self.channel_picker_open { "Close" } else { "Change" })
                                    .tooltip("For accounts with several channels (Brand Accounts)")
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_channel_picker(cx))),
                            ),
                    )
                    .when(self.channel_picker_open, |d| {
                        let content = match &self.yt_channels {
                            None => div().text_sm().text_color(theme.muted_foreground).child("Loading channels…").into_any_element(),
                            Some(Err(e)) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
                            Some(Ok(list)) => v_flex()
                                .gap_0p5()
                                .children(list.iter().enumerate().map(|(i, ch)| {
                                    let selected = current.map(|c| &c.page_id) == Some(&ch.page_id)
                                        || (current.is_none() && ch.page_id.is_none() && i == 0);
                                    let ch2 = ch.clone();
                                    h_flex()
                                        .id(("channel", i))
                                        .gap_2()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .when(selected, |d| d.bg(theme.list_active))
                                        .hover(|d| d.bg(theme.list_hover))
                                        .child(div().text_sm().truncate().child(ch.name.replace('\n', " ")))
                                        .child(div().text_xs().text_color(theme.muted_foreground).child(ch.handle.clone()))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.select_channel(ch2.clone(), window, cx)
                                        }))
                                }))
                                .into_any_element(),
                        };
                        d.child(content)
                    })
                })
                .when(logged_in, |d| {
                    d.child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(div().text_sm().text_color(theme.muted_foreground).child("Import:"))
                            .children(quick.into_iter().map(|(id, label, link)| {
                                Button::new(id)
                                    .small()
                                    .icon(IconName::Import)
                                    .label(label)
                                    .loading(self.importing)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.import_link(link.clone(), false, window, cx)
                                    }))
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Private playlists: paste their link in \"Import playlist\" or the Add field. Playlists on your other channels are found automatically."),
                    )
                })
                .when(editing && !logged_in, |d| {
                    d.child(Textarea::new(&self.cookie_input).h(px(120.)))
                        .child(
                            div().text_xs().text_color(theme.muted_foreground).child(match p {
                                Platform::YouTube => "Export cookies for youtube.com while logged in (e.g. with a \"cookies.txt\" browser extension). Needs SAPISID or __Secure-3PAPISID.",
                                Platform::SoundCloud => "Export cookies for soundcloud.com while logged in, or paste just: oauth_token=<value>",
                            }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(
                                    Button::new(SharedString::from(format!("help-{}", p.short())))
                                        .ghost()
                                        .small()
                                        .icon(IconName::CircleQuestionMark)
                                        .label("How do I get cookies?")
                                        .on_click(cx.listener(|_, _, _, cx| cx.open_url(COOKIES_HELP_URL))),
                                )
                                .child(div().flex_1())
                                .child(
                                    Button::new(SharedString::from(format!("cancel-{}", p.short())))
                                        .ghost()
                                        .small()
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cookie_target = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("save-{}", p.short())))
                                        .primary()
                                        .small()
                                        .label("Save cookies")
                                        .on_click(cx.listener(|this, _, window, cx| this.save_pasted_cookies(window, cx))),
                                ),
                        )
                })
        });

        // ---- components
        let installing = self.installing.as_ref().map(|s| s.lock().unwrap().0.clone());
        let comp_row = |c: Component, title: &'static str, desc: &'static str, cx: &mut Context<Self>| {
            let source = match c {
                Component::YtDlp => self.ytdlp.ytdlp_source,
                Component::JsRuntime => self.ytdlp.js_source,
            };
            let state = self.components.as_ref().map(|s| match c {
                Component::YtDlp => &s.ytdlp,
                Component::JsRuntime => &s.js,
            });
            let missing = matches!(state, Some(State::Missing));
            h_flex()
                .gap_3()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_medium().child(title))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(desc))
                        .child(
                            div()
                                .text_sm()
                                .text_color(if missing { theme.warning } else { theme.muted_foreground })
                                .child(Self::state_label(state)),
                        ),
                )
                .child(
                    h_flex()
                        .child(
                            Button::new(SharedString::from(format!("{c:?}-managed")))
                                .small()
                                .label("Bundled")
                                .when(source == Source::Managed, |b| b.primary())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_source(c, Source::Managed, window, cx)
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("{c:?}-system")))
                                .small()
                                .label("System")
                                .when(source == Source::System, |b| b.primary())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_source(c, Source::System, window, cx)
                                })),
                        ),
                )
        };
        let ytdlp_row = comp_row(
            Component::YtDlp,
            "yt-dlp",
            "Fetches audio. Bundled: the official build of the latest master commit.",
            cx,
        );
        let js_row = comp_row(
            Component::JsRuntime,
            "JavaScript runtime",
            "Solves YouTube's playback challenges. Bundled: QuickJS (~3 MB). System: deno, node, bun or qjs from PATH.",
            cx,
        );

        v_flex()
            .id("settings")
            .size_full()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(760.))
                    .p_6()
                    .gap_6()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(div().text_2xl().font_semibold().child("Settings"))
                            .child(
                                Button::new("close-settings")
                                    .ghost()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.view = View::Playlist;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_3()
                            .child(section("Appearance", "Material-style light and dark themes.".into()))
                            .child(h_flex().gap_1().children(
                                [
                                    (Appearance::System, "System", IconName::Monitor),
                                    (Appearance::Light, "Light", IconName::Sun),
                                    (Appearance::Dark, "Dark", IconName::Moon),
                                ]
                                .map(|(choice, label, icon)| {
                                    Button::new(label)
                                        .small()
                                        .icon(icon)
                                        .label(label)
                                        .when(self.library.settings.appearance == choice, |b| b.primary())
                                        .when(self.library.settings.appearance != choice, |b| b.ghost())
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.library.settings.appearance = choice;
                                            this.save();
                                            crate::theme::apply(choice, Some(window), cx);
                                            cx.notify();
                                        }))
                                }),
                            )),
                    )
                    .child(
                        v_flex()
                            .gap_3()
                            .child(section(
                                "Accounts",
                                "Optional. Needed only for private playlists, Liked videos and your likes. Logging in opens the platform's own sign-in page in a private window.".into(),
                            ))
                            .children(accounts)
                            .child(div().text_xs().text_color(theme.muted_foreground).child(storage_note)),
                    )
                    .child(
                        v_flex()
                            .gap_3()
                            .child(section(
                                "Components",
                                "Downloaded once into the app's data folder, so you don't need Python or a JavaScript runtime installed.".into(),
                            ))
                            .child(card().child(ytdlp_row).child(div().h(px(1.)).bg(theme.border)).child(js_row))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(
                                        Button::new("update-components")
                                            .small()
                                            .icon(IconName::RefreshCw)
                                            .label("Update bundled components")
                                            .loading(installing.is_some())
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.install_components(Vec::new(), window, cx)
                                            })),
                                    )
                                    .when_some(installing, |d, s| {
                                        d.child(div().text_sm().text_color(theme.muted_foreground).child(s))
                                    }),
                            ),
                    )
                    .child(self.render_storage(cx)),
            )
    }

    fn change_music_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.downloads.is_empty() {
            window.push_notification(Notification::warning("Wait for downloads to finish before moving the music folder."), cx);
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose music folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(new_root) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |this, window, cx| this.set_music_dir(Some(new_root), window, cx));
        })
        .detach();
    }

    /// Switch the music root, moving existing playlist folders over.
    fn set_music_dir(&mut self, dir: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let old_root = self.music_root();
        let new_root = dir.clone().unwrap_or_else(storage::default_music_root);
        if old_root == new_root {
            return;
        }
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.state = PlayState::Idle;
        let folders: Vec<String> = self.library.playlists.iter().filter_map(|p| p.folder.clone()).collect();
        self.library.settings.music_dir = dir;
        self.save();
        self.moving_music = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let (old, new) = (old_root.clone(), new_root.clone());
            let errors = cx
                .background_spawn(async move {
                    let mut errors = Vec::new();
                    for f in folders {
                        if let Err(e) = storage::move_dir(&old.join(&f), &new.join(&f)) {
                            errors.push(format!("{f}: {e:#}"));
                        }
                    }
                    let _ = std::fs::remove_dir(&old); // only if now empty
                    errors
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.moving_music = false;
                this.on_disk.clear();
                let note = if errors.is_empty() {
                    Notification::success(format!("Music folder is now {}", new_root.display()))
                } else {
                    Notification::error(format!("Some folders couldn't be moved: {}", errors.join("; "))).autohide(false)
                };
                window.push_notification(note, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_storage(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let root = self.music_root();
        let removed = storage::removed_dir(&self.paths);
        let used = storage::dir_size(&removed) as f64 / 1e6;
        let limit = self.library.settings.removed_cache_mb;
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_lg().font_semibold().child("Storage"))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(
                        "Downloaded tracks are saved per playlist as normal audio files, so they play offline and in other players.",
                    )),
            )
            .child(
                v_flex()
                    .gap_3()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_medium().child("Music folder"))
                                    .child(div().text_sm().text_color(theme.muted_foreground).truncate().child(format!(
                                        "{}/<Playlist name>/<Platform>-<id>.m4a",
                                        root.display()
                                    ))),
                            )
                            .when(self.library.settings.music_dir.is_some(), |d| {
                                d.child(
                                    Button::new("music-default")
                                        .ghost()
                                        .small()
                                        .label("Use default")
                                        .on_click(cx.listener(|this, _, window, cx| this.set_music_dir(None, window, cx))),
                                )
                            })
                            .child(
                                Button::new("music-change")
                                    .small()
                                    .icon(IconName::FolderOpen)
                                    .label("Change…")
                                    .loading(self.moving_music)
                                    .on_click(cx.listener(|this, _, window, cx| this.change_music_dir(window, cx))),
                            ),
                    )
                    .child(div().h(px(1.)).bg(theme.border))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_medium().child("Removed tracks"))
                                    .child(div().text_sm().text_color(theme.muted_foreground).child(format!(
                                        "Files of tracks you remove are kept here so re-adding them is instant. Using {used:.0} MB of {limit} MB; oldest go first."
                                    ))),
                            )
                            .child(
                                Button::new("clear-removed")
                                    .small()
                                    .icon(IconName::Trash)
                                    .label("Clear")
                                    .on_click(cx.listener(|this, _, window, cx| this.clear_removed_cache(window, cx))),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_sm().text_color(theme.muted_foreground).child("Limit:"))
                            .children([512u64, 1024, 2048, 5120, 10240].map(|mb| {
                                let label = if mb < 1024 { format!("{mb} MB") } else { format!("{} GB", mb / 1024) };
                                Button::new(("limit", mb as usize))
                                    .small()
                                    .label(label)
                                    .when(limit == mb, |b| b.primary())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.library.settings.removed_cache_mb = mb;
                                        this.save();
                                        storage::enforce_limit(&storage::removed_dir(&this.paths), this.removed_limit());
                                        cx.notify();
                                    }))
                            })),
                    ),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                "Library file: {}",
                self.paths.library_file().display()
            )))
    }
}

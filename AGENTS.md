# AGENTS.md

Guidance for coding agents working in this repository. People should start with
[README.md](README.md) and [CONTRIBUTING.md](CONTRIBUTING.md); this file adds what an agent
needs to make correct changes without rediscovering the codebase.

## What this is

Local Playlist is a privacy-friendly, local-first music player for links from YouTube /
YouTube Music and SoundCloud, written in Rust with [GPUI Kit](https://gpui-kit.com) (GPUI,
`gpui-fast` backend). One crate (edition 2024) builds three things:

| Build | Command | Notes |
| --- | --- | --- |
| Desktop app (default) | `cargo build` | Also builds `local-playlist-login`, the webview login helper |
| Server only | `cargo build --no-default-features --features server` | `local-playlist --server`: web UI + API |
| Web UI (wasm) | `./scripts/build-web.sh` | Nightly; output in `web/dist/`, embedded by the server |

The server embeds the web UI only if `web/dist/` exists when it is built (`build.rs` sets
`cfg(web_ui)`); otherwise it serves the API alone and says so at startup.

## Repository map

| Path | Purpose |
| --- | --- |
| `src/main.rs` | Entry point: desktop app, `--server`, `--diagnose`, `--version` |
| `src/lib.rs` | Module wiring and feature gates, icon assets, window options |
| `src/app.rs` | Main window: sidebar, track list, player bar (GPUI view) |
| `src/app/` | `settings.rs` (accounts, components), `visualizer.rs` + `backdrop.rs`, `lyrics.rs` (lyrics page, its own view), `progress.rs` (seek row, its own view), `thumbs.rs` (bounded thumbnail cache) |
| `src/model.rs` | Library model, `library.json` persistence, `Paths` (data and cache dirs) |
| `src/sources/` | Link parsing; YouTube (InnerTube web API, oEmbed) and SoundCloud (api-v2) metadata, with yt-dlp as fallback |
| `src/ytdlp.rs` | Managed components (yt-dlp master build, QuickJS), downloads through yt-dlp |
| `src/audio.rs` | Streaming playback engine (decoder thread, buffering, seeking) |
| `src/download.rs` | Shared state of one background download |
| `src/storage.rs` | Music folder layout, removed-tracks cache |
| `src/cover.rs` | Cover art: fetch, disk cache, embedding into files |
| `src/remux.rs` | Fragmented MP4 → plain MP4 without ffmpeg |
| `src/sponsorblock.rs` | SponsorBlock segments (privacy-preserving hash-prefix lookups) |
| `src/lyrics/` | Lyrics: artist/title guessing for uploads (`query.rs`), LRC and Lyricsfile parsing (`formats.rs`), LRCLIB and lrc.red lookups, disk cache |
| `src/tray.rs`, `src/tray/` | Tray icon for playing in the background (ksni on Linux, Shell_NotifyIcon on Windows) |
| `src/instance.rs` | One desktop app at a time: a second start shows the running one's window |
| `src/spectrum.rs` | FFT and bands for the visualizer (no dependencies, builds for wasm) |
| `src/secrets.rs` | Encrypted cookie storage (ChaCha20-Poly1305, key in the OS keyring) |
| `src/login.rs`, `src/bin/login.rs` | Starts / implements the webview login helper |
| `src/server.rs` | `--server`: static web UI, auth, `/api/*` (resolve, segments, cover, lyrics, audio) |
| `src/web/` | Browser build: entry point, API client, and stand-ins for desktop-only modules |
| `src/theme.rs`, `assets/` | Material 3 themes (`assets/material-theme.json`), Noto Sans fonts |
| `src/mem.rs` | glibc allocator tuning (`tune`, `trim`) |
| `web/` | `index.html`, `boot.js`, `icons/` (Lucide SVGs served to the web UI) |
| `scripts/build-web.sh` | Builds the web UI |
| `.github/` | Workflows, issue forms, PR template, release and Docker scripts |

Every module starts with a `//!` doc comment explaining what it does and why. Read it before
changing the module, and keep it accurate.

## Features and cfgs

- Features: `desktop` (default; implies `ui`), `ui` (GPUI interface), `server`. On native
  targets `ui` requires `desktop` (a `compile_error!` enforces it).
- The web UI is the same GPUI code compiled for `wasm32-unknown-unknown` with
  `--no-default-features --features ui`. Three modules have web stand-ins with the same API
  in `src/web/`, chosen with `#[path]` in `lib.rs`: `audio` and `login` (desktop-only) and
  `ytdlp` (native: the desktop app and the server both use it, so don't gate it on
  `desktop`). If you change the public API of one of them, change its stand-in too.
- Gate with `#[cfg(feature = "desktop")]`, `#[cfg(feature = "server")]` and
  `#[cfg(target_family = "wasm")]`; use `crate::WEB` for runtime checks. `std::time` doesn't
  work in the browser: use `crate::now_ms()`.
- On the desktop, closing the window can leave the app running (tray icon, see
  `src/tray.rs`): the main view outlives its window and gets a new one from the tray
  (`LocalPlaylist::attach`). So `self.window` may be `None`; don't tie work that must
  finish to the window (`cx.spawn_in`): use `cx.spawn` and `LocalPlaylist::finish`, and
  put notifications in `pending_notes` or `note`. Text inputs are made per window.
- Windows-only code (`cfg(windows)`) can't be compiled on Linux; CI builds and tests it.

## Checks

Run what CI runs (`.github/workflows/ci.yml`) before calling a change done:

```sh
cargo fmt
cargo test --locked                                              # desktop
cargo test --locked --no-default-features --features server      # server only
cargo +nightly check --locked --lib --target wasm32-unknown-unknown --no-default-features --features ui
```

- The desktop build needs system libraries on Linux: see "Building" in CONTRIBUTING.md
  (Fedora package names) or the install step in `ci.yml` (Ubuntu). The server-only build
  needs none.
- Tests live in `#[cfg(test)] mod tests` inside each module. Offline tests must not touch
  the network.
- Live tests are `#[ignore]`d with a reason (`"network"`, `"needs ffmpeg"`, …). Run the
  ones for the area you changed, by name: e.g. `cargo test sources:: -- --ignored` or
  `cargo test live_ -- --ignored`. They talk to YouTube, SoundCloud and GitHub, download
  yt-dlp and QuickJS, and work in temporary directories, not the user's library. A bare
  `cargo test -- --ignored` also runs tests that need input and fail without it:
  `decode_file` (`LP_DECODE_FILE=<audio file>`), `calibrate`
  (`LP_CALIBRATE=<file>:<file>`, with `--nocapture`), and the ffmpeg ones
  (`embeds_into_each_format`, `matches_ffmpeg_decode`).
- Use public links only, in tests and anywhere else.
- Release builds (`opt-level = "s"`, fat LTO, one codegen unit) are slow. Use `cargo check`
  or debug builds while iterating.
- The full web build needs `rustup target add wasm32-unknown-unknown --toolchain nightly`,
  `wasm-bindgen-cli` at the exact `wasm-bindgen` version in `Cargo.lock`, and optionally
  `wasm-opt`. The `cargo +nightly check` above needs only the nightly target.

## Formatting

- `cargo fmt`, configured by `rustfmt.toml` (120 columns, `use_small_heuristics = "Max"`).
  CI fails on unformatted code.
- `cargo fmt` formats the whole crate, including uncommitted changes that aren't yours.
  `rustfmt <file>` also formats every module that file declares (`rustfmt src/app.rs`
  touches `src/app/*.rs`). If the working tree has someone else's uncommitted work, ask
  before formatting.

## Conventions

- **Errors:** `anyhow::Result` with `.context(...)`; print chains with `{e:#}`.
- **Logging:** `eprintln!` (there is no logging crate). Never print cookie values, tokens,
  passwords, or (in the server) request URLs.
- **Concurrency:** views run async work as GPUI tasks (`cx.spawn`, `cx.spawn_in`). A blocking
  call whose result the view needs (resolving a link, a login, component status) runs as
  `cx.background_spawn(async move { blocking_call() }).await` inside such a task. Long-lived
  jobs (downloads, the decoder, file moves, server workers) get their own
  `std::thread::spawn`. After bursts of large allocations (big JSON pages, many covers) call
  `crate::mem::trim()`.
- **Size, memory and redraws matter:** release sizes are listed in CONTRIBUTING.md, memory use
  is kept bounded (`mem.rs`, `app/thumbs.rs`), and per-frame work is kept small
  (`app/progress.rs` is its own view so a ticking clock doesn't redraw the window). Before
  adding a dependency,
  prefer a crate already in `Cargo.lock`, turn off default features you don't need, and say
  why in a comment in `Cargo.toml`, as the existing entries do. Builds use `--locked`.
- **Comments:** explain why, not what. Prose uses British spelling (colour, behaviour,
  optimised).
- **GPUI APIs:** the UI uses GPUI Kit with the `gpui-fast` backend, at the versions in
  `Cargo.lock`. Look APIs up in those crates' sources (`~/.cargo/registry/src/*/gpui-kit-*`
  and `gpui-fast-*`) rather than from memory of other GPUI versions.

## Privacy and safety rules

These are product requirements, not style preferences:

- No telemetry, analytics or new network destinations. The app talks only to YouTube and
  SoundCloud (and their image/script CDNs), SponsorBlock, LRCLIB and lrc.red (lyrics, only
  while the lyrics page is shown), GitHub (component downloads) and PyPI (the `yt-dlp-ejs`
  wheel, on platforms without a yt-dlp build). A new destination needs the maintainer's
  explicit approval. (Genius sits behind a bot challenge and Musixmatch needs a session
  token, so neither is used for lyrics.)
- Platform cookies go only to their own platform, and only when needed: the anonymous
  path comes first, and the session is used for personal lists or as a retry.
- The server and the web UI never receive platform credentials. The server accepts only
  YouTube and SoundCloud links (no arbitrary URL reaches yt-dlp), requires its password,
  and doesn't log request URLs.
- Credentials stay encrypted (`credentials.bin`, key in the OS keyring, a `0600` key file as
  fallback). yt-dlp downloads stay verified against the published SHA-256 sums.
- Never commit cookies, tokens or `cookies.txt` files. Secret scanning push protection is
  on.

## Data on disk

Paths come from `directories::ProjectDirs` (`model::Paths`); on Linux:

- `~/.local/share/local-playlist/`: `library.json`, `credentials.bin`, `components/` (yt-dlp,
  QuickJS), `instance.lock` (held while the desktop app runs).
- `~/.cache/local-playlist/`: `covers/`, `removed/` (removed-tracks cache), `yt-dlp/`,
  `lyrics/` (one JSON file per track), `sponsorblock.json`, `soundcloud_client_id`.
- Music: `<XDG music dir or ~/Music>/local-playlist/<Playlist>/<Platform>-<id>.<ext>`;
  downloads in progress go to `.downloading/` inside the playlist folder.

The desktop app always uses these real locations, so don't run it against the user's library
unless asked. The server takes `LOCAL_PLAYLIST_DATA` (use a temporary directory),
`LOCAL_PLAYLIST_LISTEN` and `LOCAL_PLAYLIST_PASSWORD`.

## Platform extraction

YouTube and SoundCloud change things, and extraction breaks (issues labelled
`site-change`). Metadata comes from their public web endpoints (`src/sources/`); SoundCloud's
anonymous `client_id` is read from its JavaScript bundles and cached. yt-dlp is the fallback
for metadata and does all audio downloads. When fixing breakage, keep the anonymous path
first and update the matching ignored live tests. `local-playlist --diagnose <YouTube
playlist link> [channel name]` shows what YouTube returns for the saved session (cookie names
only).

## CI, releases and repository settings

- `ci.yml`: formatting, tests (Linux and Windows) and the wasm check on pull requests and
  pushes to `main`.
- `dev.yml`: every code push to `main` publishes the pre-release `dev-<commit>` and the Docker
  image `ghcr.io/jakeayy/local-playlist:dev`.
- `release.yml`: changing `version` in `Cargo.toml` on `main` publishes the release
  `v<version>` and the images `:<version>`, `:latest` and `:dev`. **Don't bump the version
  unless asked**: merging it publishes a release.
- `build.yml` builds every download. Linux binaries are built on Ubuntu 24.04 and need
  glibc 2.39, which is why the Docker runtime image is Debian trixie.
- Actions are pinned to full commit SHAs with the version in a comment
  (`uses: owner/action@<sha> # v1.2.3`); Dependabot updates them. Pin any new action the
  same way. Workflows get a read-only token; only the jobs that publish get write permissions.
- Pull requests are squash-merged: the PR title and description become the commit on `main`,
  and its message ends up in the dev release notes. `main` can't be force-pushed and `v*` tags
  can't be moved or deleted.
- Commit subjects describe the change, optionally with an area prefix: `CI: retry Docker
  pushes that ghcr.io fails`, `Windows: no console window behind the app or the login window`.
  Bodies wrap at about 72 columns and say why.

## Keep in sync

- `README.md`: written for users, not developers: features, download, getting started,
  privacy, the web version and Docker. Keep build and code details out of it.
- `CONTRIBUTING.md`: building, the Cargo features, development commands, the module table,
  releases.
- `ADDING-COOKIES.md`: anything about logging in or cookies.
- `.github/pull_request_template.md`: the build and test checklist.
- `.github/release-downloads.sh`: the text of the release notes.

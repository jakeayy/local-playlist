# Contributing

Thanks for helping. Bug reports, site-breakage reports ("YouTube changed something") and
feature requests go through the issue forms; questions and ideas go to Discussions.
Security problems are reported privately: see [SECURITY.md](SECURITY.md).

Local Playlist is written in Rust with [GPUI Kit](https://gpui-kit.com) (GPUI Fast
backend), rodio and yt-dlp.

## Building

One crate builds three things:

| Build | Command |
| --- | --- |
| Desktop app (default) | `cargo build` |
| Server only | `cargo build --no-default-features --features server` |
| Web UI (wasm, nightly) | `./scripts/build-web.sh` |

### Desktop app

Install the system libraries (Fedora package names; CI's Ubuntu list is in
`.github/workflows/ci.yml`), then build and run:

```sh
sudo dnf install alsa-lib-devel webkit2gtk4.1-devel gtk3-devel dbus-devel
cargo build --release
target/release/local-playlist
```

This builds two executables: the app, `local-playlist`, and its login window,
`local-playlist-login`, which the app starts from its own folder when you log in to a
platform. Keep them side by side. (The login window is separate so the app itself never
loads the webview libraries. `cargo run` builds only the app.)

At runtime nothing else is needed: the app downloads yt-dlp and QuickJS. **ffmpeg** is
optional. Downloads are turned into plain m4a files with the cover embedded either way
(built in); ffmpeg, when present, does that conversion instead and can also convert the
rare tracks without AAC/MP3 audio. With "System" components, you need `yt-dlp` and one of
`deno`/`node`/`bun`/`qjs` on `PATH`.

### Server and web UI

The browser version is GPUI compiled to WebAssembly. It needs a nightly toolchain because
GPUI's web backend uses unstable wasm-threading features; the desktop app builds on stable.

```sh
rustup toolchain install nightly --profile minimal --target wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <wasm-bindgen version from Cargo.lock> --locked
# optional, shrinks the download ~40%: wasm-opt from https://github.com/WebAssembly/binaryen/releases
./scripts/build-web.sh      # writes web/dist/
cargo build --release --features server   # desktop app + server, embeds web/dist
```

Without `web/dist`, `--server` still runs but serves only the API.

For a server-only binary (no GUI libraries, links only libc; this is what the Docker
image uses), which needs none of the system libraries above:

```sh
cargo build --release --no-default-features --features server
```

The Docker image can also be built from source: `docker build -t local-playlist .`

Cargo features: `desktop` (GPUI app, audio, webview login, keyring; implies `ui`) is on
by default. `server` adds `--server`; it embeds the web UI (~6 MB), so it's opt-in for
desktop builds. The browser build uses only `ui`.

The server's options are listed by `local-playlist --server --help`: `--listen ADDR`,
`--data-dir DIR` (or `$LOCAL_PLAYLIST_DATA`), `--password-file FILE` (or
`$LOCAL_PLAYLIST_PASSWORD`), `--system-tools`, `--cache-minutes N`.

## Development

```sh
cargo fmt                        # format (CI checks it; settings in rustfmt.toml)
cargo test                       # offline unit tests
cargo test -- --ignored          # live tests against YouTube / SoundCloud / GitHub
local-playlist --version         # version + commit, for bug reports
```

Where things live:

| File | Purpose |
| --- | --- |
| `src/app.rs` | Main window: sidebar, track list, player bar |
| `src/model.rs` | Library model and JSON persistence |
| `src/sources/` | Link parsing; YouTube and SoundCloud metadata |
| `src/app/settings.rs` | Settings page: accounts and components |
| `src/ytdlp.rs` | Components (yt-dlp build, QuickJS), metadata fallback, audio download |
| `src/login.rs` | Starts the login window and reads the session cookies it returns |
| `src/bin/login.rs` | The login window (`local-playlist-login <platform>`), a webview |
| `src/secrets.rs` | Encrypted cookie storage, cookie parsing |
| `src/server.rs` | `--server`: web UI, auth, resolve and audio streaming API |
| `src/web/` | Browser build: entry point, server API client, `<audio>` playback, stand-ins |
| `src/audio.rs` | Streaming playback engine (decoder thread, buffering, seeking) |
| `src/download.rs` | Shared download state |
| `src/storage.rs` | Music folder layout, removed-tracks cache |
| `src/lyrics/` | Lyrics lookup (LRCLIB, lrc.red), artist/title guessing, LRC and Lyricsfile parsing |
| `src/app/lyrics.rs` | The lyrics page (karaoke highlighting) |
| `src/tray.rs` | Tray icon for playing in the background (Linux: ksni, Windows: Shell_NotifyIcon) |

[AGENTS.md](AGENTS.md) has a more detailed map and the project's conventions.

On Windows the desktop app is a GUI program (no console window). Started from a terminal,
its output and logs go to that terminal. The terminal doesn't wait for it, though, so they
appear after the prompt. The server build is an ordinary console program.

### Releases

Builds for Linux and Windows (x64), the desktop app and the server (with the web UI), are
published on the Releases page by GitHub Actions, each built on its own platform
(`.github/workflows/build.yml`):

- Every push to `main` that changes the code is built and published as the pre-release
  `dev-<commit>`, replacing the previous one, and as the Docker image `:dev`
  (`.github/workflows/dev.yml`).
- Bumping `version` in `Cargo.toml` publishes the release `v<version>` and the Docker
  images `:<version>` and `:latest`, and removes the development pre-release
  (`.github/workflows/release.yml`). It can also be run by hand from the Actions tab
  (Release → Run workflow).

Release builds use full LTO with a single codegen unit (smaller, at least as fast, but
slower to compile). The web build uses the size-focused `wasm-release` profile plus
`wasm-opt`. Approximate release sizes: desktop app (including server and web UI) 48 MB,
server-only 9.5 MB, web UI 5.8 MB gzipped.

## Before opening a pull request

- **Format with `cargo fmt`.** `rustfmt.toml` matches the project's style (120 columns);
  CI fails on unformatted code.
- **Run `cargo test`**, and check the server-only build still compiles.
- **Run `./scripts/build-web.sh`** if you changed UI code shared with the web build.
- **Run `cargo test -- --ignored`** if you changed sources, components or playback. These
  are live tests against YouTube, SoundCloud and GitHub, so use public links only.
- Keep a pull request to one change, and update `README.md` / `ADDING-COOKIES.md` if
  behaviour changes.

CI runs formatting, the tests on Linux and Windows, and a wasm build check on every pull
request. The pull request template has the full checklist.

## Privacy rules

Local Playlist doesn't phone home. Changes must keep it that way:

- No telemetry, analytics or new network destinations other than the platforms
  themselves, SponsorBlock, the lyrics databases LRCLIB and lrc.red (only while the
  lyrics page is open), GitHub for components, and PyPI for the `yt-dlp-ejs` wheel.
- Platform cookies are sent only to their own platform, and only when needed.
- The web UI and the server never receive platform credentials.
- No cookies, tokens or passwords in logs or error messages.

## License

By contributing, you agree that your contributions are licensed under the
[GNU General Public License v3.0 or later](LICENSE), like the rest of the project.

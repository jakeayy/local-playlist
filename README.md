# Local Playlist

A privacy-friendly, local-first music player for links from **YouTube / YouTube Music** and
**SoundCloud**. Paste links, build playlists that live on your machine, and play them
directly in the app. No accounts, no logins, no telemetry.

Built with Rust, [GPUI Kit](https://gpui-kit.com) (GPUI Fast backend), rodio, and yt-dlp.

## Features

- **Material-style light and dark themes** that can follow the system.
- **Mixed playlists:** add YouTube, YouTube Music and SoundCloud tracks to the same playlist
  by pasting their link (`watch?v=`, `youtu.be/`, `shorts/`, `music.youtube.com`,
  `soundcloud.com/artist/track`, `on.soundcloud.com` short links).
- **Import** whole collections as a new playlist, without logging in:
  - YouTube / YouTube Music public or unlisted playlists
  - SoundCloud sets/albums, a profile's tracks, and a profile's public likes (`/likes`)
- **Built-in playback with streaming:** playback starts after about 3 s of audio has
  downloaded instead of waiting for the whole file, and you can seek while it's still
  downloading. Play/pause, next/previous, volume, shuffle, and repeat (playlist / track)
  are all there. The next track is prefetched so playback continues without a gap.
- **Real files on disk:** tracks are saved as
  `<Music>/local-playlist/<Playlist name>/<Platform>-<id>.m4a`, so they play offline and
  in any player. "Download all" fetches a whole playlist. Renaming a playlist renames its
  folder.
- **Removed-tracks cache:** removing a track moves its file to a size-limited cache
  (2 GB by default; oldest files go first). Re-adding the track restores it instantly.
  The same song in several playlists is hard-linked or copied, never downloaded twice.
- **Optional accounts** for private playlists, YouTube *Liked videos* / *Liked music* /
  *Watch later*, and your SoundCloud likes:
  - **Log in** opens the platform's normal sign-in page in a private (incognito) webview
    that identifies as Firefox. Once you're signed in, the app keeps only the session
    cookies. There are no OAuth apps or client IDs.
  - **Paste cookies…** accepts a Netscape `cookies.txt` export or a raw `Cookie:` header.
    See [ADDING-COOKIES.md](ADDING-COOKIES.md) for how to get them.
  - Sessions are used only when needed: for personal lists, or as a retry when anonymous
    access fails.
- **Batteries included:** on first run the app offers to download
  - **yt-dlp:** the official build of the latest `master` commit
    (`yt-dlp/yt-dlp-master-builds`, SHA-256 verified), with Python and the YouTube
    challenge solver bundled.
  - **QuickJS** (~3 MB), the JavaScript runtime yt-dlp uses for YouTube.

  Each component can be switched to the system version in Settings. "Update bundled
  components" fetches the newest builds.

## How it stays private

- Your library is a plain JSON file at `~/.local/share/local-playlist/library.json`.
- Logins are stored in `credentials.bin`, encrypted with ChaCha20-Poly1305. The key lives
  in the system keyring (Secret Service / Windows Credential Manager / macOS Keychain).
  "Log out" deletes the session.
- Metadata comes from each platform's public web endpoints, the same ones their websites
  use for logged-out visitors: YouTube InnerTube and oEmbed, and SoundCloud api-v2 with the
  public web `client_id`. Unless you log in, no cookies are sent or stored.
- Audio is downloaded by yt-dlp into your music folder (configurable in Settings → Storage)
  and played locally. Removed tracks wait in `~/.cache/local-playlist/removed/` up to the
  size limit.
- The only network destinations are YouTube, SoundCloud, and their CDNs. GitHub is
  contacted only to download or update components.

## Web version (self-hosted)

The same app also runs in the browser, served by the binary itself:

```sh
LOCAL_PLAYLIST_PASSWORD='a long password' local-playlist --server --listen 0.0.0.0:8080
# or interactively: local-playlist --server   (prompts for the password)
```

Open `http://<host>:8080`, enter the password, and use it like the desktop app.

- **Privacy:** your playlists and settings stay in the browser (local storage). The server
  only looks up public links and streams their audio. It never receives platform logins,
  so private playlists, likes and offline folders are desktop-only. Request URLs aren't
  logged, and cached audio is deleted after `--cache-minutes` (default 120).
- **Safety:** an access password (at least 8 characters) is required, and repeated wrong
  guesses are throttled. Only YouTube and SoundCloud links are accepted, so the server
  can't be used as a general proxy. Put it behind HTTPS (a reverse proxy) when exposing
  it beyond your network.
- **Components:** yt-dlp and QuickJS are downloaded into the data directory on first
  start (`--system-tools` uses the ones on PATH), and yt-dlp is updated daily.
- **Tip:** run it on a home connection. Datacenter IPs often get YouTube's "confirm
  you're not a bot" wall.

Options: `--listen ADDR`, `--data-dir DIR` (or `$LOCAL_PLAYLIST_DATA`), `--password-file FILE`,
`--system-tools`, `--cache-minutes N`.

### Docker

```sh
docker build -t local-playlist .
docker run -d --name local-playlist -p 8080:8080 -v lp-data:/data \
    -e LOCAL_PLAYLIST_PASSWORD='a long password' local-playlist
```

### Building the web UI yourself

The browser build is GPUI compiled to WebAssembly. It needs a nightly toolchain because
GPUI's web backend uses unstable wasm-threading features. The desktop app builds on stable.

```sh
rustup toolchain install nightly --profile minimal --target wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <wasm-bindgen version from Cargo.lock> --locked
./scripts/build-web.sh      # writes web/dist/
cargo build --release       # embeds web/dist into the binary
```

Without `web/dist`, `--server` still runs but serves only the API.

## Requirements

Runtime: nothing beyond the OS. yt-dlp and QuickJS are downloaded by the app.
**ffmpeg** is optional; when present it remuxes downloads into plain m4a.
If you choose "System" components instead, you need `yt-dlp` and one of `deno`/`node`/
`bun`/`qjs` on `PATH`.

Build (Fedora package names):

```sh
sudo dnf install alsa-lib-devel webkit2gtk4.1-devel gtk3-devel dbus-devel
cargo run --release
```

## Development

```sh
cargo test                       # offline unit tests
cargo test -- --ignored          # live tests against YouTube / SoundCloud / GitHub
```

Layout:

| File | Purpose |
| --- | --- |
| `src/app.rs` | Main window: sidebar, track list, player bar |
| `src/model.rs` | Library model and JSON persistence |
| `src/sources/` | Link parsing; YouTube and SoundCloud metadata |
| `src/app/settings.rs` | Settings page: accounts and components |
| `src/ytdlp.rs` | Components (yt-dlp build, QuickJS), metadata fallback, audio download |
| `src/login.rs` | Webview login helper process (`local-playlist --login <platform>`) |
| `src/secrets.rs` | Encrypted cookie storage, cookie parsing |
| `src/server.rs` | `--server`: web UI, auth, resolve and audio streaming API |
| `src/web/` | Browser build: entry point, server API client, `<audio>` playback, stand-ins |
| `src/audio.rs` | Streaming playback engine (decoder thread, buffering, seeking) |
| `src/download.rs` | Shared download state |
| `src/storage.rs` | Music folder layout, removed-tracks cache |

# Local Playlist

A privacy-friendly, local-first music player for links from **YouTube / YouTube Music** and
**SoundCloud**. Paste links, build playlists that live on your machine, and play them
directly in the app. No accounts, no logins, no telemetry.

Built with Rust, [GPUI Kit](https://gpui-kit.com) (GPUI Fast backend), rodio, and yt-dlp.

## Features

- **Mixed playlists:** add YouTube, YouTube Music and SoundCloud tracks to the same playlist
  by pasting their link (`watch?v=`, `youtu.be/`, `shorts/`, `music.youtube.com`,
  `soundcloud.com/artist/track`, `on.soundcloud.com` short links).
- **Import** whole collections as a new playlist, without logging in:
  - YouTube / YouTube Music public or unlisted playlists
  - SoundCloud sets/albums, a profile's tracks, and a profile's public likes (`/likes`)
- **Built-in playback:** play/pause, next/previous, seek, volume, shuffle, and repeat
  (playlist / track). The next track is prefetched so playback continues without a gap.
- **Optional accounts** for private playlists, YouTube *Liked videos* / *Liked music* /
  *Watch later*, and your SoundCloud likes:
  - **Log in** opens the platform's normal sign-in page in a private (incognito) webview
    that identifies as Firefox. Once you're signed in, the app keeps only the session
    cookies. There are no OAuth apps or client IDs.
  - **Paste cookies…** accepts a Netscape `cookies.txt` export or a raw `Cookie:` header.
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
  public web `client_id`. No cookies are sent or stored.
- Audio is downloaded by yt-dlp into `~/.cache/local-playlist/audio/` and played locally.
  The trash button under "Update yt-dlp" clears that cache.
- The only network destinations are YouTube, SoundCloud, and their CDNs. GitHub is
  contacted only to download or update components.

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
| `src/audio.rs` | rodio playback wrapper |

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
- **One-click "Update yt-dlp":** fetches the latest commit of yt-dlp's `master` branch
  plus the matching `yt-dlp-ejs` YouTube challenge solver, and installs it into the app's
  data directory. Nothing to compile or `pip install`.

## How it stays private

- Your library is a plain JSON file at `~/.local/share/local-playlist/library.json`.
- Metadata comes from each platform's public web endpoints, the same ones their websites
  use for logged-out visitors: YouTube InnerTube and oEmbed, and SoundCloud api-v2 with the
  public web `client_id`. No cookies are sent or stored.
- Audio is downloaded by yt-dlp into `~/.cache/local-playlist/audio/` and played locally.
  The trash button under "Update yt-dlp" clears that cache.
- The only network destinations are YouTube, SoundCloud, and their CDNs. GitHub and PyPI
  are contacted only when you click "Update yt-dlp".

## Requirements

Runtime:

- **Python 3.10+** (`python3`), used to run the managed yt-dlp. If you haven't clicked
  "Update yt-dlp" yet, a `yt-dlp` on your `PATH` is used instead.
- A **JavaScript runtime** (`deno`, `node`, `bun` or `qjs`) on `PATH`. Recent yt-dlp
  versions need one to solve YouTube's challenges.
- **ffmpeg** (recommended): converts formats rodio can't decode (e.g. Opus) to AAC.

Build (Fedora package names):

```sh
sudo dnf install alsa-lib-devel   # audio output (cpal)
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
| `src/ytdlp.rs` | Managed yt-dlp: update from master, metadata fallback, audio download |
| `src/audio.rs` | rodio playback wrapper |

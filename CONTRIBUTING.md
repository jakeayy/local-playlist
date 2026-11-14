# Contributing

Thanks for helping. Bug reports, site-breakage reports ("YouTube changed something") and
feature requests go through the issue forms; questions and ideas go to Discussions.
Security problems are reported privately: see [SECURITY.md](SECURITY.md).

## Building

See **Requirements** and **Development** in the [README](README.md) for the desktop app,
and **Building the web UI yourself** for the browser build. There are three builds to
keep working:

| Build | Command |
| --- | --- |
| Desktop app (default) | `cargo build` |
| Server only | `cargo build --no-default-features --features server` |
| Web UI (wasm, nightly) | `./scripts/build-web.sh` |

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
  themselves, SponsorBlock, and GitHub for components.
- Platform cookies are sent only to their own platform, and only when needed.
- The web UI and the server never receive platform credentials.
- No cookies, tokens or passwords in logs or error messages.

## License

By contributing, you agree that your contributions are licensed under the
[GNU General Public License v3.0 or later](LICENSE), like the rest of the project.

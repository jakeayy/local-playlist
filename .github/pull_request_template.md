## Summary

<!-- What does this change and why? Link related issues: "Fixes #123". -->

## Type

- [ ] Bug fix
- [ ] Platform / extraction fix (YouTube, SoundCloud)
- [ ] New feature
- [ ] Refactor / performance / size
- [ ] Docs / CI / templates

## Affected builds

- [ ] Desktop app (`desktop` feature)
- [ ] Web UI (wasm, `ui` feature)
- [ ] Server (`server` feature / Docker)

## Testing

- [ ] `cargo fmt`
- [ ] `cargo test`
- [ ] `cargo build --no-default-features --features server` (server-only build still compiles)
- [ ] `./scripts/build-web.sh` if shared UI code changed (the wasm build compiles)
- [ ] `cargo test -- --ignored` if sources, components or playback changed (live network tests)
- [ ] Tried it in the app; screenshots below for UI changes (light **and** dark theme)

<!-- Describe manual testing: what you clicked, which links you used (public ones only). -->

## Privacy checklist

- [ ] No telemetry, analytics or new network destinations (other than the platforms themselves, SponsorBlock, GitHub for components and PyPI for the `yt-dlp-ejs` wheel)
- [ ] Platform cookies are only sent to their own platform, and only when needed
- [ ] The web UI / server never receive platform credentials
- [ ] No secrets (cookies, tokens, passwords) in logs or error messages
- [ ] Docs updated if behaviour changed (`README.md`, `ADDING-COOKIES.md`)

## Screenshots

<!-- For UI changes. Remove this section otherwise. -->

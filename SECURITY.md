# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through
[GitHub's security advisories](https://github.com/jakeayy/local-playlist/security/advisories/new),
not in a public issue. Include the version (`local-playlist --version`), your platform,
and steps to reproduce.

## Supported versions

Fixes go into the latest release and the current development build (the `dev-<commit>`
pre-release). Older releases aren't patched; please update first.

## In scope

Anything that could expose a user's accounts, files or machine, for example:

- Platform cookies or sessions leaking: `credentials.bin`, its key in the system keyring,
  logs, error messages, or cookies sent to the wrong site.
- The server (`--server`): getting past its password, reading or writing files outside the
  music folder, or the web UI receiving platform credentials.
- Downloaded components (yt-dlp, QuickJS): getting the app to run a file that wasn't
  verified.
- Crafted links, playlists or metadata that make the app run code or write outside its
  folders.

## Out of scope

- Bugs in yt-dlp itself: report them to [yt-dlp](https://github.com/yt-dlp/yt-dlp/security).
- Problems with YouTube, SoundCloud or SponsorBlock services.
- Attacks that need someone who already controls your user account or machine.

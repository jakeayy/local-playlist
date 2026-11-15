# Local Playlist website (`gh-pages`)

This branch holds only the project's website, published by GitHub Pages at
<https://jakeayy.github.io/local-playlist/>. The app itself is on `main`.

It's a static page with no build step: plain HTML, CSS and JavaScript, and nothing loaded from
third parties (no web fonts, CDNs or analytics). The only requests to other hosts go to GitHub
itself: the latest release (for direct download links) and the contributors list, both from
`api.github.com`, plus contributors' avatars. Without them, the page falls back to the static links.

| File | Purpose |
| --- | --- |
| `index.html` | The page: hero, demo, features, screenshots, privacy, download, FAQ, contributors |
| `assets/site.css` | Styles; Material 3 colours from the app's `assets/material-theme.json` |
| `assets/site.js` | Theme switch, download links for the visitor's system, contributors, copy button |
| `assets/demo.js` | The interactive preview (a simulated player with placeholder data) |
| `img/` | Icon, favicons, social preview image (`og-image.png`, 1200×630), screenshots (WebP) |
| `tools/faq-jsonld.py` | Regenerates the FAQ's structured data after editing the FAQ |
| `404.html`, `sitemap.xml`, `site.webmanifest`, `.nojekyll` | Pages plumbing |

## Preview locally

Serve the folder that *contains* this checkout, so the page runs under `/local-playlist/` as on
GitHub Pages (the 404 page relies on that path):

```sh
git worktree add ../lp-site gh-pages      # from a checkout of main
cd .. && python3 -m http.server 8000      # then open http://localhost:8000/lp-site/
```

(Under a different folder name everything works except the 404 page's links.)

## Editing

- **Placeholders:** the demo's tracks, artists, covers and SponsorBlock segments are invented
  (`PLACEHOLDER` comments in `assets/demo.js` and `index.html`). The screenshots come from
  `docs/screenshots/` on `main`, converted to WebP:
  `magick in.png -strip -quality 82 -define webp:method=6 out.webp`.
- **FAQ:** after changing a question or answer, run `python3 tools/faq-jsonld.py`. Search engines
  ignore FAQ data that doesn't match the visible text.
- **Icons** are Lucide symbols inlined at the top of `index.html` (`<symbol id="i-…">`), copied
  from `web/icons/` on `main`.
- **Facts** (platforms, features, privacy) mirror README.md on `main`; keep them in step.
- Prose uses British spelling, as in the rest of the project.

## SEO checklist

- After the first deployment, add the site in Google Search Console and Bing Webmaster Tools, and
  submit `https://jakeayy.github.io/local-playlist/sitemap.xml`. A `robots.txt` only counts at the
  root of a host, so a project site can't have one. The sitemap has to be submitted by hand.
- Set the repository's website field to the Pages URL. Links from the repository and its README
  are the strongest signal the page will get early on.
- With a custom domain, update `canonical`, `og:url`, `og:image`, the JSON-LD URLs, `sitemap.xml`
  and the root-relative links in `404.html`, and add a `robots.txt` pointing to the sitemap.

## Licence

Like the app, the site is under GPL-3.0-or-later. Icons: Lucide (ISC). Music in the screenshots:
releases by NoCopyrightSounds.

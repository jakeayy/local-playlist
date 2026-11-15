// Interactive preview of Local Playlist: an HTML mock of the app's main window.
//
// PLACEHOLDER: everything here is simulated. Tracks, artists, covers and SponsorBlock segments are made
// up; no audio plays; links typed into the demo are only checked against patterns on this page, never
// requested. Playback is a clock, downloads are timers, and the visualizer draws a synthetic spectrum.
(() => {
  'use strict';

  const demo = document.getElementById('lp-demo');
  if (!demo) return;
  const $ = (id) => document.getElementById(id);
  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
  const YT = 'youtube';
  const SC = 'soundcloud';
  const BASE_HUE = 262;

  // SponsorBlock categories as the app lists them in Settings (key, name, description, on by default).
  const SB_CATS = [
    ['music_offtopic', 'Non-music', 'Talking, skits and other non-music parts of music videos', true],
    ['sponsor', 'Sponsor', 'Paid promotion and sponsored segments', true],
    ['selfpromo', 'Self-promotion', 'Unpaid promotion: merch, donations, shout-outs', true],
    ['interaction', 'Interaction reminder', '"Like and subscribe" and similar reminders', true],
    ['intro', 'Intro', 'Intro animations and pauses without content', false],
    ['outro', 'Outro', 'Credits and end cards', false],
    ['preview', 'Preview / recap', "Previews of what's coming up, recaps", false],
    ['filler', 'Filler', 'Tangents and jokes not needed for the main content', false],
  ];

  // --- Placeholder library -------------------------------------------------------------------------
  let uid = 0;
  function T(title, artist, platform, dur, h1, h2, extra) {
    const [m, s] = dur.split(':').map(Number);
    uid += 1;
    return Object.assign({ key: 'demo:' + uid, title, artist, platform, dur: m * 60 + s, h1, h2, saved: false, dl: -1, segs: [] }, extra || {});
  }
  // PLACEHOLDER: invented titles and artists.
  const playlists = [
    { name: 'Demo mix', tracks: [
      T('Glass Horizon', 'Aria Vance', YT, '3:12', 268, 205, { segs: [[0, 7, 'music_offtopic'], [96, 104, 'interaction']] }),
      T('Paper Satellites', 'The Quiet Hours', SC, '3:42', 8, 330, { saved: true }),
      T('Neon Tide', 'Kairo, Lumen', YT, '3:30', 318, 250, { saved: true, segs: [[150, 166, 'sponsor']] }),
      T('Slow Orbit', 'Mira Sol', SC, '3:20', 36, 8, { saved: true }),
      T('Static Bloom', 'Northbound', YT, '2:41', 190, 232, { segs: [[0, 5, 'intro']] }),
      T('Lanterns', 'Ola Ferris, Juno', YT, '3:13', 150, 190),
      T('Wherever the Road Goes', 'Vesper', SC, '3:00', 220, 280),
      T('Afterglow', 'Syl & Kazu', YT, '2:48', 345, 20, { segs: [[142, 168, 'outro']] }),
    ] },
    { name: 'Focus', tracks: [
      T('Rain on Tin', 'Low Season', SC, '4:05', 200, 170, { saved: true }),
      T('Long Division', 'Halcyon Desk', YT, '3:48', 120, 160, { segs: [[60, 72, 'selfpromo']] }),
      T('Margins', 'Pale Office', SC, '2:59', 60, 30),
      T('Quiet Hours', 'Ines Moreau', YT, '3:33', 280, 300, { saved: true }),
    ] },
    { name: 'Late-night drive', tracks: [
      T('Sodium Lights', 'Route 9', YT, '3:57', 28, 300, { saved: true, segs: [[200, 220, 'sponsor']] }),
      T('Overpass', 'Kite Season', SC, '3:21', 240, 200),
      T('Exit 44', 'Marlow', YT, '4:12', 330, 270),
    ] },
    { name: 'House party', tracks: [
      T('Strobe Garden', 'Pixel Saints', YT, '3:05', 300, 40, { segs: [[0, 9, 'music_offtopic']] }),
      T('Kitchen Floor', 'DJ Halfmoon', SC, '5:30', 50, 0),
      T('Loud Neighbours', 'The Balconies', SC, '3:44', 160, 100, { saved: true }),
      T('Last Bus Home', 'Junie Park', YT, '3:16', 210, 330),
    ] },
  ];

  // --- State --------------------------------------------------------------------------------------
  const S = {
    sel: 0,              // index of the playlist on screen
    queue: null,         // playlist the current track plays from
    current: null,       // current track (object)
    paused: true,
    pos: 0,
    shuffle: false,
    repeat: 0,           // 0 off, 1 all, 2 one
    view: 'list',
    vizMode: 'circle',
    volume: 0.7,
    muted: false,
    query: '',
    coverColours: true,
    dtheme: 'system',
    sb: Object.fromEntries(SB_CATS.map((c) => [c[0], c[3]])),
    downloads: [],       // tracks waiting or downloading
    busy: false,         // a link is being "looked up"
  };

  // --- Helpers ------------------------------------------------------------------------------------
  const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const icon = (name, cls) => '<svg class="i' + (cls ? ' ' + cls : '') + '" aria-hidden="true"><use href="#i-' + name + '"></use></svg>';
  function fmt(sec) {
    sec = Math.max(0, Math.floor(sec));
    const h = Math.floor(sec / 3600);
    const m = Math.floor((sec % 3600) / 60);
    const s = String(sec % 60).padStart(2, '0');
    return h ? h + ':' + String(m).padStart(2, '0') + ':' + s : m + ':' + s;
  }
  const cover = (t) => 'radial-gradient(circle at 30% 25%, hsl(' + t.h1 + ' 90% 75% / .9), transparent 55%), linear-gradient(135deg, hsl(' + t.h1 + ' 70% 55%), hsl(' + t.h2 + ' 65% 28%))';
  const platformName = (p) => (p === YT ? 'YouTube' : 'SoundCloud');
  const badge = (p) => '<span class="d-badge ' + (p === YT ? 'yt' : 'sc') + '">' + platformName(p) + '</span>';
  const hashHue = (s) => { let h = 0; for (const c of s) h = (h * 31 + c.charCodeAt(0)) >>> 0; return h % 360; };
  const titleCase = (s) => s.replace(/[-_]+/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase()).trim();
  const plOf = () => playlists[S.sel];

  // --- "Things to try" progress ----------------------------------------------------------------------
  const goals = new Set();
  const goalItems = Array.from(document.querySelectorAll('.try-list li[data-goal]'));
  if ($('try-total')) $('try-total').textContent = goalItems.length;
  function done(goal) {
    if (goals.has(goal)) return;
    const li = document.querySelector('.try-list li[data-goal="' + goal + '"]');
    if (!li) return;
    goals.add(goal);
    li.classList.add('done');
    const use = li.querySelector('use');
    if (use) use.setAttribute('href', '#i-circle-check');
    $('try-count').textContent = goals.size;
    $('try-bar').style.width = (goals.size / goalItems.length) * 100 + '%';
    if (goals.size === goalItems.length) $('try-done').hidden = false;
  }

  // --- Rendering ----------------------------------------------------------------------------------
  function renderLists() {
    const html = playlists.map((p, i) => {
      const playing = S.queue === p && S.current && !S.paused;
      return '<button class="d-pl' + (playing ? ' is-playing' : '') + '" type="button" data-act="select-pl" data-i="' + i + '" aria-current="' + (i === S.sel && S.view === 'list') + '">' +
        icon(playing ? 'volume-2' : 'list-music') + '<span class="d-pl-name">' + esc(p.name) + '</span><span class="d-pl-count">' + p.tracks.length + '</span></button>';
    }).join('');
    $('d-lists').innerHTML = html;
    $('d-mobile-lists').innerHTML = html +
      '<button class="d-pl d-chip-new" type="button" data-act="new-playlist" aria-label="New playlist">' + icon('plus') + '</button>' +
      '<button class="d-pl d-chip-settings" type="button" data-act="settings">' + icon('settings') + '<span class="d-pl-name">Settings</span></button>';
  }

  function renderHead() {
    const p = plOf();
    $('d-title').textContent = p ? p.name : 'No playlists';
    const total = p ? p.tracks.reduce((a, t) => a + t.dur, 0) : 0;
    const n = p ? p.tracks.length : 0;
    $('d-meta').textContent = n + (n === 1 ? ' track' : ' tracks') + (n ? ' · ' + fmt(total) : '');
  }

  function renderTracks() {
    const p = plOf();
    const list = $('d-tracks');
    const empty = $('d-empty');
    if (!p) {
      list.innerHTML = '';
      empty.hidden = false;
      empty.textContent = 'No playlists yet. Make one with + or import one.';
      return;
    }
    const q = S.query.trim().toLowerCase();
    const rows = [];
    p.tracks.forEach((t, i) => {
      if (q && !(t.title.toLowerCase().includes(q) || t.artist.toLowerCase().includes(q))) return;
      const cur = t === S.current;
      const state = t.saved ? icon('circle-check') : t.dl >= 0 ? icon('loader-circle', 'spin') : '';
      const stateLabel = t.saved ? 'Saved for offline listening' : t.dl >= 0 ? 'Downloading' : '';
      rows.push(
        '<li class="d-row' + (cur ? ' is-current' : '') + (t.isNew ? ' is-new' : '') + '" data-i="' + i + '">' +
          '<span class="d-num">' + (cur && !S.paused ? icon('volume-2') : i + 1) + '</span>' +
          '<span class="d-cover" style="background:' + cover(t) + '"></span>' +
          '<button class="d-row-main" type="button" data-act="play-track" data-i="' + i + '">' +
            '<span class="visually-hidden">' + (cur && !S.paused ? 'Pause ' : 'Play ') + '</span>' +
            '<span class="d-row-title">' + esc(t.title) + '</span><span class="d-row-artist">' + esc(t.artist) + '</span>' +
          '</button>' +
          '<span class="d-state"' + (stateLabel ? ' title="' + stateLabel + '"' : '') + '>' + state + (stateLabel ? '<span class="visually-hidden">' + stateLabel + '</span>' : '') + '</span>' +
          badge(t.platform) +
          '<span class="d-dur">' + fmt(t.dur) + '</span>' +
          '<span class="d-row-tools">' +
            '<button class="d-icon d-up" type="button" data-act="up" data-i="' + i + '" aria-label="Move up" title="Move up">' + icon('arrow-up') + '</button>' +
            '<button class="d-icon d-down" type="button" data-act="down" data-i="' + i + '" aria-label="Move down" title="Move down">' + icon('arrow-down') + '</button>' +
            '<button class="d-icon d-remove" type="button" data-act="remove" data-i="' + i + '" aria-label="Remove ' + esc(t.title) + '" title="Remove">' + icon('x') + '</button>' +
          '</span>' +
        '</li>');
      t.isNew = false;
    });
    list.innerHTML = rows.join('');
    empty.hidden = rows.length > 0;
    if (!rows.length) {
      empty.textContent = q ? 'No tracks match "' + S.query.trim() + '".' : 'This playlist is empty. Paste a YouTube or SoundCloud link above to add a track.';
    }
  }

  function renderPlayer() {
    const t = S.current;
    $('d-now-title').textContent = t ? t.title : 'Nothing playing';
    $('d-now-sub').textContent = t ? t.artist + ' · ' + platformName(t.platform) : 'Click a track to start';
    $('d-now-cover').style.background = t ? cover(t) : '';
    const play = $('d-play');
    play.querySelector('use').setAttribute('href', S.current && !S.paused ? '#i-pause' : '#i-play');
    play.setAttribute('aria-label', S.current && !S.paused ? 'Pause' : 'Play');
    demo.querySelector('[data-act="shuffle"]').setAttribute('aria-pressed', S.shuffle);
    const rep = $('d-repeat');
    rep.setAttribute('aria-pressed', S.repeat > 0);
    rep.setAttribute('aria-label', 'Repeat: ' + ['off', 'all', 'one'][S.repeat]);
    rep.querySelector('use').setAttribute('href', S.repeat === 2 ? '#i-repeat-1' : '#i-repeat');
    const seek = $('d-seek');
    seek.disabled = !t;
    seek.max = t ? t.dur : 100;
    $('d-dur').textContent = fmt(t ? t.dur : 0);
    $('d-segs').innerHTML = t ? t.segs.filter((s) => S.sb[s[2]]).map((s) =>
      '<span style="left:' + (s[0] / t.dur) * 100 + '%;width:' + ((s[1] - s[0]) / t.dur) * 100 + '%"></span>').join('') : '';
    $('d-viz-title').textContent = t ? t.title : 'Nothing playing';
    $('d-viz-artist').textContent = t ? t.artist : 'Pick a track to see it move';
    const muted = S.muted || S.volume === 0;
    $('d-mute').querySelector('use').setAttribute('href', muted ? '#i-volume-x' : '#i-volume-2');
    $('d-mute').setAttribute('aria-label', muted ? 'Unmute' : 'Mute');
    renderProgress();
    applyAccent();
  }

  function renderProgress() {
    const t = S.current;
    const seek = $('d-seek');
    if (document.activeElement !== seek || !t) seek.value = t ? S.pos : 0;
    seek.style.setProperty('--p', t ? (S.pos / t.dur) * 100 + '%' : '0%');
    $('d-pos').textContent = fmt(t ? S.pos : 0);
  }

  function renderVolume() {
    const v = $('d-volume');
    v.value = S.muted ? 0 : Math.round(S.volume * 100);
    v.style.setProperty('--p', v.value + '%');
  }

  function renderSettings() {
    demo.querySelectorAll('[data-act="dtheme"]').forEach((b) => b.setAttribute('aria-checked', b.dataset.val === S.dtheme));
    demo.querySelector('[data-act="cover-colours"]').setAttribute('aria-checked', S.coverColours);
    $('d-sb').querySelectorAll('input').forEach((c) => { c.checked = !!S.sb[c.value]; });
  }

  function renderAll() {
    renderLists();
    renderHead();
    renderTracks();
    renderPlayer();
  }

  function applyAccent() {
    const hue = S.coverColours && S.current ? S.current.h1 : BASE_HUE;
    demo.style.setProperty('--dh', hue);
  }

  // --- Views --------------------------------------------------------------------------------------
  function setView(v) {
    S.view = v;
    demo.dataset.view = v;
    $('d-view-list').hidden = v !== 'list';
    $('d-view-viz').hidden = v !== 'viz';
    $('d-view-settings').hidden = v !== 'settings';
    $('d-viz-btn').setAttribute('aria-pressed', v === 'viz');
    renderLists();
    if (v === 'viz') { done('viz'); sizeCanvas(); kickViz(); }
  }

  // --- Toast --------------------------------------------------------------------------------------
  let toastTimer = 0;
  function toast(text, action) {
    const el = $('d-toast');
    const btn = $('d-toast-action');
    $('d-toast-text').textContent = text;
    btn.hidden = !action;
    btn.onclick = action ? () => { action.run(); hideToast(); } : null;
    if (action) btn.textContent = action.label;
    el.hidden = true;
    void el.offsetWidth; // restart the entry animation
    el.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(hideToast, action ? 6500 : 4000);
  }
  function hideToast() { $('d-toast').hidden = true; }

  // --- Playback (simulated) -------------------------------------------------------------------------
  function play(pl, i) {
    const t = pl.tracks[i];
    if (!t) return;
    S.queue = pl;
    S.current = t;
    S.pos = 0;
    S.paused = false;
    startDownload(t, true);
    done('play');
    checkSegments();
    renderAll();
    startClock();
  }

  function toggle() {
    if (!S.current) {
      const p = plOf();
      if (p && p.tracks.length) play(p, S.shuffle ? Math.floor(Math.random() * p.tracks.length) : 0);
      return;
    }
    S.paused = !S.paused;
    if (!S.paused) done('play');
    renderAll();
  }

  function next(auto) {
    const pl = S.queue;
    if (!pl || !pl.tracks.length) return;
    if (auto && S.repeat === 2) { S.pos = 0; checkSegments(); return; }
    let i = pl.tracks.indexOf(S.current);
    if (S.shuffle && pl.tracks.length > 1) {
      let j;
      do { j = Math.floor(Math.random() * pl.tracks.length); } while (j === i);
      i = j;
    } else {
      i += 1;
      if (i >= pl.tracks.length) {
        if (auto && S.repeat === 0) { S.paused = true; S.pos = 0; renderAll(); return; }
        i = 0;
      }
    }
    play(pl, i);
  }

  function prev() {
    const pl = S.queue;
    if (!pl || !S.current) return;
    if (S.pos > 3) { S.pos = 0; checkSegments(); renderProgress(); return; }
    let i = pl.tracks.indexOf(S.current) - 1;
    if (i < 0) i = pl.tracks.length - 1;
    play(pl, i);
  }

  // Jump past an enabled SponsorBlock segment that the position is in.
  function checkSegments() {
    const t = S.current;
    if (!t) return;
    for (const [s, e, cat] of t.segs) {
      if (S.sb[cat] && S.pos >= s && S.pos < e - 0.25) {
        S.pos = e;
        const name = SB_CATS.find((c) => c[0] === cat)[1];
        toast('Skipped ' + name.toLowerCase() + ' (' + fmt(s) + '–' + fmt(e) + ') with SponsorBlock');
        done('skip');
        renderProgress();
        return;
      }
    }
  }

  // --- Downloads (simulated): the playing track first, two at a time. ------------------------------------
  function startDownload(t, urgent) {
    if (t.saved) return; // plays from disk
    if (S.downloads.includes(t)) {
      if (urgent && t.dl < 0) { S.downloads = [t].concat(S.downloads.filter((x) => x !== t)); t.dl = 0; }
      return;
    }
    if (urgent) { S.downloads.unshift(t); t.dl = 0; } else S.downloads.push(t);
    startClock();
  }
  function stepDownloads(dt) {
    let changed = false;
    let active = 0;
    for (const t of S.downloads) {
      if (t.dl < 0 && active < 2) { t.dl = 0; changed = true; }
      if (t.dl >= 0) {
        active += 1;
        t.dl += dt / (t === S.current ? 2.5 : 3.5);
        if (t.dl >= 1) { t.dl = -1; t.saved = true; changed = true; }
      }
    }
    if (changed) {
      const before = S.downloads.length;
      S.downloads = S.downloads.filter((t) => !t.saved);
      if (before && !S.downloads.length && S.batch) { toast('All tracks saved for offline listening'); S.batch = false; }
      renderTracks();
    }
  }

  // --- Clock ----------------------------------------------------------------------------------------
  let clock = 0;
  let last = 0;
  function startClock() {
    if (clock) return;
    last = performance.now();
    clock = setInterval(tick, 200);
  }
  function tick() {
    const now = performance.now();
    const dt = Math.min(1, (now - last) / 1000);
    last = now;
    if (S.downloads.length) stepDownloads(dt);
    if (S.current && !S.paused) {
      S.pos += dt;
      checkSegments();
      if (S.pos >= S.current.dur) { S.pos = S.current.dur; next(true); }
      renderProgress();
    }
    if (!S.downloads.length && (!S.current || S.paused)) { clearInterval(clock); clock = 0; }
  }

  // --- Links (checked locally, never requested) -----------------------------------------------------------
  function parseLink(raw) {
    let u;
    try { u = new URL(raw.trim()); } catch (e) { return { error: "That doesn't look like a link. Paste a YouTube or SoundCloud link." }; }
    if (!/^https?:$/.test(u.protocol)) return { error: 'Only YouTube and SoundCloud links work.' };
    const host = u.hostname.toLowerCase().replace(/^(www\.|m\.)/, '');
    const parts = u.pathname.split('/').filter(Boolean);
    if (host === 'youtube.com' || host === 'music.youtube.com') {
      const v = u.searchParams.get('v');
      const list = u.searchParams.get('list');
      if (parts[0] === 'watch' && v && /^[\w-]{11}$/.test(v)) return { kind: 'track', platform: YT, id: v };
      if (parts[0] === 'shorts' && /^[\w-]{11}$/.test(parts[1] || '')) return { kind: 'track', platform: YT, id: parts[1] };
      if (parts[0] === 'playlist' && list) {
        const personal = { LL: 'Liked videos', WL: 'Watch later', LM: 'Liked music' }[list];
        if (personal) return { login: personal };
        return { kind: 'playlist', platform: YT, id: list, name: 'Imported playlist' };
      }
      return { error: 'Paste a link to a YouTube video, short or playlist.' };
    }
    if (host === 'youtu.be' && /^[\w-]{11}$/.test(parts[0] || '')) return { kind: 'track', platform: YT, id: parts[0] };
    if (host === 'on.soundcloud.com' && parts[0]) return { kind: 'track', platform: SC, id: 'on/' + parts[0] };
    if (host === 'soundcloud.com' && parts[0]) {
      const user = parts[0];
      if (parts.length === 1 || (parts.length === 2 && parts[1] === 'tracks')) return { kind: 'playlist', platform: SC, id: user + '/tracks', name: titleCase(user), user };
      if (parts[1] === 'likes') return { kind: 'playlist', platform: SC, id: user + '/likes', name: titleCase(user) + "'s likes", user };
      if (parts[1] === 'sets' && parts[2]) return { kind: 'playlist', platform: SC, id: user + '/sets/' + parts[2], name: titleCase(parts[2]), user };
      if (parts.length === 2) return { kind: 'track', platform: SC, id: user + '/' + parts[1], title: titleCase(parts[1]), artist: user };
    }
    return { error: 'Only YouTube and SoundCloud links work.' };
  }

  function trackFromLink(r) {
    const hue = hashHue(r.id);
    // PLACEHOLDER: the real app looks the title, artist, length and cover up on the platform.
    return T(r.title || 'Your track (' + r.id + ')', r.artist || 'Title appears in the app', r.platform, '3:' + String(10 + (hue % 45)).padStart(2, '0'), hue, (hue + 50) % 360,
      { key: r.platform + ':' + r.id, isNew: true });
  }
  function tracksFromList(r) {
    const n = 5;
    const out = [];
    for (let k = 1; k <= n; k += 1) {
      const hue = hashHue(r.id + k);
      out.push(T('Playlist track ' + k, r.user || 'From the playlist', r.platform, (2 + (hue % 3)) + ':' + String(hue % 60).padStart(2, '0'), hue, (hue + 40) % 360,
        { key: r.platform + ':' + r.id + '#' + k, isNew: true }));
    }
    return out;
  }

  function showError(msg, info) {
    const el = $('d-add-error');
    el.textContent = msg;
    el.classList.toggle('is-info', !!info);
    el.hidden = !msg;
  }

  function resolving(btn, on) {
    S.busy = on;
    btn.disabled = on;
    btn.querySelector('use').setAttribute('href', on ? '#i-loader-circle' : btn.dataset.icon);
    btn.querySelector('svg').classList.toggle('spin', on);
  }

  function addLink() {
    const input = $('d-add-input');
    const raw = input.value;
    if (!raw.trim() || S.busy) return;
    const r = parseLink(raw);
    if (r.error) { showError(r.error); return; }
    if (r.login) { showError('"' + r.login + '" is personal, so it needs a login (Settings & accounts). The demo has no accounts.', true); return; }
    showError('');
    let p = plOf();
    if (!p) { playlists.push(p = { name: 'New playlist', tracks: [] }); S.sel = playlists.length - 1; }
    const btn = $('d-add-btn');
    resolving(btn, true);
    setTimeout(() => {
      resolving(btn, false);
      const fresh = r.kind === 'track' ? [trackFromLink(r)] : tracksFromList(r);
      const have = new Set(p.tracks.map((t) => t.key));
      const add = fresh.filter((t) => !have.has(t.key));
      if (!add.length) { showError('That track is already in this playlist.', true); return; }
      p.tracks.push(...add);
      input.value = '';
      S.query = '';
      $('d-search').value = '';
      renderAll();
      const list = $('d-tracks');
      list.scrollTop = list.scrollHeight;
      toast(add.length === 1 ? 'Added. The app fills in the real title, artist and cover.' : 'Added ' + add.length + ' placeholder tracks from the playlist.');
      done('add');
    }, 650);
  }

  function importLink() {
    const input = $('d-import-input');
    const raw = input.value;
    if (!raw.trim() || S.busy) { input.focus(); return; }
    const r = parseLink(raw);
    if (r.error) { toast(r.error); return; }
    if (r.login) { toast('"' + r.login + '" needs a login (Settings & accounts).'); return; }
    if (r.kind !== 'playlist') { toast("That's a single track: paste it in the box above the track list."); return; }
    const btn = demo.querySelector('[data-act="import"]');
    resolving(btn, true);
    setTimeout(() => {
      resolving(btn, false);
      let name = r.name;
      let n = 2;
      while (playlists.some((p) => p.name === name)) name = r.name + ' ' + n++;
      playlists.push({ name, tracks: tracksFromList(r) });
      S.sel = playlists.length - 1;
      input.value = '';
      setView('list');
      renderAll();
      toast('Imported "' + name + '" with placeholder tracks.');
      done('add');
    }, 800);
  }

  // --- Editing --------------------------------------------------------------------------------------
  function move(i, d) {
    const p = plOf();
    const j = i + d;
    if (j < 0 || j >= p.tracks.length) return;
    [p.tracks[i], p.tracks[j]] = [p.tracks[j], p.tracks[i]];
    renderTracks();
    const btn = $('d-tracks').querySelector('[data-act="' + (d < 0 ? 'up' : 'down') + '"][data-i="' + j + '"]');
    if (btn) btn.focus();
  }

  function remove(i) {
    const p = plOf();
    const [t] = p.tracks.splice(i, 1);
    if (!t) return;
    renderAll();
    // The app has no undo: pasting the link again restores the file from the removed-tracks cache.
    toast('Removed "' + t.title + '". Its file waits in the cache, so adding the link again is instant.', {
      label: 'Add it back',
      run: () => {
        if (!p.tracks.includes(t)) p.tracks.push(t);
        t.isNew = true;
        done('undo');
        renderAll();
        const list = $('d-tracks');
        list.scrollTop = list.scrollHeight;
      },
    });
  }

  function deletePlaylist() {
    const i = S.sel;
    const [p] = playlists.splice(i, 1);
    if (!p) return;
    if (S.queue === p) { S.current = null; S.queue = null; S.paused = true; S.pos = 0; }
    S.sel = Math.max(0, Math.min(i, playlists.length - 1));
    renderAll();
    toast('Deleted "' + p.name + '". Reload the page to reset the demo.');
  }

  function newPlaylist() {
    let name = 'New playlist';
    let n = 2;
    while (playlists.some((p) => p.name === name)) name = 'New playlist ' + n++;
    playlists.push({ name, tracks: [] });
    S.sel = playlists.length - 1;
    setView('list');
    renderAll();
    $('d-add-input').focus();
  }

  function downloadAll() {
    const p = plOf();
    if (!p) return;
    const todo = p.tracks.filter((t) => !t.saved);
    if (!todo.length) { toast('Every track in "' + p.name + '" is already saved.'); return; }
    todo.forEach((t) => startDownload(t, false));
    S.batch = true;
    renderTracks();
    toast('Saving ' + todo.length + (todo.length === 1 ? ' track' : ' tracks') + ' to Music/local-playlist/' + p.name);
  }

  // --- Events ---------------------------------------------------------------------------------------
  demo.addEventListener('click', (e) => {
    const el = e.target.closest('[data-act]');
    const row = e.target.closest('.d-row');
    if (!el && row) { onTrackClick(+row.dataset.i); return; }
    if (!el) return;
    const i = +el.dataset.i;
    switch (el.dataset.act) {
      case 'select-pl':
        S.sel = i; S.query = ''; $('d-search').value = ''; showError('');
        setView('list'); renderAll(); break;
      case 'play-track': onTrackClick(i); break;
      case 'play-all': {
        const p = plOf();
        if (p && p.tracks.length) play(p, S.shuffle ? Math.floor(Math.random() * p.tracks.length) : 0);
        break;
      }
      case 'toggle': toggle(); break;
      case 'next': next(false); break;
      case 'prev': prev(); break;
      case 'shuffle': S.shuffle = !S.shuffle; renderPlayer(); break;
      case 'repeat': S.repeat = (S.repeat + 1) % 3; renderPlayer(); break;
      case 'viz': setView(S.view === 'viz' ? 'list' : 'viz'); break;
      case 'viz-mode':
        S.vizMode = el.dataset.mode;
        demo.querySelectorAll('[data-act="viz-mode"]').forEach((b) => b.setAttribute('aria-pressed', b === el));
        kickViz();
        break;
      case 'mute': S.muted = !S.muted; renderVolume(); renderPlayer(); break;
      case 'up': move(i, -1); break;
      case 'down': move(i, 1); break;
      case 'remove': remove(i); break;
      case 'new-playlist': newPlaylist(); break;
      case 'delete-playlist': deletePlaylist(); break;
      case 'download-all': downloadAll(); break;
      case 'import': importLink(); break;
      case 'settings': setView(S.view === 'settings' ? 'list' : 'settings'); break;
      case 'close-settings': setView('list'); break;
      case 'dtheme':
        S.dtheme = el.dataset.val;
        demo.dataset.dtheme = S.dtheme;
        renderSettings();
        done('theme');
        break;
      case 'cover-colours':
        S.coverColours = !S.coverColours;
        renderSettings();
        applyAccent();
        done('theme');
        if (S.coverColours && !S.current) toast('Play a track to see its cover colour.');
        break;
      case 'login':
        toast('In the app, this opens a small login window. The demo has no accounts.');
        break;
      default: break;
    }
  });

  function onTrackClick(i) {
    const p = plOf();
    if (!p) return;
    if (p.tracks[i] === S.current) toggle(); else play(p, i);
  }

  $('d-add-form').addEventListener('submit', (e) => { e.preventDefault(); addLink(); });
  $('d-add-input').addEventListener('input', () => showError(''));
  $('d-import-input').addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); importLink(); } });
  $('d-search').addEventListener('input', (e) => { S.query = e.target.value; renderTracks(); });
  const seek = $('d-seek');
  seek.addEventListener('input', () => { if (!S.current) return; S.pos = +seek.value; renderProgress(); });
  seek.addEventListener('change', () => { checkSegments(); seek.blur(); });
  $('d-volume').addEventListener('input', (e) => { S.volume = e.target.value / 100; S.muted = false; renderVolume(); renderPlayer(); });
  $('d-sb').addEventListener('change', (e) => {
    if (e.target.type !== 'checkbox') return;
    S.sb[e.target.value] = e.target.checked;
    renderPlayer();
  });
  for (const b of demo.querySelectorAll('[data-act="import"], #d-add-btn')) b.dataset.icon = b.querySelector('use').getAttribute('href');

  // SponsorBlock checkboxes.
  $('d-sb').insertAdjacentHTML('beforeend', SB_CATS.map((c) =>
    '<label class="d-check"><input type="checkbox" value="' + c[0] + '"' + (c[3] ? ' checked' : '') + '><span>' + esc(c[1]) + '</span><small>' + esc(c[2]) + '</small></label>').join(''));

  // --- "Things to try": point at the right part of the demo -----------------------------------------------
  const SAMPLES = [
    'https://soundcloud.com/example-artist/placeholder-track',
    'https://www.youtube.com/watch?v=xxxxxxxxxxx',
    'https://www.youtube.com/playlist?list=PLxxxxxxxxxxxxxxxx',
  ];
  let sample = 0;
  function point(el) {
    if (!el) return;
    el.classList.remove('pulse');
    void el.offsetWidth;
    el.classList.add('pulse');
    el.focus({ preventScroll: true });
    setTimeout(() => el.classList.remove('pulse'), 3700);
  }
  function bringDemoIntoView() {
    const r = demo.getBoundingClientRect();
    if (r.top < 64 || r.bottom > window.innerHeight) {
      demo.scrollIntoView({ block: r.height + 80 > window.innerHeight ? 'start' : 'center', behavior: reduceMotion.matches ? 'auto' : 'smooth' });
    }
  }
  function firstListPlaylist() {
    if (!playlists[S.sel] || !playlists[S.sel].tracks.length) {
      const i = playlists.findIndex((p) => p.tracks.length);
      if (i >= 0) S.sel = i;
    }
    setView('list');
    renderAll();
  }
  document.querySelectorAll('[data-try]').forEach((btn) => btn.addEventListener('click', () => {
    bringDemoIntoView();
    switch (btn.dataset.try) {
      case 'play':
        firstListPlaylist();
        point($('d-tracks').querySelector('.d-row-main'));
        break;
      case 'skip': {
        // Play a YouTube track whose start is an enabled segment, so the skip happens right away.
        let found = null;
        playlists.some((p) => p.tracks.some((t, i) => {
          if (t.segs.some((s) => s[0] === 0 && S.sb[s[2]])) { found = [p, i]; return true; }
          return false;
        }));
        if (!found) { S.sb.music_offtopic = true; renderSettings(); }
        if (found) {
          S.sel = playlists.indexOf(found[0]);
          setView('list');
          play(found[0], found[1]);
        } else {
          setView('settings');
          point($('d-sb'));
        }
        break;
      }
      case 'add':
        if (!plOf()) newPlaylist();
        setView('list');
        $('d-add-input').value = SAMPLES[sample++ % SAMPLES.length];
        showError('');
        point($('d-add-btn'));
        break;
      case 'viz':
        point($('d-viz-btn'));
        break;
      case 'theme':
        setView('settings');
        point($('d-theme-seg').querySelector('[aria-checked="true"]'));
        break;
      case 'undo':
        firstListPlaylist();
        point($('d-tracks').querySelector('.d-remove'));
        break;
      default: break;
    }
  }));

  // --- Visualizer: a night sky with an aurora and ridges, and a synthetic spectrum. ----------------------
  const canvas = $('d-canvas');
  const ctx = canvas.getContext('2d');
  const BANDS = 40;
  const AURORA_SCALE = 8;
  const aurora = document.createElement('canvas');
  const actx = aurora.getContext('2d');
  const bands = new Float32Array(BANDS);
  const peaks = new Float32Array(BANDS);
  let stars = [];
  let ridges = [];
  let raf = 0;
  let lastFrame = 0;
  let onScreen = true;
  let cw = 0;
  let ch = 0;

  function sizeCanvas() {
    const r = canvas.getBoundingClientRect();
    if (!r.width || !r.height) return;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.round(r.width * dpr);
    canvas.height = Math.round(r.height * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    if (Math.abs(r.width - cw) > 2 || Math.abs(r.height - ch) > 2) {
      cw = r.width;
      ch = r.height;
      stars = Array.from({ length: Math.round((cw * ch) / 2600) }, () => ({
        x: Math.random() * cw, y: Math.random() * ch * 0.75, r: Math.random() * 1.3 + 0.3, p: Math.random() * 6.3, s: 0.4 + Math.random() * 1.6,
      }));
      ridges = [ridge(ch * 0.74, ch * 0.09, 0.011), ridge(ch * 0.86, ch * 0.06, 0.019)];
    }
  }
  function ridge(base, amp, f) {
    const ph = Math.random() * 10;
    const pts = [];
    for (let x = 0; x <= cw + 8; x += 8) {
      pts.push([x, base - amp * (0.6 * Math.sin(x * f + ph) + 0.3 * Math.sin(x * f * 2.3 + ph * 2) + 0.15 * Math.sin(x * f * 5.1 + ph * 3))]);
    }
    return pts;
  }

  function updateBands(t, dt) {
    const on = S.current && !S.paused;
    const beat = Math.pow(Math.max(0, Math.sin(t * Math.PI * 2 * 0.96)), 12);
    for (let i = 0; i < BANDS; i += 1) {
      const f = i / (BANDS - 1);
      const base = 0.72 * Math.pow(1 - f, 1.3) + 0.12;
      const wob = 0.5 + 0.5 * Math.sin(t * (1.7 + f * 4.3) + i * 0.9) * Math.sin(t * (0.6 + f) + i * 2.1);
      const target = on ? Math.min(1, base * (0.45 + 0.55 * wob) + beat * Math.pow(1 - f, 2) * 0.45) : 0;
      const k = target > bands[i] ? 0.45 : 0.08;
      bands[i] += (target - bands[i]) * Math.min(1, k * dt * 60);
      peaks[i] = Math.max(peaks[i] - dt * 0.45, bands[i]);
    }
  }

  function draw(now) {
    const w = cw;
    const h = ch;
    if (!w || !h) return;
    const t = now / 1000;
    const hue = S.coverColours && S.current ? S.current.h1 : BASE_HUE;
    const bass = (bands[0] + bands[1] + bands[2]) / 3;

    const sky = ctx.createLinearGradient(0, 0, 0, h);
    sky.addColorStop(0, 'hsl(' + hue + ' 35% 5%)');
    sky.addColorStop(0.7, 'hsl(' + hue + ' 38% 11%)');
    sky.addColorStop(1, 'hsl(' + hue + ' 32% 16%)');
    ctx.fillStyle = sky;
    ctx.fillRect(0, 0, w, h);

    ctx.fillStyle = '#fff';
    for (const s of stars) {
      ctx.globalAlpha = 0.3 + 0.6 * Math.abs(Math.sin(t * s.s + s.p));
      ctx.fillRect(s.x, s.y, s.r, s.r);
    }
    ctx.globalAlpha = 1;

    // Aurora curtains, drawn small and blurred, then scaled up: soft edges for little work.
    const aw = Math.ceil(w / AURORA_SCALE);
    const ah = Math.ceil(h / AURORA_SCALE);
    if (aurora.width !== aw || aurora.height !== ah) { aurora.width = aw; aurora.height = ah; }
    actx.setTransform(1, 0, 0, 1, 0, 0);
    actx.clearRect(0, 0, aw, ah);
    actx.filter = 'blur(2px)';
    actx.setTransform(1 / AURORA_SCALE, 0, 0, 1 / AURORA_SCALE, 0, 0);
    for (let k = 0; k < 3; k += 1) {
      const top = (x) => h * (0.12 + 0.06 * k) + Math.sin(x * 0.006 + t * 0.22 + k * 2) * h * 0.05 + Math.sin(x * 0.013 - t * 0.37 + k) * h * 0.025;
      const depth = h * (0.24 - 0.04 * k);
      const g = actx.createLinearGradient(0, h * 0.04, 0, h * 0.6);
      const a = 0.16 + bass * 0.14;
      const c = 'hsla(' + (hue + 70 + k * 35) + ', 85%, 62%, ';
      g.addColorStop(0, c + '0)');
      g.addColorStop(0.3, c + a + ')');
      g.addColorStop(1, c + '0)');
      actx.fillStyle = g;
      actx.beginPath();
      actx.moveTo(0, top(0));
      for (let x = 0; x <= w; x += 24) actx.lineTo(x, top(x));
      for (let x = w; x >= 0; x -= 24) actx.lineTo(x, top(x) + depth);
      actx.closePath();
      actx.fill();
    }
    ctx.globalCompositeOperation = 'lighter';
    ctx.imageSmoothingEnabled = true;
    ctx.drawImage(aurora, 0, 0, w, h);
    ctx.globalCompositeOperation = 'source-over';

    ridges.forEach((pts, k) => {
      ctx.fillStyle = 'hsl(' + hue + ' 28% ' + (k ? 5 : 9) + '%)';
      ctx.beginPath();
      ctx.moveTo(0, h);
      for (const [x, y] of pts) ctx.lineTo(x, y);
      ctx.lineTo(w, h);
      ctx.closePath();
      ctx.fill();
    });

    if (S.vizMode === 'bars') drawBars(w, h, hue); else drawCircle(w, h, hue, bass);
  }

  function drawCircle(w, h, hue, bass) {
    const cx = w / 2;
    const cy = h * 0.5;
    const m = Math.min(w, h);
    const R = m * 0.19 * (1 + 0.16 * bass);
    const N = 80;
    const layers = [
      [1, 'hsla(' + (hue + 25) + ', 30%, 52%, .55)'],
      [0.8, 'hsla(' + hue + ', 70%, 80%, .75)'],
      [0.58, 'hsl(' + hue + ' 35% 94%)'],
    ];
    for (const [scale, colour] of layers) {
      ctx.fillStyle = colour;
      ctx.beginPath();
      for (let j = 0; j <= N; j += 1) {
        const a = -Math.PI / 2 + (j / N) * Math.PI * 2;
        // Bass at the sides, treble towards the top and bottom, mirrored: the "wings".
        const fromSide = Math.abs(Math.asin(Math.max(-1, Math.min(1, Math.sin(a))))) / (Math.PI / 2);
        const at = fromSide * (BANDS - 1);
        const lo = Math.floor(at);
        const v = bands[lo] + (bands[Math.min(BANDS - 1, lo + 1)] - bands[lo]) * (at - lo);
        const r = R + m * 0.4 * v * scale * (0.25 + 0.75 * Math.abs(Math.cos(a)));
        const x = cx + Math.cos(a) * r;
        const y = cy + Math.sin(a) * r * 0.82;
        if (j) ctx.lineTo(x, y); else ctx.moveTo(x, y);
      }
      ctx.closePath();
      ctx.fill();
    }
    // The cover in the middle.
    const t = S.current;
    const g = ctx.createLinearGradient(cx - R, cy - R, cx + R, cy + R);
    g.addColorStop(0, 'hsl(' + (t ? t.h1 : hue) + ' 70% 58%)');
    g.addColorStop(1, 'hsl(' + (t ? t.h2 : hue + 40) + ' 65% 26%)');
    ctx.fillStyle = g;
    ctx.beginPath();
    ctx.arc(cx, cy, R, 0, Math.PI * 2);
    ctx.fill();
    ctx.lineWidth = Math.max(3, m * 0.014);
    ctx.strokeStyle = 'hsl(' + hue + ' 20% 8%)';
    ctx.stroke();
  }

  function drawBars(w, h, hue) {
    const slot = w / BANDS;
    const bw = slot * 0.78;
    const base = h * 0.94;
    const maxH = h * 0.6;
    const g = ctx.createLinearGradient(0, base - maxH, 0, base);
    g.addColorStop(0, 'hsl(' + hue + ' 90% 85%)');
    g.addColorStop(1, 'hsl(' + (hue + 30) + ' 70% 55%)');
    for (let i = 0; i < BANDS; i += 1) {
      const x = i * slot + (slot - bw) / 2;
      const bh = Math.max(2, bands[i] * maxH);
      ctx.fillStyle = g;
      ctx.beginPath();
      if (ctx.roundRect) ctx.roundRect(x, base - bh, bw, bh, [Math.min(4, bw / 2), Math.min(4, bw / 2), 0, 0]); else ctx.rect(x, base - bh, bw, bh);
      ctx.fill();
      ctx.fillStyle = 'hsl(' + hue + ' 60% 92%)';
      ctx.fillRect(x, base - peaks[i] * maxH - 6, bw, 3);
    }
  }

  const vizActive = () => S.view === 'viz' && onScreen && !document.hidden;
  function frame(now) {
    raf = 0;
    if (!vizActive()) return;
    const dt = lastFrame ? Math.min(0.1, (now - lastFrame) / 1000) : 0.016;
    lastFrame = now;
    updateBands(now / 1000, reduceMotion.matches ? 1 : dt);
    draw(now);
    if (reduceMotion.matches) setTimeout(kickViz, 600); else raf = requestAnimationFrame(frame);
  }
  function kickViz() {
    if (!raf && vizActive()) { lastFrame = 0; raf = requestAnimationFrame(frame); }
  }
  if ('ResizeObserver' in window) new ResizeObserver(() => { if (S.view === 'viz') { sizeCanvas(); kickViz(); } }).observe(canvas);
  if ('IntersectionObserver' in window) {
    new IntersectionObserver((entries) => { onScreen = entries[0].isIntersecting; kickViz(); }).observe(demo);
  }
  document.addEventListener('visibilitychange', kickViz);

  // --- Start ----------------------------------------------------------------------------------------
  renderAll();
  renderVolume();
  renderSettings();
})();

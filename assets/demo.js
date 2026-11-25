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

  // SponsorBlock categories (the app skips them all), named in the "Skipped …" message.
  const SB_NAMES = {
    music_offtopic: 'Non-music',
    sponsor: 'Sponsor',
    selfpromo: 'Self-promotion',
    interaction: 'Interaction reminder',
    intro: 'Intro',
    outro: 'Outro',
    preview: 'Preview / recap',
    filler: 'Filler',
  };

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
    sponsorblock: true,
    importOpen: false,  // the sidebar's "Import playlist" section
    downloads: [],       // tracks waiting or downloading
    busy: false,         // a link is being "looked up"
    closed: false,       // the window is closed and the desktop shows (easter egg)
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
    $('d-segs').innerHTML = t && S.sponsorblock ? t.segs.map((s) =>
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
    $('d-sb').setAttribute('aria-checked', S.sponsorblock);
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

  // Jump past the SponsorBlock segment that the position is in.
  function checkSegments() {
    const t = S.current;
    if (!t || !S.sponsorblock) return;
    for (const [s, e, cat] of t.segs) {
      if (S.pos >= s && S.pos < e - 0.25) {
        S.pos = e;
        const name = SB_NAMES[cat];
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
      case 'sponsorblock':
        S.sponsorblock = !S.sponsorblock;
        renderSettings();
        renderPlayer();
        break;
      case 'import-toggle':
        S.importOpen = !S.importOpen;
        el.setAttribute('aria-expanded', S.importOpen);
        $('d-import-body').hidden = !S.importOpen;
        if (S.importOpen) $('d-import-input').focus();
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
      case 'desk-app': openWindow(el); break;
      case 'desk-term': openTerm(el); break;
      case 'term-close': closeTerm(); break;
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
  for (const b of demo.querySelectorAll('[data-act="import"], #d-add-btn')) b.dataset.icon = b.querySelector('use').getAttribute('href');

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
    if (S.closed) openWindow(null, true);
    bringDemoIntoView();
    switch (btn.dataset.try) {
      case 'play':
        firstListPlaylist();
        point($('d-tracks').querySelector('.d-row-main'));
        break;
      case 'skip': {
        // Play a YouTube track that starts with a segment, so the skip happens right away.
        if (!S.sponsorblock) { S.sponsorblock = true; renderSettings(); }
        let found = null;
        playlists.some((p) => p.tracks.some((t, i) => {
          if (t.segs.some((s) => s[0] === 0)) { found = [p, i]; return true; }
          return false;
        }));
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

  const vizActive = () => S.view === 'viz' && onScreen && !document.hidden && !S.closed;
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

  // --- Easter egg -----------------------------------------------------------------------------------
  // The window's first dot closes it. It stays a plain dot on purpose (no button, cursor, hover or
  // focus), so it's a surprise. Closed, the demo shows a desktop with the app, which opens the window
  // again, and a terminal with a tiny shell, where `rm -rf` removes the whole page. The window's state
  // survives closing; nothing is saved, so a reload brings everything back.
  const HOME = ['home', 'visitor'];
  const DESKTOP_FILES = {
    'local-playlist.desktop': '[Desktop Entry]\nType=Application\nName=Local Playlist\nExec=local-playlist\nIcon=local-playlist',
    'terminal.desktop': '[Desktop Entry]\nType=Application\nName=Terminal\nExec=terminal',
  };
  let desk = null;
  let term = null;
  let deskClock = 0;
  let winBusy = false;
  let wiping = false;
  let cwd = HOME.slice();
  const hist = [];
  let histAt = 0;

  const winParts = () => Array.from(demo.children).filter((el) => el.matches('.d-chrome, .d-body, .d-player'));
  const settle = (anims) => Promise.all(anims.map((a) => a.finished.catch(() => {})));
  // Scale every part of the window around the same point (in the demo's padding box), so they move as one.
  function originAt(parts, x, y) {
    parts.forEach((p) => { p.style.transformOrigin = (x - p.offsetLeft) + 'px ' + (y - p.offsetTop) + 'px'; });
  }

  function buildDesk() {
    if (desk) return;
    demo.insertAdjacentHTML('beforeend',
      '<div class="d-desk" id="d-desk" role="group" aria-label="Desktop" hidden>' +
        '<div class="d-desk-icons">' +
          '<button class="d-desk-icon" type="button" data-act="desk-app"><img src="img/icon.svg" width="48" height="48" alt=""><span>Local Playlist</span></button>' +
          '<button class="d-desk-icon" type="button" data-act="desk-term"><span class="d-desk-glyph"><svg class="i" viewBox="3 3 18 18" aria-hidden="true"><path d="m7 11 2-2-2-2"/><path d="M11 13h4"/></svg></span><span>Terminal</span></button>' +
        '</div>' +
        '<div class="d-term" id="d-term" role="group" aria-label="Terminal" hidden>' +
          '<div class="d-term-bar" id="d-term-bar">' +
            '<button class="d-term-dot" type="button" data-act="term-close" aria-label="Close terminal"></button><span class="d-term-dot"></span><span class="d-term-dot"></span>' +
            '<span class="d-term-title" id="d-term-title"></span>' +
          '</div>' +
          '<div class="d-term-body" id="d-term-body">' +
            '<div class="d-term-out" id="d-term-out" role="log"></div>' +
            '<form class="d-term-line" id="d-term-form"><span id="d-term-ps"></span>' +
              '<input class="d-term-in" id="d-term-in" type="text" aria-label="Command" autocomplete="off" autocapitalize="off" autocorrect="off" spellcheck="false" enterkeyhint="go">' +
            '</form>' +
          '</div>' +
        '</div>' +
        '<div class="d-desk-bar"><span id="d-desk-clock"></span></div>' +
      '</div>');
    desk = $('d-desk');
    term = $('d-term');
    const input = $('d-term-in');
    $('d-term-form').addEventListener('submit', (e) => {
      e.preventDefault();
      const line = input.value;
      input.value = '';
      run(line);
    });
    input.addEventListener('keydown', (e) => {
      if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
        if (!hist.length) return;
        e.preventDefault();
        histAt = Math.max(0, Math.min(hist.length, histAt + (e.key === 'ArrowUp' ? -1 : 1)));
        input.value = hist[histAt] || '';
      } else if (e.ctrlKey && (e.key === 'c' || e.key === 'C') && input.selectionStart === input.selectionEnd) {
        e.preventDefault();
        promptLine(input.value + '^C');
        input.value = '';
        histAt = hist.length;
      } else if (e.ctrlKey && (e.key === 'l' || e.key === 'L')) {
        e.preventDefault();
        $('d-term-out').textContent = '';
      }
    });
    $('d-term-body').addEventListener('click', () => {
      if (!$('d-term-form').hidden && !String(window.getSelection())) input.focus({ preventScroll: true });
    });
    // Move the terminal by its title bar, keeping the bar on the desktop.
    const bar = $('d-term-bar');
    bar.addEventListener('pointerdown', (e) => {
      if (e.button !== 0 || e.target.closest('button')) return;
      const sx = e.clientX;
      const sy = e.clientY;
      const l = term.offsetLeft;
      const t = term.offsetTop;
      bar.setPointerCapture(e.pointerId);
      const move = (ev) => {
        term.style.left = Math.max(0, Math.min(desk.clientWidth - term.offsetWidth, l + ev.clientX - sx)) + 'px';
        term.style.top = Math.max(0, Math.min(desk.clientHeight - 64, t + ev.clientY - sy)) + 'px';
      };
      const up = () => {
        bar.removeEventListener('pointermove', move);
        bar.removeEventListener('pointerup', up);
        bar.removeEventListener('pointercancel', up);
      };
      bar.addEventListener('pointermove', move);
      bar.addEventListener('pointerup', up);
      bar.addEventListener('pointercancel', up);
    });
  }

  function tickDesk() {
    const now = new Date();
    $('d-desk-clock').textContent = now.toLocaleDateString([], { weekday: 'short', day: 'numeric', month: 'short' }) + '  ' +
      now.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }

  function closeWindow() {
    if (S.closed || winBusy) return;
    buildDesk();
    winBusy = true;
    S.closed = true;
    hideToast();
    desk.hidden = false;
    tickDesk();
    clearInterval(deskClock);
    deskClock = setInterval(tickDesk, 10000);
    const parts = winParts();
    originAt(parts, demo.clientWidth / 2, demo.clientHeight / 2);
    const anims = parts.map((p) => p.animate([{ opacity: 1, transform: 'none' }, { opacity: 0, transform: 'scale(.92)' }],
      { duration: reduceMotion.matches ? 0 : 200, easing: 'ease-in', fill: 'forwards' }));
    settle(anims).then(() => {
      // Opened again meanwhile (a "Things to try" button): just drop the animation.
      if (S.closed) demo.dataset.closed = '';
      anims.forEach((a) => a.cancel());
      parts.forEach((p) => { p.style.transformOrigin = ''; });
      winBusy = false;
      if (S.closed) desk.querySelector('[data-act="desk-app"]').focus({ preventScroll: true });
    });
  }

  // Open the window again, growing out of `from` (the desktop icon) unless `instant`.
  function openWindow(from, instant) {
    if (!S.closed || (winBusy && !instant)) return;
    const box = demo.getBoundingClientRect();
    const f = from && from.getBoundingClientRect();
    S.closed = false;
    delete demo.dataset.closed;
    const parts = winParts();
    const finish = () => {
      winBusy = false;
      if (S.closed) return;
      const refocus = desk.contains(document.activeElement);
      desk.hidden = true;
      clearInterval(deskClock);
      if (refocus) $('d-play').focus({ preventScroll: true });
      // Measured mid-animation, the canvas came out scaled down: size it at its real size.
      if (S.view === 'viz') { sizeCanvas(); kickViz(); }
    };
    if (instant || !f || reduceMotion.matches) finish();
    else {
      winBusy = true;
      originAt(parts, f.left + f.width / 2 - box.left - demo.clientLeft, f.top + f.height / 2 - box.top - demo.clientTop);
      settle(parts.map((p) => p.animate([{ opacity: 0, transform: 'scale(.05)' }, { opacity: 1, transform: 'none' }],
        { duration: 320, easing: 'cubic-bezier(.2, .8, .2, 1)' }))).then(() => {
        parts.forEach((p) => { p.style.transformOrigin = ''; });
        finish();
      });
    }
    kickViz();
  }

  function openTerm(from) {
    const input = $('d-term-in');
    if (!term.hidden) { input.focus({ preventScroll: true }); return; }
    $('d-term-out').textContent = '';
    cwd = HOME.slice();
    updatePrompt();
    term.hidden = false;
    const left = (desk.clientWidth - term.offsetWidth) / 2 + (desk.clientWidth > 720 ? 48 : 0);
    // Too narrow to sit beside the icons (phones): below them instead, as far as the panel allows.
    const beside = desk.clientWidth >= term.offsetWidth + 130;
    const top = beside ? (desk.clientHeight - 34 - term.offsetHeight) / 2 : Math.min(desk.clientHeight - 46 - term.offsetHeight, 212);
    term.style.left = Math.max(0, Math.min(desk.clientWidth - term.offsetWidth, left)) + 'px';
    term.style.top = Math.max(12, top) + 'px';
    if (from && !reduceMotion.matches) {
      const f = from.getBoundingClientRect();
      const t = term.getBoundingClientRect();
      term.style.transformOrigin = (f.left + f.width / 2 - t.left) + 'px ' + (f.top + f.height / 2 - t.top) + 'px';
      term.animate([{ opacity: 0, transform: 'scale(.1)' }, { opacity: 1, transform: 'none' }], { duration: 260, easing: 'cubic-bezier(.2, .8, .2, 1)' });
    }
    input.focus({ preventScroll: true });
  }

  function closeTerm() {
    if (term.hidden || wiping) return;
    term.style.transformOrigin = '50% 50%';
    const a = term.animate([{ opacity: 1 }, { opacity: 0, transform: 'scale(.94)' }],
      { duration: reduceMotion.matches ? 0 : 160, easing: 'ease-in', fill: 'forwards' });
    settle([a]).then(() => {
      term.hidden = true;
      a.cancel();
      if (!S.closed) return;
      desk.querySelector('[data-act="desk-term"]').focus({ preventScroll: true });
    });
  }

  // --- The shell -------------------------------------------------------------------------------------
  // A pretend file system: folders are objects, files are strings. The music folder follows the demo's
  // playlists and the tracks it has "saved", named the way the app names them.
  function fsRoot() {
    const music = {};
    for (const p of playlists) {
      const dir = {};
      for (const t of p.tracks) {
        if (t.saved) dir[platformName(t.platform) + '-' + t.key.replace(/^demo:/, 'demo').replace(/^[a-z]+:/, '').replace(/[^\w-]/g, '_') + '.m4a'] = '';
      }
      music[p.name] = dir;
    }
    return { bin: {}, etc: {}, home: { visitor: { Desktop: Object.assign({}, DESKTOP_FILES), Music: { 'local-playlist': music } } }, tmp: {}, usr: {} };
  }
  function resolve(arg) {
    let parts;
    if (!arg || arg === '~') return HOME.slice();
    if (arg.startsWith('~/')) parts = HOME.concat(arg.slice(2).split('/'));
    else if (arg.startsWith('/')) parts = arg.split('/');
    else parts = cwd.concat(arg.split('/'));
    const path = [];
    for (const p of parts) {
      if (p === '..') path.pop();
      else if (p && p !== '.') path.push(p);
    }
    return path;
  }
  function lookup(path) {
    let node = fsRoot();
    for (const p of path) {
      if (typeof node !== 'object' || !Object.prototype.hasOwnProperty.call(node, p)) return undefined;
      node = node[p];
    }
    return node;
  }
  function pretty(path) {
    const s = '/' + path.join('/');
    const home = '/' + HOME.join('/');
    return s === home || s.startsWith(home + '/') ? '~' + s.slice(home.length) : s;
  }
  const quote = (name) => (/[\s'"]/.test(name) ? "'" + name.replace(/'/g, "'\\''") + "'" : name);
  function words(line) {
    const found = [];
    const re = /"([^"]*)"|'([^']*)'|(\S+)/g;
    let m;
    while ((m = re.exec(line))) found.push(m[1] !== undefined ? m[1] : m[2] !== undefined ? m[2] : m[3]);
    return found;
  }

  // A line of output: a string, or [text, class, colour] pieces.
  function lineOf(content, cls) {
    const line = document.createElement('div');
    if (cls) line.className = 'd-term-' + cls;
    if (typeof content === 'string') line.textContent = content || ' ';
    else for (const [text, c, colour] of content) {
      const s = document.createElement('span');
      if (c) s.className = 'd-term-' + c;
      if (colour) s.style.color = colour;
      s.textContent = text;
      line.append(s);
    }
    return line;
  }
  function emit(el) {
    const box = $('d-term-out');
    box.append(el);
    while (box.childElementCount > 300) box.firstElementChild.remove();
    const body = $('d-term-body');
    body.scrollTop = body.scrollHeight;
  }
  const out = (content, cls) => emit(lineOf(content, cls));
  const promptPieces = () => [['visitor@desktop', 'u'], [':', ''], [pretty(cwd), 'p'], ['$ ', '']];
  const promptLine = (text) => out(promptPieces().concat([[text, '']]));
  function updatePrompt() {
    const ps = $('d-term-ps');
    ps.textContent = '';
    for (const [text, c] of promptPieces()) {
      const s = document.createElement('span');
      if (c) s.className = 'd-term-' + c;
      s.textContent = text;
      ps.append(s);
    }
    $('d-term-title').textContent = 'visitor@desktop: ' + pretty(cwd);
  }

  // Any `rm` with both -r and -f, in whatever spelling, with or without a target.
  function isWipe(args) {
    let r = false;
    let f = false;
    for (const a of args) {
      const w = a.toLowerCase();
      if (w === '--recursive') r = true;
      else if (w === '--force') f = true;
      else if (/^-[a-z]+$/.test(w)) { r = r || w.includes('r'); f = f || w.includes('f'); }
    }
    return r && f;
  }

  function run(line) {
    promptLine(line);
    const text = line.trim();
    if (text) hist.push(text);
    histAt = hist.length;
    let args = words(text);
    if (!args.length) return;
    let cmd = args[0].toLowerCase();
    const sudo = cmd === 'sudo';
    if (sudo) {
      args = args.slice(1);
      cmd = (args[0] || '').toLowerCase();
      if (!cmd) { out('usage: sudo command', 'err'); return; }
    }
    if (cmd === 'rm' && isWipe(args.slice(1))) {
      wipe(args.slice(1).some((a) => a.toLowerCase() === '--verbose' || /^-[a-z]*v/i.test(a)));
      return;
    }
    if (sudo) { out('visitor is not in the sudoers file. This incident will be reported.', 'err'); return; }
    const ops = args.slice(1).filter((a) => !a.startsWith('-'));
    switch (cmd) {
      case 'help':
        out('Commands: cat  cd  clear  date  echo  exit  fastfetch  help  history  local-playlist  ls  pwd  whoami');
        break;
      case 'fastfetch': fastfetch(); break;
      case 'ls': {
        const targets = ops.length ? ops : ['.'];
        targets.forEach((t, k) => {
          const node = lookup(resolve(t));
          if (node === undefined) { out("ls: cannot access '" + t + "': No such file or directory", 'err'); return; }
          if (typeof node !== 'object') { out(quote(t)); return; }
          if (targets.length > 1) out((k ? '\n' : '') + t + ':');
          const names = Object.keys(node).sort((a, b) => a.localeCompare(b));
          const pieces = [];
          names.forEach((n, j) => {
            if (j) pieces.push(['  ', '']);
            pieces.push([quote(n), typeof node[n] === 'object' ? 'dir' : '']);
          });
          if (pieces.length) out(pieces);
        });
        break;
      }
      case 'cd': {
        const path = resolve(ops[0]);
        const node = lookup(path);
        if (node === undefined) out('cd: ' + ops[0] + ': No such file or directory', 'err');
        else if (typeof node !== 'object') out('cd: ' + ops[0] + ': Not a directory', 'err');
        else { cwd = path; updatePrompt(); }
        break;
      }
      case 'pwd': out('/' + cwd.join('/')); break;
      case 'cat':
        if (!ops.length) out('cat: which file?', 'err');
        ops.forEach((o) => {
          const node = lookup(resolve(o));
          if (node === undefined) out('cat: ' + o + ': No such file or directory', 'err');
          else if (typeof node === 'object') out('cat: ' + o + ': Is a directory', 'err');
          else if (!node) out('cat: ' + o + ": it's music, not text. Play it in Local Playlist.", 'err');
          else out(node);
        });
        break;
      case 'echo': out(args.slice(1).join(' ')); break;
      case 'whoami': out('visitor'); break;
      case 'date': out(new Date().toString()); break;
      case 'history': hist.forEach((h, i) => out(String(i + 1).padStart(4) + '  ' + h)); break;
      case 'clear': $('d-term-out').textContent = ''; break;
      case 'exit': closeTerm(); break;
      case 'local-playlist': openWindow(desk.querySelector('[data-act="desk-app"]')); break;
      case 'rm': {
        if (!ops.length) { out("rm: missing operand\nTry 'rm --help' for more information.", 'err'); break; }
        const recursive = args.some((a) => a.toLowerCase() === '--recursive' || /^-[a-z]*r/i.test(a));
        ops.forEach((o) => {
          const node = lookup(resolve(o));
          const why = node === undefined ? 'No such file or directory' : typeof node === 'object' && !recursive ? 'Is a directory' : 'Permission denied';
          out("rm: cannot remove '" + o + "': " + why, 'err');
        });
        break;
      }
      default: out(args[0] + ': command not found', 'err');
    }
  }

  // A pretend fastfetch: the app's icon and a few facts about this "computer", mostly the demo's own.
  const LOGO = [
    '╭──────────────────╮',
    '│                  │',
    '│  ───────────  │  │',
    '│               │  │',
    '│  ───────      │  │',
    '│            ╭──┤  │',
    '│            │  │  │',
    '│  ───────   ╰──╯  │',
    '│                  │',
    '╰──────────────────╯',
  ];
  const SWATCHES = ['#F2B8B5', '#FFB59E', '#E0A200', '#81C995', '#8FD3E8', '#AFC6FF', '#D0BCFF', '#EFB8C8'];
  function fastfetch() {
    const ua = navigator.userAgent;
    const os = /Android/.test(ua) ? 'Android' : /iPhone|iPad|iPod/.test(ua) ? 'iOS' : /Windows/.test(ua) ? 'Windows'
      : /CrOS/.test(ua) ? 'ChromeOS' : /Mac OS/.test(ua) ? 'macOS' : /Linux/.test(ua) ? 'Linux' : 'Unknown';
    const secs = Math.floor(performance.now() / 1000);
    const count = (k, unit) => k + ' ' + unit + (k === 1 ? '' : 's');
    const uptime = secs < 60 ? count(secs, 'sec') : secs < 3600 ? count(Math.floor(secs / 60), 'min')
      : count(Math.floor(secs / 3600), 'hour') + ', ' + count(Math.floor((secs % 3600) / 60), 'min');
    const tracks = playlists.reduce((a, p) => a + p.tracks.length, 0);
    const saved = playlists.reduce((a, p) => a + p.tracks.filter((t) => t.saved).length, 0);
    const site = document.documentElement.dataset.theme;
    const dark = S.dtheme === 'system' ? site === 'dark' || (!site && window.matchMedia('(prefers-color-scheme: dark)').matches) : S.dtheme === 'dark';
    const facts = [
      ['OS', os + ' (in a browser tab)'],
      ['Host', location.hostname || 'localhost'],
      ['Uptime', uptime],
      ['Packages', '1 (local-playlist)'],
      ['Shell', 'demo.js'],
      ['Display', screen.width + 'x' + screen.height],
      ['Theme', 'Local Playlist ' + (dark ? 'Dark' : 'Light')],
      ['CPU', navigator.hardwareConcurrency ? navigator.hardwareConcurrency + ' threads' : 'yes'],
      ['Music', tracks + ' tracks in ' + playlists.length + ' playlists, ' + saved + ' saved'],
      ['Playing', S.current && !S.paused ? S.current.title + ' – ' + S.current.artist : 'nothing'],
    ];
    const rows = [[['visitor', 'u'], ['@', ''], ['desktop', 'u']], [['-'.repeat(15), '']]]
      .concat(facts.map(([k, v]) => [[k + ': ', 'p'], [v, '']]), [' ', SWATCHES.map((c) => ['███', '', c])]);
    // The logo has its own column with a tight line height, so its lines join up. On narrow
    // terminals the facts wrap below it.
    const box = document.createElement('div');
    box.className = 'd-term-ff';
    const logo = document.createElement('div');
    logo.className = 'd-term-logo';
    logo.textContent = LOGO.join('\n');
    const list = document.createElement('div');
    list.className = 'd-term-facts';
    rows.forEach((row) => list.append(lineOf(row)));
    box.append(logo, list);
    emit(box);
  }

  // --- rm -rf: what's in view disappears piece by piece, the terminal last, then the rest of the page. --
  function pathOf(el) {
    const name = (n) => n.id || n.tagName.toLowerCase();
    const segs = [];
    for (let p = el.parentElement; p && p !== document.body && segs.length < 2; p = p.parentElement) {
      if (p.id || /^(HEADER|MAIN|FOOTER|SECTION|NAV|ASIDE|UL|OL|FORM)$/.test(p.tagName)) segs.unshift(name(p));
    }
    const cls = typeof el.className === 'string' && el.className.split(/\s+/)[0];
    return '/' + segs.concat(el.id || el.tagName.toLowerCase() + (cls ? '.' + cls : '')).join('/');
  }

  // Like rm, quiet unless asked to be verbose (-v). There's no undo: a reload brings the page back.
  function wipe(verbose) {
    if (wiping) return;
    wiping = true;
    const root = document.documentElement;
    $('d-term-form').hidden = true;
    root.classList.add('lp-wiping');

    // What's in view, in small pieces: leaves, media and controls, or small blocks.
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const units = [];
    (function walk(el) {
      for (const c of el.children) {
        if (c === term || /^(SCRIPT|STYLE|LINK|META|NOSCRIPT|TEMPLATE)$/.test(c.tagName)) continue;
        if (c.contains(term)) { walk(c); continue; }
        if (!c.getClientRects().length) continue;
        const r = c.getBoundingClientRect();
        if (r.bottom <= 0 || r.top >= vh || r.right <= 0 || r.left >= vw) continue;
        const area = r.width * r.height;
        const small = /^(IMG|SVG|PICTURE|VIDEO|CANVAS|INPUT|BUTTON|SELECT|TEXTAREA)$/i.test(c.tagName) ? area < vw * vh * 0.5
          : c.firstElementChild ? r.height <= 120 && r.width <= vw * 0.7 : area < vw * vh * 0.3;
        if (small) units.push(c); else walk(c);
      }
    })(document.body);
    for (let i = units.length - 1; i > 0; i -= 1) {
      const j = Math.floor(Math.random() * (i + 1));
      [units[i], units[j]] = [units[j], units[i]];
    }

    const n = units.length;
    const span = Math.min(2000, Math.max(1000, n * 25));
    units.forEach((u, i) => setTimeout(() => {
      u.style.visibility = 'hidden';
      if (verbose) out("removed '" + pathOf(u) + "'");
    }, (i / Math.max(1, n)) * span));
    if (verbose) setTimeout(() => out("removed directory '/'"), span + 100);
    setTimeout(() => { term.style.visibility = 'hidden'; }, span + 500);
    setTimeout(() => {
      root.classList.add('lp-wiped');
      root.classList.remove('lp-wiping');
    }, span + 900);
  }

  demo.querySelector('.d-chrome .d-dot').addEventListener('click', closeWindow);

  // --- Start ----------------------------------------------------------------------------------------
  renderAll();
  renderVolume();
  renderSettings();
})();

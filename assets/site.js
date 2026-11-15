// Local Playlist website: theme switch, download links for the visitor's system, contributors, copy button.
// Everything here is progressive: without JavaScript (or without GitHub's API) the page still works.
(() => {
  'use strict';

  const REPO = 'jakeayy/local-playlist';
  const API = 'https://api.github.com/repos/' + REPO;
  const root = document.documentElement;

  // --- Theme: system → light → dark. Saved per browser; storage may be unavailable. ---------
  const themeBtn = document.getElementById('theme-toggle');
  const THEMES = ['system', 'light', 'dark'];
  const THEME_ICON = { system: 'i-monitor', light: 'i-sun', dark: 'i-moon' };
  const THEME_NAME = { system: 'system', light: 'light', dark: 'dark' };

  function savedTheme() {
    try { const t = localStorage.getItem('lp-theme'); return t === 'light' || t === 'dark' ? t : 'system'; } catch (e) { return 'system'; }
  }
  function applyTheme(t) {
    if (t === 'system') delete root.dataset.theme; else root.dataset.theme = t;
    try { if (t === 'system') localStorage.removeItem('lp-theme'); else localStorage.setItem('lp-theme', t); } catch (e) { /* not saved */ }
    if (themeBtn) {
      themeBtn.querySelector('use').setAttribute('href', '#' + THEME_ICON[t]);
      const next = THEMES[(THEMES.indexOf(t) + 1) % THEMES.length];
      themeBtn.setAttribute('aria-label', 'Theme: ' + THEME_NAME[t] + '. Switch to ' + THEME_NAME[next]);
      themeBtn.title = 'Theme: ' + THEME_NAME[t];
    }
    document.dispatchEvent(new CustomEvent('lp-theme'));
  }
  if (themeBtn) {
    applyTheme(savedTheme());
    themeBtn.addEventListener('click', () => {
      const cur = savedTheme();
      applyTheme(THEMES[(THEMES.indexOf(cur) + 1) % THEMES.length]);
    });
  }

  // --- Which system is the visitor on? -------------------------------------------------------
  function detectOs() {
    const p = ((navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '').toLowerCase();
    const ua = navigator.userAgent.toLowerCase();
    if (/android|iphone|ipad|ipod/.test(ua)) return 'mobile';
    if (p.includes('win') || ua.includes('windows')) return 'windows';
    if (p.includes('mac') || ua.includes('mac os')) return 'mac';
    if (p.includes('linux') || ua.includes('linux') || ua.includes('x11')) return 'linux';
    return 'other';
  }
  const os = detectOs();
  const cta = document.getElementById('cta-download');
  const ctaLabel = document.getElementById('cta-label');
  const ctaNote = document.getElementById('cta-note');
  const OS_NAME = { windows: 'Windows', linux: 'Linux' };
  if (OS_NAME[os]) {
    ctaLabel.textContent = 'Download for ' + OS_NAME[os];
    const card = document.querySelector('.dl[data-os="' + os + '"]');
    if (card) {
      card.classList.add('is-yours');
      const tag = document.createElement('span');
      tag.className = 'yours';
      tag.textContent = 'Your system';
      card.querySelector('h3').append(tag);
    }
  } else if (os === 'mac') {
    cta.href = '#download';
    ctaLabel.textContent = 'Get Local Playlist';
    ctaNote.innerHTML = 'There\'s no macOS download yet. Windows and Linux are available, or <a href="#self-host">host the web version</a>.';
  } else if (os === 'mobile') {
    cta.href = '#self-host';
    ctaLabel.textContent = 'Use it from your phone';
    ctaNote.innerHTML = 'The desktop app is for Windows and Linux. Phones can use the <a href="#self-host">self-hosted web version</a>.';
  }

  // --- Latest release: direct download links, version and size. Falls back to the Releases page. ---
  function fmtSize(bytes) { return (bytes / 1048576).toFixed(1) + ' MB'; }
  function useRelease(rel) {
    if (!rel || !Array.isArray(rel.assets)) return;
    const version = rel.tag_name || '';
    const line = document.getElementById('release-line');
    if (line && version) {
      line.innerHTML = 'Latest version: <a href="' + rel.html_url + '">' + version.replace(/[<>&"]/g, '') + '</a>. Older and pre-release builds are on the <a href="https://github.com/' + REPO + '/releases">Releases page</a>.';
    }
    document.querySelectorAll('[data-asset]').forEach((a) => {
      const suffix = a.getAttribute('data-asset');
      const asset = rel.assets.find((x) => x.name.endsWith('-' + suffix));
      if (!asset) return;
      a.href = asset.browser_download_url;
      const meta = document.querySelector('[data-meta="' + suffix + '"]');
      if (meta) meta.textContent = asset.name + ' · ' + fmtSize(asset.size);
      if (OS_NAME[os] && suffix === (os === 'windows' ? 'windows-x64.zip' : 'linux-x64.tar.gz')) {
        cta.href = asset.browser_download_url;
        ctaNote.textContent = version + ' · ' + asset.name.split('-').slice(-2).join('-') + ' · ' + fmtSize(asset.size);
        ctaNote.append(document.createTextNode(' · '));
        const other = document.createElement('a');
        other.href = '#download';
        other.textContent = 'Other systems';
        ctaNote.append(other);
      }
    });
  }
  function getJson(url) {
    return fetch(url, { headers: { Accept: 'application/vnd.github+json' } }).then((r) => (r.ok ? r.json() : Promise.reject(r.status)));
  }
  getJson(API + '/releases/latest').then(useRelease).catch(() => { /* keep the Releases page links */ });

  // --- Contributors: everyone GitHub lists, without bots. Loaded when the section comes into view. ---
  const people = document.getElementById('people');
  function renderPeople(list) {
    const humans = list.filter((u) => u && u.type !== 'Bot' && !/\[bot\]$/i.test(u.login));
    if (!humans.length) return;
    const owner = REPO.split('/')[0];
    people.textContent = '';
    humans.forEach((u) => {
      const li = document.createElement('li');
      li.className = 'person';
      const a = document.createElement('a');
      a.href = u.html_url;
      const img = document.createElement('img');
      img.src = u.avatar_url + (u.avatar_url.includes('?') ? '&' : '?') + 's=128';
      img.width = 64; img.height = 64; img.alt = ''; img.loading = 'lazy'; img.decoding = 'async';
      const name = document.createElement('span');
      name.className = 'person-name';
      name.textContent = u.login;
      const role = document.createElement('span');
      role.className = 'person-role';
      role.textContent = u.login === owner ? 'Creator and maintainer' : u.contributions + (u.contributions === 1 ? ' commit' : ' commits');
      a.append(img, name, role);
      li.append(a);
      people.append(li);
    });
  }
  if (people && 'IntersectionObserver' in window) {
    const io = new IntersectionObserver((entries) => {
      if (!entries.some((e) => e.isIntersecting)) return;
      io.disconnect();
      getJson(API + '/contributors?per_page=100').then(renderPeople).catch(() => { /* keep the static list */ });
    }, { rootMargin: '400px' });
    io.observe(people);
  }

  // --- Copy buttons. ----------------------------------------------------------------------------
  document.querySelectorAll('[data-copy]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const src = document.getElementById(btn.getAttribute('data-copy'));
      const text = src.textContent;
      const done = () => {
        btn.querySelector('use').setAttribute('href', '#i-check');
        btn.setAttribute('aria-label', 'Copied');
        setTimeout(() => { btn.querySelector('use').setAttribute('href', '#i-copy'); btn.setAttribute('aria-label', 'Copy the Docker command'); }, 1600);
      };
      if (navigator.clipboard && window.isSecureContext) {
        navigator.clipboard.writeText(text).then(done, () => selectText(src));
      } else {
        selectText(src);
      }
    });
  });
  function selectText(el) {
    const r = document.createRange();
    r.selectNodeContents(el);
    const s = getSelection();
    s.removeAllRanges();
    s.addRange(r);
  }

  // --- Opening a FAQ link (#faq-…) opens its answer. ---------------------------------------------
  function openFromHash() {
    const el = location.hash && document.getElementById(decodeURIComponent(location.hash.slice(1)));
    if (el && el.tagName === 'DETAILS') el.open = true;
  }
  window.addEventListener('hashchange', openFromHash);
  openFromHash();
})();

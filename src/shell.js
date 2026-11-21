(() => {
  'use strict';
  if (window.__seraph) return;
  // Top-level web pages only. The local splash page (tauri:// on macOS, tauri.localhost on
  // Windows) has nothing to do here, and WebView2 injects this script into every iframe,
  // where none of it is useful.
  if (window.top !== window || location.protocol !== 'https:' || /(^|\.)localhost$/.test(location.hostname)) return;

  const S = (window.__seraph = {
    on: true,
    hosts: new Set(),
    needles: [],
    needleIndex: new Map(),
    count: 0,
    sent: 0,
    flushTimer: null,
    performance: 'balanced',   // effective mode (user preference, or forced by the battery saver)
    perfPref: 'balanced',      // what the user chose
    saver: false,
    lim: null,
    badge: null,
    host: null,
  });

  const isMac = /Mac/i.test(navigator.platform || '');

  const invoke = (command, args) => {
    const bridge = window.__TAURI_INTERNALS__?.invoke;
    if (typeof bridge !== 'function') return Promise.reject(new Error('Tauri bridge unavailable'));
    try { return Promise.resolve(bridge(command, args)); }
    catch (error) { return Promise.reject(error); }
  };

  const onRoot = (callback) => {
    if (document.documentElement) { callback(); return; }
    const observer = new MutationObserver((_, current) => {
      if (!document.documentElement) return;
      current.disconnect();
      callback();
    });
    observer.observe(document, { childList: true });
  };

  // Constructable stylesheets: no repeated <style> nodes, and replaceSync is cheap.
  function styleSink(id) {
    try {
      if (typeof CSSStyleSheet === 'function' && 'adoptedStyleSheets' in document) {
        const sheet = new CSSStyleSheet();
        document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
        return (css) => sheet.replaceSync(css);
      }
    } catch (_) { /* Fall through to the style-element path. */ }

    let node;
    return (css) => {
      onRoot(() => {
        if (!node) {
          node = document.createElement('style');
          node.dataset.seraphStyle = id;
          (document.head || document.documentElement).appendChild(node);
        }
        node.textContent = css;
      });
    };
  }

  const setCosmetic = styleSink('cosmetic');
  const setTheme = styleSink('theme');

  const COSMETIC = [
    'ytd-ad-slot-renderer',
    'ytd-in-feed-ad-layout-renderer',
    'ytd-display-ad-renderer',
    'ytd-promoted-sparkles-web-renderer',
    'ytd-promoted-sparkles-text-search-renderer',
    'ytd-promoted-video-renderer',
    'ytd-banner-promo-renderer',
    'ytd-statement-banner-renderer',
    'ytd-companion-slot-renderer',
    'ytd-action-companion-ad-renderer',
    'ytd-mealbar-promo-renderer',
    'ytd-shopping-panel-renderer',
    'ytd-merchandise-shelf-renderer',
    'ytd-paid-content-overlay-renderer',
    'ytm-promoted-sparkles-web-renderer',
    'ytm-ad-slot-renderer',
    '#masthead-ad',
    '#player-ads',
    '.video-ads',
    '.ytp-ad-module',
    '.ytp-ad-player-overlay',
    '.ytp-ad-overlay-container',
    '.ytp-ad-image-overlay',
    '.ytp-ad-preview-container',
    '.ytp-ad-progress-list',
    '.ytp-ad-text',
    '.ytp-ad-duration-remaining',
    'ytd-engagement-panel-section-list-renderer[target-id="engagement-panel-ads"]',
    'ytd-popup-container:has(tp-yt-paper-dialog[style-target="host"])',
    'tp-yt-paper-dialog[aria-labelledby*="adblock" i]',
    'tp-yt-paper-dialog:has(yt-formatted-string:has-text(/ad blocker|adblock|ads are blocked/i))',
  ].join(',') + '{display:none!important}';
  // :has() re-evaluates on DOM changes, so Performance mode leaves it out. The data layer
  // already removes most ad slots before they render.
  const COSMETIC_HAS = '@supports selector(a:has(b)){ytd-rich-item-renderer:has(ytd-ad-slot-renderer),ytd-rich-item-renderer:has(ytd-in-feed-ad-layout-renderer),ytd-rich-item-renderer:has(ytd-display-ad-renderer){display:none!important}}';

  const applyCosmetic = () =>
    setCosmetic(S.on ? COSMETIC + (S.performance === 'performance' ? '' : COSMETIC_HAS) : '');

  // ---- counters -------------------------------------------------------------------------
  function updateBadge() {
    if (S.badge) S.badge.textContent = S.count ? String(S.count) : '';
  }

  function flushBlocked() {
    S.flushTimer = null;
    const amount = S.count - S.sent;
    if (amount <= 0) return;
    S.sent = S.count;
    invoke('record_block', { n: amount }).catch(() => {});
  }

  function countBlocked(amount = 1) {
    if (!Number.isFinite(amount) || amount < 1) return;
    S.count += amount;
    updateBadge();
    if (S.flushTimer === null) S.flushTimer = setTimeout(flushBlocked, 650);
  }

  // ---- page-level request filter (bundled hosts only; native rules do the heavy lifting) ----
  function isBlocked(input) {
    if (!S.on || !input || (S.hosts.size === 0 && S.needles.length === 0)) return false;
    try {
      const url = new URL(input, location.href);
      for (let host = url.hostname.toLowerCase(); host; ) {
        if (S.hosts.has(host)) return true;
        const dot = host.indexOf('.');
        host = dot >= 0 ? host.slice(dot + 1) : '';
      }
      if (S.needles.length === 0) return false;
      const href = url.href.toLowerCase();
      for (let index = 0; index < href.length - 1; index += 1) {
        const candidates = S.needleIndex.get(href.slice(index, index + 2));
        if (!candidates) continue;
        for (const needle of candidates) if (href.startsWith(needle, index)) return true;
      }
    } catch (_) { /* Unparseable URL: let the browser decide. */ }
    return false;
  }

  // ---- ad removal from data ------------------------------------------------------------
  // Player/API payloads carry ad fields; feed payloads carry ad items inside arrays. Removing
  // them before YouTube's own code sees them is cheaper and cleaner than hiding them later.
  const AD_KEYS = [
    'adPlacements', 'playerAds', 'adSlots', 'adBreakHeartbeatParams',
  ];
  // Nested keys to remove from objects (not just top-level arrays).
  const AD_NESTED_KEYS = ['enforcementMessageViewModel', 'adBlockMessageViewModel'];
  const AD_ITEM_KEYS = [
    'adSlotRenderer',
    'promotedSparklesWebRenderer',
    'promotedSparklesTextSearchRenderer',
    'promotedVideoRenderer',
    'compactPromotedVideoRenderer',
    'displayAdRenderer',
    'inFeedAdLayoutRenderer',
    'searchPyvRenderer',
    'shoppingDisplayAdRenderer',
    'brandVideoRenderer',
    'brandVideoSingletonRenderer',
    'mastheadAdRenderer',
    'videoMastheadAdRenderer',
    'engagementPanelAds',
  ];
  const AD_MARKERS = AD_KEYS.concat(AD_ITEM_KEYS).concat(AD_NESTED_KEYS).map((key) => '"' + key + '"');

  function isAdItem(item) {
    if (!item || typeof item !== 'object') return false;
    for (const key of AD_ITEM_KEYS) if (key in item) return true;
    const inner = item.richItemRenderer && item.richItemRenderer.content;
    if (inner && typeof inner === 'object') {
      for (const key of AD_ITEM_KEYS) if (key in inner) return true;
    }
    return false;
  }

  // Iterative and bounded, so a huge browse response can never turn into a CPU spike.
  function pruneAds(root) {
    if (!S.on || !root || typeof root !== 'object') return root;

    const budget = S.performance === 'performance' ? 4000 : 12000;
    const stack = [root];
    const visited = new WeakSet();
    let seen = 0;
    let removed = 0;

    while (stack.length && seen < budget) {
      const current = stack.pop();
      if (visited.has(current)) continue;
      visited.add(current);
      seen += 1;

      if (Array.isArray(current)) {
        for (let index = current.length - 1; index >= 0; index -= 1) {
          const value = current[index];
          if (!value || typeof value !== 'object') continue;
          if (isAdItem(value)) {
            try { current.splice(index, 1); removed += 1; } catch (_) { /* Frozen array. */ }
          } else {
            stack.push(value);
          }
        }
        continue;
      }

      for (const key of AD_KEYS) {
        if (key in current) {
          try { delete current[key]; removed += 1; } catch (_) { /* Immutable object. */ }
        }
      }
      for (const key in current) {
        const value = current[key];
        if (!value || typeof value !== 'object') continue;
        // Remove known nested ad keys (e.g. auxiliaryUi.messageRenderers.enforcementMessageViewModel)
        // without deleting the whole parent object.
        if (AD_NESTED_KEYS.includes(key)) {
          try { current[key] = {}; removed += 1; continue; } catch (_) {}
        }
        stack.push(value);
      }
    }

    if (removed) countBlocked(removed);
    return root;
  }
  S.prune = pruneAds;

  const originalParse = JSON.parse;
  JSON.parse = function (text, reviver) {
    const parsed = originalParse.call(this, text, reviver);
    if (S.on && typeof text === 'string' && text.length > 256) {
      for (const marker of AD_MARKERS) {
        if (text.includes(marker)) {
          try { return pruneAds(parsed); } catch (_) { return parsed; }
        }
      }
    }
    return parsed;
  };

  if (window.Response && Response.prototype.json) {
    const originalResponseJson = Response.prototype.json;
    Response.prototype.json = function () {
      const pending = originalResponseJson.call(this);
      const url = this.url;
      if (S.on && typeof url === 'string' && url.includes('/youtubei/')) {
        return pending.then((data) => { try { return pruneAds(data); } catch (_) { return data; } });
      }
      return pending;
    };
  }

  for (const name of ['ytInitialPlayerResponse', 'ytInitialData', 'playerResponse']) {
    let value;
    try {
      Object.defineProperty(window, name, {
        configurable: true,
        enumerable: true,
        get: () => value,
        set: (next) => { value = pruneAds(next); },
      });
    } catch (_) { /* YouTube may have defined a non-configurable property. */ }
  }

  // ---- anti-adblock countermeasures ----------------------------------------------------
  // YouTube checks a few global flags and prototype values to decide whether to show the
  // "Ad blockers violate YouTube's Terms of Service" dialog. Neutralise them before the
  // first script runs, and hide any dialog that still appears.
  function antiAdblockPatch() {
    try {
      if (typeof Object.defineProperty === 'function') {
        const zeroProps = ['adBlocksFound', 'adBlockCount'];
        for (const key of zeroProps) {
          try {
            Object.defineProperty(Object.prototype, key, {
              configurable: true,
              enumerable: false,
              get: () => 0,
              set: () => {},
            });
          } catch (_) {}
        }
      }
      // Neutralise the popup model config if it is already present.
      const cfg = window.yt && window.yt.config_ && window.yt.config_.openPopupConfig;
      if (cfg && cfg.supportedPopups && cfg.supportedPopups.adBlockMessageViewModel) {
        cfg.supportedPopups.adBlockMessageViewModel = false;
      }
    } catch (_) { /* best effort */ }
  }
  antiAdblockPatch();

  // Hide any anti-adblock dialog that the page still tries to render.
  // We hide rather than remove so YouTube's dialog lifecycle stays intact.
  function hideAdblockDialogs() {
    if (!S.on) return;
    const dialogs = document.querySelectorAll('tp-yt-paper-dialog, ytd-modal-with-title-and-button-renderer, ytd-enforcement-message-view-model');
    for (const dialog of dialogs) {
      const text = dialog.textContent || '';
      if (/ad blocker|adblock|ads are blocked|turn off.*ad|disable.*ad/i.test(text)) {
        if (dialog.getAttribute('hidden') && dialog.style.display === 'none') continue;
        dialog.setAttribute('hidden', '');
        dialog.style.display = 'none';
        dialog.style.visibility = 'hidden';
        dialog.style.pointerEvents = 'none';
        countBlocked();
      }
    }
  }
  const dialogObserver = new MutationObserver(hideAdblockDialogs);
  onRoot(() => dialogObserver.observe(document.documentElement, { childList: true, subtree: true }));

  // ---- fetch / XHR / beacon -----------------------------------------------------------------
  const originalFetch = window.fetch;
  if (typeof originalFetch === 'function') {
    window.fetch = function (input, ...rest) {
      const url = input && typeof input === 'object' && 'url' in input ? input.url : String(input);
      if (isBlocked(url)) {
        countBlocked();
        return Promise.reject(new TypeError('Failed to fetch'));
      }
      return originalFetch.call(this, input, ...rest);
    };
  }

  const originalXhrOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url, ...rest) {
    if (isBlocked(String(url))) {
      countBlocked();
      return originalXhrOpen.call(this, method, 'data:,', ...rest);
    }
    return originalXhrOpen.call(this, method, url, ...rest);
  };

  if (typeof navigator.sendBeacon === 'function') {
    const originalBeacon = navigator.sendBeacon.bind(navigator);
    try {
      navigator.sendBeacon = (url, data) => {
        if (isBlocked(String(url))) { countBlocked(); return true; }
        return originalBeacon(url, data);
      };
    } catch (_) { /* Browser may expose sendBeacon as non-writable. */ }
  }

  // ---- in-stream ads: skip, or race through them --------------------------------------------
  // Server-inserted ads can't be removed from the data. The player is observed only on watch
  // pages (no document-wide observer), and video state is restored the moment the ad ends.
  // We keep manipulation conservative: high playback rates and seeks are easy for YouTube to
  // detect, so we prefer clicking the skip button and only speed up unskippable short ads.
  const WATCH_PAGE = /^\/(watch|shorts|live|embed)(\/|$)/;
  const RETRY_MS = [120, 300, 700, 1500, 3000];

  let player = null;
  let playerObserver = null;
  let video = null;
  let saved = null;
  let lastSkip = null;
  let stateTimer = null;
  let retryTimer = null;

  function restoreVideo() {
    if (video && saved) {
      try {
        video.muted = saved.muted;
        video.playbackRate = saved.playbackRate;
      } catch (_) { /* The media element may already be detached. */ }
    }
    saved = null;
  }

  function findSkipButton() {
    const selectors = [
      '.ytp-skip-ad-button',
      '.ytp-ad-skip-button',
      '.ytp-ad-skip-button-modern',
      '.ytp-skip-ad-button__text',
      'button[class*="skip-ad"]',
      'button[class*="ad-skip"]',
      '.ytp-ad-skip-button-slot',
      '.ytp-ad-skip-button-container',
    ];
    for (const selector of selectors) {
      const btn = player.querySelector(selector);
      if (btn && !btn.disabled && btn.offsetParent !== null) return btn;
    }
    return null;
  }

  function onPlayerState() {
    if (!player) return;

    const nextVideo = player.querySelector('video.html5-main-video');
    if (nextVideo !== video) { restoreVideo(); video = nextVideo; }

    if (!player.classList.contains('ad-showing')) {
      restoreVideo();
      lastSkip = null;
      return;
    }

    const skip = findSkipButton();
    if (skip && skip !== lastSkip) {
      lastSkip = skip;
      skip.click();
      countBlocked();
      return;
    }

    if (!video) return;
    if (!saved) saved = { muted: video.muted, playbackRate: video.playbackRate };
    try {
      // Mute so the ad is not heard; use a moderate speed to avoid detection.
      if (!video.muted) video.muted = true;
      if (video.playbackRate < 2) video.playbackRate = 4;
      // Only seek on very short ads where jumping is unlikely to be measured.
      const length = video.duration;
      if (Number.isFinite(length) && length > 0 && length <= 6 && video.currentTime < length - 0.3) {
        video.currentTime = length - 0.05;
      }
    } catch (_) { /* The player can reject a rate change or seek. */ }
  }

  function queuePlayerState() {
    if (stateTimer !== null) return;
    stateTimer = setTimeout(() => { stateTimer = null; onPlayerState(); }, 30);
  }

  function detachPlayer() {
    if (playerObserver) { playerObserver.disconnect(); playerObserver = null; }
    restoreVideo();
    player = null;
    video = null;
  }

  function attachPlayer(attempt = 0) {
    clearTimeout(retryTimer);
    if (!WATCH_PAGE.test(location.pathname)) { detachPlayer(); return; }

    const next = document.querySelector('.html5-video-player');
    if (!next) {
      if (attempt < RETRY_MS.length) retryTimer = setTimeout(() => attachPlayer(attempt + 1), RETRY_MS[attempt]);
      return;
    }
    if (next === player) { onPlayerState(); return; }

    detachPlayer();
    player = next;
    playerObserver = new MutationObserver(queuePlayerState);
    playerObserver.observe(player, { attributes: true, attributeFilter: ['class'], childList: true, subtree: true });
    onPlayerState();
  }

  document.addEventListener('yt-navigate-finish', () => { attachPlayer(0); if (S.saver) setTimeout(capQuality, 1500); });
  document.addEventListener('yt-page-data-updated', () => attachPlayer(0));
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', () => attachPlayer(0), { once: true });
  else attachPlayer(0);

  // ---- picture in picture + shortcuts (Windows has no menu bar) ------------------------------
  function togglePip() {
    const v = document.querySelector('video.html5-main-video') || document.querySelector('video');
    if (!v) return;
    if (typeof v.webkitSetPresentationMode === 'function') {
      v.webkitSetPresentationMode(v.webkitPresentationMode === 'picture-in-picture' ? 'inline' : 'picture-in-picture');
    } else if (document.pictureInPictureElement) {
      document.exitPictureInPicture().catch(() => {});
    } else if (v.requestPictureInPicture) {
      v.requestPictureInPicture().catch(() => {});
    }
  }

  if (!isMac) {
    window.addEventListener('keydown', (event) => {
      if (!event.ctrlKey || event.altKey || event.metaKey) return;
      if (event.key === ',') {
        event.preventDefault();
        invoke('open_settings').catch(() => {});
      } else if (event.shiftKey && (event.key === 'P' || event.key === 'p')) {
        event.preventDefault();
        togglePip(); // a keydown is a user gesture, so Chromium allows this
      }
    }, true);
  }

  // ---- performance + theme ---------------------------------------------------------------------
  // Performance mode trims our own work (smaller prune budget, no :has() rule, no effects on
  // the settings button). It never touches YouTube's animations: forcing every animation to
  // ~0ms makes the looping loading spinner flicker and overlap.
  function effectivePerformance() {
    S.performance = S.perfPref === 'performance' || S.saver ? 'performance' : 'balanced';
    if (S.host) S.host.dataset.performance = S.performance;
    document.documentElement?.setAttribute('data-seraph-performance', S.performance);
    applyCosmetic();
  }

  function setPerformance(mode) {
    S.perfPref = mode === 'performance' ? 'performance' : 'balanced';
    effectivePerformance();
  }

  S.setPerformance = setPerformance;
  S.apply = (payload) => {
    if (!payload || typeof payload !== 'object') return;
    S.on = Boolean(payload.on);
    S.hosts = new Set(Array.isArray(payload.hosts) ? payload.hosts : []);
    S.needles = Array.isArray(payload.needles) ? payload.needles : [];
    const needleIndex = new Map();
    for (const needle of S.needles) {
      if (typeof needle !== 'string' || needle.length < 2) continue;
      const prefix = needle.slice(0, 2);
      let bucket = needleIndex.get(prefix);
      if (!bucket) needleIndex.set(prefix, bucket = []);
      bucket.push(needle);
    }
    S.needleIndex = needleIndex;
    applyCosmetic();
    if (payload.theme) {
      setTheme(payload.theme.css || '');
      onRoot(() => document.documentElement.setAttribute('data-seraph-theme', payload.theme.id || 'default'));
    }
  };

  if (window.__SERAPH_BOOT) S.apply(window.__SERAPH_BOOT);

  // The remote-page capability grants read-only access to this small enum.
  invoke('get_performance').then(setPerformance).catch(() => {});

  // ---- resource limits ----------------------------------------------------------------------------
  // The app measures memory and battery and pushes the result here every few seconds
  // (limits.rs). This side does the work: battery saver, trimming, and recycling the page.
  const SAVER_CSS = 'ytd-video-preview,ytd-moving-thumbnail-renderer{display:none!important}';
  const setSaverStyle = styleSink('saver');
  const RECYCLE_KEY = 'seraph:recycled';
  const OVER_FOR_MS = 25000;       // how long we stay over the limit before recycling
  const IDLE_FOR_MS = 20000;       // and how long nothing must be playing, so a pause never reloads under you
  const TRIM_EVERY_MS = 30000;
  const RECYCLE_COOLDOWN_MS = 180000;
  let overSince = 0;
  let idleSince = 0;
  let lastTrim = 0;

  // Best-effort: YouTube's player exposes this call, and it may change without notice.
  function capQuality() {
    try {
      const p = document.getElementById('movie_player');
      if (p && typeof p.setPlaybackQualityRange === 'function') p.setPlaybackQualityRange('tiny', 'hd720');
    } catch (_) { /* player not ready */ }
  }

  function setSaver(on) {
    on = Boolean(on);
    if (S.saver === on) return;
    S.saver = on;
    setSaverStyle(on ? SAVER_CSS : '');
    document.documentElement?.toggleAttribute('data-seraph-saver', on);
    effectivePerformance();
    if (on) capQuality();
  }

  function trimSoft() {
    try {
      document.querySelectorAll('ytd-video-preview').forEach((element) => element.remove());
      const main = document.querySelector('video.html5-main-video');
      document.querySelectorAll('video').forEach((v) => { if (v !== main && !v.paused) v.pause(); });
      if (performance.clearResourceTimings) performance.clearResourceTimings();
      if (performance.clearMeasures) performance.clearMeasures();
      if (performance.clearMarks) performance.clearMarks();
    } catch (_) { /* best effort */ }
  }

  // Never interrupt playback: only recycle when nothing is playing.
  function canRecycle() {
    const v = document.querySelector('video.html5-main-video');
    return !v || v.paused || v.ended;
  }

  function recycle() {
    try {
      const last = Number(sessionStorage.getItem(RECYCLE_KEY) || 0);
      if (Date.now() - last < RECYCLE_COOLDOWN_MS) return;   // a limit below what YouTube needs must not loop
      sessionStorage.setItem(RECYCLE_KEY, String(Date.now()));
    } catch (_) { /* storage unavailable: still recycle once per call */ }

    const url = new URL(location.href);
    const v = document.querySelector('video.html5-main-video');
    if (v && /^\/watch/.test(url.pathname) && Number.isFinite(v.currentTime) && v.currentTime > 5) {
      url.searchParams.set('t', Math.floor(v.currentTime) + 's');   // resume where you were
    }
    location.replace(url.href);
  }

  S.limits = (info) => {
    if (!info || typeof info !== 'object') return;
    S.lim = info;
    setSaver(info.saver);

    if (!info.over) { overSince = 0; idleSince = 0; return; }
    const now = Date.now();
    if (!overSince) overSince = now;
    if (now - lastTrim > TRIM_EVERY_MS) { lastTrim = now; trimSoft(); }

    if (now - overSince > OVER_FOR_MS) {
      if (!canRecycle()) { idleSince = 0; return; }          // playing: leave it alone
      if (!idleSince) idleSince = now;
      if (now - idleSince > IDLE_FOR_MS) { overSince = 0; idleSince = 0; recycle(); }
    }
  };

  // ---- floating settings button ------------------------------------------------------------------
  function mount() {
    if (!document.documentElement || document.getElementById('seraph-root')) return;

    const host = document.createElement('div');
    host.id = 'seraph-root';
    host.dataset.performance = S.performance;
    S.host = host;

    const root = host.attachShadow({ mode: 'open' });
    const styles = document.createElement('style');
    styles.textContent = `
      :host { all: initial; }
      button {
        position: fixed; right: 14px; bottom: 14px; z-index: 2147483647;
        display: grid; place-items: center; width: 32px; height: 32px; padding: 0;
        border: 1px solid var(--st-accent, #c8a45a); border-radius: 50%;
        color: var(--st-accent, #c8a45a); background: var(--st-bg-elev, #1d1a1f);
        opacity: .3; cursor: pointer; transition: opacity .15s ease;
      }
      button:hover, button:focus-visible { opacity: 1; outline: none; }
      button:focus-visible { box-shadow: 0 0 0 2px var(--st-accent, #c8a45a); }
      b {
        position: absolute; top: -6px; right: -6px; min-width: 14px; padding: 1px 4px;
        border-radius: 8px; color: #fff; background: var(--st-accent-2, #8e2f45);
        font: 600 10px/1.4 system-ui, sans-serif; text-align: center;
      }
      b:empty { display: none; }
      :host([data-performance="performance"]) button { transition: none; }
      @media (prefers-reduced-motion: reduce) { button { transition: none; } }
    `;

    const ns = 'http://www.w3.org/2000/svg';
    const svgNode = (tag, attributes) => {
      const element = document.createElementNS(ns, tag);
      for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, value);
      return element;
    };
    const icon = svgNode('svg', {
      viewBox: '0 0 24 24', width: 17, height: 17, fill: 'none',
      stroke: 'currentColor', 'stroke-width': 1.5, 'stroke-linecap': 'round', 'aria-hidden': 'true',
    });
    icon.append(
      svgNode('circle', { cx: 12, cy: 12, r: 8.5 }),
      svgNode('circle', { cx: 12, cy: 12, r: 2.6 }),
      svgNode('path', { d: 'M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6L5.6 18.4' })
    );

    const badge = document.createElement('b');
    const button = document.createElement('button');
    button.type = 'button';
    button.title = 'Seraphtube settings (' + (isMac ? '\u2318,' : 'Ctrl+,') + ')';
    button.setAttribute('aria-label', 'Open Seraphtube settings');
    button.append(icon, badge);
    button.addEventListener('click', () => invoke('open_settings').catch(() => {}));
    S.badge = badge;
    root.append(styles, button);
    document.documentElement.appendChild(host);
    setPerformance(S.performance);
    updateBadge();
  }

  if (document.documentElement) mount();
  else onRoot(mount);
})();

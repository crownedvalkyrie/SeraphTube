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
    performance: 'balanced',
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
    'ytm-promoted-sparkles-web-renderer',
    '#masthead-ad',
    '#player-ads',
    '.video-ads',
    '.ytp-ad-module',
    '.ytp-ad-player-overlay',
    '.ytp-ad-overlay-container',
    '.ytp-ad-image-overlay',
    '.ytp-ad-preview-container',
    '.ytp-ad-progress-list',
    'ytd-engagement-panel-section-list-renderer[target-id="engagement-panel-ads"]',
  ].join(',') + '{display:none!important}';
  // :has() re-evaluates on DOM changes, so Performance mode leaves it out. The data layer
  // already removes most ad slots before they render.
  const COSMETIC_HAS = '@supports selector(a:has(b)){ytd-rich-item-renderer:has(ytd-ad-slot-renderer){display:none!important}}';

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

  // ---- ad removal from data ---------------------------------------------------------------
  // Player/API payloads carry ad fields; feed payloads carry ad items inside arrays. Removing
  // them before YouTube's own code sees them is cheaper and cleaner than hiding them later.
  const AD_KEYS = ['adPlacements', 'playerAds', 'adSlots', 'adBreakHeartbeatParams'];
  const AD_ITEM_KEYS = [
    'adSlotRenderer',
    'promotedSparklesWebRenderer',
    'promotedSparklesTextSearchRenderer',
    'promotedVideoRenderer',
    'compactPromotedVideoRenderer',
    'displayAdRenderer',
    'inFeedAdLayoutRenderer',
    'searchPyvRenderer',
  ];
  const AD_MARKERS = AD_KEYS.concat(AD_ITEM_KEYS).map((key) => '"' + key + '"');

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
        if (value && typeof value === 'object') stack.push(value);
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
        if (text.includes(marker)) return pruneAds(parsed);
      }
    }
    return parsed;
  };

  if (window.Response && Response.prototype.json) {
    const originalResponseJson = Response.prototype.json;
    Response.prototype.json = function () {
      const pending = originalResponseJson.call(this);
      const url = this.url;
      return S.on && typeof url === 'string' && url.includes('/youtubei/') ? pending.then(pruneAds) : pending;
    };
  }

  for (const name of ['ytInitialPlayerResponse', 'ytInitialData']) {
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

  function onPlayerState() {
    if (!player) return;

    const nextVideo = player.querySelector('video.html5-main-video');
    if (nextVideo !== video) { restoreVideo(); video = nextVideo; }

    if (!player.classList.contains('ad-showing')) {
      restoreVideo();
      lastSkip = null;
      return;
    }

    const skip = player.querySelector('.ytp-skip-ad-button, .ytp-ad-skip-button, .ytp-ad-skip-button-modern');
    if (skip && !skip.disabled && skip !== lastSkip) {
      lastSkip = skip;
      skip.click();
      countBlocked();
      return;
    }

    if (!video) return;
    if (!saved) saved = { muted: video.muted, playbackRate: video.playbackRate };
    try {
      video.muted = true;
      video.playbackRate = 16;
      // Ads are short; if the length is known, jump to the end instead of waiting at 16x.
      const length = video.duration;
      if (Number.isFinite(length) && length > 0 && length < 180 && video.currentTime < length - 0.5) {
        video.currentTime = length - 0.1;
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

  document.addEventListener('yt-navigate-finish', () => attachPlayer(0));
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
  function setPerformance(mode) {
    S.performance = mode === 'performance' ? 'performance' : 'balanced';
    if (S.host) S.host.dataset.performance = S.performance;
    document.documentElement?.setAttribute('data-seraph-performance', S.performance);
    applyCosmetic();
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

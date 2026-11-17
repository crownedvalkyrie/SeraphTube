(() => {
  'use strict';
  if (window.__seraph) return;

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

  const invoke = (command, args) => {
    const bridge = window.__TAURI_INTERNALS__?.invoke;
    if (typeof bridge !== 'function') return Promise.reject(new Error('Tauri bridge unavailable'));
    try { return Promise.resolve(bridge(command, args)); }
    catch (error) { return Promise.reject(error); }
  };

  const onRoot = (callback) => {
    if (document.documentElement) {
      callback();
      return;
    }
    const observer = new MutationObserver((_, current) => {
      if (!document.documentElement) return;
      current.disconnect();
      callback();
    });
    observer.observe(document, { childList: true });
  };

  // Constructable stylesheets are fast and avoid adding repeated <style> nodes.
  // Older WebKitGTK builds may not support them, so keep a simple fallback.
  function styleSink(id) {
    try {
      if (typeof CSSStyleSheet === 'function' && 'adoptedStyleSheets' in document) {
        const sheet = new CSSStyleSheet();
        document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
        return (css) => sheet.replaceSync(css);
      }
    } catch (_) { /* Fall through to the style-element path. */ }

    let node;
    const update = (css) => {
      onRoot(() => {
        if (!node) {
          node = document.createElement('style');
          node.dataset.seraphStyle = id;
          (document.head || document.documentElement).appendChild(node);
        }
        node.textContent = css;
      });
    };
    return update;
  }

  const setCosmetic = styleSink('cosmetic');
  const setTheme = styleSink('theme');
  const setPerformanceStyle = styleSink('performance');
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
  ].join(',') + '{display:none!important}' +
    '@supports selector(ytd-rich-item-renderer:has(ytd-ad-slot-renderer)){' +
    'ytd-rich-item-renderer:has(ytd-ad-slot-renderer){display:none!important}}';

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
    if (S.flushTimer === null) {
      S.flushTimer = setTimeout(flushBlocked, 650);
    }
  }

  function isBlocked(input) {
    if (!S.on || !input) return false;
    try {
      const url = new URL(input, location.href);
      const hostname = url.hostname.toLowerCase();
      for (let host = hostname; host; ) {
        if (S.hosts.has(host)) return true;
        const dot = host.indexOf('.');
        host = dot >= 0 ? host.slice(dot + 1) : '';
      }
      const href = url.href.toLowerCase();
      // Instead of testing every filter substring for every request, use a
      // two-character index and only test candidates at matching positions.
      for (let index = 0; index < href.length - 1; index += 1) {
        const candidates = S.needleIndex.get(href.slice(index, index + 2));
        if (!candidates) continue;
        for (const needle of candidates) {
          if (href.startsWith(needle, index)) return true;
        }
      }
      return false;
    } catch (_) {
      return false;
    }
  }

  // Remove known ad-only fields from parsed player/API payloads. A bounded
  // traversal avoids turning parsing of large browse responses into a memory or
  // CPU spike. The budget is deliberately lower in Performance mode.
  const AD_KEYS = new Set(['adPlacements', 'playerAds', 'adSlots', 'adBreakHeartbeatParams']);
  function pruneAds(root) {
    if (!S.on || !root || typeof root !== 'object') return root;

    const maxNodes = S.performance === 'performance' ? 900 : 2400;
    const stack = [root];
    const visited = new WeakSet();
    let visitedCount = 0;
    let changed = false;

    while (stack.length && visitedCount < maxNodes) {
      const current = stack.pop();
      if (!current || typeof current !== 'object' || visited.has(current)) continue;
      visited.add(current);
      visitedCount += 1;

      if (Array.isArray(current)) {
        for (let index = 0; index < current.length && index < 200; index += 1) {
          const value = current[index];
          if (value && typeof value === 'object') stack.push(value);
        }
        continue;
      }

      for (const key of AD_KEYS) {
        if (Object.prototype.hasOwnProperty.call(current, key)) {
          try { delete current[key]; changed = true; } catch (_) { /* Immutable object. */ }
        }
      }

      for (const key of ['playerResponse', 'response', 'contents', 'items', 'data']) {
        const value = current[key];
        if (value && typeof value === 'object') stack.push(value);
      }
    }

    if (changed) countBlocked();
    return root;
  }

  const JSON_AD_MARKER = /"(?:adPlacements|playerAds|adSlots|adBreakHeartbeatParams)"/;
  const originalParse = JSON.parse;
  JSON.parse = function (text, reviver) {
    const parsed = originalParse.call(this, text, reviver);
    if (!S.on || typeof text !== 'string' || !JSON_AD_MARKER.test(text)) return parsed;
    return pruneAds(parsed);
  };

  if (window.Response?.prototype?.json) {
    const originalResponseJson = Response.prototype.json;
    Response.prototype.json = function () {
      let isYouTubeApi = false;
      try {
        const hostname = new URL(this?.url || '').hostname.toLowerCase();
        isYouTubeApi = hostname === 'youtubei.googleapis.com'
          || hostname === 'youtube.com'
          || hostname.endsWith('.youtube.com');
      } catch (_) { /* Non-network or opaque response URL. */ }
      return originalResponseJson.call(this).then((value) => isYouTubeApi ? pruneAds(value) : value);
    };
  }

  let initialPlayerResponse;
  try {
    Object.defineProperty(window, 'ytInitialPlayerResponse', {
      configurable: true,
      get: () => initialPlayerResponse,
      set: (value) => { initialPlayerResponse = pruneAds(value); },
    });
  } catch (_) { /* YouTube may have defined a non-configurable property. */ }

  // Network hooks cover page-initiated fetch/XHR/beacon calls. macOS and Linux
  // also attach native WebKit rules for browser-managed subresources; Windows
  // still relies on this page-level fallback until its WebView2 hook is installed.
  const originalFetch = window.fetch;
  if (typeof originalFetch === 'function') {
    window.fetch = function (input, ...rest) {
      const url = input && typeof input === 'object' && 'url' in input
        ? input.url
        : String(input);
      if (isBlocked(url)) {
        countBlocked();
        return Promise.reject(new TypeError('Failed to fetch'));
      }
      return originalFetch.call(this, input, ...rest);
    };
  }

  const originalXhrOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url, ...rest) {
    const requestedUrl = String(url);
    if (isBlocked(requestedUrl)) {
      countBlocked();
      return originalXhrOpen.call(this, method, 'data:,', ...rest);
    }
    return originalXhrOpen.call(this, method, url, ...rest);
  };

  if (typeof navigator.sendBeacon === 'function') {
    const originalBeacon = navigator.sendBeacon.bind(navigator);
    try {
      navigator.sendBeacon = (url, data) => {
        if (isBlocked(String(url))) {
          countBlocked();
          return true;
        }
        return originalBeacon(url, data);
      };
    } catch (_) { /* Browser may expose sendBeacon as non-writable. */ }
  }

  // Event-driven YouTube ad helper: no permanent 400 ms timer. The player is
  // observed only while it exists, and video state is restored as soon as the
  // ad-showing class is removed.
  let currentPlayer = null;
  let playerObserver = null;
  let playerDiscoveryObserver = null;
  let playerStateQueued = false;
  let currentVideo = null;
  let savedVideoState = null;
  let lastSkipButton = null;

  function restoreVideo() {
    if (currentVideo && savedVideoState) {
      try {
        currentVideo.muted = savedVideoState.muted;
        currentVideo.playbackRate = savedVideoState.playbackRate;
      } catch (_) { /* The media element may already have been detached. */ }
    }
    savedVideoState = null;
  }

  function findPlayerVideo() {
    const nextVideo = currentPlayer?.querySelector('video.html5-main-video') || null;
    if (nextVideo === currentVideo) return;
    restoreVideo();
    currentVideo = nextVideo;
  }

  function handlePlayerState() {
    if (!currentPlayer) return;
    findPlayerVideo();

    if (!currentPlayer.classList.contains('ad-showing')) {
      restoreVideo();
      lastSkipButton = null;
      return;
    }

    const skip = currentPlayer.querySelector(
      '.ytp-skip-ad-button, .ytp-ad-skip-button, .ytp-ad-skip-button-modern'
    );
    if (skip && !skip.disabled && skip !== lastSkipButton) {
      lastSkipButton = skip;
      skip.click();
      countBlocked();
      return;
    }

    if (currentVideo && !savedVideoState) {
      savedVideoState = {
        muted: currentVideo.muted,
        playbackRate: currentVideo.playbackRate,
      };
    }
    if (currentVideo) {
      try {
        currentVideo.muted = true;
        currentVideo.playbackRate = 16;
      } catch (_) { /* The player can reject a rate change. */ }
    }
  }

  function attachPlayer() {
    const nextPlayer = document.querySelector('.html5-video-player');
    if (!nextPlayer || nextPlayer === currentPlayer) {
      handlePlayerState();
      return;
    }

    if (playerObserver) playerObserver.disconnect();
    restoreVideo();
    currentPlayer = nextPlayer;
    currentVideo = null;
    playerObserver = new MutationObserver(queuePlayerState);
    playerObserver.observe(currentPlayer, {
      attributes: true,
      attributeFilter: ['class'],
      childList: true,
      subtree: true,
    });
    if (playerDiscoveryObserver) playerDiscoveryObserver.disconnect();
    handlePlayerState();
  }

  function queuePlayerState() {
    if (playerStateQueued) return;
    playerStateQueued = true;
    const schedule = window.requestAnimationFrame || ((callback) => setTimeout(callback, 16));
    schedule(() => {
      playerStateQueued = false;
      handlePlayerState();
    });
  }

  const queueAttachPlayer = () => (window.queueMicrotask || ((fn) => Promise.resolve().then(fn)))(attachPlayer);
  document.addEventListener('yt-navigate-finish', queueAttachPlayer);
  document.addEventListener('yt-page-data-updated', queueAttachPlayer);
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', queueAttachPlayer, { once: true });
  } else {
    queueAttachPlayer();
  }
  if (!document.querySelector('.html5-video-player') && document.documentElement) {
    playerDiscoveryObserver = new MutationObserver(queueAttachPlayer);
    playerDiscoveryObserver.observe(document.documentElement, { childList: true, subtree: true });
  }

  function setPerformance(mode) {
    S.performance = mode === 'performance' ? 'performance' : 'balanced';
    if (S.host) S.host.dataset.performance = S.performance;
    document.documentElement?.setAttribute('data-seraph-performance', S.performance);
    setPerformanceStyle(S.performance === 'performance'
      ? 'html[data-seraph-performance=\"performance\"]{scroll-behavior:auto!important}html[data-seraph-performance=\"performance\"] *,html[data-seraph-performance=\"performance\"] *::before,html[data-seraph-performance=\"performance\"] *::after{animation-duration:.001ms!important;transition-duration:.001ms!important}'
      : '');
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
    setCosmetic(S.on ? COSMETIC : '');
    if (payload.theme) {
      setTheme(payload.theme.css || '');
      onRoot(() => document.documentElement.setAttribute('data-seraph-theme', payload.theme.id || 'default'));
    }
  };

  if (window.__SERAPH_BOOT) S.apply(window.__SERAPH_BOOT);

  // The remote-page capability grants read-only access to this small enum.
  invoke('get_performance').then(setPerformance).catch(() => {});

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
        display: grid; place-items: center; width: 34px; height: 34px;
        padding: 0; border: 1px solid var(--st-accent, #c8a45a);
        border-radius: 50%; color: var(--st-accent, #c8a45a);
        background: rgba(10, 10, 12, .55); opacity: .32; cursor: pointer;
        transition: opacity .16s ease, box-shadow .16s ease;
        backdrop-filter: blur(8px);
      }
      button:hover, button:focus-visible { opacity: 1; box-shadow: 0 0 12px var(--st-accent, #c8a45a); outline: none; }
      b {
        position: absolute; top: -6px; right: -6px; padding: 1px 5px;
        border-radius: 8px; color: #fff; background: #8e2f45;
        font: 600 10px/1.4 system-ui, sans-serif;
      }
      b:empty { display: none; }
      :host([data-performance="performance"]) button { backdrop-filter: none; transition: none; box-shadow: none; }
      @media (prefers-reduced-motion: reduce) { button { transition: none; } }
    `;

    const ns = 'http://www.w3.org/2000/svg';
    const svgNode = (tag, attributes) => {
      const element = document.createElementNS(ns, tag);
      for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, value);
      return element;
    };
    const icon = svgNode('svg', {
      viewBox: '0 0 24 24', width: 18, height: 18, fill: 'none',
      stroke: 'currentColor', 'stroke-width': 1.4, 'aria-hidden': 'true',
    });
    icon.append(
      svgNode('circle', { cx: 12, cy: 12, r: 8.5 }),
      svgNode('circle', { cx: 12, cy: 12, r: 2.6 }),
      svgNode('path', { d: 'M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6L5.6 18.4' })
    );

    const badge = document.createElement('b');
    const button = document.createElement('button');
    button.type = 'button';
    button.title = 'Seraphtube settings (Ctrl+, / Cmd+,)';
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

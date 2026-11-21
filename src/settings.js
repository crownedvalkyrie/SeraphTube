(() => {
  'use strict';

  const invoke =
    window.__TAURI_INTERNALS__?.invoke ||
    window.__TAURI__?.core?.invoke;
  const $ = (selector) => document.querySelector(selector);
  const h = (tag, props = {}, ...children) => {
    const element = document.createElement(tag);
    for (const [key, value] of Object.entries(props)) {
      if (key.startsWith('aria-') || key.startsWith('data-')) element.setAttribute(key, String(value));
      else element[key] = value;
    }
    element.append(...children.filter((child) => child !== null && child !== undefined));
    return element;
  };

  let state = null;
  let statsTimer = null;
  let activeTab = 'appearance';
  let themeQuery = '';
  let limitsTimer = null;
  let splashPreviewed = false;

  function showError(error) {
    const message = String(error?.message || error || 'Unknown error');
    const target = $('#error');
    if (!target) return;
    target.textContent = message;
    target.hidden = false;
  }
  const clearError = () => { const target = $('#error'); if (target) target.hidden = true; };

  window.addEventListener('error', (event) => showError(event.error || event.message));
  window.addEventListener('unhandledrejection', (event) => showError(event.reason));

  if (typeof invoke !== 'function') {
    showError('Tauri bridge is unavailable. Open this page inside the Seraphtube desktop app.');
    return;
  }

  // ---- tabs -----------------------------------------------------------------------------------
  const tabs = [...document.querySelectorAll('.tab')];

  function selectTab(name, focus = false) {
    activeTab = name;
    for (const tab of tabs) {
      const on = tab.dataset.tab === name;
      tab.setAttribute('aria-selected', String(on));
      tab.tabIndex = on ? 0 : -1;
      if (on && focus) tab.focus();
      const pane = $('#pane-' + tab.dataset.tab);
      if (pane) pane.hidden = !on;
    }
    if (name === 'ward') startStats(); else stopStats();
    if (name === 'performance') startLimits(); else stopLimits();
  }

  for (const tab of tabs) {
    tab.addEventListener('click', () => selectTab(tab.dataset.tab));
    tab.addEventListener('keydown', (event) => {
      const index = tabs.indexOf(tab);
      let next = null;
      if (event.key === 'ArrowDown' || event.key === 'ArrowRight') next = tabs[(index + 1) % tabs.length];
      else if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') next = tabs[(index - 1 + tabs.length) % tabs.length];
      if (next) { event.preventDefault(); selectTab(next.dataset.tab, true); }
    });
  }

  // ---- themes ---------------------------------------------------------------------------------
  const paint = (id, css) => {
    document.documentElement.dataset.seraphTheme = id || 'default';
    const styleEl = $('#theme-css');
    if (styleEl) styleEl.textContent = css || '';
  };

  async function pickTheme(id) {
    try {
      const css = await invoke('set_theme', { id });
      if (state && state.settings) state.settings.theme = id;
      paint(id, css);
      renderThemes();
      await loadState(); // cursor previews take the new theme's accent colour
    } catch (error) {
      showError(error);
    }
  }

  // A tiny mock of a YouTube page in the theme's own colours.
  function preview(swatch) {
    const [bg = '#222', accent = '#888', other = '#fff'] = swatch || [];
    const bar = h('span', { className: 'bar' });
    bar.style.background = other;
    const thumbs = h('span', { className: 'grid' }, h('span'), h('span'), h('span'), h('span'), h('span'));
    thumbs.children[0].style.background = accent;
    thumbs.children[1].style.background = other;
    thumbs.children[1].style.opacity = '.35';
    for (const i of [2, 3, 4]) { thumbs.children[i].style.background = accent; thumbs.children[i].style.opacity = '.5'; }
    const box = h('div', { className: 'preview' }, bar, thumbs);
    box.style.background = bg;
    return box;
  }

  function renderThemes() {
    const container = $('#themes');
    if (!container) return;
    const themes = state?.themes || [];
    const q = themeQuery.trim().toLowerCase();
    const filtered = q
      ? themes.filter((theme) => {
          const kind = theme.builtin ? 'built-in builtin' : 'custom';
          const haystack = `${theme.name || ''} ${theme.id || ''} ${kind}`.toLowerCase();
          return haystack.includes(q);
        })
      : themes;

    const clearBtn = $('#theme-search-clear');
    if (clearBtn) clearBtn.hidden = !themeQuery;

    if (filtered.length === 0) {
      container.replaceChildren(
        h('p', { className: 'empty-themes' }, q ? `No themes match "${themeQuery.trim()}".` : 'No themes found.')
      );
      return;
    }

    container.replaceChildren(...filtered.map((theme) => {
      const selected = theme.id === state?.settings?.theme;
      const button = h('button', {
        type: 'button',
        className: 'theme',
        'aria-pressed': selected,
        onclick: () => pickTheme(theme.id),
      });
      button.append(
        preview(theme.swatch),
        h('b', {}, theme.name, h('i', { 'aria-hidden': 'true' }, '\u2713')),
        h('small', {}, theme.builtin ? 'Built-in' : 'Custom'),
      );
      return button;
    }));
  }

  // ---- cursors ---------------------------------------------------------------------------------
  async function pickCursor(id) {
    try {
      const css = await invoke('set_cursor', { id });
      if (state && state.settings) state.settings.cursor = id;
      paint(state?.settings?.theme, css);
      renderCursors();
    } catch (error) {
      showError(error);
    }
  }

  function renderCursors() {
    const box = $('#cursors');
    if (!box) return;
    const current = state?.settings?.cursor || 'system';
    box.replaceChildren(...(state?.cursors || []).map((item) => {
      const images = item.arrow
        ? h('span', { className: 'cursor-imgs' },
            h('img', { src: item.arrow, alt: '', width: 32, height: 32 }),
            h('img', { src: item.hand, alt: '', width: 32, height: 32 }))
        : h('span', { className: 'cursor-imgs sys' }, 'Aa');
      const button = h('button', {
        type: 'button',
        className: 'cursor-opt',
        'aria-checked': item.id === current,
        'aria-label': item.name,
        onclick: () => pickCursor(item.id),
      }, images, h('small', {}, item.name));
      button.setAttribute('role', 'radio');
      return button;
    }));
  }

  // ---- launch animation ---------------------------------------------------------------------
  const SPLASHES = [
    ['wings', 'Seraph Wings', 'Halo and wings unfurl feather by feather.'],
    ['rose', 'Rose Window', 'A stained-glass rose window assembles.'],
    ['stars', 'Constellation', 'Stars wake and draw a winged cross.'],
    ['gates', 'Cathedral Gates', 'Great doors swing open on a flood of light.'],
    ['sigil', 'Sigil Circle', 'Rings and runes ignite around an eye.'],
    ['vigil', 'Candle Vigil', 'Candles kindle one by one.'],
    ['crown', 'Gothic Crown', 'A crown of points and jewels assembles.'],
    ['hourglass', 'Hourglass', 'Sand turns from top to bottom.'],
    ['moon', 'Moonrise', 'A crescent moon rises among twinkling stars.'],
    ['random', 'Surprise me', 'A different one every launch.'],
  ];

  function previewSplash(id) {
    const frame = $('#splash-preview');
    if (!frame) return;
    frame.src = 'about:blank';                       // a hash change alone would not replay it
    requestAnimationFrame(() => { frame.src = 'splash.html#' + id; });
  }

  async function pickSplash(id) {
    try {
      await invoke('set_splash', { id });
      if (state && state.settings) state.settings.splash = id;
      renderSplashes();
      previewSplash(id);
    } catch (error) {
      showError(error);
    }
  }

  function renderSplashes() {
    const box = $('#splashes');
    if (!box) return;
    const current = state?.settings?.splash || 'wings';
    box.replaceChildren(...SPLASHES.map(([id, name, blurb]) => {
      const button = h('button', {
        type: 'button',
        className: 'splash-opt',
        'aria-checked': id === current,
        onclick: () => pickSplash(id),
      }, h('b', {}, name), h('small', {}, blurb));
      button.setAttribute('role', 'radio');
      return button;
    }));
  }

  // ---- resource limits ------------------------------------------------------------------------
  const RAM_STEPS = [0, 512, 768, 1024, 1536, 2048, 3072, 4096];
  const fmtMb = (mb) => (mb >= 1024 ? (mb / 1024).toFixed(mb % 1024 ? 1 : 0) + ' GB' : mb + ' MB');
  const ramLabel = (mb) => (mb === 0 ? 'Off' : fmtMb(mb));
  const batteryLabel = (pct) => (pct === 0 ? 'Off' : pct >= 100 ? 'Whenever unplugged' : 'At ' + pct + '% or below');
  const nearestStep = (mb) => RAM_STEPS.reduce((best, step, i) => (Math.abs(step - mb) < Math.abs(RAM_STEPS[best] - mb) ? i : best), 0);

  function renderLimits() {
    const s = state?.settings || {};
    const ram = $('#ram-limit');
    const battery = $('#battery-limit');
    if (ram) { ram.value = String(nearestStep(Number(s.ram_limit_mb) || 0)); $('#ram-value').textContent = ramLabel(RAM_STEPS[Number(ram.value)]); }
    if (battery) { battery.value = String(Number(s.battery_saver_pct ?? 20)); $('#bat-value').textContent = batteryLabel(Number(battery.value)); }
  }

  function showLimitsLive(data) {
    const status = data?.status || {};
    const limit = Number(data?.ramLimitMb) || 0;
    const used = Number(status.used_mb) || 0;
    const ramLive = $('#ram-live');
    if (ramLive) {
      ramLive.textContent = used > 0
        ? 'Using about ' + fmtMb(used) + (limit ? ' of ' + fmtMb(limit) : '') + (status.over_limit ? ' \u00b7 over the limit' : '')
        : 'Measuring\u2026';
    }
    const batLive = $('#bat-live');
    if (batLive) {
      batLive.textContent = status.battery_percent == null
        ? 'No battery detected.'
        : status.battery_percent + '% \u00b7 ' + (status.on_battery ? 'on battery' : 'plugged in') + (status.saver ? ' \u00b7 saver active' : '');
    }
  }

  async function refreshLimits() {
    if (document.hidden) return;
    try { showLimitsLive(await invoke('get_limits')); } catch (_) { /* optional readout */ }
  }
  function startLimits() { stopLimits(); limitsTimer = setInterval(refreshLimits, 3000); refreshLimits(); }
  function stopLimits() { if (limitsTimer !== null) clearInterval(limitsTimer); limitsTimer = null; }

  async function saveLimits() {
    const ram = RAM_STEPS[Number($('#ram-limit').value)] || 0;
    const pct = Number($('#battery-limit').value) || 0;
    try { await invoke('set_limits', { ramMb: ram, batteryPct: pct }); refreshLimits(); }
    catch (error) { showError(error); }
  }

  // ---- filter lists ---------------------------------------------------------------------------
  const LIST_INFO = {
    core: 'Third-party ad and tracker hosts. Never touches YouTube or its video servers.',
    'google-ads': 'Google ad servers. Stricter, and likelier to make YouTube slow playback down.',
    easylist: 'Large general list. Slower to compile and adds little inside YouTube.',
    easyprivacy: 'Large tracker list. Slower to compile and adds little inside YouTube.',
  };

  function renderLists() {
    const listEl = $('#lists');
    if (!listEl) return;
    const lists = state?.settings?.lists || [];
    const rows = lists.map((list) => {
      const when = list.updated
        ? 'Updated ' + new Date(list.updated * 1000).toLocaleDateString()
        : list.builtin ? 'Bundled' : 'Not downloaded yet';
      const toggle = h('input', {
        type: 'checkbox',
        className: 'switch',
        checked: Boolean(list.enabled),
        'aria-label': 'Enable ' + list.name,
        role: 'switch',
        onchange: async (event) => {
          const box = event.target;
          box.disabled = true;
          try {
            await invoke('toggle_filter_list', { id: list.id, on: box.checked });
            await loadState();
          } catch (error) {
            box.checked = !box.checked;
            showError(error);
          } finally {
            box.disabled = false;
          }
        },
      });
      const label = h('span', {},
        h('span', { className: 'name' }, list.name, list.builtin ? h('em', { className: 'tag' }, 'Built-in') : null),
        h('small', {}, [LIST_INFO[list.id], when].filter(Boolean).join(' · ')),
      );
      const remove = list.builtin ? null : h('button', {
        type: 'button',
        onclick: async () => {
          try { await invoke('remove_filter_list', { id: list.id }); await loadState(); }
          catch (error) { showError(error); }
        },
      }, 'Remove');
      return h('li', {}, toggle, label, remove);
    });
    listEl.replaceChildren(...rows);
    if (rows.length === 0) listEl.append(h('li', {}, h('small', {}, 'No filter lists are configured.')));
  }

  // ---- engine status --------------------------------------------------------------------------
  const ENGINE = {
    0: ['off', 'Off', ''],
    1: ['warn', 'Compiling\u2026', 'Preparing the network rules.'],
    2: ['ok', 'WebKit rules', 'WebKit content rules filter every browser-managed request; the page layer also removes ads from YouTube\u2019s data.'],
    3: ['bad', 'Fallback', 'The native rules could not be installed, so only the page layer is active. Try toggling blocking off and on.'],
    4: ['bad', 'Page layer only', 'No native request hook is available here.'],
    5: ['ok', 'WebView2 filter', 'The WebView2 request filter checks every request the page makes; the page layer also removes ads from YouTube\u2019s data.'],
  };

  function updateNativeStatus(code) {
    const [stateName, label, note] = ENGINE[code] || ['off', 'Unknown', ''];
    const pill = $('#native');
    if (pill) {
      pill.dataset.state = stateName;
      pill.textContent = label;
    }
    const blockNote = $('#block-note');
    if (blockNote) blockNote.textContent = note;
  }

  async function refreshStats() {
    if (document.hidden) return;
    try {
      const stats = await invoke('get_stats');
      $('#blocked').textContent = Number(stats.blocked || 0).toLocaleString();
      $('#rules').textContent = Number(stats.rules || 0).toLocaleString();
      updateNativeStatus(Number(stats.native));
    } catch (error) {
      stopStats();
      showError(error);
    }
  }

  function startStats() {
    stopStats();
    statsTimer = setInterval(refreshStats, 2000);
    refreshStats();
  }
  function stopStats() {
    if (statsTimer !== null) clearInterval(statsTimer);
    statsTimer = null;
  }

  // ---- state ------------------------------------------------------------------------------------
  const PLATFORM = { macos: 'macOS', windows: 'Windows' };

  async function loadState() {
    state = await invoke('get_state');
    paint(state.settings?.theme, state.css);
    renderThemes();
    renderCursors();
    renderSplashes();
    renderLimits();
    if (!splashPreviewed) { splashPreviewed = true; previewSplash(state.settings?.splash || 'wings'); }
    renderLists();
    if ($('#adblock')) $('#adblock').checked = Boolean(state.settings?.adblock);
    if ($('#intro')) $('#intro').checked = state.settings?.intro !== false;
    if ($('#blocked')) $('#blocked').textContent = Number(state.blocked || 0).toLocaleString();
    if ($('#rules')) $('#rules').textContent = Number(state.rules || 0).toLocaleString();
    updateNativeStatus(Number(state.native));

    const mac = state.platform === 'macos';
    const platform = PLATFORM[state.platform] || state.platform || '';
    if ($('#version')) $('#version').textContent = 'v' + (state.version || '') + (platform ? ' \u00b7 ' + platform : '');
    if ($('#about-line')) $('#about-line').textContent = 'Seraphtube ' + (state.version || '') + (platform ? ' on ' + platform : '');
    if ($('#key-settings')) $('#key-settings').textContent = mac ? '\u2318 ,' : 'Ctrl + ,';
    if ($('#key-pip')) $('#key-pip').textContent = mac ? '\u2318 \u21e7 P' : 'Ctrl + Shift + P';
    if ($('#key-reload')) $('#key-reload').textContent = mac ? '\u2318 R' : 'Ctrl + R  (or F5)';
    if ($('#coverage')) {
      $('#coverage').textContent = mac
        ? 'On macOS, WebKit content rules filter every request the browser makes, and the page layer removes ads from YouTube\u2019s data and hides leftover placements.'
        : 'On Windows, a native WebView2 filter checks every request the page makes against the same rules, and the page layer removes ads from YouTube\u2019s data and hides leftover placements.';
    }
  }

  // ---- wiring -----------------------------------------------------------------------------------
  const themeSearch = $('#theme-search');
  const themeSearchClear = $('#theme-search-clear');
  if (themeSearch) {
    themeSearch.addEventListener('input', (event) => {
      themeQuery = event.currentTarget.value;
      renderThemes();
    });
    themeSearch.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && themeSearch.value) {
        event.preventDefault();
        themeQuery = '';
        themeSearch.value = '';
        renderThemes();
      }
    });
  }
  if (themeSearchClear) {
    themeSearchClear.addEventListener('click', () => {
      themeQuery = '';
      if (themeSearch) {
        themeSearch.value = '';
        themeSearch.focus();
      }
      renderThemes();
    });
  }

  $('#splash-replay')?.addEventListener('click', () => previewSplash(state?.settings?.splash || 'wings'));

  $('#ram-limit')?.addEventListener('input', (event) => { $('#ram-value').textContent = ramLabel(RAM_STEPS[Number(event.target.value)] || 0); });
  $('#battery-limit')?.addEventListener('input', (event) => { $('#bat-value').textContent = batteryLabel(Number(event.target.value)); });
  $('#ram-limit')?.addEventListener('change', saveLimits);
  $('#battery-limit')?.addEventListener('change', saveLimits);

  $('#adblock')?.addEventListener('change', async (event) => {
    const toggle = event.currentTarget;
    toggle.disabled = true;
    clearError();
    try {
      await invoke('set_adblock', { on: toggle.checked });
      await refreshStats();
    } catch (error) {
      toggle.checked = !toggle.checked;
      showError(error);
    } finally {
      toggle.disabled = false;
    }
  });

  $('#intro')?.addEventListener('change', async (event) => {
    const toggle = event.currentTarget;
    try {
      await invoke('set_intro', { on: toggle.checked });
    } catch (error) {
      toggle.checked = !toggle.checked;
      showError(error);
    }
  });

  for (const radio of document.querySelectorAll('input[name="performance"]')) {
    radio.addEventListener('change', async () => {
      if (!radio.checked) return;
      try {
        const saved = await invoke('set_performance', { mode: radio.value });
        setPerformanceRadio(saved);
      } catch (error) {
        showError(error);
        invoke('get_performance').then(setPerformanceRadio).catch(() => {});
      }
    });
  }
  function setPerformanceRadio(mode) {
    const value = mode === 'performance' ? 'performance' : 'balanced';
    for (const radio of document.querySelectorAll('input[name="performance"]')) radio.checked = radio.value === value;
  }

  $('#open-themes')?.addEventListener('click', async () => {
    try { await invoke('open_themes_dir'); } catch (error) { showError(error); }
  });
  $('#rescan')?.addEventListener('click', async () => {
    try { await loadState(); } catch (error) { showError(error); }
  });

  $('#update')?.addEventListener('click', async () => {
    const button = $('#update');
    button.disabled = true;
    $('#status').textContent = 'Updating filter lists\u2026';
    try {
      $('#status').textContent = await invoke('update_filters');
      await loadState();
    } catch (error) {
      $('#status').textContent = 'Update failed';
      showError(error);
    } finally {
      button.disabled = false;
    }
  });

  $('#add')?.addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = event.currentTarget;
    const name = $('#name').value.trim();
    const url = $('#url').value.trim();
    if (!url.toLowerCase().startsWith('https://')) {
      showError('Filter lists must use HTTPS URLs.');
      return;
    }
    try {
      await invoke('add_filter_list', { name, url });
      form.reset();
      $('#status').textContent = 'Filter list added';
      await loadState();
    } catch (error) {
      showError(error);
    }
  });

  document.addEventListener('visibilitychange', () => {
    if (document.hidden) { stopStats(); stopLimits(); }
    else if (activeTab === 'ward') startStats();
    else if (activeTab === 'performance') startLimits();
  });

  (async function initialize() {
    try {
      await loadState();
      try {
        setPerformanceRadio(await invoke('get_performance'));
      } catch (_) { /* Optional performance query fallback. */ }
    } catch (error) {
      showError(error);
    }
  })();
})();

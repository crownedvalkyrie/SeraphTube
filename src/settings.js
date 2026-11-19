(() => {
  'use strict';

  const invoke = window.__TAURI__?.core?.invoke;
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
      $('#pane-' + tab.dataset.tab).hidden = !on;
    }
    if (name === 'ward') startStats(); else stopStats();
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
    $('#theme-css').textContent = css || '';
  };

  async function pickTheme(id) {
    try {
      const css = await invoke('set_theme', { id });
      if (state) state.settings.theme = id;
      paint(id, css);
      renderThemes();
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
    const themes = state?.themes || [];
    $('#themes').replaceChildren(...themes.map((theme) => {
      const selected = theme.id === state.settings.theme;
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

  // ---- filter lists ---------------------------------------------------------------------------
  const LIST_INFO = {
    core: 'Third-party ad and tracker hosts. Never touches YouTube or its video servers.',
    'google-ads': 'Google ad servers. Stricter, and likelier to make YouTube slow playback down.',
    easylist: 'Large general list. Slower to compile and adds little inside YouTube.',
    easyprivacy: 'Large tracker list. Slower to compile and adds little inside YouTube.',
  };

  function renderLists() {
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
    $('#lists').replaceChildren(...rows);
    if (rows.length === 0) $('#lists').append(h('li', {}, h('small', {}, 'No filter lists are configured.')));
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
    pill.dataset.state = stateName;
    pill.textContent = label;
    $('#block-note').textContent = note;
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
    renderLists();
    $('#adblock').checked = Boolean(state.settings?.adblock);
    $('#intro').checked = state.settings?.intro !== false;
    $('#blocked').textContent = Number(state.blocked || 0).toLocaleString();
    $('#rules').textContent = Number(state.rules || 0).toLocaleString();
    updateNativeStatus(Number(state.native));

    const mac = state.platform === 'macos';
    const platform = PLATFORM[state.platform] || state.platform || '';
    $('#version').textContent = 'v' + (state.version || '') + (platform ? ' \u00b7 ' + platform : '');
    $('#about-line').textContent = 'Seraphtube ' + (state.version || '') + (platform ? ' on ' + platform : '');
    $('#key-settings').textContent = mac ? '\u2318 ,' : 'Ctrl + ,';
    $('#key-pip').textContent = mac ? '\u2318 \u21e7 P' : 'Ctrl + Shift + P';
    $('#key-reload').textContent = mac ? '\u2318 R' : 'Ctrl + R  (or F5)';
    $('#coverage').textContent = mac
      ? 'On macOS, WebKit content rules filter every request the browser makes, and the page layer removes ads from YouTube\u2019s data and hides leftover placements.'
      : 'On Windows, a native WebView2 filter checks every request the page makes against the same rules, and the page layer removes ads from YouTube\u2019s data and hides leftover placements.';
  }

  // ---- wiring -----------------------------------------------------------------------------------
  $('#adblock').addEventListener('change', async (event) => {
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

  $('#intro').addEventListener('change', async (event) => {
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

  $('#open-themes').addEventListener('click', async () => {
    try { await invoke('open_themes_dir'); } catch (error) { showError(error); }
  });
  $('#rescan').addEventListener('click', async () => {
    try { await loadState(); } catch (error) { showError(error); }
  });

  $('#update').addEventListener('click', async () => {
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

  $('#add').addEventListener('submit', async (event) => {
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
    if (document.hidden) stopStats();
    else if (activeTab === 'ward') startStats();
  });

  (async function initialize() {
    try {
      await loadState();
      setPerformanceRadio(await invoke('get_performance'));
    } catch (error) {
      showError(error);
    }
  })();
})();

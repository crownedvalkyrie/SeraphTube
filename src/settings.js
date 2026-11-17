(() => {
  'use strict';

  const invoke = window.__TAURI__?.core?.invoke;
  const $ = (selector) => document.querySelector(selector);
  const h = (tag, props = {}, ...children) => {
    const element = document.createElement(tag);
    for (const [key, value] of Object.entries(props)) {
      if (key.startsWith('aria-') || key.startsWith('data-')) {
        element.setAttribute(key, String(value));
      } else {
        element[key] = value;
      }
    }
    element.append(...children.filter((child) => child !== null && child !== undefined));
    return element;
  };

  let state = null;
  let statsTimer = null;
  let reportingError = false;

  function showError(error) {
    const message = String(error?.message || error || 'Unknown error');
    const target = $('#error');
    if (!target) return;
    target.textContent = message;
    target.hidden = false;
  }

  window.addEventListener('error', (event) => showError(event.error || event.message));
  window.addEventListener('unhandledrejection', (event) => showError(event.reason));

  if (typeof invoke !== 'function') {
    showError('Tauri bridge is unavailable. Open this page inside the Seraphtube desktop app.');
    return;
  }

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

  function renderThemes() {
    const themes = state?.themes || [];
    $('#themes').replaceChildren(...themes.map((theme) => {
      const button = h('button', {
        type: 'button',
        className: 'theme' + (theme.id === state.settings.theme ? ' on' : ''),
        'aria-pressed': theme.id === state.settings.theme,
        onclick: () => pickTheme(theme.id),
      });
      const swatch = h('span', { className: 'sw' });
      for (const color of (theme.swatch || []).slice(0, 8)) {
        const chip = h('i');
        chip.style.backgroundColor = color;
        swatch.append(chip);
      }
      button.append(swatch, h('b', {}, theme.name), h('small', {}, theme.builtin ? 'Built-in' : 'Custom'));
      return button;
    }));
  }

  async function renderLists() {
    const lists = state?.settings?.lists || [];
    const rows = lists.map((list) => {
      const when = list.updated
        ? 'Updated ' + new Date(list.updated * 1000).toLocaleDateString()
        : list.builtin ? 'Bundled' : 'Not downloaded yet';
      const toggle = h('input', {
        type: 'checkbox',
        checked: Boolean(list.enabled),
        'aria-label': 'Enable ' + list.name,
        onchange: async (event) => {
          event.target.disabled = true;
          try {
            await invoke('toggle_filter_list', { id: list.id, on: event.target.checked });
            await loadState();
          } catch (error) {
            event.target.checked = !event.target.checked;
            showError(error);
          } finally {
            event.target.disabled = false;
          }
        },
      });
      const label = h('span', {}, list.name + ' ', h('small', {}, when));
      const remove = list.builtin ? null : h('button', {
        type: 'button',
        onclick: async () => {
          try {
            await invoke('remove_filter_list', { id: list.id });
            await loadState();
          } catch (error) {
            showError(error);
          }
        },
      }, 'Remove');
      return h('li', {}, toggle, label, remove);
    });

    $('#lists').replaceChildren(...rows);
    if (rows.length === 0) {
      $('#lists').append(h('li', {}, h('small', {}, 'No filter lists are configured.')));
    }
  }

  async function loadState() {
    state = await invoke('get_state');
    paint(state.settings?.theme, state.css);
    renderThemes();
    await renderLists();
    $('#adblock').checked = Boolean(state.settings?.adblock);
    $('#rules').textContent = Number(state.rules || 0).toLocaleString();
    updateNativeStatus(Number(state.native));
  }

  function updateNativeStatus(status) {
    const labels = {
      0: 'Off',
      1: 'Compiling native filters…',
      2: 'Native WebKit rules active (macOS / Linux)',
      3: 'Native filter compilation failed · fallback active',
      4: 'JavaScript fallback active · no native request hook',
    };
    $('#native').textContent = labels[status] || 'Unknown';
    $('#block-note').textContent = status === 4
      ? 'Browser-managed subresource requests are not all visible to the injected JavaScript layer on this platform.'
      : status === 2
        ? 'WebKit content rules filter browser-managed requests; the JavaScript layer also handles YouTube player cleanup.'
        : status === 3
          ? 'The native rule compiler rejected the full list; the app attempted its bundled fallback rules.'
          : '';
  }

  async function refreshStats() {
    if (document.hidden || reportingError) return;
    try {
      const stats = await invoke('get_stats');
      $('#blocked').textContent = Number(stats.blocked || 0).toLocaleString();
      $('#rules').textContent = Number(stats.rules || 0).toLocaleString();
      updateNativeStatus(Number(stats.native));
    } catch (error) {
      reportingError = true;
      showError(error);
    }
  }

  function startStats() {
    if (statsTimer !== null) clearInterval(statsTimer);
    statsTimer = setInterval(refreshStats, 5000);
    refreshStats();
  }

  $('#adblock').addEventListener('change', async (event) => {
    const toggle = event.currentTarget;
    toggle.disabled = true;
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

  $('#performance').addEventListener('change', async (event) => {
    const select = event.currentTarget;
    const previous = select.value === 'performance' ? 'balanced' : 'performance';
    select.disabled = true;
    try {
      const saved = await invoke('set_performance', { mode: select.value });
      select.value = saved;
    } catch (error) {
      select.value = previous;
      showError(error);
    } finally {
      select.disabled = false;
    }
  });

  $('#open-themes').addEventListener('click', async () => {
    try { await invoke('open_themes_dir'); } catch (error) { showError(error); }
  });

  $('#rescan').addEventListener('click', async () => {
    try { await loadState(); } catch (error) { showError(error); }
  });

  $('#update').addEventListener('click', async () => {
    const button = $('#update');
    button.disabled = true;
    $('#status').textContent = 'Updating filter lists…';
    try {
      $('#status').textContent = await invoke('update_filters');
      await loadState();
      await refreshStats();
    } catch (error) {
      $('#status').textContent = 'Update failed';
      showError(error);
    } finally {
      button.disabled = false;
    }
  });

  $('#add').addEventListener('submit', async (event) => {
    event.preventDefault();
    const name = $('#name').value.trim();
    const url = $('#url').value.trim();
    if (!url.toLowerCase().startsWith('https://')) {
      showError('Filter lists must use HTTPS URLs.');
      return;
    }
    try {
      await invoke('add_filter_list', { name, url });
      event.currentTarget.reset();
      $('#status').textContent = 'Filter list added';
      await loadState();
    } catch (error) {
      showError(error);
    }
  });

  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      if (statsTimer !== null) clearInterval(statsTimer);
      statsTimer = null;
    } else {
      startStats();
    }
  });

  async function initialize() {
    try {
      await loadState();
      const mode = await invoke('get_performance');
      $('#performance').value = mode === 'performance' ? 'performance' : 'balanced';
      startStats();
    } catch (error) {
      showError(error);
    }
  }

  initialize();
})();

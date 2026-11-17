// Логика окна настроек (внешний скрипт для строгой совместимости с CSP `script-src 'self'`).
(() => {
  const { invoke } = window.__TAURI__.core;
  const win = window.__TAURI__.window.getCurrentWindow();
  const I18N = window.DI_I18N;

  const tabs = [...document.querySelectorAll('.tab-btn')];
  const panes = [...document.querySelectorAll('.pane')];
  const inputs = [...document.querySelectorAll('input[data-key]')];
  const segs = [...document.querySelectorAll('[data-seg]')];
  const swatches = [...document.querySelectorAll('.swatch[data-accent]')];
  const hkBtns = [...document.querySelectorAll('[data-hk]')];
  const updBtn = document.getElementById('updBtn');
  const updStatus = document.getElementById('updStatus');
  const resetBtn = document.getElementById('resetBtn');

  let cur = {};
  let recording = null;
  let releaseUrl = '';
  let shown = false;

  function lang() { return I18N.resolveLang(cur.lang); }
  function T(k, v) { return I18N.t(lang(), k, v); }

  for (const tb of tabs) {
    tb.addEventListener('click', () => {
      const id = tb.dataset.tab;
      tabs.forEach((b) => b.classList.toggle('active', b === tb));
      panes.forEach((p) => p.classList.toggle('active', p.dataset.pane === id));
    });
  }

  function applyLocale() {
    const l = lang();
    document.documentElement.lang = l;
    document.title = T('s_title');
    document.querySelectorAll('[data-i18n]').forEach((el) => { el.textContent = T(el.dataset.i18n); });
    document.getElementById('close').ariaLabel = T('close');
  }

  function fill(s) {
    cur = s || {};
    applyLocale();
    for (const el of inputs) el.checked = !!s[el.dataset.key];
    for (const seg of segs) {
      const key = seg.dataset.seg;
      const isNum = seg.dataset.type === 'num';
      const val = s[key] ?? seg.dataset.default;
      const buttons = [...seg.querySelectorAll('button')];
      let idx = buttons.findIndex((b) => (isNum ? +b.dataset.v === +val : b.dataset.v === val));
      if (idx < 0) idx = buttons.findIndex((b) => (isNum ? +b.dataset.v === +seg.dataset.default : b.dataset.v === seg.dataset.default));
      if (idx < 0) idx = 0;
      buttons.forEach((b, i) => b.setAttribute('aria-pressed', String(i === idx)));
      seg.querySelector('.pill').style.transform = `translateX(${idx * 100}%)`;
    }
    const accent = (s.accent_color || 'auto').toLowerCase();
    for (const sw of swatches) {
      sw.setAttribute('aria-pressed', String(sw.dataset.accent.toLowerCase() === accent));
    }
    for (const b of hkBtns) {
      if (recording !== b) b.textContent = s[b.dataset.hk] || '';
      const ok = s[b.dataset.hk + '_ok'] !== false;
      b.classList.toggle('err', !ok && !!s.hotkeys);
      b.title = !ok && !!s.hotkeys ? T('s_hk_busy') : '';
    }
    document.getElementById('ver').textContent = T('s_ver', { v: s.version || '' });
    document.getElementById('pomoStats').textContent = T('s_pomo_fmt', {
      t: s.pomodoros_today || 0,
      w: s.pomodoros_week || 0,
      s: s.pomodoros_streak || 0,
    });

    if (!shown) {
      shown = true;
      requestAnimationFrame(() => {
        if (win.show) win.show().catch(() => {});
        if (win.setFocus) win.setFocus().catch(() => {});
      });
    }
  }

  async function set(key, value) {
    try { fill(await invoke('set_setting', { key, value })); } catch { /* ignore */ }
  }

  for (const el of inputs) el.addEventListener('change', () => set(el.dataset.key, el.checked));
  for (const seg of segs) {
    const key = seg.dataset.seg;
    const isNum = seg.dataset.type === 'num';
    seg.querySelectorAll('button').forEach((b) =>
      b.addEventListener('click', () => set(key, isNum ? +b.dataset.v : b.dataset.v))
    );
  }
  for (const sw of swatches) {
    sw.addEventListener('click', () => set('accent_color', sw.dataset.accent));
  }

  function keyLabel(e) {
    if (['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return null;
    const mods = [];
    if (e.ctrlKey) mods.push('Ctrl');
    if (e.altKey) mods.push('Alt');
    if (e.shiftKey) mods.push('Shift');
    if (!mods.length) return null;
    let k = e.code;
    if (k.startsWith('Key')) k = k.slice(3);
    else if (k.startsWith('Digit')) k = k.slice(5);
    else if (e.key === ' ') k = 'Space';
    else if (e.key === 'Enter') k = 'Enter';
    else if (e.key === 'Tab') k = 'Tab';
    else if (/^F([1-9]|1[0-2])$/i.test(e.key)) k = e.key.toUpperCase();
    else return null;
    return [...mods, k].join('+');
  }

  for (const b of hkBtns) {
    b.addEventListener('click', () => {
      if (recording) recording.classList.remove('rec');
      recording = b;
      b.classList.add('rec');
      b.textContent = T('s_hk_press');
    });
  }

  updBtn.addEventListener('click', async () => {
    if (releaseUrl) {
      invoke('open_release_url', { url: releaseUrl }).catch(() => {});
      return;
    }
    updBtn.disabled = true;
    updStatus.classList.remove('err');
    updStatus.textContent = T('s_upd_checking');
    try {
      const info = await invoke('check_update');
      if (info && info.available) {
        releaseUrl = info.url;
        updStatus.textContent = T('s_upd_avail', { v: info.version });
        updBtn.textContent = T('s_upd_avail', { v: info.version });
        updBtn.classList.add('accent');
      } else if (info && info.error) {
        updStatus.classList.add('err');
        updStatus.textContent = info.error === 'rate_limit' ? T('s_upd_rate') : T('s_upd_err');
      } else {
        updStatus.textContent = T('s_upd_latest');
      }
    } catch {
      updStatus.classList.add('err');
      updStatus.textContent = T('s_upd_err');
    } finally {
      updBtn.disabled = false;
    }
  });

  if (resetBtn) {
    resetBtn.addEventListener('click', async () => {
      try {
        fill(await invoke('reset_settings'));
        resetBtn.textContent = T('s_reset_done');
        setTimeout(() => { resetBtn.textContent = T('s_reset_btn'); }, 1200);
      } catch { /* ignore */ }
    });
  }

  document.getElementById('close').addEventListener('click', () => win.close());
  window.addEventListener('keydown', (e) => {
    if (recording) {
      e.preventDefault();
      if (e.key === 'Escape') {
        recording.classList.remove('rec');
        recording.textContent = cur[recording.dataset.hk] || '';
        recording = null;
        return;
      }
      const combo = keyLabel(e);
      if (combo) {
        const btn = recording;
        const dup = hkBtns.some((o) => o !== btn && (cur[o.dataset.hk] || '').toLowerCase() === combo.toLowerCase());
        if (dup) {
          btn.classList.remove('rec');
          btn.classList.add('err');
          btn.textContent = cur[btn.dataset.hk] || '';
          btn.title = T('s_hk_dup');
          recording = null;
          setTimeout(() => fill(cur), 1400);
          return;
        }
        recording = null;
        btn.classList.remove('rec');
        set(btn.dataset.hk, combo);
      }
      return;
    }
    if (e.key === 'Escape') win.close();
  });

  if (window.__TAURI__.event) {
    window.__TAURI__.event.listen('settings', () => {
      invoke('get_settings').then(fill).catch(() => {});
    });
  }
  invoke('get_settings').then(fill).catch(() => {
    if (win.show) win.show().catch(() => {});
  });
})();

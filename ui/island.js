// Логика острова: выбирает, что показать, плавно меняет размер и сообщает Rust-части,
// где остров на экране и можно ли сейчас ловить мышь.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const I18N = window.DI_I18N;
  const $ = (id) => document.getElementById(id);
  const island = $('island');
  const pdots = $('pdots');
  const views = [...document.querySelectorAll('.view')];
  const $$home = document.querySelector('.v-home');
  const mediaRestore = $('mediaRestore');
  const L = () => I18N.resolveLang(st.settings.lang);
  const T = (k, v) => I18N.t(L(), k, v);
  // Анимации выключаются только своей настройкой в трее. Системный флаг «уменьшить движение»
  // не слушаем: его часто выключают твикеры (Atlas OS и подобные), и остров начинает прыгать.
  let reduceMotion = false;

  // ширина, высота, скругление
  const SIZE = {
    hidden: [90, 26, 13],
    sliver: [150, 5, 5],
    sliverHot: [170, 7, 6],
    idle: [124, 30, 15],
    mini: [206, 32, 16],
    tmini: [150, 32, 16],
    home: [480, 116, 30],
    music: [392, 166, 38],
    audio: [384, 120, 26],
    shelf: [412, 132, 28],
    clips: [412, 132, 28],
    peek: [330, 54, 27],
    volume: [290, 36, 18],
    charge: [290, 36, 18],
    low: [290, 36, 18],
    clip: [330, 40, 20],
    device: [330, 52, 26],
    layout: [176, 36, 18],
    caps: [196, 36, 18],
    done: [420, 62, 31],
  };
  const VIEW = { sliverHot: 'sliver' };
  const topMargin = () => (typeof st?.settings?.top_margin === 'number' ? st.settings.top_margin : 6);
  // Сколько держать курсор над островом, прежде чем он раскроется.
  // Над вкладками и заголовками дольше, чтобы остров не выскакивал, когда тянешься к вкладке.
  const DWELL = { normal: 90, busy: 450 }; // busy меняется в настройках

  // эквалайзеры: внешний столбик «приседает» на паузе, внутренний дышит
  document.querySelectorAll('.eq').forEach((e) => { e.innerHTML = '<i><b></b></i>'.repeat(5); });
  // каждый экран сразу своего размера: остров при росте «открывает» его, текст не прыгает
  for (const v of views) {
    const [w, h] = SIZE[v.dataset.view];
    v.style.width = w + 'px';
    v.style.height = h + 'px';
  }

  const st = {
    media: { active: false },
    mediaHidden: false,
    preferHome: false,
    power: null,
    volume: null,
    hover: false,
    pinned: false,
    dragHover: false,
    subview: null,
    pending: false,
    hud: null,
    device: null,
    shelf: [],
    clips: [],
    update: null,
    dnd: false,
    full: false,
    topbusy: false,
    privacy: { mic: [], cam: [] },
    settings: { sounds: true, top_margin: 6, lang: 'auto' },
    demo: false,
    sys: null,
    pomodoros: 0,
    pausedSince: 0,
  };
  let hudTimer = null;
  let hoverTimer = null;
  let wheelUntil = 0;
  let lastRect = '';
  let customOpen = false;
  let customMin = 10;

  // ---------- пружины ----------
  // Размер и форму ведёт затухающая пружина. Если цель меняется посреди движения, остров
  // спокойно поворачивает к новой цели, сохраняя скорость, без рывков.
  // 280/28: почти без перелёта, успокаивается за ~0,45 с — аккуратно, без «прыжков».
  const SPRING_PRESETS = {
    soft: [190, 26],
    normal: [280, 28],
    bouncy: [340, 19],
    snappy: [460, 38],
  };
  const uiScale = () => Math.min(1.15, Math.max(0.85, (st.settings.ui_scale || 100) / 100));
  const pomoWorkMin = () => st.settings.pomo_work_min || 25;
  const pomoBreakMin = () => st.settings.pomo_break_min || 5;
  const spring = (v, k = 280, c = 28) => ({ v, t: v, vel: 0, k, c, from: v });
  const S = {
    w: spring(124),
    h: spring(30),
    r: spring(15),
    rt: spring(15),
    top: spring(6),
    x: spring(0, 220, 20), // лёгкий сдвиг вбок при переключении трека
  };
  let raf = 0;
  let lastT = 0;
  let perf = null;

  function frame(now) {
    const gap = now - lastT;
    const dt = Math.min(gap / 1000, 1 / 30);
    lastT = now;
    if (perf) { perf.n++; perf.max = Math.max(perf.max, gap); perf.sum += gap; }
    let moving = false;
    for (const s of Object.values(S)) {
      const steps = 4;
      const h = dt / steps;
      for (let i = 0; i < steps; i++) {
        const f = -s.k * (s.v - s.t) - s.c * s.vel;
        s.vel += f * h;
        s.v += s.vel * h;
      }
      if (Math.abs(s.v - s.t) > 0.03 || Math.abs(s.vel) > 0.3) moving = true;
      else { s.v = s.t; s.vel = 0; }
    }
    paint();
    maybeShowView();
    if (moving) raf = requestAnimationFrame(frame);
    else { raf = 0; showView(); finishPerf(); }
  }
  function animate() {
    if (raf) return;
    lastT = performance.now();
    if (st.demo && !perf) perf = { n: 0, max: 0, sum: 0, t0: lastT, label: island.dataset.mode };
    raf = requestAnimationFrame(frame);
  }
  function paint() {
    const w = Math.max(0, S.w.v);
    const h = Math.max(0, S.h.v);
    const r = Math.min(Math.max(0, S.r.v), h / 2);
    const rt = Math.min(Math.max(0, S.rt.v), h / 2);
    const sc = uiScale();
    const s = island.style;
    s.width = w.toFixed(2) + 'px';
    s.height = h.toFixed(2) + 'px';
    s.top = S.top.v.toFixed(2) + 'px';
    s.borderRadius = `${rt.toFixed(2)}px ${rt.toFixed(2)}px ${r.toFixed(2)}px ${r.toFixed(2)}px`;
    s.transform = sc === 1
      ? `translateX(calc(-50% + ${S.x.v.toFixed(2)}px))`
      : `translateX(calc(-50% + ${S.x.v.toFixed(2)}px)) scale(${sc})`;
    const dotY = Math.max(4, S.top.v + (h * sc) / 2 - 3);
    pdots.style.transform = `translate(${((w * sc) / 2 + 8 + S.x.v).toFixed(2)}px, ${dotY.toFixed(2)}px)`;
  }
  function setTargets(w, h, r, rt, top) {
    for (const [s, t] of [[S.w, w], [S.h, h], [S.r, r], [S.rt, rt], [S.top, top]]) {
      if (s.t !== t) { s.from = s.v; s.t = t; }
    }
    if (reduceMotion) {
      for (const s of Object.values(S)) { s.v = s.t; s.vel = 0; }
      paint();
      showView();
      return;
    }
    animate();
  }
  function kick(dir) {
    if (reduceMotion) return;
    S.x.vel += dir * 150;
    animate();
  }

  // ---------- экраны ----------
  // Старый экран гаснет сразу, новый включается, когда остров прошёл больше половины пути
  // (или через 200 мс, если путь короткий). Так содержимое не мелькает обрезанным.
  let curView = '';
  let nextView = '';
  let nextAt = 0;
  function requestView(v) {
    if (v === curView && !nextView) return;
    if (v === nextView) return;
    for (const el of views) if (el.dataset.view === curView) el.classList.remove('on');
    curView = '';
    nextView = v;
    nextAt = performance.now() + 200;
    if (!raf || reduceMotion) { showView(); return; }
    // страховка: если кадры почему-то не идут, экран всё равно появится
    clearTimeout(viewTimer);
    viewTimer = setTimeout(showView, 240);
  }
  let viewTimer = null;
  function progress() {
    let p = 1;
    for (const s of [S.w, S.h]) {
      const span = Math.abs(s.t - s.from);
      if (span > 1) p = Math.min(p, 1 - Math.abs(s.t - s.v) / span);
    }
    return p;
  }
  function maybeShowView() {
    if (nextView && (progress() > 0.55 || performance.now() > nextAt)) showView();
  }
  function showView() {
    if (!nextView) return;
    for (const el of views) el.classList.toggle('on', el.dataset.view === nextView);
    curView = nextView;
    nextView = '';
  }

  // Домашний экран и панели измеряют свою естественную высоту по содержимому.
  function measureHeight(el, maxH) {
    el.style.height = 'auto';
    const h = Math.ceil(el.offsetHeight);
    return maxH ? Math.min(h, maxH) : h;
  }
  function sizeOf(m) {
    if (m === 'idle') return [st.settings.idle_style === 'clock' && st.settings.clock_seconds ? 140 : 124, 30, 15];
    if (m === 'home') return [480, measureHeight($$home), 30];
    if (m === 'audio') return [384, measureHeight(document.querySelector('[data-view="audio"]'), 172), 26];
    if (m === 'shelf') return [412, measureHeight(document.querySelector('[data-view="shelf"]'), 172), 28];
    if (m === 'clips') return [412, measureHeight(document.querySelector('[data-view="clips"]'), 172), 28];
    return SIZE[m];
  }

  // ---------- выбор состояния ----------
  function compact() {
    if (st.topbusy) return 'sliver';
    if (st.media.active && !st.mediaHidden && !longPaused()) return 'mini';
    if (timer.running()) return 'tmini';
    return 'idle';
  }
  // Музыка на паузе дольше трёх минут — сворачиваемся, чтобы обложка не висела часами.
  // При наведении плеер по-прежнему открывается.
  function longPaused() {
    return st.settings.hide_paused !== false && !st.media.playing && st.pausedSince && Date.now() - st.pausedSince > 180000;
  }
  setInterval(() => { if (!st.hover && !st.pinned && !st.hud && mode() !== island.dataset.mode) render(); }, 5000);

  function mode() {
    if (st.full) return 'hidden';
    if (st.dragHover) return 'shelf';
    const expanded = st.hover || st.pinned;
    if (st.hud && (st.hud.force || !expanded)) return st.hud.type;
    if (expanded) {
      if (st.subview) return st.subview;
      return st.media.active && !st.mediaHidden && !st.preferHome ? 'music' : 'home';
    }
    const c = compact();
    if (c === 'sliver' && st.pending) return 'sliverHot';
    return c;
  }

  function render() {
    const m = mode();
    const v = VIEW[m] || m;
    island.dataset.mode = m;
    island.classList.toggle('playing', !!st.media.playing && !st.mediaHidden);
    island.classList.toggle('drag-over', !!st.dragHover);
    mediaRestore.hidden = !(st.media.active && (st.mediaHidden || st.preferHome));
    const [w, h, r] = sizeOf(m);
    if (m === 'idle' || m === 'home' || m === 'audio' || m === 'shelf' || m === 'clips') {
      const viewEl = document.querySelector(`[data-view="${m}"]`);
      if (viewEl) {
        viewEl.style.width = w + 'px';
        viewEl.style.height = h + 'px';
      }
    }
    const sliver = v === 'sliver';
    const top = topMargin();
    island.dataset.idle = st.settings.idle_style || 'pill';
    island.classList.toggle('stealth', m === 'idle' && st.settings.idle_style === 'hidden' && !st.pending);
    setTargets(w, h, r, sliver ? 0 : r, sliver ? 0 : top);
    requestView(v);

    // Зона попадания курсора: в свёрнутом состоянии строго по контуру самой пилюли (без лишних полей).
    const sc = uiScale();
    const sw = w * sc;
    const sh = h * sc;
    const expMode = st.settings.expand_mode || 'hover';
    const interactive = st.hover || st.pinned || st.dragHover || m === 'done';
    const clickTrigger = !interactive && expMode === 'click' && !st.topbusy && (m === 'idle' || m === 'mini' || m === 'tmini') && st.settings.idle_style !== 'hidden';
    const wheelOnly = !interactive && !!st.settings.wheel_collapsed && !st.topbusy && (m === 'idle' || m === 'mini' || m === 'tmini');
    if (m === 'hidden' || (!interactive && expMode === 'hotkey')) {
      sendRect(0, 0, 0, 0, false);
    } else if (sliver) {
      sendRect((innerWidth - 96 * sc) / 2, 0, 96 * sc, 7, false);
    } else if (!interactive) {
      // От самого верха экрана (y = 0) до низа пилюли: если увести мышь до упора вверх (y = 0),
      // курсор не выпадает в зазор над островом и не вызывает ложное закрытие.
      sendRect((innerWidth - sw) / 2, 0, sw, top + sh, wheelOnly || clickTrigger);
    } else {
      sendRect((innerWidth - sw) / 2 - 6, 0, sw + 12, top + sh + 6, true);
    }
    live();
  }

  function sendRect(x, y, w, h, interactive) {
    const key = [x, y, w, h].map(Math.round).join(',') + (interactive ? '+' : '-');
    if (key === lastRect) return;
    lastRect = key;
    invoke('set_hit_rect', { x, y, w, h, interactive }).catch(() => {});
  }

  const HUD_PRIO = { done: 100, low: 80, device: 60, charge: 60, peek: 40, volume: 30, clip: 20, layout: 10, caps: 10 };
  function isQuiet() {
    return st.dnd || (!!st.settings.quiet_pomo && timer.running() && !timer.paused && timer.kind === 'work');
  }
  function setHud(type, ms, force) {
    if (!force && isQuiet() && (HUD_PRIO[type] || 0) < 50) return;
    if (st.hud && (HUD_PRIO[type] || 0) < (HUD_PRIO[st.hud.type] || 0)) return;
    st.hud = { type, force: !!force };
    clearTimeout(hudTimer);
    hudTimer = setTimeout(() => { st.hud = null; render(); }, ms);
    render();
  }
  function clearHud() {
    clearTimeout(hudTimer);
    st.hud = null;
    render();
  }

  // ---------- текст, который меняется со временем ----------
  const pad = (n) => String(n).padStart(2, '0');
  const setLive = (key, text) => document.querySelectorAll(`[data-live="${key}"]`).forEach((el) => {
    if (el.textContent !== text) el.textContent = text;
  });
  function fmt(sec) {
    sec = Math.max(0, Math.ceil(sec));
    const h = Math.floor(sec / 3600);
    const m = Math.floor((sec % 3600) / 60);
    return h ? `${h}:${pad(m)}:${pad(sec % 60)}` : `${m}:${pad(sec % 60)}`;
  }

  function live() {
    const d = new Date();
    const lang = L();
    const use24 = st.settings.clock_24h !== false;
    const rawH = d.getHours();
    const hStr = use24 ? pad(rawH) : String(rawH % 12 || 12);
    const secStr = st.settings.clock_seconds ? `:${pad(d.getSeconds())}` : '';
    const timeEl = document.querySelector('.v-home .time');
    if (timeEl) timeEl.style.fontSize = st.settings.clock_seconds ? '20px' : '';
    setLive('time', `${hStr}:${pad(d.getMinutes())}${secStr}`);
    const date = d.toLocaleDateString(lang === 'en' ? 'en-US' : 'ru-RU', { weekday: 'short', day: 'numeric', month: 'short' });
    setLive('date', st.pomodoros ? `${date} · 🍅 ${st.pomodoros}` : date);

    const running = timer.running();
    island.classList.toggle('has-timer', running);
    island.classList.toggle('tpaused', running && timer.paused);
    $('presets').classList.toggle('away', running || customOpen);
    $('tCustom').classList.toggle('away', running || !customOpen);
    $('tRun').classList.toggle('away', !running);
    if (running) {
      const left = timer.left();
      if (left <= 0 && !timer.paused) { finishTimer(); return; }
      setLive('tleft', fmt(left));
      setLive('tlabel', timer.paused ? T('paused') : timer.label());
      const off = (50.27 * (1 - left / timer.total)).toFixed(2);
      document.querySelectorAll('.ring .rfg').forEach((c) => { c.style.strokeDashoffset = off; });
    }
    if (curView === 'music' || nextView === 'music') tickProgress();
  }
  setInterval(live, 250);

  // ---------- таймер и помодоро ----------
  const timer = {
    total: 0,
    end: 0,
    rest: 0,
    paused: false,
    min: 0,
    kind: '',
    label() {
      if (this.kind === 'work') return T('pomo_work');
      if (this.kind === 'break') return T('pomo_break');
      return T('timer_for', { n: this.min });
    },
    running() { return this.total > 0; },
    left() { return this.paused ? this.rest : Math.max(0, (this.end - Date.now()) / 1000); },
  };
  const TIMER_KEY = 'di_timer_v1';
  function saveTimer() {
    try {
      if (!timer.running()) localStorage.removeItem(TIMER_KEY);
      else localStorage.setItem(TIMER_KEY, JSON.stringify({
        total: timer.total, end: timer.end, rest: timer.rest,
        paused: timer.paused, min: timer.min, kind: timer.kind,
      }));
    } catch { /* ignore */ }
  }
  function restoreTimer() {
    try {
      const raw = localStorage.getItem(TIMER_KEY);
      if (!raw) return;
      const t = JSON.parse(raw);
      if (!t || !t.total) return;
      if (!t.paused && t.end <= Date.now()) { localStorage.removeItem(TIMER_KEY); return; }
      Object.assign(timer, t);
    } catch { /* ignore */ }
  }
  function startTimer(min, kind) {
    customOpen = false;
    timer.total = min * 60;
    timer.min = min;
    timer.end = Date.now() + timer.total * 1000;
    timer.paused = false;
    timer.kind = kind || 'plain';
    saveTimer();
    render();
  }
  function setCustomMin(next) {
    customMin = Math.min(180, Math.max(1, next));
    $('tCustomVal').textContent = T('min_short', { n: customMin });
    const pomoBtn = $('pomoPresetBtn');
    if (pomoBtn) {
      const wm = pomoWorkMin();
      pomoBtn.dataset.t = String(wm);
      pomoBtn.textContent = T('pomodoro_n', { n: wm });
    }
  }
  function pauseTimer() {
    if (timer.paused) { timer.end = Date.now() + timer.rest * 1000; timer.paused = false; }
    else { timer.rest = timer.left(); timer.paused = true; }
    saveTimer();
    render();
  }
  function addMinute() {
    if (!timer.running()) return;
    timer.total += 60;
    timer.min = Math.round(timer.total / 60);
    if (timer.paused) timer.rest += 60;
    else timer.end += 60000;
    saveTimer();
    live();
  }
  function stopTimer() { timer.total = 0; saveTimer(); render(); }
  function finishTimer() {
    const kind = timer.kind;
    const min = Math.round(timer.total / 60);
    timer.total = 0;
    saveTimer();
    $('doneTitle').textContent = kind === 'work' ? T('pomo_done') : kind === 'break' ? T('break_done') : T('time_up');
    $('doneSub').textContent = kind === 'work' ? T('time_to_rest') : kind === 'break' ? T('back_to_work') : T('elapsed_min', { n: min });
    const btn = $('doneBreak');
    if (kind === 'work') {
      btn.hidden = false;
      btn.textContent = T('break_n', { n: pomoBreakMin() });
      btn.dataset.nextKind = 'break';
    } else if (kind === 'break') {
      btn.hidden = false;
      btn.textContent = T('another_pomo');
      btn.dataset.nextKind = 'work';
    } else {
      btn.hidden = true;
    }
    if (kind === 'work') invoke('pomodoro_done').catch(() => {});
    chime();
    setHud('done', 12000, true);
  }

  let audio;
  function chime() {
    const vol = st.settings.sound_volume ?? 70;
    if (!st.settings.sounds || vol <= 0) return;
    const peak = 0.2 * (vol / 100);
    try {
      audio = audio || new AudioContext();
      if (audio.state === 'suspended') audio.resume().catch(() => {});
      const t0 = audio.currentTime;
      [0, 0.75].forEach((rep) => [880, 1174.7, 1568].forEach((f, i) => {
        const t = t0 + rep + i * 0.16;
        const o = audio.createOscillator();
        const g = audio.createGain();
        o.type = 'sine';
        o.frequency.value = f;
        g.gain.setValueAtTime(0, t);
        g.gain.linearRampToValueAtTime(peak, t + 0.02);
        g.gain.exponentialRampToValueAtTime(0.001, t + 0.9);
        o.connect(g).connect(audio.destination);
        o.start(t);
        o.stop(t + 1);
      }));
    } catch { /* без звука */ }
  }

  // ---------- музыка ----------
  let trackKey = '';
  let artSrc = null;
  let artTimer = null;
  let autoAccent = '#8fd6ff';

  function syncAccent() {
    const custom = st.settings.accent_color;
    island.style.setProperty('--accent', custom && custom !== 'auto' ? custom : autoAccent);
  }

  function tickProgress() {
    if (seeking) return;
    const m = st.media;
    const bar = $('bar');
    if (!m.active || !m.dur) { bar.classList.add('off'); return; }
    bar.classList.remove('off');
    let cur = m.pos;
    if (m.playing && m.updated) cur += (Date.now() - m.updated) / 1000;
    cur = Math.min(Math.max(cur, 0), m.dur);
    $('fill').style.transform = `scaleX(${(cur / m.dur).toFixed(4)})`;
    $('tCur').textContent = fmt(Math.floor(cur));
    $('tLeft').textContent = '-' + fmt(Math.floor(m.dur - cur));
  }

  // Обложка меняется переливом: старая гаснет, новая проявляется.
  function setArt(src) {
    if (src === artSrc) return;
    artSrc = src;
    const arts = document.querySelectorAll('.art');
    const apply = () => {
      arts.forEach((el) => {
        el.style.backgroundImage = src ? `url("${src}")` : '';
        el.classList.toggle('empty', !src);
        el.classList.remove('fading');
      });
    };
    clearTimeout(artTimer);
    if (reduceMotion || !curView) { apply(); }
    else {
      arts.forEach((el) => el.classList.add('fading'));
      artTimer = setTimeout(apply, 200);
    }
    if (src) accentFrom(src);
    else { autoAccent = '#8fd6ff'; syncAccent(); }
  }

  // Цвет эквалайзера берём из обложки: самый насыщенный оттенок, приведённый к яркому.
  function accentFrom(src) {
    const img = new Image();
    img.onload = () => {
      if (src !== artSrc) return;
      const c = document.createElement('canvas');
      c.width = c.height = 16;
      const ctx = c.getContext('2d');
      ctx.drawImage(img, 0, 0, 16, 16);
      const d = ctx.getImageData(0, 0, 16, 16).data;
      let r = 0, g = 0, b = 0, n = 0;
      for (let i = 0; i < d.length; i += 4) {
        const mx = Math.max(d[i], d[i + 1], d[i + 2]);
        const s = mx - Math.min(d[i], d[i + 1], d[i + 2]);
        if (s > 30 && mx > 50) { r += d[i] * s; g += d[i + 1] * s; b += d[i + 2] * s; n += s; }
      }
      if (!n) { autoAccent = '#f5f5f7'; syncAccent(); return; }
      const [h, sat] = rgbToHs(r / n, g / n, b / n);
      autoAccent = `hsl(${h} ${Math.max(sat, 50)}% 66%)`;
      syncAccent();
    };
    img.src = src;
  }
  function rgbToHs(r, g, b) {
    r /= 255; g /= 255; b /= 255;
    const mx = Math.max(r, g, b), mn = Math.min(r, g, b), d = mx - mn;
    const l = (mx + mn) / 2;
    if (!d) return [0, 0];
    const s = d / (1 - Math.abs(2 * l - 1));
    let h = mx === r ? ((g - b) / d) % 6 : mx === g ? (b - r) / d + 2 : (r - g) / d + 4;
    h = Math.round(h * 60);
    if (h < 0) h += 360;
    return [h, Math.round(s * 100)];
  }

  function applyMedia(m, initial) {
    m = m || { active: false };
    const prevKey = trackKey;
    const wasActive = st.media.active;
    trackKey = m.active ? JSON.stringify([m.source || m.app, m.title, m.artist]) : '';
    if (trackKey !== prevKey) st.mediaHidden = false;
    st.media = m;
    if (!m.active) { st.pausedSince = 0; render(); return; }

    document.querySelectorAll('[data-bind]').forEach((el) => {
      const v = m[el.dataset.bind] || (el.dataset.bind === 'artist' ? T('unknown_artist') : '');
      if (el.textContent !== v) el.textContent = v;
    });
    const srcEl = $('mediaSrc');
    const multi = (m.sessions || 0) > 1;
    const srcLabel = multi ? `${m.app || ''} · ${(m.sessionIndex || 0) + 1}/${m.sessions} ▾` : (m.app || '');
    if (srcEl.textContent !== srcLabel) srcEl.textContent = srcLabel;
    srcEl.classList.toggle('multi', multi);
    srcEl.title = multi ? T('switch_source') : '';
    if (m.playing) st.pausedSince = 0;
    else if (!st.pausedSince) st.pausedSince = Date.now();

    if (!initial && wasActive && trackKey !== prevKey && m.playing && !st.hover) setHud('peek', 2800);
    else render();
  }

  function media(action) {
    if (action === 'toggle') {
      // Фиксируем текущую позицию до смены флага, чтобы полоса прогресса не прыгала назад на паузе.
      if (st.media.playing) {
        let cur = st.media.pos || 0;
        if (st.media.updated) cur += (Date.now() - st.media.updated) / 1000;
        if (st.media.dur) cur = Math.min(Math.max(cur, 0), st.media.dur);
        st.media.pos = cur;
      }
      st.media.playing = !st.media.playing;
      st.media.updated = Date.now();
      island.classList.toggle('playing', st.media.playing && !st.mediaHidden);
      if (curView === 'music' || nextView === 'music') tickProgress();
    }
    if (action === 'next') kick(1);
    if (action === 'prev') kick(-1);
    invoke('media_action', { action }).catch(() => {});
  }

  // ---------- клики и жесты ----------
  island.addEventListener('click', (e) => {
    if (!st.hover && !st.pinned && island.dataset.mode !== 'done' && (st.settings.expand_mode === 'click')) {
      st.hover = true;
      render();
      return;
    }
    const t = e.target.closest('[data-t]');
    if (t) { startTimer(+t.dataset.t, t.dataset.kind); return; }
    const b = e.target.closest('[data-a]');
    if (!b) return;
    const a = b.dataset.a;
    if (a === 'tpause') pauseTimer();
    else if (a === 'tstop') stopTimer();
    else if (a === 'tplus') addMinute();
    else if (a === 'tcustom-open') { customOpen = true; live(); }
    else if (a === 'tcustom-close') { customOpen = false; live(); }
    else if (a === 'tcustom-dec') setCustomMin(customMin - (e.shiftKey ? 5 : 1));
    else if (a === 'tcustom-inc') setCustomMin(customMin + (e.shiftKey ? 5 : 1));
    else if (a === 'tcustom-start') startTimer(customMin, 'plain');
    else if (a === 'break') {
      clearHud();
      const next = b.dataset.nextKind === 'work' ? [pomoWorkMin(), 'work'] : [pomoBreakMin(), 'break'];
      startTimer(next[0], next[1]);
    }
    else if (a === 'dismiss') clearHud();
    else if (a === 'settings') invoke('open_settings').catch(() => {});
    else if (a === 'privacy-settings') invoke('open_privacy_settings').catch(() => {});
    else if (a === 'sound-settings') invoke('open_sound_settings').catch(() => {});
    else if (a === 'open-audio') openAudioPanel();
    else if (a === 'open-shelf') { st.subview = 'shelf'; render(); }
    else if (a === 'open-clips') { st.subview = 'clips'; render(); }
    else if (a === 'panel-back') { st.subview = null; render(); }
    else if (a === 'mute-toggle') invoke('volume_mute_toggle').catch(() => {});
    else if (a === 'toggle-dnd') {
      st.dnd = !st.dnd;
      st.settings.dnd = st.dnd;
      $('dndBtn').classList.toggle('on', st.dnd);
      invoke('set_setting', { key: 'dnd', value: st.dnd }).catch(() => {});
    }
    else if (a === 'open-update') {
      if (st.update && st.update.url) invoke('open_release_url', { url: st.update.url }).catch(() => {});
    }
    else if (a === 'set-audio-dev') {
      invoke('set_audio_device', { id: b.dataset.id }).then(() => { st.subview = null; render(); }).catch(() => {});
    }
    else if (a === 'shelf-paste') invoke('shelf_from_clipboard').catch(() => {});
    else if (a === 'shelf-clear') invoke('shelf_clear').catch(() => {});
    else if (a === 'shelf-remove') invoke('shelf_remove', { path: b.dataset.path }).catch(() => {});
    else if (a === 'clips-clear') invoke('clips_clear').catch(() => {});
    else if (a === 'clip-pick') {
      const idx = +b.dataset.idx;
      const item = st.clips[idx];
      if (item) {
        const payload = typeof item === 'string' ? { text: item } : { id: item.id, text: item.kind === 'text' ? item.text : null };
        invoke('clip_copy', payload).then((ok) => {
          if (ok === false) return;
          const chk = b.querySelector('.adev-chk');
          if (chk) { chk.textContent = '✓'; setTimeout(() => { chk.textContent = ''; }, 900); }
        }).catch(() => {});
      }
    }
    else if (a === 'cycle-media') invoke('media_cycle').catch(() => {});
    else if (a === 'go-home') {
      st.preferHome = true;
      st.subview = null;
      render();
    }
    else if (a === 'hide-media') {
      st.mediaHidden = true;
      render();
    }
    else if (a === 'show-media' || a === 'go-music') {
      st.preferHome = false;
      st.mediaHidden = false;
      render();
    }
    else media(a);
  });
  $('tCustom').addEventListener('wheel', (e) => {
    e.stopPropagation();
    setCustomMin(customMin + (e.deltaY < 0 ? 1 : -1));
  }, { passive: true });
  // двойной клик по обложке или названию: пауза; средняя кнопка по острову: Mute
  $('mTop').addEventListener('dblclick', (e) => {
    if (!e.target.closest('button')) media('toggle');
  });
  island.addEventListener('auxclick', (e) => {
    if (e.button === 1) {
      e.preventDefault();
      invoke('volume_mute_toggle').catch(() => {});
    }
  });

  // Колёсико — громкость (кроме прокручиваемых списков). Горизонтальный свайп — переключение трека.
  let swipeAcc = 0;
  let swipeLock = 0;
  island.addEventListener('wheel', (e) => {
    const scrollBox = e.target.closest('.audio-list, .shelf-grid');
    if (scrollBox && scrollBox.scrollHeight > scrollBox.clientHeight) {
      return;
    }
    if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      if (!st.media.active || st.mediaHidden || Date.now() < swipeLock) return;
      swipeAcc += e.deltaX;
      if (Math.abs(swipeAcc) > 70) {
        media(swipeAcc > 0 ? 'next' : 'prev');
        swipeAcc = 0;
        swipeLock = Date.now() + 700;
      }
      return;
    }
    wheelUntil = Date.now() + 1200;
    invoke('volume_step', { delta: e.deltaY < 0 ? 0.02 : -0.02 }).catch(() => {});
  }, { passive: true });

  // ---------- громкость ----------
  let volDragging = false;
  let volSendAt = 0;
  const aVolHit = $('aVolHit');
  function paintAudioVol(level, muted) {
    const muteBtn = $('muteBtn');
    if (muteBtn) muteBtn.classList.toggle('muted', !!muted || level === 0);
    const fill = $('aVolFill');
    if (fill) fill.style.transform = `scaleX(${muted ? 0 : Math.min(Math.max(level / 100, 0), 1)})`;
    const pct = $('aVolPct');
    if (pct) pct.textContent = (muted ? 0 : level) + '%';
  }
  function applyVolume(v) {
    st.volume = v;
    const view = island.querySelector('.v-volume');
    view.classList.toggle('muted', !!v.muted || v.level === 0);
    view.classList.toggle('lowvol', v.level < 40);
    $('vFill').style.transform = `scaleX(${v.muted ? 0 : v.level / 100})`;
    $('vPct').textContent = v.muted ? '0' : v.level;
    if (!volDragging) paintAudioVol(v.level, v.muted);
  }
  if (aVolHit) {
    const fracOf = (e) => {
      const r = aVolHit.getBoundingClientRect();
      return Math.min(Math.max((e.clientX - r.left) / (r.width || 1), 0), 1);
    };
    const pushVol = (f, force) => {
      const lvl = Math.round(f * 100);
      paintAudioVol(lvl, lvl === 0);
      const now = performance.now();
      if (force || now - volSendAt > 35) {
        volSendAt = now;
        invoke('volume_set', { level: f }).catch(() => {});
      }
    };
    aVolHit.addEventListener('pointerdown', (e) => {
      volDragging = true;
      aVolHit.classList.add('drag');
      aVolHit.setPointerCapture(e.pointerId);
      pushVol(fracOf(e), true);
    });
    aVolHit.addEventListener('pointermove', (e) => {
      if (volDragging) pushVol(fracOf(e), false);
    });
    aVolHit.addEventListener('pointerup', (e) => {
      if (!volDragging) return;
      volDragging = false;
      aVolHit.classList.remove('drag');
      pushVol(fracOf(e), true);
    });
  }

  // ---------- батарея ----------
  function applyPower(p, initial) {
    const prev = st.power;
    st.power = p;
    const batt = $('clkBatt');
    batt.hidden = !p.battery;
    batt.classList.toggle('charging', p.ac);
    batt.classList.toggle('low', !p.ac && p.percent <= 20);
    $('clkPct').textContent = p.percent + '%';
    const fillW = Math.max(1.5, 21.5 * p.percent / 100);
    document.querySelectorAll('.bfill').forEach((el) => el.setAttribute('width', fillW));
    $('chPct').textContent = p.percent + '%';
    $('lowPct').textContent = p.percent + '%';
    let tip = p.ac ? T('batt_charging', { p: p.percent }) : (p.percent + '%');
    if (!p.ac && typeof p.secsLeft === 'number' && p.secsLeft > 60) {
      const hm = p.secsLeft >= 3600
        ? `${Math.floor(p.secsLeft / 3600)}h ${Math.round((p.secsLeft % 3600) / 60)}m`
        : `${Math.round(p.secsLeft / 60)}m`;
      tip = T('batt_left', { p: p.percent, t: hm });
    }
    if (p.saver) tip += ` · ${T('batt_saver')}`;
    batt.title = tip;

    if (initial || !prev || !p.battery) { render(); return; }
    if (!prev.ac && p.ac) { $('chLbl').textContent = T('charging'); setHud('charge', 2800); }
    else if (p.ac && p.percent >= 100 && prev.percent < 100) { $('chLbl').textContent = T('charged'); setHud('charge', 3200); }
    else if (!p.ac && ((prev.percent > 20 && p.percent <= 20) || (prev.percent > 10 && p.percent <= 10))) setHud('low', 4500);
    else render();
  }

  // ---------- микрофон и камера ----------
  function applyPrivacy(p) {
    st.privacy = p || { mic: [], cam: [] };
    const { mic, cam } = st.privacy;
    document.body.classList.toggle('mic-on', mic.length > 0);
    document.body.classList.toggle('cam-on', cam.length > 0);
    const parts = [];
    if (mic.length) parts.push(`${T('mic')}: ${mic.join(', ')}`);
    if (cam.length) parts.push(`${T('cam')}: ${cam.join(', ')}`);
    const line = $('privLine');
    line.hidden = !parts.length;
    line.textContent = parts.join(' · ');
    render();
  }

  // ---------- буфер обмена ----------
  function applyClipboard(c) {
    let text = '';
    if (c.kind === 'text') text = c.text;
    else if (c.kind === 'image') text = T('image');
    else if (c.kind === 'files') text = `${c.count} ${I18N.plural(L(), c.count, 'files_plural')}`;
    else return;
    $('clipText').textContent = text;
    if (st.hover || st.pinned) return;
    setHud('clip', 1800);
  }

  function applyLocale() {
    document.documentElement.lang = L();
    document.querySelectorAll('[data-i18n]').forEach((el) => { el.textContent = T(el.dataset.i18n); });
    document.querySelectorAll('[data-i18n-min]').forEach((el) => { el.textContent = T('min_short', { n: el.dataset.i18nMin }); });
    $('tCustomVal').textContent = T('min_short', { n: customMin });
    const pomoBtn = $('pomoPresetBtn');
    if (pomoBtn) {
      const wm = pomoWorkMin();
      pomoBtn.dataset.t = String(wm);
      pomoBtn.textContent = T('pomodoro_n', { n: wm });
    }
    $('tCustomBtn').title = $('tCustomBtn').ariaLabel = T('custom_timer');
    $('tCustomVal').title = T('custom_hint');
    $('privLine').title = T('privacy_tip');
    $('mediaRestore').title = $('mediaRestore').ariaLabel = T('restore_player');
    const hubBtn = $('musicHubBtn');
    if (hubBtn) hubBtn.title = hubBtn.ariaLabel = T('hide_player');
    $('soundSettingsBtn').title = $('soundSettingsBtn').ariaLabel = T('sound_settings');
    $('muteBtn').title = $('muteBtn').ariaLabel = T('mute_toggle');
    $('shelfBtn').title = $('shelfBtn').ariaLabel = T('shelf_open');
    $('clipsBtn').title = $('clipsBtn').ariaLabel = T('clips_open');
    $('dndBtn').title = $('dndBtn').ariaLabel = T('dnd_tip');
    $('gearBtn').title = $('gearBtn').ariaLabel = T('settings');
    if (st.update && st.update.available) $('updBadge').title = T('upd_badge', { v: st.update.version });
    $('vClipLbl') && ($('vClipLbl').textContent = T('copied'));
    const clipLbl = island.querySelector('.v-clip .lbl');
    if (clipLbl) clipLbl.textContent = T('copied');
    const lowLbl = island.querySelector('.v-low .lbl');
    if (lowLbl) lowLbl.textContent = T('low_battery');
    if (st.privacy) {
      const { mic, cam } = st.privacy;
      const parts = [];
      if (mic.length) parts.push(`${T('mic')}: ${mic.join(', ')}`);
      if (cam.length) parts.push(`${T('cam')}: ${cam.join(', ')}`);
      $('privLine').textContent = parts.join(' · ');
    }
    if (st.device) applyDevice(st.device, true);
    if (st.media && st.media.active) applyMedia(st.media, true);
  }

  function applySettings(v) {
    st.settings = v || st.settings;
    reduceMotion = st.settings.animations === false;
    if (st.settings.dwell_ms) DWELL.busy = st.settings.dwell_ms;
    if (typeof st.settings.dnd === 'boolean') {
      st.dnd = st.settings.dnd;
      $('dndBtn').classList.toggle('on', st.dnd);
    }
    const [sk, sc] = SPRING_PRESETS[st.settings.spring_preset] || SPRING_PRESETS.normal;
    for (const key of ['w', 'h', 'r', 'rt', 'top']) {
      S[key].k = sk;
      S[key].c = sc;
    }
    island.dataset.border = st.settings.border_style || 'subtle';
    island.dataset.backdrop = st.settings.backdrop || 'oled';
    syncAccent();
    $('sysRow').hidden = st.settings.sysload === false;
    if (st.device) $('devChip').hidden = st.settings.devices === false;
    $('clipsBtn').hidden = st.settings.clipboard === false || st.clips.length === 0;
    lastRect = '';
    applyLocale();
    paint();
    render();
    document.body.classList.toggle('no-motion', reduceMotion);
  }

  // ---------- наушники и колонки ----------
  const DEV_ICON = {
    headphones: '<svg viewBox="0 0 24 24"><path d="M4 15v-3a8 8 0 0 1 16 0v3" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/><rect x="3" y="14" width="4.5" height="7" rx="2" fill="currentColor"/><rect x="16.5" y="14" width="4.5" height="7" rx="2" fill="currentColor"/></svg>',
    headset: '<svg viewBox="0 0 24 24"><path d="M4 14v-2a8 8 0 0 1 16 0v2" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/><rect x="3" y="13" width="4.5" height="6.5" rx="2" fill="currentColor"/><rect x="16.5" y="13" width="4.5" height="6.5" rx="2" fill="currentColor"/><path d="M19 19.5c0 1.5-2 2.5-5 2.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>',
    display: '<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="12" rx="2" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M8 20h8M12 16v4" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>',
    speakers: '<svg viewBox="0 0 24 24"><rect x="6" y="3" width="12" height="18" rx="2.5" fill="none" stroke="currentColor" stroke-width="1.8"/><circle cx="12" cy="14" r="3.2" fill="none" stroke="currentColor" stroke-width="1.8"/><circle cx="12" cy="7.5" r="1.2" fill="currentColor"/></svg>',
  };
  function applyDevice(d, initial) {
    if (!d || !d.name) return;
    st.device = d;
    const ico = DEV_ICON[d.kind] || DEV_ICON.speakers;
    const has = typeof d.battery === 'number';
    const chip = $('devChip');
    chip.hidden = st.settings.devices === false;
    $('devChipIco').innerHTML = ico;
    const chipPct = $('devChipPct');
    chipPct.hidden = !has;
    if (has) chipPct.textContent = d.battery + '%';
    chip.classList.toggle('low', has && d.battery <= 20);
    chip.title = has ? T('dev_tip_bat', { name: d.name, bat: d.battery }) : T('dev_tip', { name: d.name });

    if (initial || d.silent) { render(); return; }
    $('devIco').innerHTML = ico;
    $('devName').textContent = d.name;
    const plural = d.kind === 'headphones' || d.kind === 'speakers';
    $('devSub').textContent = plural ? T('connected_pl') : T('connected_sg');
    const box = $('devBatt');
    box.hidden = !has;
    if (has) {
      $('devPct').textContent = d.battery;
      box.classList.toggle('low', d.battery <= 20);
      const ring = box.querySelector('.dfg');
      ring.style.strokeDashoffset = '50.27';
      // кольцо заряда «наливается» уже на показанном экране
      requestAnimationFrame(() => requestAnimationFrame(() => { ring.style.strokeDashoffset = (50.27 * (1 - d.battery / 100)).toFixed(2); }));
    }
    if (!st.hover && !st.pinned) setHud('device', 3400);
  }

  async function openAudioPanel() {
    st.subview = 'audio';
    const list = await invoke('get_audio_devices').catch(() => []);
    const box = $('audioList');
    box.innerHTML = '';
    for (const dev of list) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'adev' + (dev.active ? ' active' : '');
      btn.dataset.a = 'set-audio-dev';
      btn.dataset.id = dev.id;
      btn.innerHTML = `${DEV_ICON[dev.kind] || DEV_ICON.speakers}<span class="adev-name"></span>${dev.active ? '<span class="adev-chk">✓</span>' : ''}`;
      btn.querySelector('.adev-name').textContent = dev.name;
      box.appendChild(btn);
    }
    render();
  }

  // ---------- полка файлов (drag & drop) ----------
  function applyShelf(items) {
    st.shelf = Array.isArray(items) ? items : [];
    const btn = $('shelfBtn');
    btn.hidden = st.shelf.length === 0;
    $('shelfCount').textContent = String(st.shelf.length);
    $('shelfClearBtn').hidden = st.shelf.length === 0;
    const grid = $('shelfGrid');
    grid.innerHTML = '';
    for (const item of st.shelf) {
      const chip = document.createElement('div');
      chip.className = 'shelf-item';
      chip.title = T('shelf_item_tip', { path: item.path });
      const ico = document.createElement('span');
      ico.textContent = item.is_dir ? '📁' : '📄';
      const name = document.createElement('span');
      name.className = 'shelf-name';
      name.textContent = item.name;
      const rm = document.createElement('button');
      rm.type = 'button';
      rm.className = 'shelf-x';
      rm.dataset.a = 'shelf-remove';
      rm.dataset.path = item.path;
      rm.textContent = '×';
      chip.append(ico, name, rm);
      let clickTimer = null;
      chip.addEventListener('pointerdown', (e) => {
        if (e.button !== 0 || e.target.closest('.shelf-x')) return;
        const x0 = e.clientX, y0 = e.clientY;
        let dragged = false;
        const onMove = (ev) => {
          if (Math.hypot(ev.clientX - x0, ev.clientY - y0) > 6) {
            dragged = true;
            clearTimeout(clickTimer);
            clickTimer = null;
            cleanup();
            invoke('shelf_drag', { path: item.path }).catch(() => {});
          }
        };
        const onUp = (ev) => {
          cleanup();
          if (dragged) return;
          if (ev.detail >= 2) {
            clearTimeout(clickTimer);
            clickTimer = null;
            invoke('shelf_open', { path: item.path }).catch(() => {});
          } else {
            clearTimeout(clickTimer);
            clickTimer = setTimeout(() => {
              clickTimer = null;
              invoke('shelf_reveal', { path: item.path }).catch(() => {});
            }, 210);
          }
        };
        const cleanup = () => {
          window.removeEventListener('pointermove', onMove);
          window.removeEventListener('pointerup', onUp);
        };
        window.addEventListener('pointermove', onMove);
        window.addEventListener('pointerup', onUp);
      });
      grid.appendChild(chip);
    }
    render();
  }

  // ---------- оперативная история буфера обмена ----------
  function applyClips(items) {
    st.clips = Array.isArray(items) ? items : [];
    $('clipsBtn').hidden = st.settings.clipboard === false || st.clips.length === 0;
    $('clipsCount').textContent = String(st.clips.length);
    $('clipsClearBtn').hidden = st.clips.length === 0;
    $('clipsEmpty').hidden = st.clips.length > 0;
    const list = $('clipsList');
    list.innerHTML = '';
    st.clips.forEach((raw, i) => {
      const item = typeof raw === 'string' ? { id: i + 1, kind: 'text', text: raw } : raw;
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'adev';
      btn.dataset.a = 'clip-pick';
      btn.dataset.idx = String(i);
      if (item.kind === 'image') {
        btn.title = T('clip_img_tip', { dim: item.text || 'PNG' });
        if (item.preview) {
          const img = document.createElement('img');
          img.className = 'clip-thumb';
          img.src = item.preview;
          img.alt = '';
          btn.appendChild(img);
        }
        const name = document.createElement('span');
        name.className = 'adev-name';
        name.textContent = `${T('image')} · ${item.text || 'PNG'}`;
        const chk = document.createElement('span');
        chk.className = 'adev-chk';
        btn.append(name, chk);
        btn.addEventListener('pointerdown', (e) => {
          if (e.button !== 0) return;
          const x0 = e.clientX, y0 = e.clientY;
          const onMove = (ev) => {
            if (Math.hypot(ev.clientX - x0, ev.clientY - y0) > 6) {
              cleanup();
              invoke('clip_drag', { id: item.id }).catch(() => {});
            }
          };
          const cleanup = () => {
            window.removeEventListener('pointermove', onMove);
            window.removeEventListener('pointerup', cleanup);
          };
          window.addEventListener('pointermove', onMove);
          window.addEventListener('pointerup', cleanup);
        });
      } else {
        const txt = item.text || '';
        btn.title = txt.slice(0, 300);
        btn.innerHTML = '<span class="adev-name"></span><span class="adev-chk"></span>';
        btn.querySelector('.adev-name').textContent = txt.replace(/\s+/g, ' ').trim();
      }
      list.appendChild(btn);
    });
    render();
  }

  function applyUpdate(info) {
    st.update = info || null;
    const badge = $('updBadge');
    const show = !!(info && info.available);
    badge.hidden = !show;
    if (show) badge.title = T('upd_badge', { v: info.version });
    render();
  }

  // ---------- загрузка системы и помодоро ----------
  function applySys(v) {
    st.sys = v;
    for (const [k, el, val] of [['cpu', 'cpuFill', 'cpuVal'], ['mem', 'memFill', 'memVal']]) {
      const n = v[k] || 0;
      const fill = $(el);
      fill.style.transform = `scaleX(${(n / 100).toFixed(3)})`;
      fill.classList.toggle('hot', n >= 85);
      $(val).textContent = n + '%';
    }
    if (v && v.totalGb) {
      const memMeter = $('memFill').closest('.meter');
      if (memMeter) memMeter.title = T('mem_detail', { u: v.usedGb, t: v.totalGb });
    }
  }

  // ---------- перемотка по полоске прогресса ----------
  const seekEl = $('seek');
  let seeking = false;
  function seekFraction(e) {
    const r = seekEl.getBoundingClientRect();
    return Math.min(Math.max((e.clientX - r.left) / r.width, 0), 1);
  }
  function previewSeek(f) {
    const m = st.media;
    $('fill').style.transform = `scaleX(${f.toFixed(4)})`;
    $('tCur').textContent = fmt(Math.floor(f * m.dur));
    $('tLeft').textContent = '-' + fmt(Math.floor(m.dur - f * m.dur));
  }
  seekEl.addEventListener('pointerdown', (e) => {
    if (!st.media.active || !st.media.dur) return;
    seeking = true;
    seekEl.classList.add('drag');
    seekEl.setPointerCapture(e.pointerId);
    previewSeek(seekFraction(e));
  });
  seekEl.addEventListener('pointermove', (e) => { if (seeking) previewSeek(seekFraction(e)); });
  seekEl.addEventListener('pointerup', (e) => {
    if (!seeking) return;
    seeking = false;
    seekEl.classList.remove('drag');
    const sec = seekFraction(e) * st.media.dur;
    // сразу показываем новое место, не дожидаясь плеера
    st.media.pos = sec;
    st.media.updated = Date.now();
    invoke('media_seek', { seconds: sec }).catch(() => {});
  });

  // ---------- раскладка и Caps Lock ----------
  function applyLayout(l) {
    $('langShort').textContent = l.short;
    $('langLong').textContent = l.long;
    if (!st.hover) setHud('layout', 1300);
  }
  function applyCaps(on) {
    const v = island.querySelector('.v-caps');
    v.classList.toggle('lit', !!on);
    $('capsState').textContent = on ? T('caps_on') : T('caps_off');
    if (!st.hover && !st.pinned) setHud('caps', 1400);
  }

  // ---------- события от Rust ----------
  function onHover(on) {
    clearTimeout(hoverTimer);
    if (on) {
      if (st.hover || st.pinned) { st.hover = true; return; }
      if ((st.settings.expand_mode || 'hover') !== 'hover') return;
      st.pending = true;
      render();
      hoverTimer = setTimeout(() => { st.pending = false; st.hover = true; render(); }, st.topbusy ? DWELL.busy : DWELL.normal);
    } else {
      if (st.pending) { st.pending = false; render(); }
      if (st.hover) {
        hoverTimer = setTimeout(() => {
          st.hover = false;
          if (!st.pinned) {
            st.subview = null;
            st.preferHome = false;
          }
          render();
        }, 220);
      }
    }
  }
  function onHotkey(kind) {
    if (kind === 'toggle') {
      if (st.pinned) {
        st.pinned = false;
        st.hover = false;
        st.subview = null;
        st.preferHome = false;
      } else {
        st.pinned = true;
      }
      render();
    } else if (kind === 'timer') {
      if (timer.running()) pauseTimer();
      else startTimer(pomoWorkMin(), 'work');
    }
  }
  listen('hover', (e) => onHover(!!e.payload));
  listen('hotkey', (e) => onHotkey(e.payload));
  listen('drag-hover', (e) => { st.dragHover = !!e.payload; if (st.dragHover) st.subview = 'shelf'; render(); });
  listen('shelf', (e) => applyShelf(e.payload));
  listen('clips', (e) => applyClips(e.payload));
  listen('update', (e) => applyUpdate(e.payload));
  listen('topbusy', (e) => { st.topbusy = !!e.payload; render(); });
  listen('fullscreen', (e) => { st.full = !!e.payload; render(); });
  listen('media', (e) => applyMedia(e.payload, false));
  listen('art', (e) => setArt(e.payload || null));
  listen('power', (e) => applyPower(e.payload, false));
  listen('privacy', (e) => applyPrivacy(e.payload));
  listen('clipboard', (e) => applyClipboard(e.payload));
  listen('settings', (e) => applySettings(e.payload));
  listen('device', (e) => applyDevice(e.payload, false));
  listen('layout', (e) => applyLayout(e.payload));
  listen('caps', (e) => applyCaps(e.payload));
  listen('screen', () => { lastRect = ''; render(); });
  listen('sys', (e) => applySys(e.payload));
  listen('stats', (e) => { st.pomodoros = (e.payload && e.payload.pomodoros) || 0; live(); });
  listen('volume', (e) => {
    applyVolume(e.payload);
    if (!e.payload.silent) setHud('volume', 1600, Date.now() < wheelUntil);
  });

  // ---------- демо-режим (отладка): проигрывает переходы и меряет кадры ----------
  function finishPerf() {
    if (!perf) return;
    const p = perf;
    perf = null;
    if (p.n < 2) return;
    invoke('perf', {
      label: `${p.label} -> ${island.dataset.mode}`,
      frames: p.n,
      ms: Math.round(performance.now() - p.t0),
      avg: +(p.sum / p.n).toFixed(1),
      max: +p.max.toFixed(1),
    }).catch(() => {});
  }
  async function demo() {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    // сколько кадров в секунду окно получает вообще
    let n = 0;
    const t0 = performance.now();
    await new Promise((res) => { (function f() { n++; if (performance.now() - t0 < 1000) requestAnimationFrame(f); else res(); })(); });
    invoke('perf', { label: `raf-rate (animations=${!reduceMotion})`, frames: n, ms: 1000, avg: +(1000 / n).toFixed(1), max: 0 }).catch(() => {});
    const steps = [
      () => onHover(true), () => onHover(false),
      () => applyMedia({ active: true, playing: true, title: 'Демо', artist: 'Проверка плавности', app: 'Demo', pos: 30, dur: 200, updated: Date.now() }, false),
      () => onHover(true), () => onHover(false),
      () => setHud('volume', 1400), () => setHud('clip', 1400),
      () => { st.topbusy = true; render(); }, () => { st.topbusy = false; render(); },
      () => applyMedia({ active: false }, false),
    ];
    for (const s of steps) { s(); await wait(1700); }
    invoke('perf', { label: 'demo-done', frames: 0, ms: 0, avg: 0, max: 0 }).catch(() => {});
  }

  // ---------- старт ----------
  restoreTimer();
  window.addEventListener('resize', () => { lastRect = ''; render(); });
  paint();
  render();
  invoke('snapshot').then((s) => {
    if (s.settings) applySettings(s.settings);
    if (s.power) applyPower(s.power, true);
    if (s.volume) applyVolume(s.volume);
    if (s.privacy) applyPrivacy(s.privacy);
    if (s.device) applyDevice(s.device, true);
    if (s.shelf) applyShelf(s.shelf);
    if (s.clips) applyClips(s.clips);
    if (s.update) applyUpdate(s.update);
    if (s.sys) applySys(s.sys);
    if (s.stats) st.pomodoros = s.stats.pomodoros || 0;
    if (s.art !== undefined) setArt(s.art || null);
    if (s.media) applyMedia(s.media, true);
    render();
    if (s.demo) { st.demo = true; demo(); }
  }).catch(() => {});
})();

// Логика острова: выбирает, что показать, плавно меняет размер и сообщает Rust-части,
// где остров на экране и можно ли сейчас ловить мышь.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const $ = (id) => document.getElementById(id);
  const island = $('island');
  const pdots = $('pdots');
  const views = [...document.querySelectorAll('.view')];
  const $$home = document.querySelector('.v-home');
  const mediaRestore = $('mediaRestore');
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
    home: [392, 116, 30],
    music: [392, 166, 38],
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
  const TOP = 6;
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
    power: null,
    volume: null,
    hover: false,
    pinned: false,
    pending: false,
    hud: null,
    device: null,
    full: false,
    topbusy: false,
    privacy: { mic: [], cam: [] },
    settings: { sounds: true },
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
  const spring = (v, k = 280, c = 28) => ({ v, t: v, vel: 0, k, c, from: v });
  const S = {
    w: spring(124),
    h: spring(30),
    r: spring(15),
    rt: spring(15),
    top: spring(TOP),
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
    const s = island.style;
    s.width = w.toFixed(2) + 'px';
    s.height = h.toFixed(2) + 'px';
    s.top = S.top.v.toFixed(2) + 'px';
    s.borderRadius = `${rt.toFixed(2)}px ${rt.toFixed(2)}px ${r.toFixed(2)}px ${r.toFixed(2)}px`;
    s.transform = `translateX(calc(-50% + ${S.x.v.toFixed(2)}px))`;
    const dotY = Math.max(4, S.top.v + h / 2 - 3);
    pdots.style.transform = `translate(${(w / 2 + 8 + S.x.v).toFixed(2)}px, ${dotY.toFixed(2)}px)`;
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

  // Домашний экран измеряет свою естественную высоту по содержимому.
  function homeHeight() {
    $$home.style.height = 'auto';
    return Math.ceil($$home.offsetHeight);
  }
  function sizeOf(m) {
    if (m === 'home') return [392, homeHeight(), 30];
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
  setInterval(() => { if (!st.hover && !st.hud && mode() !== island.dataset.mode) render(); }, 5000);

  function mode() {
    if (st.full) return 'hidden';
    const expanded = st.hover || st.pinned;
    if (st.hud && (st.hud.force || !expanded)) return st.hud.type;
    if (expanded) return st.media.active && !st.mediaHidden ? 'music' : 'home';
    const c = compact();
    if (c === 'sliver' && st.pending) return 'sliverHot';
    return c;
  }

  function render() {
    const m = mode();
    const v = VIEW[m] || m;
    island.dataset.mode = m;
    island.classList.toggle('playing', !!st.media.playing && !st.mediaHidden);
    mediaRestore.hidden = !st.mediaHidden;
    const [w, h, r] = sizeOf(m);
    if (m === 'home') {
      $$home.style.width = w + 'px';
      $$home.style.height = h + 'px';
    }
    const sliver = v === 'sliver';
    setTargets(w, h, r, sliver ? 0 : r, sliver ? 0 : TOP);
    requestView(v);

    // Мышь остров ловит, когда раскрыт, ждёт ответа («таймер закончился») или
    // включена опция колёсика над свёрнутым островом при свободном верхе экрана.
    const interactive = st.hover || st.pinned || m === 'done';
    const wheelOnly = !interactive && !!st.settings.wheel_collapsed && !st.topbusy && (m === 'idle' || m === 'mini' || m === 'tmini');
    if (m === 'hidden') sendRect(0, 0, 0, 0, false);
    else if (sliver) sendRect((innerWidth - w) / 2 - 12, 0, w + 24, 14, false);
    else sendRect((innerWidth - w) / 2 - 4, 0, w + 8, h + TOP + 4, interactive || wheelOnly);
    live();
  }

  function sendRect(x, y, w, h, interactive) {
    const key = [x, y, w, h].map(Math.round).join(',') + (interactive ? '+' : '-');
    if (key === lastRect) return;
    lastRect = key;
    invoke('set_hit_rect', { x, y, w, h, interactive }).catch(() => {});
  }

  const HUD_PRIO = { done: 100, low: 80, device: 60, charge: 60, peek: 40, volume: 30, clip: 20, layout: 10, caps: 10 };
  function setHud(type, ms, force) {
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
    setLive('time', `${pad(d.getHours())}:${pad(d.getMinutes())}`);
    const date = d.toLocaleDateString('ru-RU', { weekday: 'short', day: 'numeric', month: 'long' });
    setLive('date', st.pomodoros ? `${date} · ${st.pomodoros} ${plural(st.pomodoros, 'помодоро', 'помодоро', 'помодоро')}` : date);

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
      setLive('tlabel', timer.paused ? 'на паузе' : timer.label);
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
    label: '',
    kind: '',
    running() { return this.total > 0; },
    left() { return this.paused ? this.rest : Math.max(0, (this.end - Date.now()) / 1000); },
  };
  function startTimer(min, kind) {
    customOpen = false;
    timer.total = min * 60;
    timer.end = Date.now() + timer.total * 1000;
    timer.paused = false;
    timer.kind = kind || 'plain';
    timer.label = kind === 'work' ? 'помодоро · работа' : kind === 'break' ? 'перерыв' : `таймер на ${min} мин`;
    render();
  }
  function setCustomMin(next) {
    customMin = Math.min(180, Math.max(1, next));
    $('tCustomVal').textContent = `${customMin} мин`;
  }
  function pauseTimer() {
    if (timer.paused) { timer.end = Date.now() + timer.rest * 1000; timer.paused = false; }
    else { timer.rest = timer.left(); timer.paused = true; }
    render();
  }
  function addMinute() {
    if (!timer.running()) return;
    timer.total += 60;
    if (timer.paused) timer.rest += 60;
    else timer.end += 60000;
    live();
  }
  function stopTimer() { timer.total = 0; render(); }
  function finishTimer() {
    const kind = timer.kind;
    const min = Math.round(timer.total / 60);
    timer.total = 0;
    $('doneTitle').textContent = kind === 'work' ? 'Помодоро завершён' : kind === 'break' ? 'Перерыв окончен' : 'Время вышло';
    $('doneSub').textContent = kind === 'work' ? 'Пора отдохнуть' : kind === 'break' ? 'Можно возвращаться к делам' : `Прошло ${min} мин`;
    const btn = $('doneBreak');
    if (kind === 'work') {
      btn.hidden = false;
      btn.textContent = 'Перерыв 5 мин';
      btn.dataset.nextKind = 'break';
    } else if (kind === 'break') {
      btn.hidden = false;
      btn.textContent = 'Ещё помодоро';
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
    else island.style.setProperty('--accent', '#8fd6ff');
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
      if (!n) { island.style.setProperty('--accent', '#f5f5f7'); return; }
      const [h, sat] = rgbToHs(r / n, g / n, b / n);
      island.style.setProperty('--accent', `hsl(${h} ${Math.max(sat, 50)}% 66%)`);
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
      const v = m[el.dataset.bind] || (el.dataset.bind === 'artist' ? 'Неизвестный исполнитель' : '');
      if (el.textContent !== v) el.textContent = v;
    });
    const srcEl = $('mediaSrc');
    const multi = (m.sessions || 0) > 1;
    const srcLabel = multi ? `${m.app || ''} · ${(m.sessionIndex || 0) + 1}/${m.sessions} ▾` : (m.app || '');
    if (srcEl.textContent !== srcLabel) srcEl.textContent = srcLabel;
    srcEl.classList.toggle('multi', multi);
    srcEl.title = multi ? 'Нажмите, чтобы переключить источник' : '';
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
      const next = b.dataset.nextKind === 'work' ? [25, 'work'] : [5, 'break'];
      startTimer(next[0], next[1]);
    }
    else if (a === 'dismiss') clearHud();
    else if (a === 'settings') invoke('open_settings').catch(() => {});
    else if (a === 'privacy-settings') invoke('open_privacy_settings').catch(() => {});
    else if (a === 'sound-settings') invoke('open_sound_settings').catch(() => {});
    else if (a === 'cycle-media') invoke('media_cycle').catch(() => {});
    else if (a === 'hide-media' || a === 'show-media') {
      st.mediaHidden = a === 'hide-media';
      render();
    }
    else media(a);
  });
  $('tCustom').addEventListener('wheel', (e) => {
    e.stopPropagation();
    setCustomMin(customMin + (e.deltaY < 0 ? 1 : -1));
  }, { passive: true });
  // двойной клик по обложке или названию: пауза
  $('mTop').addEventListener('dblclick', (e) => {
    if (!e.target.closest('button')) media('toggle');
  });

  // Колёсико — громкость. Горизонтальный свайп двумя пальцами по тачпаду — переключение трека.
  let swipeAcc = 0;
  let swipeLock = 0;
  island.addEventListener('wheel', (e) => {
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
  function applyVolume(v) {
    st.volume = v;
    const view = island.querySelector('.v-volume');
    view.classList.toggle('muted', !!v.muted || v.level === 0);
    view.classList.toggle('lowvol', v.level < 40);
    $('vFill').style.transform = `scaleX(${v.muted ? 0 : v.level / 100})`;
    $('vPct').textContent = v.muted ? '0' : v.level;
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

    if (initial || !prev || !p.battery) { render(); return; }
    if (!prev.ac && p.ac) { $('chLbl').textContent = 'Зарядка'; setHud('charge', 2800); }
    else if (p.ac && p.percent >= 100 && prev.percent < 100) { $('chLbl').textContent = 'Заряжено'; setHud('charge', 3200); }
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
    if (mic.length) parts.push('Микрофон: ' + mic.join(', '));
    if (cam.length) parts.push('Камера: ' + cam.join(', '));
    const line = $('privLine');
    line.hidden = !parts.length;
    line.textContent = parts.join(' · ');
    render();
  }

  // ---------- буфер обмена ----------
  function plural(n, one, few, many) {
    const m10 = n % 10, m100 = n % 100;
    if (m10 === 1 && m100 !== 11) return one;
    if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return few;
    return many;
  }
  function applyClipboard(c) {
    let text = '';
    if (c.kind === 'text') text = c.text;
    else if (c.kind === 'image') text = 'изображение';
    else if (c.kind === 'files') text = `${c.count} ${plural(c.count, 'файл', 'файла', 'файлов')}`;
    else return;
    $('clipText').textContent = text;
    if (st.hover) return;
    setHud('clip', 1800);
  }

  function applySettings(v) {
    st.settings = v || st.settings;
    reduceMotion = st.settings.animations === false;
    if (st.settings.dwell_ms) DWELL.busy = st.settings.dwell_ms;
    $('sysRow').hidden = st.settings.sysload === false;
    if (st.device) $('devChip').hidden = st.settings.devices === false;
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
    chip.title = has ? `${d.name} · ${d.battery}% (параметры звука)` : `${d.name} (параметры звука)`;

    if (initial || d.silent) { render(); return; }
    $('devIco').innerHTML = ico;
    $('devName').textContent = d.name;
    const plural = d.kind === 'headphones' || d.kind === 'speakers';
    $('devSub').textContent = plural ? 'Подключены' : 'Подключено';
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
    $('capsState').textContent = on ? 'включён' : 'выключен';
    if (!st.hover) setHud('caps', 1400);
  }

  // ---------- события от Rust ----------
  function onHover(on) {
    clearTimeout(hoverTimer);
    if (on) {
      if (st.hover || st.pinned) { st.hover = true; return; }
      st.pending = true;
      render();
      hoverTimer = setTimeout(() => { st.pending = false; st.hover = true; render(); }, st.topbusy ? DWELL.busy : DWELL.normal);
    } else {
      if (st.pending) { st.pending = false; render(); }
      if (st.hover || st.pinned) hoverTimer = setTimeout(() => { st.hover = false; st.pinned = false; render(); }, 220);
    }
  }
  function onHotkey(kind) {
    if (kind === 'toggle') {
      if (st.hover || st.pinned) { st.hover = false; st.pinned = false; }
      else { st.pinned = true; }
      render();
    } else if (kind === 'timer') {
      if (timer.running()) pauseTimer();
      else startTimer(25, 'work');
    }
  }
  listen('hover', (e) => onHover(!!e.payload));
  listen('hotkey', (e) => onHotkey(e.payload));
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
  window.addEventListener('resize', () => { lastRect = ''; render(); });
  paint();
  render();
  invoke('snapshot').then((s) => {
    if (s.settings) applySettings(s.settings);
    if (s.power) applyPower(s.power, true);
    if (s.volume) applyVolume(s.volume);
    if (s.privacy) applyPrivacy(s.privacy);
    if (s.device) applyDevice(s.device, true);
    if (s.sys) applySys(s.sys);
    if (s.stats) st.pomodoros = s.stats.pomodoros || 0;
    if (s.art !== undefined) setArt(s.art || null);
    if (s.media) applyMedia(s.media, true);
    render();
    if (s.demo) { st.demo = true; demo(); }
  }).catch(() => {});
})();

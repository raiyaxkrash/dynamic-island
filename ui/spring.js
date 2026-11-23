// Физика затухающей пружины (60 FPS) и пресеты упругости для Dynamic Island.
(() => {
  const PRESETS = {
    soft: [190, 26],
    normal: [280, 28],
    bouncy: [340, 19],
    snappy: [460, 38],
  };

  function create(v, k = 280, c = 28) {
    return { v, t: v, vel: 0, k, c, from: v };
  }

  function stepAll(springs, dt) {
    let moving = false;
    const steps = 4;
    const h = dt / steps;
    for (const s of Object.values(springs)) {
      for (let i = 0; i < steps; i++) {
        const f = -s.k * (s.v - s.t) - s.c * s.vel;
        s.vel += f * h;
        s.v += s.vel * h;
      }
      if (Math.abs(s.v - s.t) > 0.03 || Math.abs(s.vel) > 0.3) {
        moving = true;
      } else {
        s.v = s.t;
        s.vel = 0;
      }
    }
    return moving;
  }

  function applyPreset(springs, keys, name) {
    const [sk, sc] = PRESETS[name] || PRESETS.normal;
    for (const key of keys) {
      springs[key].k = sk;
      springs[key].c = sc;
    }
  }

  window.DI_SPRING = { PRESETS, create, stepAll, applyPreset };
})();

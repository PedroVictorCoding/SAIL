'use strict';
// SAIL ambient backdrop (DESIGN.md §5–6).
//
// One Paper Shaders mesh gradient behind the page, lit by the colours a page already
// publishes: Home reads the ambient state contract (`--sail-light-*`, ambient-state.js),
// the Library reads the featured book's cover. Display only — this module never sets
// ambient state, and the page stays fully usable without it.
//
// Owner setting `sail.ambientVisuals` (localStorage) = off | subtle | expressive.
// Reduced transparency and forced colours turn it off; reduced motion keeps a still
// frame; touch devices default to subtle. Without WebGL2 a CSS gradient stands in.
// Liquid-glass lenses (vendor/liquidglass.js) are expressive-only and desktop-only.
(function (root, factory) {
  const api = factory(root);
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.SailBackdrop = api;
})(typeof self !== 'undefined' ? self : this, function (root) {
  const MODES = Object.freeze(['off', 'subtle', 'expressive']);
  const STORAGE_KEY = 'sail.ambientVisuals';
  const BASE = '#030407';
  const SPEED = Object.freeze({ subtle: 0.05, expressive: 0.12 });
  const MAX_PIXELS = Object.freeze({ subtle: 640 * 400, expressive: 1280 * 800 });
  const TWEEN_MS = 700;

  // ---- pure helpers (unit-tested) -------------------------------------------------

  function visualsMode(stored, media) {
    if (media('(prefers-reduced-transparency: reduce)') || media('(forced-colors: active)')) return 'off';
    if (MODES.includes(stored)) return stored;
    return media('(pointer: coarse)') ? 'subtle' : 'expressive';
  }

  function parseColor(value) {
    const text = String(value || '').trim();
    let m = /^#([0-9a-f]{3})$/i.exec(text);
    if (m) return m[1].split('').map(h => parseInt(h + h, 16));
    m = /^#([0-9a-f]{6})$/i.exec(text);
    if (m) return [0, 2, 4].map(i => parseInt(m[1].slice(i, i + 2), 16));
    m = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/i.exec(text);
    if (m) return [m[1], m[2], m[3]].map(n => Math.max(0, Math.min(255, Math.round(+n))));
    return null;
  }

  const hex = rgb => '#' + rgb.map(n => Math.round(n).toString(16).padStart(2, '0')).join('');
  const mix = (a, b, t) => a.map((v, i) => v + (b[i] - v) * t);

  function toHsl([r, g, b]) {
    r /= 255; g /= 255; b /= 255;
    const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2;
    if (max === min) return [0, 0, l];
    const d = max - min, s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    const h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
    return [h / 6, s, l];
  }
  function fromHsl([h, s, l]) {
    if (!s) return [l * 255, l * 255, l * 255];
    const q = l < 0.5 ? l * (1 + s) : l + s - l * s, p = 2 * l - q;
    const f = t => { t = (t + 1) % 1; return t < 1 / 6 ? p + (q - p) * 6 * t : t < 1 / 2 ? q : t < 2 / 3 ? p + (q - p) * (2 / 3 - t) * 6 : p; };
    return [f(h + 1 / 3) * 255, f(h) * 255, f(h - 1 / 3) * 255];
  }
  // Light, not paint: the contract's colours are semantic hues; the backdrop renders
  // them as luminous blooms (references: saturated light on a black field).
  function luminous(rgb, minS, l) {
    const [h, s] = toHsl(rgb);
    // Near-grey states (Offline) stay grey: no invented hue.
    return fromHsl([h, s < 0.3 ? s : Math.max(s, minS), s < 0.3 ? l * 0.7 : l]);
  }

  // [a, b, c] light colours → shader stops. Black dominates; a and b bloom; c tints.
  function palette(lights, mode) {
    const [a, b, c] = lights.map(parseColor);
    if (!a || !b || !c) return null;
    const base = parseColor(BASE);
    if (mode === 'subtle') {
      return [base, mix(luminous(c, 0.35, 0.16), base, 0.2), mix(luminous(a, 0.5, 0.42), base, 0.45), base, mix(luminous(b, 0.45, 0.36), base, 0.5)].map(hex);
    }
    return [base, luminous(c, 0.45, 0.2), luminous(a, 0.72, 0.56), base, luminous(b, 0.62, 0.5)].map(hex);
  }

  // Cover art → [a, b, c]: the two most vivid distinct hues and a dark tint. Takes
  // RGBA bytes (from a tiny canvas) so it is testable without a DOM.
  function paletteFromPixels(data) {
    const px = [];
    for (let i = 0; i + 3 < data.length; i += 4) if (data[i + 3] > 128) px.push([data[i], data[i + 1], data[i + 2]]);
    if (!px.length) return null;
    const score = p => { const [, s, l] = toHsl(p); return s * (1 - Math.abs(l - 0.5) * 1.4); };
    const sorted = [...px].sort((x, y) => score(y) - score(x));
    const a = sorted[0];
    const [ha] = toHsl(a);
    const hueGap = p => { const d = Math.abs(toHsl(p)[0] - ha); return Math.min(d, 1 - d); };
    const b = sorted.find(p => hueGap(p) > 0.08 && score(p) > 0.12) || luminous(a, 0.5, 0.4).map((v, i) => i === 2 ? v * 0.7 : v);
    const avg = px.reduce((s, p) => s.map((v, i) => v + p[i]), [0, 0, 0]).map(v => v / px.length);
    return [hex(a), hex(b), hex(luminous(avg, 0.3, 0.12))];
  }

  // ---- runtime ----------------------------------------------------------------------

  const media = q => !!(root.matchMedia && root.matchMedia(q).matches);
  function storedMode() { try { return root.localStorage?.getItem(STORAGE_KEY); } catch (_) { return null; } }

  // The store's target colours. tokens.css transitions the registered `--sail-light-*`
  // properties, so the computed value lags mid-transition; the inline value the store
  // wrote is the target, and the backdrop runs its own tween to it.
  function cssLights(el) {
    const style = getComputedStyle(el);
    return ['--sail-light-a', '--sail-light-b', '--sail-light-c']
      .map(n => (el.style.getPropertyValue(n) || style.getPropertyValue(n)).trim());
  }

  function mount({ host = document.body, lights, lenses = [] } = {}) {
    const html = document.documentElement;
    const mode = visualsMode(storedMode(), media);
    html.dataset.sailVisuals = mode;
    const inert = { mode, layer: null, shader: null, setLights() {}, setActive() {}, addLens() {}, destroy() {} };
    if (mode === 'off') return inert;

    const layer = document.createElement('div');
    layer.className = 'sail-backdrop';
    layer.setAttribute('aria-hidden', 'true');
    host.prepend(layer);
    html.classList.add('sail-backdrop-on');

    const still = media('(prefers-reduced-motion: reduce)');
    const speed = still ? 0 : SPEED[mode];
    let current = palette(lights || cssLights(html), mode) || palette(['#496f9d', '#384257', '#0e121a'], mode);
    let shader = null;
    const PS = root.PaperShaders;
    const uniforms = colors => ({ u_colors: colors.map(c => PS.getShaderColorFromString(c)), u_colorsCount: colors.length });
    try {
      if (!PS) throw new Error('Paper Shaders not loaded');
      shader = new PS.ShaderMount(layer, PS.meshGradientFragmentShader, {
        ...uniforms(current),
        u_distortion: 0.85, u_swirl: 0.3, u_grainMixer: 0, u_grainOverlay: 0,
        u_fit: PS.ShaderFitOptions.cover, u_scale: 1, u_rotation: 0, u_originX: 0.5, u_originY: 0.5,
        u_offsetX: 0, u_offsetY: 0, u_worldWidth: 0, u_worldHeight: 0, u_imageAspectRatio: 1,
      }, { alpha: false }, speed, 4200, 1, MAX_PIXELS[mode]);
      if (speed) layer.querySelector('canvas')?.setAttribute('data-dynamic', '');
    } catch (_) {
      layer.classList.add('sail-backdrop--css');
    }
    const paintCss = colors => colors.slice(1).forEach((c, i) => layer.style.setProperty(`--sail-backdrop-${i}`, c));
    paintCss(current);

    let tween = 0;
    function setLights(next) {
      const target = palette(next, mode);
      if (!target || target.join() === current.join()) return;
      paintCss(target);
      cancelAnimationFrame(tween);
      if (!shader) { current = target; return; }
      if (still) { current = target; shader.setUniforms(uniforms(current)); return; }
      const from = current.map(parseColor), to = target.map(parseColor), t0 = performance.now();
      const step = now => {
        const k = Math.min(1, (now - t0) / TWEEN_MS), e = k * k * (3 - 2 * k);
        current = from.map((c, i) => hex(mix(c, to[i], e)));
        shader.setUniforms(uniforms(current));
        if (k < 1) tween = requestAnimationFrame(step); else current = target;
      };
      tween = requestAnimationFrame(step);
    }

    // Liquid glass: an empty lens in the backdrop layer tracks a real element above it,
    // so the shader light refracts behind that element. Nothing is drawn over content.
    const tracked = [];
    let glass = null, syncing = 0;
    const lensAllowed = mode === 'expressive' && shader && !media('(pointer: coarse)');
    function sync() {
      for (const t of tracked) {
        const r = t.target.getBoundingClientRect();
        const visible = r.width > 0 && r.height > 0 && t.target.isConnected;
        t.lens.hidden = !visible;
        if (!visible) continue;
        const box = `${r.left}|${r.top}|${r.width}|${r.height}`;
        if (box === t.box) continue;
        t.box = box;
        Object.assign(t.lens.style, { left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px`, height: `${r.height}px` });
      }
      syncing = tracked.length ? requestAnimationFrame(sync) : 0;
    }
    function addLens(target, config = {}) {
      if (!lensAllowed || !target) return;
      const lens = document.createElement('div');
      lens.className = 'sail-lens';
      const radius = parseFloat(getComputedStyle(target).borderTopLeftRadius) || 18;
      lens.dataset.config = JSON.stringify({
        blurAmount: 0.3, refraction: 0.75, chromAberration: 0.04, edgeHighlight: 0.12, specular: 0.2,
        fresnel: 1, cornerRadius: radius, zRadius: Math.min(radius, 28), shadowOpacity: 0.22, shadowSpread: 14,
        brightness: 0.03, ...config,
      });
      layer.append(lens);
      tracked.push({ target, lens, box: '' });
      if (!syncing) sync();
      import('/vendor/liquidglass.js').then(async ({ LiquidGlass }) => {
        glass?.destroy();
        glass = await LiquidGlass.init({ root: layer, glassElements: tracked.map(t => t.lens) });
        layer.classList.add('sail-backdrop--glass');
      }).catch(() => { tracked.forEach(t => t.lens.remove()); tracked.length = 0; });
    }
    lenses.forEach(l => addLens(l.target || l, l.config));

    function setActive(active) {
      layer.hidden = !active;
      html.classList.toggle('sail-backdrop-on', active);
      shader?.setSpeed(active ? speed : 0);
    }
    function destroy() {
      cancelAnimationFrame(tween); cancelAnimationFrame(syncing); tracked.length = 0;
      glass?.destroy(); shader?.dispose(); layer.remove(); html.classList.remove('sail-backdrop-on');
    }
    return { mode, layer, get shader() { return shader; }, setLights, setActive, addLens, destroy };
  }

  // Home: follow the ambient state contract. The store writes `--sail-light-*` and
  // `data-sail-state` on <html>; watch those and re-light.
  function followAmbientState(backdrop) {
    const html = document.documentElement;
    const relight = () => backdrop.setLights(cssLights(html));
    new MutationObserver(relight).observe(html, { attributes: true, attributeFilter: ['style', 'data-sail-state'] });
    relight();
  }

  function paletteFromImage(img) {
    const size = 24, canvas = document.createElement('canvas');
    canvas.width = canvas.height = size;
    const ctx = canvas.getContext('2d', { willReadFrequently: true });
    try { ctx.drawImage(img, 0, 0, size, size); return paletteFromPixels(ctx.getImageData(0, 0, size, size).data); }
    catch (_) { return null; }
  }

  let current = null;
  function boot() {
    const kind = document.documentElement.dataset.sailBackdrop;
    if (!kind || current) return;
    current = mount({ lenses: [...document.querySelectorAll('[data-sail-lens]')] });
    if (kind === 'state') followAmbientState(current);
  }
  if (typeof document !== 'undefined') {
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot);
    else boot();
  }

  return {
    MODES, STORAGE_KEY, visualsMode, parseColor, palette, paletteFromPixels, paletteFromImage, mount,
    get current() { return current; },
  };
});

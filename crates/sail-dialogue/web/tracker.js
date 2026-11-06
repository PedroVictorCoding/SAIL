'use strict';
// SAIL Tracker: one live board. Data arrives over /api/tracker/stream (Server-Sent Events
// read with fetch so the owner token travels in a header). Feed text is untrusted: it is
// only ever inserted as text, and links open only http(s) URLs.

(() => {
  const $ = (id) => document.getElementById(id);
  const PAGE_LOADED = Date.now() / 1000;
  // Inside the SAIL workspace the sidebar already leads back; a second way home would
  // load SAIL inside its own frame.
  if (window.top !== window) document.querySelector('.tk-back')?.remove();

  // ------------------------------------------------------------------ state
  let config = null;
  let board = null;
  let editing = false;
  let draft = null;
  const ui = {newsTopic: 'all', newsQuery: '', expanded: new Set()};

  const PANELS = {
    anomalies: {title: 'Anomalies', hint: 'What deviates from its own baseline', sources: []},
    tape: {title: 'Tape', hint: 'Prices and rates', sources: ['quotes', 'fx']},
    numbers: {title: 'Numbers', hint: 'Pinned figures, as deltas', sources: []},
    clock: {title: 'Clock', hint: 'Local times and countdowns', sources: ['weather']},
    map: {title: 'Map', hint: 'Places, routes, aircraft and earthquakes', sources: ['air', 'quakes', 'routes']},
    news: {title: 'News', hint: 'Deduplicated headlines', sources: ['news']},
    markets: {title: 'Markets', hint: 'Moves against each symbol’s usual day', sources: ['quotes', 'fx']},
    odds: {title: 'Odds', hint: 'Prediction-market probabilities', sources: ['odds']},
    conditions: {title: 'Conditions', hint: 'Weather against the past week', sources: ['weather']},
    routes: {title: 'Routes & traffic', hint: 'Road corridors and air traffic', sources: ['routes', 'air']},
    hazards: {title: 'Hazards', hint: 'Earthquakes, global M4.5+ and near your places', sources: ['quakes']},
  };
  // Which panels a changed board field affects.
  const AFFECTS = {
    news: ['news'], topics: ['news', 'numbers'], quotes: ['tape', 'markets', 'numbers'],
    fx: ['tape', 'markets', 'numbers'], odds: ['odds', 'numbers'],
    conditions: ['conditions', 'clock', 'routes', 'numbers'], air: ['routes', 'map', 'numbers'],
    routes: ['routes', 'map'], quakes: ['hazards', 'map'], anomalies: ['anomalies'],
  };
  const SPANS = [3, 4, 5, 6, 7, 8, 9, 12];

  // ------------------------------------------------------------------ helpers
  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === 'class') node.className = v;
      else if (k === 'text') node.textContent = v;
      else if (k.startsWith('on')) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? '' : String(v));
    }
    for (const c of children.flat(Infinity)) {
      if (c === null || c === undefined || c === false) continue;
      node.append(c instanceof Node ? c : document.createTextNode(String(c)));
    }
    return node;
  }
  function safeUrl(u) {
    try { const p = new URL(u); return (p.protocol === 'https:' || p.protocol === 'http:') ? p.href : null; } catch (_) { return null; }
  }
  function link(text, url, cls) {
    const href = safeUrl(url);
    if (!href) return el('span', {class: cls, text});
    return el('a', {class: cls, href, target: '_blank', rel: 'noopener noreferrer', text});
  }
  function num(v, digits) {
    if (v === null || v === undefined || !isFinite(v)) return '—';
    const a = Math.abs(v);
    const d = digits ?? (a >= 10000 ? 0 : a >= 100 ? 2 : a >= 1 ? 2 : 4);
    return v.toLocaleString(undefined, {minimumFractionDigits: d, maximumFractionDigits: d});
  }
  function compact(v) {
    if (v === null || v === undefined || !isFinite(v)) return '—';
    return Intl.NumberFormat(undefined, {notation: 'compact', maximumFractionDigits: 1}).format(v);
  }
  function signed(v, digits = 2, unit = '') {
    if (v === null || v === undefined || !isFinite(v)) return '—';
    return `${v > 0 ? '+' : v < 0 ? '−' : '±'}${Math.abs(v).toFixed(digits)}${unit}`;
  }
  function dir(v) { return !isFinite(v) || v === null ? 'flat' : v > 0 ? 'up' : v < 0 ? 'down' : 'flat'; }
  function arrow(v) { return dir(v) === 'up' ? '▲' : dir(v) === 'down' ? '▼' : '■'; }
  function delta(v, digits = 2, unit = '%') {
    return el('span', {class: `tk-delta ${dir(v)}`}, el('span', {'aria-hidden': 'true'}, arrow(v)), ' ', signed(v, digits, unit));
  }
  function sigma(z) {
    if (z === null || z === undefined || !isFinite(z)) return el('span', {class: 'tk-z none', text: '—', title: 'No baseline yet'});
    const level = Math.abs(z) >= 3 ? 'hot' : Math.abs(z) >= 2 ? 'warm' : 'calm';
    return el('span', {class: `tk-z ${level}`, title: 'Deviation from this metric’s own baseline'}, `${signed(z, 1)}σ`);
  }
  function ago(ts) {
    if (!ts) return 'never';
    const s = Math.max(0, Math.round(Date.now() / 1000 - ts));
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    if (s < 86400) return `${Math.floor(s / 3600)}h`;
    return `${Math.floor(s / 86400)}d`;
  }
  function agoEl(ts, suffix = ' ago') {
    return el('span', {class: 'tk-ago', 'data-ts': ts || '', 'data-suffix': suffix, title: ts ? new Date(ts * 1000).toLocaleString() : ''}, ago(ts) + (ts ? suffix : ''));
  }
  function hhmm(ts) { return new Date(ts * 1000).toLocaleTimeString([], {hour: '2-digit', minute: '2-digit', hour12: false}); }
  function duration(sec) {
    const h = Math.floor(sec / 3600), m = Math.round((sec % 3600) / 60);
    return h ? `${h}h ${String(m).padStart(2, '0')}m` : `${m}m`;
  }
  function spark(values, {w = 96, h = 24, cls = '', baseline = null} = {}) {
    const NS = 'http://www.w3.org/2000/svg';
    const svg = document.createElementNS(NS, 'svg');
    svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
    svg.setAttribute('width', w); svg.setAttribute('height', h);
    svg.setAttribute('class', `tk-spark ${cls}`);
    svg.setAttribute('aria-hidden', 'true');
    const v = (values || []).filter((x) => x !== null && isFinite(x));
    if (v.length < 2) return svg;
    let lo = Math.min(...v), hi = Math.max(...v);
    if (baseline !== null) { lo = Math.min(lo, baseline); hi = Math.max(hi, baseline); }
    const span = hi - lo || 1;
    const y = (x) => (h - 2 - ((x - lo) / span) * (h - 4)).toFixed(1);
    if (baseline !== null) {
      const line = document.createElementNS(NS, 'line');
      line.setAttribute('x1', 0); line.setAttribute('x2', w); line.setAttribute('y1', y(baseline)); line.setAttribute('y2', y(baseline));
      line.setAttribute('class', 'tk-spark-base');
      svg.append(line);
    }
    const pl = document.createElementNS(NS, 'polyline');
    pl.setAttribute('points', v.map((x, i) => `${((i / (v.length - 1)) * (w - 2) + 1).toFixed(1)},${y(x)}`).join(' '));
    svg.append(pl);
    return svg;
  }
  function empty(text) { return el('p', {class: 'tk-empty', text}); }
  function slugify(name) {
    return name.toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '').replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 64) || 'place';
  }

  // ------------------------------------------------------------------ auth + api
  let token = null;
  async function getToken(force) {
    if (!force && token) return token;
    if (!force) {
      try { const t = localStorage.getItem('sail_owner_token'); if (t && t.trim().length >= 32) return (token = t.trim()); } catch (_) {}
    }
    const r = await fetch('/api/guardian/token', {cache: 'no-store'});
    if (!r.ok) throw new Error('Owner token unavailable. Open SAIL settings and enter the owner token.');
    const data = await r.json();
    token = data.token;
    try { localStorage.setItem('sail_owner_token', token); } catch (_) {}
    return token;
  }
  async function api(path, {method = 'GET', body, raw} = {}) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const t = await getToken(attempt > 0);
      const r = await fetch(`/api/tracker${path}`, {
        method, cache: 'no-store',
        headers: {Authorization: `Bearer ${t}`, ...(body !== undefined ? {'Content-Type': 'application/json'} : {})},
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      if (r.status === 401 && attempt === 0) { token = null; try { localStorage.removeItem('sail_owner_token'); } catch (_) {} continue; }
      if (raw) return r;
      const data = await r.json().catch(() => ({}));
      if (!r.ok) throw new Error(data.error || `Request failed (${r.status})`);
      return data;
    }
    throw new Error('Owner authentication required');
  }
  function banner(text) {
    const b = $('tkBanner');
    b.textContent = text || '';
    b.hidden = !text;
  }

  // ------------------------------------------------------------------ live stream
  let backoff = 1000;
  function setLive(state, text) {
    $('tkLive').dataset.state = state;
    $('tkLiveText').textContent = text;
  }
  async function connect() {
    setLive('connecting', 'CONNECTING');
    try {
      let t = await getToken();
      let r = await fetch('/api/tracker/stream', {cache: 'no-store', headers: {Authorization: `Bearer ${t}`}});
      if (r.status === 401) {
        t = await getToken(true);
        r = await fetch('/api/tracker/stream', {cache: 'no-store', headers: {Authorization: `Bearer ${t}`}});
      }
      if (r.status === 404) throw new Error('The tracker is not available on this SAIL server.');
      if (!r.ok || !r.body) throw new Error(`Stream failed (${r.status})`);
      banner('');
      setLive('live', 'LIVE');
      backoff = 1000;
      const reader = r.body.pipeThrough(new TextDecoderStream()).getReader();
      let buf = '';
      for (;;) {
        const {value, done} = await reader.read();
        if (done) break;
        buf += value;
        let i;
        while ((i = buf.indexOf('\n\n')) >= 0) {
          const block = buf.slice(0, i); buf = buf.slice(i + 2);
          let event = 'message'; const data = [];
          for (const line of block.split('\n')) {
            if (line.startsWith('event:')) event = line.slice(6).trim();
            else if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
          }
          if (!data.length) continue;
          try { handle(event, JSON.parse(data.join('\n'))); } catch (e) { console.warn('tracker event', e); }
        }
      }
      throw new Error('Stream closed');
    } catch (e) {
      setLive('down', 'RECONNECTING');
      if (!/closed|network|fetch/i.test(e.message)) banner(e.message);
      setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, 30000);
    }
  }
  function handle(event, data) {
    if (event === 'board') {
      config = data.config;
      board = data.board;
      renderAll();
    } else if (event === 'patch' && board) {
      const touched = new Set();
      for (const [k, v] of Object.entries(data)) {
        board[k] = v;
        for (const p of AFFECTS[k] || []) touched.add(p);
      }
      for (const p of touched) renderPanel(p);
      renderHealth();
      for (const m of Object.keys(PANELS)) renderMeta(m);
    }
  }

  // ------------------------------------------------------------------ layout
  function layout() { return config ? config.layout : []; }
  function renderAll() {
    const grid = $('tkGrid');
    const keep = new Map([...grid.children].map((n) => [n.dataset.module, n]));
    grid.replaceChildren();
    for (const p of layout()) {
      if (p.hidden) continue;
      const panel = keep.get(p.module) || makePanel(p.module);
      panel.style.setProperty('--span', p.span);
      panel.style.setProperty('--rows', p.rows);
      panel.dataset.w = p.span <= 4 ? 'sm' : p.span <= 8 ? 'md' : 'lg';
      panel.dataset.rows = p.rows;
      panel.classList.toggle('editing', editing);
      panel.draggable = editing;
      grid.append(panel);
      renderEditBar(panel, p);
      renderMeta(p.module);
      renderPanel(p.module);
    }
    renderTray();
    renderHealth();
  }
  function makePanel(module) {
    const info = PANELS[module];
    const titleId = `tkPanelTitle-${module}`;
    const panel = el('section', {class: 'tk-panel', 'data-module': module, 'aria-labelledby': titleId, tabindex: '-1'},
      el('header', {class: 'tk-panel-head'},
        el('h2', {id: titleId, class: 'tk-panel-title'}, el('span', {class: 'tk-panel-mark', 'aria-hidden': 'true', text: '▍'}), info.title),
        el('span', {class: 'tk-panel-meta', 'data-meta': ''}),
        el('div', {class: 'tk-editbar', 'data-editbar': ''})),
      el('div', {class: 'tk-panel-body', 'data-body': ''}));
    panel.addEventListener('dragstart', (e) => { if (!editing) return; e.dataTransfer.setData('text/plain', module); e.dataTransfer.effectAllowed = 'move'; panel.classList.add('dragging'); });
    panel.addEventListener('dragend', () => panel.classList.remove('dragging'));
    panel.addEventListener('dragover', (e) => { if (editing) { e.preventDefault(); e.dataTransfer.dropEffect = 'move'; } });
    panel.addEventListener('drop', (e) => {
      if (!editing) return;
      e.preventDefault();
      const from = e.dataTransfer.getData('text/plain');
      if (!from || from === module) return;
      const l = config.layout;
      const item = l.splice(l.findIndex((x) => x.module === from), 1)[0];
      l.splice(l.findIndex((x) => x.module === module), 0, item);
      layoutChanged();
    });
    return panel;
  }
  function renderEditBar(panel, p) {
    const bar = panel.querySelector('[data-editbar]');
    if (!editing) { bar.replaceChildren(); return; }
    const name = PANELS[p.module].title;
    const btn = (text, label, fn, disabled) => el('button', {class: 'tk-mini', type: 'button', 'aria-label': `${label} ${name}`, title: label, disabled, onclick: fn}, text);
    const idx = config.layout.findIndex((x) => x.module === p.module);
    const si = SPANS.indexOf(p.span);
    bar.replaceChildren(
      btn('◀', 'Move earlier', () => move(idx, -1), idx === 0),
      btn('▶', 'Move later', () => move(idx, 1), idx === config.layout.length - 1),
      btn('⇤', 'Narrower', () => { p.span = SPANS[Math.max(0, (si < 0 ? 3 : si) - 1)]; layoutChanged(); }, p.span <= 3),
      btn('⇥', 'Wider', () => { p.span = SPANS[Math.min(SPANS.length - 1, (si < 0 ? 3 : si) + 1)]; layoutChanged(); }, p.span >= 12),
      btn('⤒', 'Shorter', () => { p.rows = Math.max(1, p.rows - 1); layoutChanged(); }, p.rows <= 1),
      btn('⤓', 'Taller', () => { p.rows = Math.min(12, p.rows + 1); layoutChanged(); }, p.rows >= 12),
      btn('Hide', 'Hide', () => { p.hidden = true; layoutChanged(); }),
    );
  }
  function move(idx, by) {
    const l = config.layout;
    const j = idx + by;
    if (j < 0 || j >= l.length) return;
    [l[idx], l[j]] = [l[j], l[idx]];
    layoutChanged();
  }
  function renderTray() {
    const tray = $('tkTray');
    tray.hidden = !editing;
    if (!editing) return;
    const hidden = layout().filter((p) => p.hidden);
    $('tkTrayItems').replaceChildren(...(hidden.length ? hidden.map((p) => el('button', {class: 'tk-mini', type: 'button', onclick: () => { p.hidden = false; layoutChanged(); }}, `Show ${PANELS[p.module].title}`)) : [el('span', {class: 'tk-muted', text: 'none'})]));
  }
  let layoutTimer = null;
  function layoutChanged() {
    renderAll();
    clearTimeout(layoutTimer);
    layoutTimer = setTimeout(() => saveConfig(config).catch((e) => banner(`Layout not saved: ${e.message}`)), 400);
  }
  async function saveConfig(next) {
    const saved = await api('/config', {method: 'PUT', body: next});
    config = saved;
    return saved;
  }

  // ------------------------------------------------------------------ health + meta
  function sourceState(s) {
    if (!s || !s.fetched_at) return 'waiting';
    if (!s.ok) return 'error';
    if (Date.now() / 1000 - s.fetched_at > s.cadence_secs * 3) return 'stale';
    return s.error ? 'partial' : 'ok';
  }
  function renderHealth() {
    if (!board) return;
    const items = Object.entries(board.sources || {}).map(([name, s]) => {
      const state = sourceState(s);
      return el('li', {class: `tk-src ${state}`, title: `${s.label}: ${state}${s.error ? ` — ${s.error}` : ''}`},
        el('span', {class: 'tk-dot', 'aria-hidden': 'true'}),
        el('span', {class: 'tk-src-name', text: name}),
        el('span', {class: 'tk-sr', text: ` ${state}`}),
        s.fetched_at ? agoEl(s.fetched_at, '') : null);
    });
    $('tkHealth').replaceChildren(...items);
  }
  function renderMeta(module) {
    const panel = document.querySelector(`.tk-panel[data-module="${module}"]`);
    if (!panel || !board) return;
    const meta = panel.querySelector('[data-meta]');
    const srcs = PANELS[module].sources.map((s) => board.sources?.[s]).filter(Boolean);
    const parts = [el('span', {class: 'tk-hint-inline', text: PANELS[module].hint})];
    if (srcs.length) {
      const latest = Math.max(...srcs.map((s) => s.fetched_at || 0));
      parts.push(' · ', latest ? agoEl(latest) : el('span', {text: 'loading…'}));
      const bad = srcs.filter((s) => s.error);
      if (bad.length) parts.push(' · ', el('span', {class: 'tk-warn', text: bad.some((s) => !s.ok) ? 'source down' : 'partial'}));
    }
    meta.replaceChildren(...parts);
  }
  function sourceIssues(names) {
    const out = [];
    for (const n of names) {
      const s = board.sources?.[n];
      if (s?.error) out.push(el('p', {class: `tk-issue ${s.ok ? 'partial' : 'down'}`}, el('strong', {text: s.ok ? `${s.label} (partial): ` : `${s.label} unavailable: `}), s.error));
    }
    return out;
  }

  // ------------------------------------------------------------------ panels
  function renderPanel(module) {
    const panel = document.querySelector(`.tk-panel[data-module="${module}"]`);
    if (!panel || !board || !config) return;
    const body = panel.querySelector('[data-body]');
    if (module === 'map') { renderMap(body); return; }
    const scroll = body.scrollTop;
    const content = (RENDER[module] || (() => []))();
    body.replaceChildren(...sourceIssues(PANELS[module].sources), ...[content].flat().filter(Boolean));
    body.scrollTop = scroll;
  }

  function pinButton(key, label) {
    const pinned = config.pinned.includes(key);
    return el('button', {class: `tk-pin${pinned ? ' on' : ''}`, type: 'button', 'aria-pressed': pinned ? 'true' : 'false', 'aria-label': `${pinned ? 'Unpin' : 'Pin'} ${label} ${pinned ? 'from' : 'to'} Numbers`, title: pinned ? 'Unpin from Numbers' : 'Pin to Numbers', onclick: () => togglePin(key)}, pinned ? '★' : '☆');
  }
  async function togglePin(key) {
    const next = structuredClone(config);
    const i = next.pinned.indexOf(key);
    if (i >= 0) next.pinned.splice(i, 1);
    else if (next.pinned.length >= 16) { banner('At most 16 numbers can be pinned.'); return; }
    else next.pinned.push(key);
    try { await saveConfig(next); renderAll(); } catch (e) { banner(e.message); }
  }

  function resolveNumber(key) {
    const i = key.indexOf(':');
    const kind = key.slice(0, i), id = key.slice(i + 1);
    switch (kind) {
      case 'quote': {
        const q = board.quotes.find((x) => x.symbol === id);
        return q && {label: q.name, sub: q.symbol, value: num(q.price), delta: delta(q.change_pct), z: q.move_z, spark: q.spark, base: q.previous_close};
      }
      case 'fx': {
        const r = board.fx.find((x) => x.currency === id);
        return r && {label: `${config.base_currency}/${r.currency}`, sub: `ECB ${r.date}`, value: num(r.rate, 4), delta: delta(r.change_pct), spark: r.spark};
      }
      case 'temp': {
        const c = board.conditions.find((x) => x.place === id);
        return c && {label: `${c.name} temp`, sub: c.summary, value: `${num(c.temp, 1)}°C`, delta: delta(c.temp_vs_week, 1, '°'), z: c.temp_z, spark: c.next_hours.map((h) => h.temp)};
      }
      case 'air': {
        const a = board.air.find((x) => x.place === id);
        return a && {label: `${a.name} aircraft`, sub: `within ${a.radius_km} km`, value: String(a.airborne), delta: a.usual != null ? delta(a.airborne - a.usual, 0, '') : null, z: a.z};
      }
      case 'odds': {
        const m = board.odds.find((x) => x.slug === id);
        return m && {label: m.question, sub: m.outcomes[0]?.name, value: `${num(m.outcomes[0]?.probability * 100, 0)}%`, delta: m.change_24h_pts != null ? delta(m.change_24h_pts, 1, ' pts') : null};
      }
      case 'news': {
        const t = board.topics.find((x) => x.topic === id);
        return t && {label: `“${t.topic}” stories`, sub: 'last 6h', value: String(t.stories_6h), delta: t.usual != null ? delta(t.stories_6h - t.usual, 0, '') : null, z: t.z};
      }
    }
    return null;
  }

  const RENDER = {
    anomalies() {
      const list = board.anomalies || [];
      if (!list.length) return empty('Nothing deviates from its baseline right now. Baselines for air traffic and news volume build up over a few days of use.');
      return el('ol', {class: 'tk-anoms'}, list.map((a) => el('li', {},
        el('button', {class: `tk-anom ${a.z > 0 ? 'up' : 'down'} ${Math.abs(a.z) >= 3 ? 'hot' : ''}`, type: 'button', title: `Go to ${PANELS[a.module]?.title || a.module}`, onclick: () => focusPanel(a.module)},
          el('span', {class: 'tk-anom-arrow', 'aria-hidden': 'true', text: a.z > 0 ? '▲' : '▼'}),
          el('span', {class: 'tk-anom-label', text: a.label}),
          sigma(a.z),
          el('span', {class: 'tk-anom-detail', text: a.detail})))));
    },
    tape() {
      const items = [
        ...board.quotes.map((q) => [q.name, num(q.price), q.change_pct]),
        ...board.fx.map((r) => [`${config.base_currency}/${r.currency}`, num(r.rate, 4), r.change_pct]),
      ];
      if (!items.length) return empty('Add tickers or currencies in Watchlists.');
      const row = () => items.map(([n, p, c]) => el('span', {class: 'tk-tape-item'}, el('span', {class: 'tk-tape-name', text: n}), ' ', p, ' ', delta(c)));
      return el('div', {class: 'tk-tape', role: 'marquee', 'aria-label': 'Price tape'},
        el('div', {class: 'tk-tape-track'}, row(), el('span', {'aria-hidden': 'true', class: 'tk-tape-dup'}, row())));
    },
    numbers() {
      if (!config.pinned.length) return empty('Pin figures with ☆ in Markets, Odds, Conditions or Routes.');
      return el('ul', {class: 'tk-numbers'}, config.pinned.map((key) => {
        const n = resolveNumber(key);
        if (!n) return el('li', {class: 'tk-number waiting'}, el('span', {class: 'tk-number-label', text: key}), el('span', {class: 'tk-muted', text: 'waiting for data'}), pinButton(key, key));
        return el('li', {class: 'tk-number'},
          el('span', {class: 'tk-number-label', text: n.label, title: n.label}),
          el('span', {class: 'tk-number-sub', text: n.sub || ''}),
          el('span', {class: 'tk-number-value', text: n.value}),
          el('span', {class: 'tk-number-delta'}, n.delta || '', ' ', n.z !== undefined ? sigma(n.z) : ''),
          n.spark ? spark(n.spark, {w: 120, h: 22, baseline: n.base ?? null}) : null,
          pinButton(key, n.label));
      }));
    },
    clock() {
      const now = new Date();
      const rows = [el('li', {class: 'tk-clock-row main'}, el('span', {text: 'UTC'}), el('span', {class: 'tk-clock-time', 'data-tz': 'UTC'}, fmtTime(now, 'UTC')))];
      for (const c of board.conditions) {
        rows.push(el('li', {class: 'tk-clock-row'},
          el('span', {}, c.name, ' ', el('span', {class: 'tk-muted', text: c.is_day ? '☀' : '☾', 'aria-label': c.is_day ? 'day' : 'night'})),
          el('span', {class: 'tk-clock-time', 'data-tz': c.timezone}, fmtTime(now, c.timezone))));
      }
      const cds = [...config.countdowns].sort((a, b) => new Date(a.at) - new Date(b.at));
      return [el('ul', {class: 'tk-clock'}, rows),
        cds.length ? el('ul', {class: 'tk-countdowns', 'aria-label': 'Countdowns'}, cds.map((c) => el('li', {},
          el('span', {class: 'tk-cd-label', text: c.label}),
          el('span', {class: 'tk-cd-time', 'data-at': c.at, title: new Date(c.at).toLocaleString()}, countdown(c.at))))) : el('p', {class: 'tk-muted tk-small', text: 'Add deadlines and windows in Watchlists.'})];
    },
    news() {
      const topics = config.topics;
      const filters = el('div', {class: 'tk-filters', role: 'group', 'aria-label': 'Filter headlines'},
        ['all', ...topics].map((t) => el('button', {class: 'tk-chip', type: 'button', 'aria-pressed': ui.newsTopic === t ? 'true' : 'false', onclick: () => { ui.newsTopic = t; renderPanel('news'); }}, t === 'all' ? 'All' : t)),
        el('label', {class: 'tk-search'}, el('span', {class: 'tk-sr', text: 'Search headlines'}),
          el('input', {type: 'search', placeholder: 'grep…', value: ui.newsQuery, maxlength: 80, oninput: (e) => { ui.newsQuery = e.target.value; clearTimeout(ui.newsTimer); ui.newsTimer = setTimeout(() => { renderPanel('news'); const i = document.querySelector('.tk-panel[data-module="news"] input[type=search]'); if (i) { i.focus(); i.setSelectionRange(i.value.length, i.value.length); } }, 200); }})));
      const vols = board.topics.length ? el('p', {class: 'tk-vols'}, board.topics.map((t) => el('span', {class: 'tk-vol'}, `${t.topic} `, el('strong', {text: t.stories_6h}), '/6h', t.usual != null ? el('span', {class: 'tk-muted', text: ` (usual ${num(t.usual, 0)})`}) : '', t.z != null && Math.abs(t.z) >= 2 ? [' ', sigma(t.z)] : ''))) : null;
      const q = ui.newsQuery.trim().toLowerCase();
      const stories = board.news.filter((s) => (ui.newsTopic === 'all' || s.topic === ui.newsTopic) && (!q || s.title.toLowerCase().includes(q) || s.source.toLowerCase().includes(q))).slice(0, 120);
      const list = stories.length ? el('ol', {class: 'tk-news'}, stories.map((s) => {
        const fresh = s.first_seen > PAGE_LOADED && Date.now() / 1000 - s.published < 3 * 3600;
        const open = ui.expanded.has(s.id);
        return el('li', {class: `tk-story${fresh ? ' fresh' : ''}`},
          el('span', {class: 'tk-story-time', title: new Date(s.published * 1000).toLocaleString()}, hhmm(s.published), el('br'), agoEl(s.published, '')),
          el('div', {class: 'tk-story-main'},
            link(s.title, s.url, 'tk-story-title'),
            el('div', {class: 'tk-story-meta'},
              el('span', {class: 'tk-source', text: s.source}),
              s.also.length ? el('button', {class: 'tk-also', type: 'button', 'aria-expanded': open ? 'true' : 'false', onclick: () => { open ? ui.expanded.delete(s.id) : ui.expanded.add(s.id); renderPanel('news'); }}, `+${s.also.length} source${s.also.length > 1 ? 's' : ''}`) : null,
              s.topic ? el('span', {class: 'tk-tag', text: s.topic}) : null,
              fresh ? el('span', {class: 'tk-new', text: 'NEW'}) : null),
            open ? el('ul', {class: 'tk-also-list'}, s.also.map((a) => el('li', {}, link(a.source, a.url)))) : null));
      })) : empty(board.sources?.news?.fetched_at ? 'No headlines match.' : 'Fetching headlines…');
      return [filters, vols, list];
    },
    markets() {
      const out = [];
      if (board.quotes.length) {
        out.push(el('table', {class: 'tk-table'},
          el('thead', {}, el('tr', {}, ['Name', 'Last', 'Day', 'σ', 'Intraday', ''].map((h, i) => el('th', {scope: 'col', class: i ? 'num' : '', text: h, title: h === 'σ' ? 'Today’s move in units of this symbol’s typical daily move (3 months)' : null})))),
          el('tbody', {}, board.quotes.map((q) => el('tr', {},
            el('th', {scope: 'row'}, el('span', {class: 'tk-name', text: q.name}), el('span', {class: 'tk-sym', text: q.symbol})),
            el('td', {class: 'num'}, num(q.price), el('span', {class: 'tk-cur', text: q.currency ? ` ${q.currency}` : ''})),
            el('td', {class: 'num'}, delta(q.change_pct)),
            el('td', {class: 'num'}, sigma(q.move_z)),
            el('td', {class: 'num'}, spark(q.spark, {w: 80, h: 20, cls: dir(q.change), baseline: q.previous_close})),
            el('td', {class: 'num'}, pinButton(`quote:${q.symbol}`, q.name)))))));
      }
      if (board.fx.length) {
        out.push(el('h3', {class: 'tk-subhead', text: `FX · 1 ${config.base_currency} =`}),
          el('table', {class: 'tk-table'}, el('tbody', {}, board.fx.map((r) => el('tr', {},
            el('th', {scope: 'row', text: r.currency}),
            el('td', {class: 'num', text: num(r.rate, 4)}),
            el('td', {class: 'num'}, delta(r.change_pct)),
            el('td', {class: 'num'}, spark(r.spark, {w: 80, h: 20, cls: dir(r.change_pct)})),
            el('td', {class: 'num'}, pinButton(`fx:${r.currency}`, `${config.base_currency}/${r.currency}`)))))));
      }
      return out.length ? out : empty(config.tickers.length ? 'Fetching quotes…' : 'Add tickers in Watchlists.');
    },
    odds() {
      if (!board.odds.length) return empty(board.sources?.odds?.fetched_at ? 'No open markets found.' : 'Fetching markets…');
      return el('ol', {class: 'tk-odds'}, board.odds.map((m) => {
        const top = [...m.outcomes].sort((a, b) => b.probability - a.probability).slice(0, 2);
        return el('li', {},
          el('div', {class: 'tk-odds-head'}, link(m.question, m.url, 'tk-odds-q'), pinButton(`odds:${m.slug}`, m.question)),
          top.map((o) => el('div', {class: 'tk-prob-row'},
            el('span', {class: 'tk-prob-name', text: o.name}),
            el('span', {class: 'tk-prob'}, el('span', {class: 'tk-prob-fill', style: `width:${(o.probability * 100).toFixed(1)}%`})),
            el('span', {class: 'tk-prob-val', text: `${num(o.probability * 100, 0)}%`}))),
          el('p', {class: 'tk-odds-meta'},
            m.change_24h_pts != null ? ['24h ', delta(m.change_24h_pts, 1, ' pts'), ' · '] : '',
            `vol ${compact(m.volume_24h)}`,
            m.ends ? ` · ends ${new Date(m.ends).toLocaleDateString()}` : ''));
      }));
    },
    conditions() {
      if (!board.conditions.length) return empty(config.places.length ? 'Fetching weather…' : 'Add places in Watchlists.');
      return el('ul', {class: 'tk-conds'}, board.conditions.map((c) => el('li', {class: 'tk-cond'},
        el('div', {class: 'tk-cond-head'},
          el('strong', {text: c.name}),
          el('span', {class: 'tk-muted', text: ` ${c.local_time.slice(11, 16)} · ${c.summary}`}),
          pinButton(`temp:${c.place}`, `${c.name} temperature`)),
        el('div', {class: 'tk-cond-main'},
          el('span', {class: 'tk-cond-temp', text: `${num(c.temp, 1)}°`}),
          el('span', {class: 'tk-cond-vs'}, 'vs last week ', delta(c.temp_vs_week, 1, '°'), ' ', sigma(c.temp_z)),
          spark(c.next_hours.map((h) => h.temp), {w: 96, h: 22})),
        el('dl', {class: 'tk-facts'},
          fact('Feels', `${num(c.feels_like, 1)}°`),
          fact('Wind', `${num(c.wind, 0)}/${num(c.gusts, 0)} km/h`, c.gusts >= 60 ? 'warn' : ''),
          fact('Visibility', c.visibility_m != null ? (c.visibility_m >= 10000 ? '10+ km' : `${num(c.visibility_m / 1000, 1)} km`) : '—', c.visibility_m != null && c.visibility_m < 2000 ? 'warn' : ''),
          fact('Humidity', `${num(c.humidity, 0)}%`),
          fact('Pressure', `${num(c.pressure, 0)} hPa`),
          fact('AQI', c.aqi != null ? `${num(c.aqi, 0)} ${aqiLabel(c.aqi)}` : '—', c.aqi > 100 ? 'warn' : '')),
        c.outlook ? el('p', {class: 'tk-outlook', text: c.outlook}) : null)));
    },
    routes() {
      const out = [];
      if (board.routes.length) {
        out.push(el('ul', {class: 'tk-routes'}, board.routes.map((r) => {
          const from = config.places.find((p) => p.id === r.from), to = config.places.find((p) => p.id === r.to);
          const ends = [r.from, r.to].map((id) => board.conditions.find((c) => c.place === id)).filter(Boolean);
          const flags = ends.flatMap((c) => [
            c.visibility_m != null && c.visibility_m < 2000 ? `low visibility at ${c.name}` : null,
            c.gusts >= 60 ? `strong gusts at ${c.name}` : null,
            [65, 67, 75, 82, 86, 95, 96, 99].includes(c.weather_code) ? `${c.summary.toLowerCase()} at ${c.name}` : null,
          ]).filter(Boolean);
          return el('li', {class: 'tk-route'},
            el('div', {class: 'tk-route-head'}, el('strong', {text: r.name}), el('span', {class: 'tk-muted', text: ` ${from?.name || r.from} → ${to?.name || r.to}`})),
            el('div', {class: 'tk-route-figs'},
              el('span', {class: 'tk-route-eta', text: duration(r.duration_s)}), el('span', {class: 'tk-muted', text: ' free-flow · '}),
              `${num(r.distance_m / 1000, 0)} km`),
            el('p', {class: 'tk-route-ends'}, ends.length ? ends.map((c, i) => [i ? ' · ' : '', `${c.name}: ${c.summary}, ${num(c.temp, 0)}°, gusts ${num(c.gusts, 0)} km/h`]) : 'Conditions loading…'),
            flags.length ? el('p', {class: 'tk-route-flags'}, el('span', {'aria-hidden': 'true', text: '⚠ '}), flags.join('; ')) : null);
        })));
      } else {
        out.push(empty(config.routes.length ? 'Fetching routes…' : 'Add a route between two places in Watchlists.'));
      }
      out.push(el('p', {class: 'tk-note', text: 'Road times are free-flow estimates (OSRM). Live road delays need a traffic provider key, which is not configured.'}));
      out.push(el('h3', {class: 'tk-subhead', text: 'Air traffic overhead'}));
      if (board.air.length) {
        out.push(el('table', {class: 'tk-table'},
          el('thead', {}, el('tr', {}, ['Place', 'Airborne', 'Usual', 'σ', 'On ground', ''].map((h, i) => el('th', {scope: 'col', class: i ? 'num' : '', text: h})))),
          el('tbody', {}, board.air.map((a) => el('tr', {},
            el('th', {scope: 'row', text: a.name}),
            el('td', {class: 'num tk-strong', text: a.airborne}),
            el('td', {class: 'num', text: a.usual != null ? num(a.usual, 0) : 'learning'}),
            el('td', {class: 'num'}, sigma(a.z)),
            el('td', {class: 'num', text: a.on_ground}),
            el('td', {class: 'num'}, pinButton(`air:${a.place}`, `${a.name} aircraft`)))))));
        out.push(el('p', {class: 'tk-note', text: `Within ${board.air[0].radius_km} km, from OpenSky. Baselines compare the same time of day on earlier days.`}));
      } else {
        out.push(empty(board.sources?.air?.fetched_at ? 'No air traffic data.' : 'Fetching air traffic…'));
      }
      return out;
    },
    hazards() {
      if (!board.quakes.length) return empty(board.sources?.quakes?.fetched_at ? 'No significant earthquakes in the last day.' : 'Fetching earthquakes…');
      return el('table', {class: 'tk-table tk-quakes'},
        el('thead', {}, el('tr', {}, ['M', 'Where', 'When', 'Depth', 'Nearest place'].map((h, i) => el('th', {scope: 'col', class: i && i !== 1 && i !== 4 ? 'num' : '', text: h})))),
        el('tbody', {}, board.quakes.map((q) => el('tr', {},
          el('td', {}, el('span', {class: `tk-mag ${q.magnitude >= 6 ? 'hot' : q.magnitude >= 5 ? 'warm' : ''}`, text: num(q.magnitude, 1)})),
          el('td', {}, link(q.title.replace(/^M [\d.]+ - /, ''), q.url), q.tsunami ? el('span', {class: 'tk-tag warn', text: 'tsunami flag'}) : null, q.alert ? el('span', {class: `tk-tag pager-${q.alert}`, text: `PAGER ${q.alert}`}) : null),
          el('td', {class: 'num'}, agoEl(q.time)),
          el('td', {class: 'num', text: `${num(q.depth_km, 0)} km`}),
          el('td', {text: q.nearest ? `${num(q.nearest.km, 0)} km from ${q.nearest.place}` : '—'})))));
    },
  };
  function fact(k, v, cls) { return el('div', {class: `tk-fact ${cls || ''}`}, el('dt', {text: k}), el('dd', {text: v})); }
  function aqiLabel(a) { return a <= 50 ? 'good' : a <= 100 ? 'moderate' : a <= 150 ? 'sensitive' : a <= 200 ? 'unhealthy' : 'very unhealthy'; }
  function fmtTime(d, tz) {
    try { return d.toLocaleTimeString([], {timeZone: tz, hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false}); } catch (_) { return '—'; }
  }
  function countdown(at) {
    let s = Math.round((new Date(at) - Date.now()) / 1000);
    const past = s < 0; s = Math.abs(s);
    const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
    const body = `${d ? `${d}d ` : ''}${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`;
    return past ? `T+${body}` : `T−${body}`;
  }
  function focusPanel(module) {
    const p = document.querySelector(`.tk-panel[data-module="${module}"]`);
    if (!p) return;
    p.scrollIntoView({behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth', block: 'nearest'});
    p.focus({preventScroll: true});
    p.classList.remove('flash'); void p.offsetWidth; p.classList.add('flash');
  }

  // ------------------------------------------------------------------ map
  const map = {lat: 50, lon: -10, zoom: 4, fitted: false, tiles: new Map(), failed: new Map(), canvas: null, drag: null, placesKey: ''};
  const TILE = 256;
  function project(lat, lon, z) {
    const s = TILE * 2 ** z;
    const sin = Math.min(Math.max(Math.sin(lat * Math.PI / 180), -0.9999), 0.9999);
    return [(lon + 180) / 360 * s, (0.5 - Math.log((1 + sin) / (1 - sin)) / (4 * Math.PI)) * s];
  }
  function unproject(x, y, z) {
    const s = TILE * 2 ** z;
    const lon = x / s * 360 - 180;
    const n = Math.PI - 2 * Math.PI * y / s;
    return [180 / Math.PI * Math.atan(Math.sinh(n)), lon];
  }
  function renderMap(body) {
    if (!map.canvas || !body.contains(map.canvas)) {
      map.canvas = el('canvas', {class: 'tk-map-canvas', tabindex: '0', role: 'img', 'aria-label': 'Map'});
      const zoomBtn = (t, label, by) => el('button', {class: 'tk-mini', type: 'button', 'aria-label': label, onclick: () => zoomAt(by)}, t);
      body.replaceChildren(
        el('div', {class: 'tk-map'}, map.canvas,
          el('div', {class: 'tk-map-tools'}, zoomBtn('+', 'Zoom in', 1), zoomBtn('−', 'Zoom out', -1), el('button', {class: 'tk-mini', type: 'button', onclick: () => { map.fitted = false; fitMap(); drawMap(); }}, 'Fit'))),
        el('p', {class: 'tk-map-legend'},
          el('span', {class: 'lg place', text: '● places'}), el('span', {class: 'lg route', text: '━ routes'}),
          el('span', {class: 'lg air', text: '▲ aircraft'}), el('span', {class: 'lg quake', text: '◯ earthquakes'}),
          el('span', {class: 'tk-attrib'}, '© ', link('OpenStreetMap', 'https://www.openstreetmap.org/copyright'), ' contributors')),
        el('p', {class: 'tk-sr', id: 'tkMapSummary', 'aria-live': 'off'}));
      bindMap();
    }
    const key = JSON.stringify(config.places);
    if (key !== map.placesKey) { map.placesKey = key; map.fitted = false; }
    fitMap();
    drawMap();
  }
  function fitMap() {
    if (map.fitted || !map.canvas || !config.places.length) return;
    const w = map.canvas.clientWidth || 600, h = map.canvas.clientHeight || 400;
    const lats = config.places.map((p) => p.lat), lons = config.places.map((p) => p.lon);
    let z = 11;
    if (config.places.length > 1) {
      for (z = 11; z > 1; z--) {
        const [x1, y1] = project(Math.max(...lats), Math.min(...lons), z);
        const [x2, y2] = project(Math.min(...lats), Math.max(...lons), z);
        if (x2 - x1 < w * 0.8 && y2 - y1 < h * 0.8) break;
      }
    } else z = 8;
    const [cx1, cy1] = project(Math.max(...lats), Math.min(...lons), z);
    const [cx2, cy2] = project(Math.min(...lats), Math.max(...lons), z);
    [map.lat, map.lon] = unproject((cx1 + cx2) / 2, (cy1 + cy2) / 2, z);
    map.zoom = z;
    map.fitted = true;
  }
  function zoomAt(by, px, py) {
    const nz = Math.max(2, Math.min(14, map.zoom + by));
    if (nz === map.zoom) return;
    const w = map.canvas.clientWidth, h = map.canvas.clientHeight;
    px ??= w / 2; py ??= h / 2;
    const [cx, cy] = project(map.lat, map.lon, map.zoom);
    const [plat, plon] = unproject(cx - w / 2 + px, cy - h / 2 + py, map.zoom);
    const [nx, ny] = project(plat, plon, nz);
    [map.lat, map.lon] = unproject(nx - px + w / 2, ny - py + h / 2, nz);
    map.zoom = nz;
    drawMap();
  }
  function panBy(dx, dy) {
    const [cx, cy] = project(map.lat, map.lon, map.zoom);
    const s = TILE * 2 ** map.zoom;
    [map.lat, map.lon] = unproject(((cx - dx) % s + s) % s, Math.max(0, Math.min(s, cy - dy)), map.zoom);
    drawMap();
  }
  function bindMap() {
    const c = map.canvas;
    c.addEventListener('pointerdown', (e) => { c.setPointerCapture(e.pointerId); map.drag = {x: e.clientX, y: e.clientY}; c.classList.add('grabbing'); });
    c.addEventListener('pointermove', (e) => { if (!map.drag) return; panBy(e.clientX - map.drag.x, e.clientY - map.drag.y); map.drag = {x: e.clientX, y: e.clientY}; });
    const end = () => { map.drag = null; c.classList.remove('grabbing'); };
    c.addEventListener('pointerup', end); c.addEventListener('pointercancel', end);
    c.addEventListener('wheel', (e) => { e.preventDefault(); const r = c.getBoundingClientRect(); zoomAt(e.deltaY < 0 ? 1 : -1, e.clientX - r.left, e.clientY - r.top); }, {passive: false});
    c.addEventListener('keydown', (e) => {
      const k = {ArrowLeft: [80, 0], ArrowRight: [-80, 0], ArrowUp: [0, 80], ArrowDown: [0, -80]}[e.key];
      if (k) { e.preventDefault(); panBy(...k); } else if (e.key === '+' || e.key === '=') zoomAt(1); else if (e.key === '-') zoomAt(-1);
    });
    new ResizeObserver(() => drawMap()).observe(c);
  }
  async function loadTile(z, x, y) {
    const key = `${z}/${x}/${y}`;
    if (map.tiles.has(key)) return;
    if ((map.failed.get(key) || 0) > Date.now()) return;
    map.tiles.set(key, null);
    try {
      const r = await api(`/tiles/${z}/${x}/${y}.png`, {raw: true});
      if (!r.ok) throw new Error(String(r.status));
      map.tiles.set(key, darken(await createImageBitmap(await r.blob())));
      if (map.tiles.size > 400) map.tiles.delete(map.tiles.keys().next().value);
      scheduleDraw();
    } catch (_) {
      map.tiles.delete(key);
      map.failed.set(key, Date.now() + 60000);
    }
  }
  // Standard OSM tiles are light; render them once as a dark, desaturated variant.
  function darken(bmp) {
    const c = document.createElement('canvas');
    c.width = bmp.width; c.height = bmp.height;
    const ctx = c.getContext('2d');
    if ('filter' in ctx) {
      ctx.filter = 'invert(1) hue-rotate(180deg) saturate(0.35) brightness(0.8) contrast(0.9)';
      ctx.drawImage(bmp, 0, 0);
    } else {
      ctx.drawImage(bmp, 0, 0);
      ctx.fillStyle = 'rgba(20, 23, 26, 0.7)';
      ctx.fillRect(0, 0, c.width, c.height);
    }
    bmp.close?.();
    return c;
  }
  let drawQueued = false;
  function scheduleDraw() { if (!drawQueued) { drawQueued = true; requestAnimationFrame(() => { drawQueued = false; drawMap(); }); } }
  function drawMap() {
    const c = map.canvas;
    if (!c || !c.isConnected || !board) return;
    const dpr = window.devicePixelRatio || 1;
    const w = c.clientWidth, h = c.clientHeight;
    if (!w || !h) return;
    if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) { c.width = Math.round(w * dpr); c.height = Math.round(h * dpr); }
    const ctx = c.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const css = getComputedStyle(document.documentElement);
    const col = (n, f) => css.getPropertyValue(n).trim() || f;
    ctx.fillStyle = col('--recessed', '#191D20');
    ctx.fillRect(0, 0, w, h);
    const z = map.zoom, n = 2 ** z;
    const [cx, cy] = project(map.lat, map.lon, z);
    const ox = cx - w / 2, oy = cy - h / 2;
    for (let ty = Math.floor(oy / TILE); ty <= Math.floor((oy + h) / TILE); ty++) {
      if (ty < 0 || ty >= n) continue;
      for (let tx = Math.floor(ox / TILE); tx <= Math.floor((ox + w) / TILE); tx++) {
        const wx = ((tx % n) + n) % n;
        const bmp = map.tiles.get(`${z}/${wx}/${ty}`);
        if (bmp) ctx.drawImage(bmp, tx * TILE - ox, ty * TILE - oy, TILE, TILE);
        else loadTile(z, wx, ty);
      }
    }
    const pt = (lat, lon) => { const [x, y] = project(lat, lon, z); return [x - ox, y - oy]; };
    // Routes
    ctx.lineWidth = 3; ctx.strokeStyle = col('--accent', '#FF8A32'); ctx.lineJoin = 'round';
    for (const r of board.routes) {
      if (r.geometry.length < 2) continue;
      ctx.beginPath();
      r.geometry.forEach(([lon, lat], i) => { const [x, y] = pt(lat, lon); i ? ctx.lineTo(x, y) : ctx.moveTo(x, y); });
      ctx.stroke();
    }
    // Earthquakes
    ctx.strokeStyle = col('--warning', '#E5BE79'); ctx.lineWidth = 1.5;
    let quakesInView = 0;
    for (const q of board.quakes) {
      const [x, y] = pt(q.lat, q.lon);
      if (x < -50 || y < -50 || x > w + 50 || y > h + 50) continue;
      quakesInView++;
      ctx.beginPath(); ctx.arc(x, y, Math.max(4, (q.magnitude - 2) * 4), 0, Math.PI * 2); ctx.stroke();
    }
    // Aircraft
    ctx.fillStyle = col('--listening', '#88C8C4');
    let planes = 0;
    for (const a of board.air) for (const p of a.aircraft) {
      const [x, y] = pt(p.lat, p.lon);
      if (x < 0 || y < 0 || x > w || y > h) continue;
      planes++;
      ctx.save(); ctx.translate(x, y); ctx.rotate(((p.heading || 0) * Math.PI) / 180);
      ctx.beginPath(); ctx.moveTo(0, -5); ctx.lineTo(3.5, 4); ctx.lineTo(-3.5, 4); ctx.closePath(); ctx.fill();
      ctx.restore();
    }
    // Places
    ctx.font = '600 12px ui-monospace, SFMono-Regular, Menlo, monospace';
    for (const p of config.places) {
      const [x, y] = pt(p.lat, p.lon);
      ctx.fillStyle = col('--accent', '#FF8A32');
      ctx.beginPath(); ctx.arc(x, y, 4.5, 0, Math.PI * 2); ctx.fill();
      ctx.strokeStyle = col('--bg', '#1C2023'); ctx.lineWidth = 3;
      ctx.strokeText(p.name, x + 8, y + 4);
      ctx.fillStyle = col('--text', '#F0F2F3');
      ctx.fillText(p.name, x + 8, y + 4);
    }
    const summary = `${config.places.length} places, ${board.routes.length} routes, ${planes} aircraft and ${quakesInView} earthquakes in view. Zoom ${z}.`;
    c.setAttribute('aria-label', `Map. ${summary} Use arrow keys to pan and plus or minus to zoom.`);
  }

  // ------------------------------------------------------------------ settings
  function openSettings() {
    if (!config) return;
    draft = structuredClone(config);
    $('tkSettingsError').textContent = '';
    $('tkPlaceResults').replaceChildren();
    $('tkBaseCurrency').value = draft.base_currency;
    renderSettings();
    $('tkSettings').showModal();
  }
  function listItems(target, items, label, remove) {
    $(target).replaceChildren(...(items.length ? items.map((it, i) => el('li', {}, el('span', {class: 'tk-list-text'}, label(it)),
      el('button', {class: 'tk-mini', type: 'button', 'aria-label': `Remove ${typeof label(it) === 'string' ? label(it) : ''}`.trim(), onclick: () => { remove(i); renderSettings(); }}, 'Remove'))) : [el('li', {class: 'tk-muted', text: 'None'})]));
  }
  function renderSettings() {
    listItems('tkPlacesList', draft.places, (p) => `${p.name} (${p.lat.toFixed(3)}, ${p.lon.toFixed(3)})`, (i) => {
      const id = draft.places[i].id;
      draft.places.splice(i, 1);
      draft.routes = draft.routes.filter((r) => r.from !== id && r.to !== id);
      draft.pinned = draft.pinned.filter((k) => k !== `temp:${id}` && k !== `air:${id}`);
    });
    const placeName = (id) => draft.places.find((p) => p.id === id)?.name || id;
    listItems('tkRoutesList', draft.routes, (r) => `${r.name} (${placeName(r.from)} → ${placeName(r.to)})`, (i) => draft.routes.splice(i, 1));
    for (const sel of ['tkRouteFrom', 'tkRouteTo']) {
      const prev = $(sel).value;
      $(sel).replaceChildren(...draft.places.map((p) => el('option', {value: p.id, text: p.name})));
      if (draft.places.some((p) => p.id === prev)) $(sel).value = prev;
    }
    if ($('tkRouteFrom').value === $('tkRouteTo').value && draft.places.length > 1) $('tkRouteTo').selectedIndex = 1;
    listItems('tkTickersList', draft.tickers, (t) => t.label ? `${t.label} (${t.symbol})` : t.symbol, (i) => { draft.pinned = draft.pinned.filter((k) => k !== `quote:${draft.tickers[i].symbol}`); draft.tickers.splice(i, 1); });
    listItems('tkTopicsList', draft.topics, (t) => `Topic: ${t}`, (i) => draft.topics.splice(i, 1));
    listItems('tkFeedsList', draft.feeds, (f) => `${f.name} — ${f.url}`, (i) => draft.feeds.splice(i, 1));
    listItems('tkMarketsList', draft.markets, (m) => m, (i) => draft.markets.splice(i, 1));
    listItems('tkCurrenciesList', draft.currencies, (c) => c, (i) => draft.currencies.splice(i, 1));
    listItems('tkCountdownsList', draft.countdowns, (c) => `${c.label} — ${new Date(c.at).toLocaleString()}`, (i) => draft.countdowns.splice(i, 1));
  }
  function settingsError(msg) { $('tkSettingsError').textContent = msg || ''; }
  async function searchPlaces() {
    const q = $('tkPlaceQuery').value.trim();
    if (!q) return;
    const out = $('tkPlaceResults');
    out.replaceChildren(el('li', {class: 'tk-muted', text: 'Searching…'}));
    try {
      const {results} = await api(`/geocode?q=${encodeURIComponent(q)}`);
      out.replaceChildren(...(results.length ? results.map((r) => el('li', {}, el('button', {class: 'tk-mini', type: 'button', onclick: () => {
        let id = slugify(r.name), n = 2;
        while (draft.places.some((p) => p.id === id)) id = `${slugify(r.name).slice(0, 60)}-${n++}`;
        if (draft.places.length >= 12) { settingsError('At most 12 places.'); return; }
        draft.places.push({id, name: r.name, lat: r.lat, lon: r.lon});
        out.replaceChildren(); $('tkPlaceQuery').value = '';
        renderSettings();
      }}, `Add ${r.name}`), el('span', {class: 'tk-muted', text: ` ${r.detail}`}))) : [el('li', {class: 'tk-muted', text: 'No matches.'})]));
    } catch (e) { out.replaceChildren(el('li', {class: 'tk-error', text: e.message})); }
  }
  function bindSettings() {
    $('tkSettingsOpen').addEventListener('click', openSettings);
    $('tkSettingsCancel').addEventListener('click', () => $('tkSettings').close());
    $('tkPlaceSearch').addEventListener('click', searchPlaces);
    $('tkPlaceQuery').addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); searchPlaces(); } });
    const onEnter = (input, fn) => $(input).addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); fn(); } });
    const addRoute = () => {
      const from = $('tkRouteFrom').value, to = $('tkRouteTo').value;
      if (!from || !to || from === to) { settingsError('Choose two different places.'); return; }
      const id = `${from}-${to}`.slice(0, 64);
      if (draft.routes.some((r) => r.id === id)) { settingsError('That route is already listed.'); return; }
      const name = (p) => draft.places.find((x) => x.id === p).name;
      draft.routes.push({id, name: `${name(from)} → ${name(to)}`, from, to});
      settingsError(''); renderSettings();
    };
    $('tkRouteAdd').addEventListener('click', addRoute);
    const addTicker = () => {
      const symbol = $('tkTickerSymbol').value.trim().toUpperCase();
      if (!/^[A-Z0-9^=.\-_]{1,20}$/.test(symbol)) { settingsError('Enter a symbol such as AAPL, ^GSPC or BTC-USD.'); return; }
      if (draft.tickers.some((t) => t.symbol === symbol)) { settingsError('Already listed.'); return; }
      draft.tickers.push({symbol, label: $('tkTickerLabel').value.trim()});
      $('tkTickerSymbol').value = ''; $('tkTickerLabel').value = '';
      settingsError(''); renderSettings();
    };
    $('tkTickerAdd').addEventListener('click', addTicker); onEnter('tkTickerSymbol', addTicker); onEnter('tkTickerLabel', addTicker);
    const addTopic = () => {
      const t = $('tkTopicInput').value.trim();
      if (!t || draft.topics.includes(t)) return;
      draft.topics.push(t); $('tkTopicInput').value = ''; renderSettings();
    };
    $('tkTopicAdd').addEventListener('click', addTopic); onEnter('tkTopicInput', addTopic);
    const addFeed = () => {
      const name = $('tkFeedName').value.trim(), url = $('tkFeedUrl').value.trim();
      if (!name || !safeUrl(url)) { settingsError('Give the feed a name and an http(s) URL.'); return; }
      draft.feeds.push({name, url}); $('tkFeedName').value = ''; $('tkFeedUrl').value = '';
      settingsError(''); renderSettings();
    };
    $('tkFeedAdd').addEventListener('click', addFeed); onEnter('tkFeedUrl', addFeed);
    const addMarket = () => {
      let slug = $('tkMarketInput').value.trim();
      const m = slug.match(/polymarket\.com\/(?:event|market)\/([a-z0-9-]+)/i);
      if (m) slug = m[1];
      slug = slug.toLowerCase();
      if (!/^[a-z0-9-]{1,160}$/.test(slug)) { settingsError('Enter a Polymarket slug or market link.'); return; }
      if (!draft.markets.includes(slug)) draft.markets.push(slug);
      $('tkMarketInput').value = ''; settingsError(''); renderSettings();
    };
    $('tkMarketAdd').addEventListener('click', addMarket); onEnter('tkMarketInput', addMarket);
    const addCurrency = () => {
      const c = $('tkCurrencyInput').value.trim().toUpperCase();
      if (!/^[A-Z]{3}$/.test(c)) { settingsError('Use a 3-letter currency code.'); return; }
      if (!draft.currencies.includes(c)) draft.currencies.push(c);
      $('tkCurrencyInput').value = ''; settingsError(''); renderSettings();
    };
    $('tkCurrencyAdd').addEventListener('click', addCurrency); onEnter('tkCurrencyInput', addCurrency);
    const addCountdown = () => {
      const label = $('tkCountdownLabel').value.trim(), at = $('tkCountdownAt').value;
      if (!label || !at) { settingsError('Give the countdown a label and a time.'); return; }
      draft.countdowns.push({label, at: new Date(at).toISOString()});
      $('tkCountdownLabel').value = ''; $('tkCountdownAt').value = '';
      settingsError(''); renderSettings();
    };
    $('tkCountdownAdd').addEventListener('click', addCountdown);
    $('tkSettingsSave').addEventListener('click', async () => {
      draft.base_currency = $('tkBaseCurrency').value.trim().toUpperCase();
      draft.currencies = draft.currencies.filter((c) => c !== draft.base_currency);
      const btn = $('tkSettingsSave');
      btn.disabled = true; btn.textContent = 'Saving…';
      try {
        await saveConfig(draft);
        $('tkSettings').close();
        renderAll();
      } catch (e) { settingsError(e.message); } finally { btn.disabled = false; btn.textContent = 'Save'; }
    });
  }

  // ------------------------------------------------------------------ ticking
  function tick() {
    const now = new Date();
    $('tkBarClock').textContent = `${fmtTime(now)} local · ${fmtTime(now, 'UTC')} UTC`;
    for (const n of document.querySelectorAll('.tk-ago[data-ts]')) {
      const ts = Number(n.dataset.ts);
      if (ts) n.textContent = ago(ts) + n.dataset.suffix;
    }
    for (const n of document.querySelectorAll('.tk-clock-time[data-tz]')) n.textContent = fmtTime(now, n.dataset.tz);
    for (const n of document.querySelectorAll('.tk-cd-time[data-at]')) n.textContent = countdown(n.dataset.at);
  }

  // ------------------------------------------------------------------ boot
  function setEditing(on) {
    editing = on;
    $('tkEdit').setAttribute('aria-pressed', on ? 'true' : 'false');
    $('tkEdit').textContent = on ? 'Editing layout' : 'Edit layout';
    document.body.classList.toggle('tk-editing', on);
    renderAll();
  }
  $('tkEdit').addEventListener('click', () => setEditing(!editing));
  $('tkEditDone').addEventListener('click', () => setEditing(false));
  $('tkResetLayout').addEventListener('click', () => {
    // The server fills an empty layout with the default arrangement.
    saveConfig({...config, layout: []}).then(() => renderAll()).catch((e) => banner(e.message));
  });
  $('tkRefresh').addEventListener('click', async () => {
    const b = $('tkRefresh');
    b.disabled = true;
    try { await api('/refresh', {method: 'POST', body: {}}); } catch (e) { banner(e.message); }
    setTimeout(() => { b.disabled = false; }, 3000);
  });
  document.addEventListener('keydown', (e) => {
    if (e.target.closest('input, textarea, select, dialog')) return;
    if (e.key === 'e' && !e.metaKey && !e.ctrlKey) setEditing(!editing);
    if (e.key === 'Escape' && editing) setEditing(false);
  });
  bindSettings();
  setInterval(tick, 1000);
  tick();
  connect();
})();

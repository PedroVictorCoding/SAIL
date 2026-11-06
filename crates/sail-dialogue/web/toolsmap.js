'use strict';
// The Tools Map is SAIL's workspace: the isometric map of subsystems, with the live
// Dialogue panel (the original #homeView, moved in so voice and the composer keep their
// state) in the right column. Home shows the map in the centre: selecting a box shows its
// compact status window, and selecting it again (or its sidebar entry) opens the system.
// Any route but Home replaces the map there with that system's real controls
// (#subsystemPage, filled by the shared page router), or
// with the Tracker page. Visibility follows the router: every route but #classic shows it.

(() => {
  const SVGNS = 'http://www.w3.org/2000/svg';
  const S = 46;                       // world unit in px
  const C = Math.cos(Math.PI / 6), N = Math.sin(Math.PI / 6);
  const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');

  // ------------------------------------------------------------------ model
  // Positions are screen-oriented units (u right, v down) converted to the iso grid.
  const STATIONS = [
    {id: 'core', name: 'SAIL Core', sub: 'Orchestrator · Router · Policy', u: 0, v: 14, kind: 'core', page: 'core', endpoint: '/api/health',
      about: 'Coordinates SAIL’s subsystems: routes requests, applies policy and keeps local services running.'},
    {id: 'dialogue', name: 'Dialogue', sub: 'STT · LLM · TTS', u: -7.6, v: 9.4, icon: 'dialogue', page: 'dialogue',
      about: 'Voice and text conversation: listening, transcription, replies and speech.'},
    {id: 'mind', name: 'Mind', sub: 'Model Hub', u: -2.4, v: 7.8, icon: 'mind', page: 'mind', endpoint: '/api/mind/status',
      about: 'Local language and speech models, compute routing and resource admission.'},
    {id: 'memory', name: 'Memory', sub: 'Vector · Knowledge', u: 3.2, v: 8.6, icon: 'mesh', page: 'memory',
      about: 'Episodic memory and the associative knowledge graph SAIL learns from conversation.'},
    {id: 'system', name: 'System', sub: 'OS · Resources', u: 10.4, v: 10.4, icon: 'sense', page: 'core', endpoint: '/api/system/metrics',
      about: 'The host: CPU, GPU, memory, storage and throughput that every subsystem shares.'},
    {id: 'vision', name: 'Vision', sub: 'Camera · OCR · VLM', u: -9.4, v: 13.4, icon: 'vision', page: 'canvas',
      about: 'Camera and screen previews you choose to share, read by the vision model.'},
    {id: 'flow', name: 'Flow', sub: 'Automation · Workflows', u: -7.8, v: 17.8, icon: 'flow', page: 'pulse', endpoint: '/api/pulse/reminders',
      about: 'Triggers, schedules and durable reminders that run without a conversation.'},
    {id: 'canvas', name: 'Canvas', sub: 'Projects · Notes', u: -5, v: 21.2, icon: 'canvas', page: 'canvas',
      about: 'Visual answers, diagrams and structured results from conversations.'},
    {id: 'arena', name: 'Arena', sub: 'Learn · Eval', u: -0.4, v: 21.6, icon: 'arena', page: 'arena',
      about: 'Arena Learn teaches step by step, from an assessment to a lesson map.'},
    {id: 'forge', name: 'Forge', sub: 'Build · Agents · Tools', u: 4.2, v: 18.6, kind: 'forge', page: 'forge', endpoint: '/api/plans/',
      about: 'Development, build and tool orchestration: plans, agents, skills and system tools.'},
    {id: 'guardian', name: 'Guardian', sub: 'Safety · Policy', u: 10.6, v: 15.4, icon: 'guardian', page: 'guardian', endpoint: '/api/guardian/status',
      about: 'Approves or denies every effectful action and keeps a tamper-evident audit trail.'},
    {id: 'discover', name: 'Discover', sub: 'Proactive · Tracker', u: 10.6, v: 19.6, icon: 'discovery', page: 'discovery', endpoint: '/api/tracker/config',
      about: 'Curiosity and the Tracker board: what is changing in the world you follow.'},
    {id: 'entertainment', name: 'Entertainment', sub: 'Music · Video · Books', u: 5.2, v: 22.6, icon: 'entertainment', page: 'entertainment',
      about: 'Media spaces: music, video and books.'},
  ];
  const LINKS = [
    ['dialogue', 'core'], ['mind', 'core'], ['memory', 'core'], ['system', 'core'], ['vision', 'core'],
    ['flow', 'core'], ['canvas', 'core'], ['arena', 'core'], ['forge', 'core'], ['guardian', 'core'],
    ['discover', 'forge'], ['entertainment', 'forge'], ['dialogue', 'mind'], ['mind', 'memory'],
    ['memory', 'system'], ['vision', 'dialogue'], ['flow', 'canvas'], ['guardian', 'system'], ['arena', 'memory'],
  ];
  const SLABS = [
    {label: 'Models', u: 9.4, v: 3.4, w: 7, d: 2.2, h: 1.6},
    {label: 'Compute', u: -9.8, v: 23.6, w: 6.4, d: 2.2, h: 1.4},
    {label: 'Data', u: 9.8, v: 23.6, w: 2.2, d: 6.4, h: 1.4},
  ];
  const byId = Object.fromEntries(STATIONS.map((s) => [s.id, s]));
  // Router pages that are not a station's own page still get a matching status inspector.
  const PAGE_STATION = {bridge: 'core', agents: 'forge', flow: 'flow', pulse: 'flow'};
  // Tabs of the right-hand Dialogue column: the conversation, and Context (the open
  // system's status), which has its own row of views.
  const COLUMN_TABS = ['chat', 'context'];
  const INFO_TABS = ['overview', 'tasks', 'logs', 'config'];
  for (const s of STATIONS) if (s.page && !PAGE_STATION[s.page]) PAGE_STATION[s.page] = s.id;
  for (const s of STATIONS) { s.gx = (s.u + s.v) / 2; s.gy = (s.v - s.u) / 2; }

  const state = {
    on: false, built: false, selected: null, popup: null, tracker: false, pending: null, trigger: null, tab: 'chat', info: 'overview', chatScroll: null, mode: 'map',
    live: true, paused: false, speed: 1, k: 1, tx: 0, ty: 0,
    metrics: null, mind: null, health: null, guardian: null, approvals: 0, plans: [], reminders: [],
    latency: {}, history: {cpu: [], gpu: [], ram: [], io: []},
    activity: [], seen: new Set(), modelStates: {}, planStates: {}, version: '',
    online: null,
  };

  // ------------------------------------------------------------------ helpers
  const $ = (sel, rootNode = document) => rootNode.querySelector(sel);
  function el(tag, attrs, ...kids) {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === 'class') n.className = v; else if (k === 'text') n.textContent = v;
      else if (k.startsWith('on')) n.addEventListener(k.slice(2), v); else n.setAttribute(k, v === true ? '' : v);
    }
    for (const c of kids.flat(Infinity)) if (c !== null && c !== undefined && c !== false) n.append(c instanceof Node ? c : document.createTextNode(String(c)));
    return n;
  }
  function svg(tag, attrs, ...kids) {
    const n = document.createElementNS(SVGNS, tag);
    for (const [k, v] of Object.entries(attrs || {})) if (v !== null && v !== undefined) n.setAttribute(k, v);
    for (const c of kids.flat(Infinity)) if (c) n.append(c);
    return n;
  }
  function icon(name) {
    const s = svg('svg', {class: 'tm-icon', 'aria-hidden': 'true', viewBox: '0 0 24 24'});
    const stroke = {fill: 'none', stroke: 'currentColor', 'stroke-width': '1.8', 'stroke-linecap': 'round', 'stroke-linejoin': 'round'};
    if (name === 'forge') { s.append(svg('path', {d: 'M8 7l-5 5 5 5M16 7l5 5-5 5M14 4l-4 16', ...stroke})); return s; }
    if (name === 'away') { s.append(svg('path', {d: 'M3 12a9 9 0 1 0 2.6-6.4L3 8M3 3v5h5M12 7v5l3 2', ...stroke})); return s; }
    if (name === 'tasks') { s.append(svg('path', {d: 'M9 6h11M9 12h11M9 18h11M4 6h.01M4 12h.01M4 18h.01', ...stroke})); return s; }
    s.removeAttribute('viewBox');
    s.append(svg('use', {href: `/icons.svg#${name}`}));
    return s;
  }
  function logo() {
    const s = svg('svg', {class: 'tm-logo-mark', viewBox: '0 0 32 32', 'aria-hidden': 'true'});
    s.append(svg('path', {d: 'M16 3L29 28H21.5L16 16.5 10.5 28H3z', fill: 'currentColor'}));
    return s;
  }
  function bytes(b, unit) {
    if (b == null) return '—';
    if (unit === 'TB') return (b / 1099511627776).toFixed(1);
    return (b / 1073741824).toFixed(1);
  }
  function pct(v) { return v == null || !isFinite(v) ? '—' : `${Math.round(v)}%`; }
  function hhmmss(d) { return d.toLocaleTimeString([], {hour12: false}); }
  function level(v) { return v == null ? '' : v >= 95 ? 'crit' : v >= 80 ? 'warn' : ''; }
  function modelState(md) { return typeof md.state === 'string' ? md.state : Object.keys(md.state || {})[0] || 'unknown'; }

  // ------------------------------------------------------------------ iso projection
  const P = (x, y, z = 0) => [(x - y) * C * S, (x + y) * N * S - z * S];
  const pts = (arr) => arr.map((p) => p.map((n) => n.toFixed(1)).join(',')).join(' ');
  function box(g, x, y, z, w, d, h, cls = '', rim = false) {
    const top = [P(x, y, z + h), P(x + w, y, z + h), P(x + w, y + d, z + h), P(x, y + d, z + h)];
    const left = [P(x, y + d, z), P(x + w, y + d, z), P(x + w, y + d, z + h), P(x, y + d, z + h)];
    const right = [P(x + w, y, z), P(x + w, y + d, z), P(x + w, y + d, z + h), P(x + w, y, z + h)];
    const grp = svg('g', {class: `tm-box ${cls}`});
    grp.append(svg('polygon', {class: 'f-left', points: pts(left)}), svg('polygon', {class: 'f-right', points: pts(right)}), svg('polygon', {class: 'f-top', points: pts(top)}));
    if (rim) {
      grp.append(svg('polyline', {class: 'f-rim', points: pts([P(x, y + d, z), P(x + w, y + d, z), P(x + w, y, z)])}));
      grp.append(svg('polyline', {class: 'f-rim-top', points: pts([P(x, y + d, z + h), P(x + w, y + d, z + h), P(x + w, y, z + h)])}));
    }
    g.append(grp);
    return grp;
  }
  function cylinder(g, cx, cy, z, r, h, cls = '') {
    const [bx, by] = P(cx, cy, z), [, ty] = P(cx, cy, z + h);
    const rx = Math.SQRT2 * r * C * S, ry = Math.SQRT2 * r * N * S;
    const grp = svg('g', {class: `tm-cyl ${cls}`});
    grp.append(svg('ellipse', {class: 'c-side', cx: bx, cy: by, rx, ry}),
      svg('rect', {class: 'c-side', x: bx - rx, y: ty, width: 2 * rx, height: by - ty}),
      svg('ellipse', {class: 'c-top', cx: bx, cy: ty, rx, ry}));
    g.append(grp);
  }
  function grill(g, x, y, z, w, d) {
    for (let i = 1; i < 5; i++) {
      const t = i / 5, a = P(x + w * t, y + 0.1, z), b = P(x + w * t, y + d - 0.1, z);
      g.append(svg('line', {class: 'tm-grill', x1: a[0], y1: a[1], x2: b[0], y2: b[1]}));
    }
  }
  function ledStack(g, x, y, z, id) {
    const grp = svg('g', {class: 'tm-leds', 'data-leds': id});
    for (let i = 0; i < 5; i++) box(grp, x, y, z + i * 0.2, 0.22, 0.22, 0.14, 'led');
    g.append(grp);
  }
  function rand(seed) { let t = seed; return () => { t = (t * 9301 + 49297) % 233280; return t / 233280; }; }

  function drawStation(g, s, i) {
    const grp = svg('g', {class: 'tm-station', 'data-station': s.id});
    const x = s.gx, y = s.gy;
    if (s.kind === 'core') {
      box(grp, x - 3.2, y - 3.2, 0, 6.4, 6.4, 0.45, 'plinth', true);
      box(grp, x - 2.6, y - 2.6, 0.45, 5.2, 5.2, 0.3, 'deck', true);
      box(grp, x - 2, y - 1.6, 0.75, 1.5, 3.2, 1.1, 'tank');
      box(grp, x - 0.2, y - 1.6, 0.75, 1.5, 3.2, 1.1, 'tank');
      box(grp, x - 2.3, y - 2, 0.75, 4.6, 4, 1.6, 'glass');
      box(grp, x + 1.2, y + 1.3, 0.75, 0.9, 0.6, 0.5, 'machine');
      ledStack(grp, x + 2.2, y - 2.3, 0.75, s.id);
      g.append(grp);
      return;
    }
    const r = rand(i * 7919 + 13), F = 1.35;
    box(grp, x - 1.7 * F, y - 1.7 * F, 0, 3.4 * F, 3.4 * F, 0.36, 'plinth', true);
    const big = (0.9 + r() * 0.6) * F;
    box(grp, x - 1.3 * F, y - 1.1 * F, 0.36, 1.5 * F, 1.6 * F, big, 'machine');
    grill(grp, x - 1.2 * F, y - 1.0 * F, 0.36 + big, 1.3 * F, 1.4 * F);
    box(grp, x + 0.35 * F, y - 0.2 * F, 0.36, 1.0 * F, 1.2 * F, (0.5 + r() * 0.4) * F, 'machine alt');
    box(grp, x - 1.2 * F, y + 0.75 * F, 0.36, 0.7 * F, 0.6 * F, 0.35 * F, 'machine alt');
    if (r() > 0.45) cylinder(grp, x + 0.9 * F, y - 1.0 * F, 0.36, 0.26, (1 + r() * 0.8) * F, 'pipe');
    ledStack(grp, x - 1.65 * F, y + 1.25 * F, 0.36, s.id);
    g.append(grp);
  }
  // Orthogonal conduit along the grid axes, bending once.
  function linkPath(a, b) { return [[a.gx, a.gy], [b.gx, a.gy], [b.gx, b.gy]]; }

  // ------------------------------------------------------------------ build DOM
  let root, world, svgRoot, stage, pane, paneMain, paneEmpty, inspector, trackerHost, entertainmentHost, chatSlot, chatMarker, packets = [];
  function build() {
    if (state.built) return;
    state.built = true;
    root = el('div', {class: 'tm-root', id: 'toolsMap', role: 'region', 'aria-label': 'SAIL Tools Map'});
    const nav = el('nav', {class: 'tm-side', 'aria-label': 'Main navigation'},
      el('div', {class: 'tm-brand'}, logo(), el('div', {}, el('strong', {text: 'SAIL'}), el('span', {class: 'tm-version', id: 'tmVersion', text: 'local'}))),
      el('ul', {class: 'tm-nav'},
        navItem('home', 'Home', 'home'), navItem('dialogue', 'Dialogue', 'dialogue'), navItem('arena', 'Arena', 'arena'),
        navItem('canvas', 'Canvas', 'canvas'), navItem('forge', 'Forge', 'forge'), navItem('flow', 'Flow', 'flow'),
        navItem('system', 'System', 'sense'), navItem('mind', 'Mind', 'mind'), navItem('memory', 'Memory', 'mesh'),
        navItem('discover', 'Discover', 'discovery'),
        navItem('entertainment', 'Entertainment', 'entertainment'), navItem('guardian', 'Guardian', 'guardian')),
      el('ul', {class: 'tm-nav tm-nav-2'},
        el('li', {}, el('button', {class: 'tm-nav-item', type: 'button', onclick: (e) => openStation('forge', e.currentTarget, 'tasks')}, icon('tasks'), 'Tasks')),
        el('li', {}, el('button', {class: 'tm-nav-item', type: 'button', id: 'terminalRailBtn', onclick: () => window.sailTerminal ? window.sailTerminal.focusTerminal() : document.getElementById('terminalCard')?.classList.toggle('terminal-fullscreen')}, icon('terminal'), 'Terminal')),
        navItem('tracker', 'Tracker', 'sense')),
      el('section', {class: 'tm-np', id: 'tmNowPlaying', 'aria-label': 'Now playing', hidden: true}),
      el('div', {class: 'tm-sidecard'},
        el('p', {class: 'tm-sidecard-title'}, el('span', {class: 'tm-status', id: 'tmCoreDot'}), el('span', {id: 'tmCoreState', text: 'Connecting…'})),
        el('p', {class: 'tm-sidecard-sub', id: 'tmUptime', text: ''}),
        el('div', {class: 'tm-sidecard-actions'},
          el('button', {class: 'tm-btn', type: 'button', onclick: openSettings}, icon('settings'), 'Settings'))));

    const top = el('header', {class: 'tm-top'},
      el('div', {class: 'tm-crumbs'},
        el('h1', {}, el('span', {id: 'tmCrumb', text: 'SAIL'}), el('span', {class: 'tm-crumb-sep', text: ' / '}), 'Workspace'),
        el('p', {text: 'Spatial orchestration of SAIL’s tools, models and agents'})),
      el('form', {class: 'tm-ask', role: 'search', onsubmit: ask},
        el('span', {class: 'tm-ask-icon', 'aria-hidden': 'true', text: '⌕'}),
        el('label', {class: 'tm-sr', for: 'tmAskInput', text: 'Ask SAIL or run a command'}),
        el('input', {id: 'tmAskInput', type: 'search', placeholder: 'Ask SAIL or run a command…', autocomplete: 'off', maxlength: '2000'}),
        el('kbd', {text: 'Ctrl K'})),
      el('div', {class: 'tm-indicators', id: 'tmTopIndicators'}),
      el('ul', {class: 'tm-meters', id: 'tmMeters', 'aria-label': 'Host resources'}),
      el('button', {class: 'tm-bell', type: 'button', id: 'tmBell', 'aria-label': 'Approvals', onclick: (e) => openStation('guardian', e.currentTarget)},
        icon('guardian'), el('span', {class: 'tm-badge', id: 'tmBellCount', hidden: true})),
      el('button', {class: 'tm-bell', type: 'button', 'aria-label': 'Settings', onclick: openSettings}, icon('settings')),
      el('span', {class: 'tm-avatar', 'aria-hidden': 'true'}));

    stage = el('section', {class: 'tm-stage', 'aria-label': 'Subsystem map'},
      el('div', {class: 'tm-title'}, el('h2', {}, 'SAIL', el('br'), 'TOOLS MAP'), el('p', {text: 'Live systems, models and tools. Select one to see its status; select it again to open it.'})),
      el('div', {class: 'tm-viewport', id: 'tmViewport', tabindex: '0', 'aria-label': 'Map. Drag or use arrow keys to pan, plus and minus to zoom.'},
        world = el('div', {class: 'tm-world'})),
      el('div', {class: 'tm-ctl tm-ctl-left'},
        el('button', {class: 'tm-btn', type: 'button', id: 'tmLive', 'aria-pressed': 'true', onclick: toggleLive}, el('span', {class: 'tm-status on', 'aria-hidden': 'true'}), 'Live'),
        el('button', {class: 'tm-btn', type: 'button', id: 'tmBriefingBtn', 'aria-label': 'Session briefing deck', 'aria-pressed': 'false', onclick: toggleBriefing}, icon('discovery'), 'Briefing'),
        el('button', {class: 'tm-btn tm-icon-btn', type: 'button', id: 'tmPause', 'aria-pressed': 'false', 'aria-label': 'Pause animation', onclick: togglePause}, '❚❚'),
        el('label', {class: 'tm-sr', for: 'tmSpeed', text: 'Animation speed'}),
        el('select', {class: 'tm-btn', id: 'tmSpeed', onchange: (e) => { state.speed = Number(e.target.value); }},
          el('option', {value: '0.5', text: '0.5x'}), el('option', {value: '1', text: '1x', selected: true}), el('option', {value: '2', text: '2x'}))),
      el('div', {class: 'tm-ctl tm-ctl-mid', role: 'group', 'aria-label': 'Map view'},
        modeBtn('topology', 'Topology'), modeBtn('map', 'Tools Map'), modeBtn('flows', 'Flows'), modeBtn('resources', 'Resources'),
        modeBtn('away', 'While You Were Away')),
      el('div', {class: 'tm-ctl tm-ctl-right'},
        el('button', {class: 'tm-btn tm-icon-btn', type: 'button', 'aria-label': 'Fit map', onclick: fit}, '⛶'),
        el('button', {class: 'tm-btn tm-icon-btn', type: 'button', 'aria-label': 'Zoom out', onclick: () => zoom(1 / 1.2)}, '−'),
        el('button', {class: 'tm-btn tm-icon-btn', type: 'button', 'aria-label': 'Zoom in', onclick: () => zoom(1.2)}, '+')));

    // The return briefing is the "While You Were Away" view of the map stage (data-mode
    // "away"); briefing.js fills it by id and hides it when there is no briefing.
    const briefing = document.getElementById('returnBriefingDeck');
    if (briefing) { briefing.classList.add('tm-briefing'); stage.append(briefing); }
    stage.append(el('div', {class: 'tm-away-empty'},
      el('p', {text: 'Nothing new since you were away.'}),
      el('button', {class: 'tm-btn', type: 'button', onclick: () => window.sailBriefing?.refresh(), text: 'Prepare a briefing'})));

    // Right column: the live Dialogue panel (#homeView, moved in by showChat()) and, behind
    // the other tabs, the status of the open system (SAIL Core on the map).
    inspector = el('div', {class: 'tm-inspector', id: 'tmInspector', role: 'tabpanel', 'aria-labelledby': 'tmTab-context', hidden: true});
    const chat = el('aside', {class: 'tm-chat', id: 'tmChat', 'aria-labelledby': 'tmChatTitle'},
      el('header', {class: 'tm-chat-head'},
        el('span', {class: 'tm-insp-icon'}, logo()),
        el('h2', {id: 'tmChatTitle', text: 'SAIL'}),
        el('span', {class: 'tm-pill', id: 'tmChatPill'}, el('span', {class: 'tm-status idle'}), 'Connecting')),
      el('div', {class: 'tm-tabs', id: 'tmTabs', role: 'tablist', 'aria-label': 'Dialogue column'},
        COLUMN_TABS.map((t) => el('button', {class: 'tm-tab', type: 'button', role: 'tab', id: `tmTab-${t}`,
          'aria-controls': t === 'chat' ? 'tmChatPanel' : 'tmInspector', onclick: () => setTab(t), onkeydown: (e) => tabKey(e, COLUMN_TABS, state.tab, 'tmTab')},
          t[0].toUpperCase() + t.slice(1)))),
      chatSlot = el('div', {class: 'tm-chat-slot', id: 'tmChatPanel', role: 'tabpanel', 'aria-labelledby': 'tmTab-chat'}),
      inspector);

    // Centre pane: replaces the map for every route but Home. #subsystemPage (the router's
    // page for the route) is its main content; the Tracker page loads in a frame.
    pane = el('section', {class: 'tm-pane', id: 'tmPane', hidden: true, 'aria-labelledby': 'subsystemTitle'},
      el('button', {class: 'tm-btn tm-pane-back', type: 'button', id: 'tmPaneBack', onclick: closePane},
        el('span', {'aria-hidden': 'true', text: '←'}), 'Map'),
      paneMain = el('div', {class: 'tm-pane-main'},
        paneEmpty = el('section', {class: 'tm-pane-empty', hidden: true},
          el('h1', {id: 'tmPaneTitle', tabindex: '-1'}), el('p', {id: 'tmPaneAbout'}),
          el('p', {class: 'tm-empty', text: 'This system has no controls in SAIL yet.'}))),
      trackerHost = el('div', {class: 'tm-pane-tracker', hidden: true}),
      entertainmentHost = el('div', {class: 'tm-pane-entertainment', hidden: true}));
    const activity = el('section', {class: 'tm-activity', 'aria-label': 'Live activity'},
      el('div', {class: 'tm-activity-head'}, el('span', {class: 'tm-bolt', 'aria-hidden': 'true', text: '⚡'}),
        el('div', {}, el('h2', {text: 'Live Activity'}), el('p', {text: 'Real-time events across SAIL'}))),
      el('ol', {class: 'tm-activity-list', id: 'tmActivity'}),
      el('div', {class: 'tm-histo', id: 'tmHisto', 'aria-hidden': 'true'}));

    root.append(nav, top, stage, pane, chat, activity);
    // Inside #appShell so the moved Dialogue panel keeps its state-driven styles.
    (document.getElementById('appShell') || document.body).append(root);
    syncTabs();
    const page = document.getElementById('subsystemPage');
    if (page) paneMain.prepend(page);
    const briefingDeck = document.getElementById('returnBriefingDeck');
    if (briefingDeck) stage.append(briefingDeck);
    const indicators = document.getElementById('topIndicators');
    if (indicators) {
      indicators.hidden = false;
      top.querySelector('#tmTopIndicators')?.append(indicators);
    }
    buildWorld();
    bindViewport();
    document.addEventListener('keydown', (e) => {
      if (!state.on) return;
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); $('#tmAskInput').focus(); }
    });
  }
  function navItem(target, label, ic) {
    const attrs = {class: 'tm-nav-item', type: 'button', 'data-nav': target, onclick: (e) => navTo(target, e.currentTarget)};
    if (target === 'home') attrs.id = 'homeNav';
    return el('li', {}, el('button', attrs, icon(ic), label));
  }
  function modeBtn(mode, label) {
    return el('button', {class: 'tm-seg', type: 'button', 'data-mode': mode, 'aria-pressed': state.mode === mode ? 'true' : 'false', onclick: () => setMode(mode)},
      icon({topology: 'mesh', map: 'spaces', flows: 'flow', resources: 'sense', away: 'away'}[mode]), label,
      mode === 'away' ? el('span', {class: 'tm-seg-dot', id: 'tmAwayDot', hidden: !state.awayFresh, title: 'New briefing'}) : null);
  }

  function buildWorld() {
    const corners = [P(-14, -14), P(40, -14), P(40, 40), P(-14, 40)];
    const xs = corners.map((p) => p[0]), ys = corners.map((p) => p[1]);
    const minX = Math.min(...xs) - 60, minY = Math.min(...ys) - 140, W = Math.max(...xs) - minX + 60, H = Math.max(...ys) - minY + 60;
    state.world = {minX, minY, W, H};
    svgRoot = svg('svg', {class: 'tm-svg', viewBox: `${minX} ${minY} ${W} ${H}`, width: W, height: H, 'aria-hidden': 'true'});
    svgRoot.append(svg('defs', {},
      svg('pattern', {id: 'tmDots', width: 22, height: 22, patternUnits: 'userSpaceOnUse'}, svg('circle', {cx: 2, cy: 2, r: 1, fill: '#3a3f45'})),
      svg('filter', {id: 'tmGlow', x: '-50%', y: '-50%', width: '200%', height: '200%'},
        svg('feGaussianBlur', {stdDeviation: '3', result: 'b'}),
        svg('feMerge', {}, svg('feMergeNode', {in: 'b'}), svg('feMergeNode', {in: 'SourceGraphic'}))),
      svg('radialGradient', {id: 'tmHalo'}, svg('stop', {offset: 0, 'stop-color': '#ff8a32', 'stop-opacity': '.22'}), svg('stop', {offset: 1, 'stop-color': '#ff8a32', 'stop-opacity': '0'}))));
    svgRoot.append(svg('rect', {x: minX, y: minY, width: W, height: H, fill: 'url(#tmDots)'}));
    const [hx, hy] = P(byId.core.gx, byId.core.gy, 0);
    svgRoot.append(svg('ellipse', {cx: hx, cy: hy, rx: 380, ry: 220, fill: 'url(#tmHalo)'}));

    const gLinks = svg('g', {class: 'tm-links'});
    packets = [];
    for (const [a, b] of LINKS) {
      const path = linkPath(byId[a], byId[b]).map(([x, y]) => P(x, y, 0.08));
      const d = 'M' + path.map((p) => p.map((n) => n.toFixed(1)).join(' ')).join(' L');
      const grp = svg('g', {class: 'tm-link', 'data-a': a, 'data-b': b});
      grp.append(svg('path', {class: 'l-pipe', d}), svg('path', {class: 'l-core', d}));
      const [jx, jy] = path[1];
      grp.append(svg('rect', {class: 'l-junction', x: jx - 6, y: jy - 4, width: 12, height: 8, rx: 2}));
      gLinks.append(grp);
      const seg = []; let total = 0;
      for (let i = 1; i < path.length; i++) { const len = Math.hypot(path[i][0] - path[i - 1][0], path[i][1] - path[i - 1][1]); seg.push(len); total += len; }
      for (let n = 0; n < 2; n++) {
        const dot = svg('circle', {class: 'tm-packet', r: 3});
        gLinks.append(dot);
        packets.push({dot, path, seg, total, t: (n / 2 + ((a.length * 7 + b.length * 3) % 10) / 30) % 1, dir: n % 2 ? -1 : 1, a, b});
      }
    }
    svgRoot.append(gLinks);

    const gObj = svg('g', {class: 'tm-objects'});
    const objects = [];
    objects.push({depth: 0, draw: (g) => { cylinder(g, 0.3, 2.3, 0, 0.9, 2.2, 'silo'); cylinder(g, 2.5, 2.0, 0, 0.8, 1.8, 'silo'); }});
    for (const [u, v] of [[-6, 4], [5, 2.6], [-3.4, 10.6], [1, 11.4], [-4.6, 15.6], [6.6, 13.2], [-2.8, 18.6], [1.6, 24.4], [-7.8, 21.6]]) {
      const gx = (u + v) / 2, gy = (v - u) / 2;
      objects.push({depth: gx + gy, draw: (g) => { box(g, gx - 0.25, gy - 0.25, 0, 0.5, 0.5, 0.25, 'machine alt'); cylinder(g, gx, gy, 0.25, 0.08, 0.9, 'pipe'); box(g, gx - 0.12, gy - 0.12, 1.15, 0.24, 0.24, 0.12, 'led on'); }});
    }
    for (const s of SLABS) {
      const gx = (s.u + s.v) / 2, gy = (s.v - s.u) / 2;
      objects.push({depth: gx + gy, draw: (g) => {
        box(g, gx - s.w / 2, gy - s.d / 2, 0, s.w, s.d, s.h, 'slab');
        const [tx, ty] = P(gx, gy, s.h);
        // Lay the label on the slab's top face, running along its long side.
        const m = s.d > s.w ? `matrix(${C},${-N},${C},${N},${tx},${ty})` : `matrix(${C},${N},${-C},${N},${tx},${ty})`;
        g.append(svg('text', {class: 'tm-slab-label', transform: m, 'text-anchor': 'middle', 'dominant-baseline': 'middle'}, document.createTextNode(s.label)));
      }});
    }
    STATIONS.forEach((s, i) => objects.push({depth: s.gx + s.gy, draw: (g) => drawStation(g, s, i)}));
    objects.sort((a, b) => a.depth - b.depth).forEach((o) => o.draw(gObj));
    svgRoot.append(gObj);
    world.append(svgRoot);

    const labels = el('div', {class: 'tm-labels'});
    for (const s of STATIONS) {
      const [lx, ly] = P(s.gx, s.gy, s.kind === 'core' ? 2.7 : 2.9);
      labels.append(el('button', {class: `tm-label${s.kind === 'core' ? ' core' : ''}`, type: 'button', 'data-station': s.id, 'aria-pressed': 'false',
        style: `left:${lx - minX}px;top:${ly - minY}px`, 'aria-controls': 'tmDetail',
        // First selection shows the status window; selecting it again opens the system
        // (the only way in on phones, where the sidebar is hidden).
        onclick: (e) => { if (state.selected === s.id) openStation(s.id, e.currentTarget); else select(s.id); }},
        s.kind === 'core' ? el('span', {class: 'tm-status', 'data-status': s.id}) : el('span', {class: 'tm-label-icon'}, icon(s.kind === 'forge' ? 'forge' : s.icon)),
        el('span', {class: 'tm-label-text'},
          el('strong', {}, s.name, s.kind !== 'core' ? el('span', {class: 'tm-status small', 'data-status': s.id}) : null),
          el('span', {class: 'tm-label-sub', 'data-sub': s.id, text: s.sub}))));
    }
    labels.append(el('div', {class: 'tm-detail', id: 'tmDetail', hidden: true}));
    world.append(labels);
    world.style.width = `${W}px`; world.style.height = `${H}px`;
  }

  // ------------------------------------------------------------------ viewport
  function applyView() {
    world.style.transform = `translate(${state.tx}px, ${state.ty}px) scale(${state.k})`;
    // Labels keep a readable size as the map zooms out.
    world.style.setProperty('--ls', labelScale().toFixed(3));
  }
  function labelScale() { return Math.max(state.k, innerWidth < 860 ? 0.62 : 0.95) / state.k; }
  function fit() {
    const vp = $('#tmViewport'); if (!vp || !state.world || !vp.clientWidth) return;
    const xs = STATIONS.flatMap((s) => [P(s.gx - 3, s.gy + 3)[0], P(s.gx + 3, s.gy - 3)[0]]);
    const ys = STATIONS.flatMap((s) => [P(s.gx - 3, s.gy - 3, 3.2)[1], P(s.gx + 3, s.gy + 3)[1]]);
    const bx0 = Math.min(...xs) - state.world.minX, bx1 = Math.max(...xs) - state.world.minX;
    const by0 = Math.min(...ys) - state.world.minY, by1 = Math.max(...ys) - state.world.minY;
    const w = vp.clientWidth, h = vp.clientHeight;
    if (!w || !h) return;
    state.k = Math.min(w / (bx1 - bx0), h / (by1 - by0)) * 0.98;
    state.tx = (w - (bx1 - bx0) * state.k) / 2 - bx0 * state.k;
    state.ty = (h - (by1 - by0) * state.k) / 2 - by0 * state.k;
    applyView();
  }
  function zoom(f, cx, cy) {
    const vp = $('#tmViewport');
    cx ??= vp.clientWidth / 2; cy ??= vp.clientHeight / 2;
    const k = Math.max(0.3, Math.min(2.5, state.k * f));
    state.tx = cx - (cx - state.tx) * (k / state.k);
    state.ty = cy - (cy - state.ty) * (k / state.k);
    state.k = k; applyView();
  }
  function bindViewport() {
    const vp = $('#tmViewport');
    let drag = null;
    vp.addEventListener('pointerdown', (e) => { if (e.target.closest('button')) return; drag = {x: e.clientX, y: e.clientY}; vp.setPointerCapture(e.pointerId); vp.classList.add('grabbing'); });
    vp.addEventListener('pointermove', (e) => { if (!drag) return; state.tx += e.clientX - drag.x; state.ty += e.clientY - drag.y; drag = {x: e.clientX, y: e.clientY}; applyView(); });
    const end = () => { drag = null; vp.classList.remove('grabbing'); };
    vp.addEventListener('pointerup', end); vp.addEventListener('pointercancel', end);
    vp.addEventListener('wheel', (e) => { e.preventDefault(); const r = vp.getBoundingClientRect(); zoom(e.deltaY < 0 ? 1.1 : 1 / 1.1, e.clientX - r.left, e.clientY - r.top); }, {passive: false});
    vp.addEventListener('keydown', (e) => {
      const m = {ArrowLeft: [60, 0], ArrowRight: [-60, 0], ArrowUp: [0, 60], ArrowDown: [0, -60]}[e.key];
      if (m && e.target === vp) { e.preventDefault(); state.tx += m[0]; state.ty += m[1]; applyView(); }
      else if (e.key === '+' || e.key === '=') zoom(1.2); else if (e.key === '-') zoom(1 / 1.2);
    });
    new ResizeObserver(() => { if (state.on) fit(); }).observe(vp);
  }

  // ------------------------------------------------------------------ animation
  let lastFrame = 0;
  function frame(ts) {
    if (!state.on) return;
    const dt = lastFrame ? Math.min(0.1, (ts - lastFrame) / 1000) : 0;
    lastFrame = ts;
    const still = state.paused || reduceMotion.matches;
    for (const p of packets) {
      if (!still) p.t = (p.t + p.dir * dt * state.speed * (state.mode === 'flows' ? 0.45 : 0.22) * (90 / Math.max(60, p.total)) + 1) % 1;
      let dist = p.t * p.total, i = 0;
      while (i < p.seg.length - 1 && dist > p.seg[i]) { dist -= p.seg[i]; i++; }
      const a = p.path[i], b = p.path[i + 1], f = p.seg[i] ? Math.min(1, dist / p.seg[i]) : 0;
      p.dot.setAttribute('cx', (a[0] + (b[0] - a[0]) * f).toFixed(1));
      p.dot.setAttribute('cy', (a[1] + (b[1] - a[1]) * f).toFixed(1));
    }
    requestAnimationFrame(frame);
  }

  // ------------------------------------------------------------------ data
  async function token() {
    if (typeof getOrFetchOwnerToken === 'function') return getOrFetchOwnerToken();
    try { return localStorage.getItem('sail_owner_token'); } catch (_) { return null; }
  }
  async function get(path) {
    const t = await token();
    const started = performance.now();
    const r = await fetch(path, {cache: 'no-store', headers: t ? {Authorization: `Bearer ${t}`} : {}});
    const ms = Math.round(performance.now() - started);
    for (const s of STATIONS) if (s.endpoint === path) state.latency[s.id] = r.ok ? ms : null;
    if (!r.ok) throw new Error(`${path} ${r.status}`);
    return r.json();
  }
  function log(station, text, at = new Date(), key) {
    if (key) { if (state.seen.has(key)) return; state.seen.add(key); }
    state.activity.push({at, station, text});
    state.activity.sort((a, b) => b.at - a.at);
    if (state.activity.length > 200) state.activity.length = 200;
  }
  function push(arr, v) { arr.push(v); if (arr.length > 40) arr.shift(); }

  const AUDIT_STATION = {core: 'core', dialogue: 'dialogue', arena: 'arena', canvas: 'canvas', flow: 'flow', discovery: 'discover', agents: 'forge', mind: 'mind', bridge: 'system', guardian: 'guardian', forge: 'forge', ui: 'core'};
  function capabilityText(c) {
    if (c == null) return 'action';
    if (typeof c === 'string') return c;
    const parts = [];
    const walk = (v) => { if (v && typeof v === 'object') for (const [k, x] of Object.entries(v)) { parts.push(k); if (typeof x === 'string') parts.push(x); else walk(x); } };
    walk(c);
    return parts.slice(0, 3).join(' · ') || 'action';
  }

  async function poll() {
    if (!state.on || !state.live) return;
    const jobs = {
      metrics: '/api/system/metrics', mind: '/api/mind/status', health: '/api/health',
      guardian: '/api/guardian/status', audit: '/api/guardian/audit', approvals: '/api/guardian/approvals',
      plans: '/api/plans/', reminders: '/api/pulse/reminders', tracker: '/api/tracker/config',
      spotify: '/api/spotify/status',
    };
    const res = {};
    await Promise.all(Object.entries(jobs).map(([k, path]) => get(path).then((v) => { res[k] = v; }, () => { res[k] = null; })));
    state.spotify = res.spotify;
    renderNowPlaying();
    const was = state.online;
    state.online = !!res.health || !!res.metrics;
    if (was !== null && was !== state.online) log('core', state.online ? 'Connection to SAIL restored' : 'Lost connection to SAIL');
    if (res.metrics) {
      const m = res.metrics; state.metrics = m;
      if (m.version) state.version = m.version;
      const g = m.gpus?.[0];
      push(state.history.cpu, m.cpu_percent ?? 0);
      push(state.history.gpu, g?.busy_percent ?? 0);
      push(state.history.ram, m.ram_total_bytes ? (m.ram_used_bytes / m.ram_total_bytes) * 100 : 0);
      push(state.history.io, (m.io_bytes_per_sec ?? 0) / 1048576);
      for (const d of m.disks || []) {
        const p = d.used_bytes / d.total_bytes * 100;
        if (p >= 95) log('system', `Disk ${d.path} is ${Math.round(p)}% full`, new Date(), `disk-${d.path}-${new Date().toDateString()}`);
      }
    }
    if (res.mind) {
      state.mind = res.mind;
      for (const md of res.mind.models || []) {
        const prev = state.modelStates[md.model], now = modelState(md);
        if (prev !== undefined && prev !== now) log('mind', `${md.model.split('/').pop()}: ${prev} → ${now}`);
        state.modelStates[md.model] = now;
      }
    }
    state.health = res.health;
    state.guardian = res.guardian;
    if (res.approvals) {
      const list = Array.isArray(res.approvals) ? res.approvals : (res.approvals.approvals || res.approvals.pending || []);
      const n = list.length || 0;
      if (n > state.approvals) log('guardian', `${n} action${n === 1 ? '' : 's'} awaiting your approval`);
      state.approvals = n;
    }
    if (res.audit) {
      for (const a of (res.audit.audit || []).slice().reverse()) {
        log(AUDIT_STATION[a.requester] || 'core', `${capabilityText(a.capability)} — ${a.outcome}${a.reason ? `: ${a.reason}` : ''}`, new Date(a.timestamp), `audit-${a.receipt_id}`);
      }
    }
    if (res.plans) {
      state.plans = res.plans.plans || [];
      for (const pv of state.plans) {
        const p = pv.plan, prev = state.planStates[p.id];
        if (prev !== undefined && prev !== p.state) log('forge', `Plan “${p.goal}”: ${prev.replace('_', ' ')} → ${p.state.replace('_', ' ')}`);
        state.planStates[p.id] = p.state;
      }
    }
    if (res.reminders) state.reminders = res.reminders.reminders || [];
    if (!state.activity.length) log('core', 'Tools Map connected');
    render();
  }
  let pollTimer = null;
  function startPolling() { stopPolling(); poll(); pollTimer = setInterval(poll, 5000); }
  function stopPolling() { clearInterval(pollTimer); pollTimer = null; }

  // ------------------------------------------------------------------ station status
  function stationStatus(id) {
    if (!state.online) return 'off';
    switch (id) {
      case 'core': case 'system': return state.metrics ? 'on' : 'off';
      case 'mind': {
        const models = state.mind?.models || [];
        if (models.some((m) => m.last_error)) return 'warn';
        return models.some((m) => /ready|loaded|running/i.test(modelState(m))) ? 'on' : (state.mind ? 'idle' : 'off');
      }
      case 'dialogue': return state.health?.speech ? 'on' : 'idle';
      case 'guardian': return state.approvals ? 'warn' : state.guardian ? 'on' : 'idle';
      case 'forge': return state.plans.some((p) => p.plan.state === 'running') ? 'on' : state.plans.some((p) => p.plan.state === 'awaiting_approval') ? 'warn' : 'idle';
      case 'flow': return state.reminders.length ? 'on' : 'idle';
      case 'discover': return state.latency.discover != null ? 'on' : 'idle';
      default: return 'idle';
    }
  }
  function resourceLine(id) {
    const m = state.metrics, g = m?.gpus?.[0];
    switch (id) {
      case 'core': return `CPU ${pct(m?.cpu_percent)} · load ${m?.load1?.toFixed(2) ?? '—'}`;
      case 'system': return `RAM ${bytes(m?.ram_used_bytes)}/${bytes(m?.ram_total_bytes)} GB`;
      case 'mind': { const md = state.mind?.models?.[0]; return md ? `${md.model.split('/').pop().slice(0, 18)} · ${modelState(md)}` : 'No model status'; }
      case 'guardian': return `${state.approvals} pending approval${state.approvals === 1 ? '' : 's'}`;
      case 'forge': return `${state.plans.filter((p) => p.plan.state === 'running').length} running · ${state.plans.filter((p) => p.plan.state === 'awaiting_approval').length} awaiting`;
      case 'flow': return `${state.reminders.length} reminder${state.reminders.length === 1 ? '' : 's'}`;
      case 'vision': return g ? `GPU ${pct(g.busy_percent)}` : 'GPU unavailable';
      default: return state.latency[id] != null ? `${state.latency[id]} ms` : byId[id].sub;
    }
  }
  function stateWord(st) { return {on: 'active', warn: 'attention', idle: 'idle', off: 'offline'}[st] || st; }

  // ------------------------------------------------------------------ render
  function render() {
    if (!state.on) return;
    $('#tmVersion').textContent = state.version ? `v${state.version}` : 'local';
    renderMeters();
    for (const s of STATIONS) {
      const st = stationStatus(s.id);
      for (const n of root.querySelectorAll(`[data-status="${s.id}"]`)) n.className = `tm-status ${n.classList.contains('small') ? 'small ' : ''}${st}`;
      const sub = root.querySelector(`[data-sub="${s.id}"]`);
      if (sub) sub.textContent = state.mode === 'resources' ? resourceLine(s.id) : s.sub;
      const lab = root.querySelector(`.tm-label[data-station="${s.id}"]`);
      lab?.setAttribute('aria-label', `${s.name}, ${stateWord(st)}. ${sub?.textContent || ''}`);
      svgRoot.querySelector(`[data-station="${s.id}"]`)?.classList.toggle('active', st === 'on' || st === 'warn');
      const lit = s.id === 'core' || s.id === 'system' ? Math.ceil((state.metrics?.cpu_percent ?? 0) / 20) : st === 'on' ? 3 : st === 'warn' ? 5 : 1;
      svgRoot.querySelectorAll(`[data-leds="${s.id}"] .tm-box`).forEach((b, i) => b.classList.toggle('on', i < lit));
    }
    for (const l of svgRoot.querySelectorAll('.tm-link')) {
      l.classList.toggle('live', [l.dataset.a, l.dataset.b].every((id) => stationStatus(id) !== 'off'));
      l.classList.toggle('hot', !!state.selected && (l.dataset.a === state.selected || l.dataset.b === state.selected));
    }
    for (const p of packets) p.dot.style.display = stationStatus(p.a) === 'off' ? 'none' : '';
    $('#tmCoreDot').className = `tm-status ${state.online ? 'on' : state.online === false ? 'off' : 'idle'}`;
    $('#tmCoreState').textContent = state.online ? 'SAIL Core online' : state.online === false ? 'SAIL Core unreachable' : 'Connecting…';
    const core = state.online ? 'on' : state.online === false ? 'off' : 'idle';
    $('#tmChatPill').className = `tm-pill ${core}`;
    $('#tmChatPill').replaceChildren(el('span', {class: `tm-status ${core}`}), state.online ? 'Online' : state.online === false ? 'Offline' : 'Connecting');
    const up = state.metrics?.uptime_secs;
    $('#tmUptime').textContent = up ? `Host up ${Math.floor(up / 86400)}d ${Math.floor((up % 86400) / 3600)}h` : '';
    const bell = $('#tmBellCount');
    bell.hidden = !state.approvals; bell.textContent = state.approvals || '';
    $('#tmBell').setAttribute('aria-label', state.approvals ? `${state.approvals} approvals waiting` : 'Approvals: none waiting');
    renderDetail();
    renderInspector();
    renderActivity();
  }

  function meter(label, value, sub, p) {
    return el('li', {class: `tm-meter ${level(p)}`},
      el('span', {class: 'tm-meter-label', text: label}),
      el('span', {class: 'tm-meter-value'}, el('span', {class: 'tm-meter-bar', 'aria-hidden': 'true'}, el('span', {style: `width:${Math.max(0, Math.min(100, p ?? 0))}%`})), value),
      sub ? el('span', {class: 'tm-meter-sub', text: sub}) : null);
  }
  function renderMeters() {
    const m = state.metrics, g = m?.gpus?.[0];
    const disk = (m?.disks || []).slice().sort((a, b) => b.used_bytes / b.total_bytes - a.used_bytes / a.total_bytes)[0];
    const diskP = disk ? disk.used_bytes / disk.total_bytes * 100 : null;
    const unit = disk && disk.total_bytes >= 1099511627776 ? 'TB' : 'GB';
    $('#tmMeters').replaceChildren(
      meter('CPU', pct(m?.cpu_percent), null, m?.cpu_percent),
      meter('GPU', g ? pct(g.busy_percent) : 'n/a', null, g?.busy_percent),
      meter('RAM', m ? `${bytes(m.ram_used_bytes)} / ${bytes(m.ram_total_bytes)} GB` : '—', null, m?.ram_total_bytes ? m.ram_used_bytes / m.ram_total_bytes * 100 : null),
      meter('Disk', disk ? `${bytes(disk.used_bytes, unit)} / ${bytes(disk.total_bytes, unit)} ${unit}` : '—', disk ? `fullest: ${disk.path}` : null, diskP));
  }

  function row(k, v) { return el('div', {}, el('dt', {text: k}), el('dd', {text: v})); }
  function renderDetail() {
    const box = $('#tmDetail');
    const s = state.selected && byId[state.selected];
    if (!s) { box.hidden = true; return; }
    const [ax, ay] = P(s.gx, s.gy, 2.9);
    const x = ax - state.world.minX, y = ay - state.world.minY;
    const m = state.metrics, g = m?.gpus?.[0], st = stationStatus(s.id);
    const running = state.plans.filter((p) => p.plan.state === 'running').length;
    const waiting = state.plans.filter((p) => p.plan.state === 'awaiting_approval').length;
    box.hidden = false;
    box.replaceChildren(
      el('p', {class: 'tm-detail-title'}, el('span', {class: `tm-status ${st}`}), s.name, ' ', el('span', {class: `tm-detail-state ${st}`, text: `(${stateWord(st)})`})),
      el('dl', {},
        row('Tasks', s.id === 'forge' ? String(state.plans.length) : '—'),
        row('Queue', s.id === 'forge' ? String(waiting + running) : s.id === 'guardian' ? String(state.approvals) : '—'),
        row('CPU', pct(m?.cpu_percent)), row('GPU', g ? pct(g.busy_percent) : 'n/a'), row('RAM', m ? `${bytes(m.ram_used_bytes)} GB` : '—')));
    placeDetail(box, s, x, y);
  }
  // World-space rectangle of a label (anchored bottom-centre, scaled by --ls).
  function labelRect(n, ls) {
    const w = n.offsetWidth * ls, h = n.offsetHeight * ls, x = parseFloat(n.style.left), y = parseFloat(n.style.top);
    return {l: x - w / 2, t: y - h, r: x + w / 2, b: y};
  }
  // Prefer a position clear of other labels; keep status available on crowded maps.
  function placeDetail(box, s, x, y) {
    const ls = labelScale();
    const own = root.querySelector(`.tm-label[data-station="${s.id}"]`);
    const others = [...root.querySelectorAll('.tm-label')].filter((n) => n !== own).map((n) => labelRect(n, ls));
    const me = labelRect(own, ls);
    const w = box.offsetWidth * ls, h = box.offsetHeight * ls, gap = 8 * ls;
    const spots = [
      [x - w * 0.18, me.t - gap - h], [me.r + gap, me.b - h], [me.l - gap - w, me.b - h], [x - w / 2, me.b + gap],
      [me.r + gap, me.t - gap - h], [me.l - gap - w, me.t - gap - h],
      [me.r + gap, me.b + gap], [me.l - gap - w, me.b + gap],
    ];
    const free = spots.find(([l, t]) => others.every((o) => l + w <= o.l || o.r <= l || t + h <= o.t || o.b <= t)) || spots[0];
    box.style.left = `${free[0]}px`; box.style.top = `${free[1]}px`;
  }

  function sparkline(values, max) {
    const s = svg('svg', {class: 'tm-spark', viewBox: '0 0 120 20', preserveAspectRatio: 'none', 'aria-hidden': 'true'});
    if (values.length < 2) return s;
    const hi = max ?? Math.max(1, ...values);
    s.append(svg('polyline', {points: values.map((v, i) => `${(i / (values.length - 1) * 120).toFixed(1)},${(19 - Math.min(1, v / hi) * 18).toFixed(1)}`).join(' ')}));
    return s;
  }
  // Status of the open system (SAIL Core on the map), shown in the column's Context tab.
  function renderInspector() {
    if (state.tab !== 'context') return;
    const s = byId[state.popup || 'core'];
    const st = stationStatus(s.id);
    const insp = $('#tmInspector');
    const prevBody = insp.querySelector('.tm-insp-body');
    const scroll = prevBody?.scrollTop || 0;
    const hadFocus = insp.contains(document.activeElement) ? document.activeElement : null;
    const focusKey = hadFocus && (hadFocus.id || hadFocus.textContent);
    insp.replaceChildren(
      el('header', {class: 'tm-insp-head'},
        el('span', {class: 'tm-insp-icon'}, s.kind === 'core' ? logo() : icon(s.kind === 'forge' ? 'forge' : s.icon)),
        el('h3', {text: s.name}),
        el('span', {class: `tm-pill ${st}`}, el('span', {class: `tm-status ${st}`}), stateWord(st).replace(/^./, (c) => c.toUpperCase()))),
      el('div', {class: 'tm-subtabs', role: 'tablist', 'aria-label': `${s.name} context`},
        INFO_TABS.map((t) => el('button', {class: 'tm-subtab', type: 'button', role: 'tab', id: `tmInfo-${t}`, 'aria-controls': 'tmInfoPanel',
          'aria-selected': String(state.info === t), tabindex: state.info === t ? '0' : '-1',
          onclick: () => setTab(t), onkeydown: (e) => tabKey(e, INFO_TABS, state.info, 'tmInfo')}, t[0].toUpperCase() + t.slice(1)))),
      el('div', {class: 'tm-insp-body', id: 'tmInfoPanel', role: 'tabpanel', 'aria-labelledby': `tmInfo-${state.info}`}, tabBody(s)));
    const body = insp.querySelector('.tm-insp-body'); body.scrollTop = scroll;
    // Live refreshes rebuild the panel; keep keyboard focus on the same control.
    if (focusKey) [...insp.querySelectorAll('button')].find((b) => (b.id || b.textContent) === focusKey)?.focus({preventScroll: true});
  }
  function tabBody(s) {
    const m = state.metrics, g = m?.gpus?.[0];
    if (state.info === 'overview') {
      const conns = LINKS.filter(([a, b]) => a === s.id || b === s.id).map(([a, b]) => byId[a === s.id ? b : a]);
      return [
        el('p', {class: 'tm-about', text: s.about}),
        el('section', {class: 'tm-card'}, el('div', {class: 'tm-card-row'}, el('h3', {text: 'Live Metrics'}), el('span', {class: 'tm-card-note', text: 'host-wide'})),
          el('ul', {class: 'tm-metrics'},
            metricRow('CPU', pct(m?.cpu_percent), state.history.cpu, 100),
            metricRow('GPU', g ? pct(g.busy_percent) : 'n/a', state.history.gpu, 100),
            metricRow('RAM', m ? `${bytes(m.ram_used_bytes)} / ${bytes(m.ram_total_bytes)} GB` : '—', state.history.ram, 100),
            metricRow('IO', m?.io_bytes_per_sec != null ? `${(m.io_bytes_per_sec / 1048576).toFixed(1)} MB/s` : '—', state.history.io))),
        el('section', {class: 'tm-card'}, el('h3', {text: `Active Connections (${conns.length})`}),
          el('ul', {class: 'tm-conns'}, conns.map((c) => {
            const cst = stationStatus(c.id);
            return el('li', {}, el('span', {class: `tm-status ${cst}`}),
              el('button', {class: 'tm-link-btn', type: 'button', onclick: (e) => openStation(c.id, e.currentTarget), text: c.name}),
              el('span', {class: 'tm-conn-sub', text: ` — ${c.sub.split(' · ')[0]}`}),
              el('span', {class: 'tm-ms', text: state.latency[c.id] != null ? `${state.latency[c.id]} ms` : cst === 'off' ? 'offline' : 'local'}));
          }))),
        taskCard(4),
      ];
    }
    if (state.info === 'tasks') return [taskCard(20), reminderCard()];
    if (state.info === 'logs') {
      const rows = state.activity.filter((a) => a.station === s.id || s.id === 'core').slice(0, 60);
      return el('section', {class: 'tm-card'}, el('h3', {text: 'Recent events'}),
        rows.length ? el('ol', {class: 'tm-log'}, rows.map((a) => el('li', {}, el('time', {text: hhmmss(a.at)}), el('span', {text: a.text})))) : el('p', {class: 'tm-empty', text: 'No events from this subsystem yet.'}));
    }
    return configCard(s);
  }
  function metricRow(k, v, hist, max) {
    return el('li', {}, el('span', {class: 'tm-metric-k', text: k}), el('span', {class: 'tm-metric-v', text: v}), sparkline(hist, max));
  }
  function taskCard(limit) {
    const plans = state.plans.slice(0, limit);
    return el('section', {class: 'tm-card'},
      el('div', {class: 'tm-card-row'}, el('h3', {text: 'Task Queue'}),
        state.info === 'overview' && state.plans.length ? el('button', {class: 'tm-link-btn', type: 'button', onclick: () => setTab('tasks'), text: 'View All'}) : null),
      plans.length ? el('ul', {class: 'tm-tasks'}, plans.map((pv) => {
        const p = pv.plan, done = p.steps.filter((x) => x.state === 'done').length, total = p.steps.length || 1;
        const progress = p.state === 'succeeded' ? 100 : Math.round(done / total * 100);
        const label = {awaiting_approval: 'Awaiting approval', running: `${progress}%`, succeeded: '100%', failed: 'Failed', cancelled: 'Cancelled'}[p.state] || p.state;
        return el('li', {class: `tm-task ${p.state}`},
          el('span', {class: 'tm-task-icon', 'aria-hidden': 'true', text: p.state === 'succeeded' ? '✓' : p.state === 'failed' ? '!' : '▣'}),
          el('div', {class: 'tm-task-main'}, el('span', {class: 'tm-task-goal', text: p.goal}),
            p.state === 'running' ? el('span', {class: 'tm-progress', role: 'progressbar', 'aria-valuenow': progress, 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-label': `${p.goal} progress`}, el('span', {style: `width:${progress}%`})) : null),
          el('span', {class: 'tm-task-state', text: label}));
      })) : el('p', {class: 'tm-empty', text: 'No tasks. Ask SAIL “Plan: …” to start one.'}));
  }
  function reminderCard() {
    const rs = state.reminders.slice(0, 8);
    const when = (r) => { const t = r.due_at || r.fire_at || r.at; return t ? new Date(t).toLocaleString([], {hour12: false, month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'}) : ''; };
    return el('section', {class: 'tm-card'}, el('h3', {text: 'Scheduled (Flow)'}),
      rs.length ? el('ol', {class: 'tm-log'}, rs.map((r) => el('li', {}, el('time', {text: when(r)}), el('span', {text: r.title || r.text || r.message || 'Reminder'})))) : el('p', {class: 'tm-empty', text: 'Nothing scheduled.'}));
  }
  function configCard(s) {
    const items = [];
    if (s.id === 'mind') for (const md of state.mind?.models || []) {
      items.push(['Model', md.model.split('/').pop()], ['State', modelState(md)], ['Backend', String(md.observed_backend || md.requested_backend || '—')]);
      if (md.active_profile) items.push(['Context', String(md.active_profile.context_tokens)], ['Device', md.active_profile.device || '—']);
    }
    if (s.id === 'system' || s.id === 'core') {
      const m = state.metrics;
      items.push(['Cores', String(m?.cpu_cores ?? '—')], ['Load (1m)', m?.load1?.toFixed(2) ?? '—'], ['Swap', m ? `${bytes(m.swap_used_bytes)} / ${bytes(m.swap_total_bytes)} GB` : '—']);
      for (const d of m?.disks || []) items.push([`Disk ${d.path}`, `${pct(d.used_bytes / d.total_bytes * 100)} of ${bytes(d.total_bytes)} GB`]);
      for (const g of m?.gpus || []) items.push([`GPU ${g.card}`, g.vram_total_bytes ? `VRAM ${bytes(g.vram_used_bytes)} / ${bytes(g.vram_total_bytes)} GB` : pct(g.busy_percent)]);
    }
    if (s.id === 'guardian' && state.guardian) for (const [k, v] of Object.entries(state.guardian).slice(0, 8)) if (typeof v !== 'object') items.push([k.replace(/_/g, ' '), String(v)]);
    return el('section', {class: 'tm-card'}, el('h3', {text: 'Configuration'}),
      items.length ? el('dl', {class: 'tm-config'}, items.map(([k, v]) => row(k, v))) : el('p', {class: 'tm-empty', text: 'Settings for this subsystem live in SAIL Settings.'}),
      el('button', {class: 'tm-btn', type: 'button', onclick: openSettings}, icon('settings'), 'Open Settings'));
  }

  // ------------------------------------------------------------------ Now Playing (Spotify)
  // The owner's Spotify player. SAIL plays music as soon as it is asked for in the
  // conversation; this card shows what is playing and offers the basic controls.
  function renderNowPlaying() {
    const box = $('#tmNowPlaying'); if (!box) return;
    const sp = state.spotify;
    box.hidden = !sp;
    if (!sp) return;
    const focused = box.contains(document.activeElement) ? document.activeElement.dataset.key : null;
    const now = sp.now_playing;
    const head = (text) => el('p', {class: 'tm-np-label'}, el('span', {class: 'tm-np-mark', 'aria-hidden': 'true', text: '●'}), text);
    let kids;
    if (!sp.configured) {
      kids = [head('Spotify'), el('p', {class: 'tm-np-note', text: 'Let SAIL play your music and playlists.'}),
        el('button', {class: 'tm-btn tm-np-action', type: 'button', 'data-key': 'setup', onclick: openSpotifySetup, text: 'Set up Spotify'})];
    } else if (!sp.connected) {
      kids = [head('Spotify'), el('p', {class: 'tm-np-note', text: 'Sign in to Spotify to play music.'}),
        el('button', {class: 'tm-btn tm-np-action', type: 'button', 'data-key': 'connect', onclick: connectSpotify, text: 'Connect Spotify'})];
    } else if (!now) {
      kids = [head('Spotify'), el('p', {class: 'tm-np-note', text: sp.error || 'Nothing playing. Ask SAIL to play something.'})];
    } else {
      const btn = (key, label, glyph) => el('button', {class: 'tm-np-btn', type: 'button', 'data-key': key, 'aria-label': label, title: label, onclick: () => spotifyControl(key)}, glyph);
      kids = [
        el('div', {class: 'tm-np-row'},
          now.image ? el('img', {class: 'tm-np-art', src: now.image, alt: '', width: 44, height: 44, crossorigin: 'anonymous', referrerpolicy: 'no-referrer'}) : el('span', {class: 'tm-np-art', 'aria-hidden': 'true'}),
          el('div', {class: 'tm-np-text'},
            el('p', {class: 'tm-np-label', text: now.is_playing ? 'Now Playing' : 'Paused'}),
            el('p', {class: 'tm-np-name', text: now.name, title: now.name}),
            el('p', {class: 'tm-np-by', text: now.by, title: now.device ? `${now.by} · on ${now.device}` : now.by}))),
        el('div', {class: 'tm-np-ctl', role: 'group', 'aria-label': 'Playback'},
          btn('previous', 'Previous', '⏮'),
          now.is_playing ? btn('pause', 'Pause', '❚❚') : btn('resume', 'Play', '▶'),
          btn('next', 'Next', '⏭')),
      ];
    }
    box.replaceChildren(...kids);
    if (focused) box.querySelector(`[data-key="${focused}"]`)?.focus({preventScroll: true})
      || (['pause', 'resume'].includes(focused) && box.querySelector('[data-key="pause"],[data-key="resume"]')?.focus({preventScroll: true}));
  }
  async function spotifyPost(path, body) {
    const t = await token();
    const r = await fetch(`/api/spotify/${path}`, {method: 'POST', cache: 'no-store',
      headers: {'Content-Type': 'application/json', ...(t ? {Authorization: `Bearer ${t}`} : {})}, body: JSON.stringify(body || {})});
    const data = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(data.error || `Spotify request failed (${r.status})`);
    return data;
  }
  async function spotifyControl(action) {
    try {
      const data = await spotifyPost('control', {action});
      state.spotify = {...state.spotify, now_playing: data.now_playing, error: null};
    } catch (e) {
      state.spotify = {...state.spotify, error: e.message};
      log('dialogue', `Spotify: ${e.message}`);
    }
    renderNowPlaying();
  }
  async function connectSpotify() {
    // Open the tab first so the browser treats it as a user action, then point it at Spotify.
    const tab = window.open('about:blank', '_blank');
    try {
      const {url} = await spotifyPost('login');
      if (tab) tab.location.href = url; else location.assign(url);
    } catch (e) {
      tab?.close();
      log('dialogue', `Spotify: ${e.message}`);
      state.spotify = {...state.spotify, error: e.message}; renderNowPlaying();
    }
  }
  function openSpotifySetup() {
    let dlg = $('#tmSpotifySetup');
    const redirect = state.spotify?.redirect_uri || `${location.origin}/api/spotify/callback`;
    if (!dlg) {
      dlg = el('dialog', {class: 'tm-dialog', id: 'tmSpotifySetup', 'aria-labelledby': 'tmSpotifySetupTitle'},
        el('form', {method: 'dialog', class: 'tm-dialog-body', onsubmit: saveSpotifySetup},
          el('h2', {id: 'tmSpotifySetupTitle', text: 'Set up Spotify'}),
          el('ol', {class: 'tm-steps'},
            el('li', {}, 'Create an app at ', el('a', {href: 'https://developer.spotify.com/dashboard', target: '_blank', rel: 'noopener', text: 'developer.spotify.com/dashboard'}), ' and select Web API.'),
            el('li', {}, 'Add this Redirect URI to the app:',
              el('div', {class: 'tm-copy'}, el('code', {id: 'tmSpotifyRedirect'}),
                el('button', {class: 'tm-btn', type: 'button', onclick: () => navigator.clipboard?.writeText($('#tmSpotifyRedirect').textContent), text: 'Copy'}))),
            el('li', {}, el('label', {for: 'tmSpotifyClient', text: 'Paste the app’s Client ID:'}),
              el('input', {id: 'tmSpotifyClient', class: 'tm-input', required: true, autocomplete: 'off', spellcheck: 'false', minlength: '16', maxlength: '64', pattern: '[A-Za-z0-9]+'}))),
          el('p', {class: 'tm-np-note', text: 'Playback control needs Spotify Premium and an open Spotify app or speaker.'}),
          el('p', {class: 'tm-form-error', id: 'tmSpotifyError', role: 'alert'}),
          el('div', {class: 'tm-dialog-actions'},
            el('button', {class: 'tm-btn', type: 'button', onclick: () => dlg.close(), text: 'Cancel'}),
            el('button', {class: 'tm-btn tm-btn-primary', type: 'submit', value: 'save', text: 'Save and connect'}))));
      root.append(dlg);
    }
    $('#tmSpotifyRedirect').textContent = redirect;
    $('#tmSpotifyError').textContent = '';
    dlg.showModal();
    $('#tmSpotifyClient').focus();
  }
  async function saveSpotifySetup(e) {
    e.preventDefault();
    const tab = window.open('about:blank', '_blank');
    try {
      const saved = await spotifyPost('setup', {client_id: $('#tmSpotifyClient').value.trim()});
      state.spotify = {...state.spotify, configured: true, redirect_uri: saved.redirect_uri};
      const {url} = await spotifyPost('login');
      if (tab) tab.location.href = url;
      $('#tmSpotifySetup').close();
      renderNowPlaying();
    } catch (err) {
      tab?.close();
      $('#tmSpotifyError').textContent = err.message;
    }
  }
  window.addEventListener('message', (e) => {
    if (e.origin === location.origin && e.data?.type === 'sail-spotify' && state.live) poll();
  });

  function renderActivity() {
    $('#tmActivity').replaceChildren(...state.activity.slice(0, 30).map((a) => el('li', {},
      el('span', {class: `tm-status ${stationStatus(a.station)}`}),
      el('time', {text: hhmmss(a.at)}),
      el('button', {class: 'tm-link-btn tm-act-station', type: 'button', onclick: (e) => openStation(a.station, e.currentTarget), text: byId[a.station]?.name || a.station}),
      el('span', {class: 'tm-act-text', text: a.text}))));
    const now = Date.now(), bins = new Array(40).fill(0);
    for (const a of state.activity) { const i = 39 - Math.floor((now - a.at) / 60000); if (i >= 0 && i < 40) bins[i]++; }
    const hi = Math.max(1, ...bins);
    $('#tmHisto').replaceChildren(...bins.map((b) => el('span', {style: `height:${Math.max(4, b / hi * 100)}%`, class: b ? 'on' : ''})));
  }

  // ------------------------------------------------------------------ actions
  function select(id) {
    state.selected = id;
    for (const l of root.querySelectorAll('.tm-label')) l.setAttribute('aria-pressed', l.dataset.station === id ? 'true' : 'false');
    for (const n of root.querySelectorAll('.tm-nav-item[data-nav]')) {
      const on = n.dataset.nav === current();
      n.classList.toggle('selected', on);
      if (on) n.setAttribute('aria-current', 'page'); else n.removeAttribute('aria-current');
    }
    $('#tmCrumb').textContent = state.tracker ? 'Tracker' : state.popup ? byId[state.popup].name : 'SAIL';
    svgRoot.querySelectorAll('.tm-station').forEach((g) => g.classList.toggle('selected', g.dataset.station === id));
    render();
  }
  // Chat keeps the Dialogue panel mounted (voice and replies keep running); Context covers
  // it with the open system's status. Naming one of Context's views opens it there.
  function setTab(t) {
    if (INFO_TABS.includes(t)) { state.info = t; t = 'context'; }
    if (!COLUMN_TABS.includes(t)) return;
    const conv = document.getElementById('conversation');
    if (state.tab === 'chat' && t !== 'chat' && conv) {
      state.chatScroll = conv.scrollHeight - conv.scrollTop - conv.clientHeight < 24 ? 'end' : conv.scrollTop;
    }
    const wasChat = state.tab === 'chat';
    state.tab = t;
    syncTabs();
    if (t === 'chat' && !wasChat && conv) conv.scrollTop = state.chatScroll === 'end' || state.chatScroll == null ? conv.scrollHeight : state.chatScroll;
    if (t !== 'chat' && wasChat) $('#tmInspector').replaceChildren();
    renderInspector();
  }
  function syncTabs() {
    for (const t of COLUMN_TABS) {
      const b = $(`#tmTab-${t}`), on = state.tab === t;
      b.setAttribute('aria-selected', String(on)); b.tabIndex = on ? 0 : -1;
    }
    chatSlot.hidden = state.tab !== 'chat';
    inspector.hidden = state.tab === 'chat';
  }
  // Arrow keys, Home and End move between the tabs of one row (ARIA tabs pattern).
  function tabKey(e, tabs, current, prefix) {
    const i = tabs.indexOf(current), n = tabs.length;
    const j = {ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: n - 1}[e.key];
    if (j === undefined) return;
    e.preventDefault();
    const next = tabs[(j + n) % n];
    setTab(next);
    $(`#${prefix}-${next}`).focus();
  }
  function setMode(m) {
    state.mode = m;
    root.dataset.mode = m;
    root.querySelectorAll('.tm-seg').forEach((b) => b.setAttribute('aria-pressed', b.dataset.mode === m ? 'true' : 'false'));
    render();
  }
  function navTo(target, trigger) {
    if (target === 'home') { if (!pane.hidden) closePane(); else { select(null); fit(); } return; }
    if (target === 'tracker') { state.trigger = trigger || document.activeElement; window.sailNavigate?.('tracker'); return; }
    openStation(target, trigger);
  }
  // The sidebar entry for what the centre shows.
  function current() { return state.tracker ? 'tracker' : state.popup || 'home'; }

  // ------------------------------------------------------------------ centre pane
  // Stations with a page go through the router (#mind, #forge…) so links, reloads and
  // Back work; the router fills #subsystemPage and announces the route on `sail:page`.
  // `tab` switches the Dialogue column to one of its info tabs; otherwise it stays put.
  function openStation(id, trigger, tab) {
    const s = byId[id]; if (!s) return;
    state.trigger = trigger || document.activeElement;
    if (tab) setTab(tab);
    if (!s.page) {
      // No page and no route: show it in place without leaving Home's address.
      if (window.sailCurrentPage?.() !== 'home') window.sailNavigate?.('home');
      showStation(id, false); $('#tmPaneTitle').focus({preventScroll: true}); return;
    }
    state.pending = id;
    window.sailNavigate?.(s.page);
  }
  function showPane(mode) {
    const opening = pane.hidden;
    if (pane.dataset.mode === 'entertainment' && mode !== 'entertainment') readerVisibility(false);
    pane.dataset.mode = mode;
    paneMain.hidden = mode === 'tracker' || mode === 'entertainment';
    trackerHost.hidden = mode !== 'tracker';
    entertainmentHost.hidden = mode !== 'entertainment';
    pane.setAttribute('aria-labelledby', mode === 'entertainment' ? 'tmEntertainmentLabel' : mode === 'tracker' ? 'tmTrackerLabel' : paneEmpty.hidden ? 'subsystemTitle' : 'tmPaneTitle');
    if (opening) {
      pane.hidden = false; stage.hidden = true;
      document.documentElement.classList.add('tm-pane-open');
    }
  }
  function showStation(id, routed) {
    paneEmpty.hidden = routed;
    const page = document.getElementById('subsystemPage');
    if (page) page.hidden = !routed;
    if (!routed) { $('#tmPaneTitle').textContent = byId[id].name; $('#tmPaneAbout').textContent = byId[id].about; }
    if (state.popup !== id || pane.hidden || state.tracker) paneMain.scrollTop = 0;
    state.tracker = false;
    showPane(id === 'arena' ? 'arena' : 'station');
    state.popup = id;
    select(id);
  }
  function showTracker() {
    if (!trackerHost.firstChild) {
      trackerHost.append(
        el('span', {class: 'tm-sr', id: 'tmTrackerLabel', text: 'Tracker'}),
        el('iframe', {class: 'tm-tracker-frame', id: 'tmTracker', src: '/tracker', title: 'Tracker'}));
    }
    state.popup = null; state.tracker = true;
    showPane('tracker');
    select(null);
  }
  const MEDIA_TABS = ['reader', 'music', 'movies'];
  let mediaTab = 'reader';
  function readerVisibility(visible) {
    document.getElementById('tmReader')?.contentWindow?.postMessage(
      {type: 'sail:reader-visibility', visible}, location.origin);
  }
  function switchMedia(tab, focus = false) {
    if (!MEDIA_TABS.includes(tab)) return;
    if (mediaTab === 'reader' && tab !== 'reader') readerVisibility(false);
    mediaTab = tab;
    for (const id of MEDIA_TABS) {
      const selected = id === tab;
      const control = document.getElementById(`tmMediaTab-${id}`);
      control.setAttribute('aria-selected', String(selected));
      control.tabIndex = selected ? 0 : -1;
      document.getElementById(`tmMediaPanel-${id}`).hidden = !selected;
    }
    if (tab === 'reader') readerVisibility(true);
    if (focus) document.getElementById(`tmMediaTab-${tab}`).focus();
  }
  function showEntertainment() {
    if (!entertainmentHost.firstChild) {
      const tabs = el('nav', {class: 'tm-media-tabs', role: 'tablist', 'aria-label': 'Entertainment'});
      const labels = {reader: 'Reader', music: 'Music player', movies: 'Movies'};
      entertainmentHost.append(el('span', {class: 'tm-sr', id: 'tmEntertainmentLabel', text: 'Entertainment'}), tabs);
      for (const id of MEDIA_TABS) {
        tabs.append(el('button', {type: 'button', class: 'tm-btn', role: 'tab', id: `tmMediaTab-${id}`,
          'aria-controls': `tmMediaPanel-${id}`, onclick: () => switchMedia(id), onkeydown: event => {
            let index = MEDIA_TABS.indexOf(mediaTab);
            if (event.key === 'ArrowRight') index++;
            else if (event.key === 'ArrowLeft') index--;
            else if (event.key === 'Home') index = 0;
            else if (event.key === 'End') index = MEDIA_TABS.length - 1;
            else return;
            event.preventDefault();
            switchMedia(MEDIA_TABS[(index + MEDIA_TABS.length) % MEDIA_TABS.length], true);
          }}, labels[id]));
        const panel = el('section', {class: 'tm-media-panel', id: `tmMediaPanel-${id}`,
          role: 'tabpanel', 'aria-labelledby': `tmMediaTab-${id}`, hidden: id !== 'reader'});
        if (id === 'reader') {
          const frame = el('iframe', {class: 'tm-tracker-frame', id: 'tmReader', src: '/reader', title: 'Book reader'});
          frame.addEventListener('load', () => readerVisibility(
            state.on && !pane.hidden && pane.dataset.mode === 'entertainment' && mediaTab === 'reader'));
          panel.append(frame);
        } else {
          panel.tabIndex = 0;
          panel.append(el('div', {class: 'tm-media-empty'}, el('h2', {text: labels[id]}),
            el('p', {text: `${labels[id]} will be available in a later update.`})));
        }
        entertainmentHost.append(panel);
      }
    }
    state.tracker = false; state.popup = 'entertainment';
    showPane('entertainment');
    switchMedia(mediaTab);
    select('entertainment');
  }
  function hidePane() {
    if (pane.hidden) return;
    readerVisibility(false);
    pane.hidden = true; stage.hidden = false;
    state.popup = null; state.tracker = false;
    document.documentElement.classList.remove('tm-pane-open');
    select(state.selected);
    const back = state.trigger?.isConnected && stage.contains(state.trigger) ? state.trigger : $('#tmViewport');
    state.trigger = null;
    back?.focus({preventScroll: true});
  }
  function closePane() {
    if (pane.hidden) return;
    if (window.sailCurrentPage?.() !== 'home') window.sailNavigate?.('home'); else hidePane();
  }

  // ------------------------------------------------------------------ Dialogue column
  function showChat() {
    const panel = document.getElementById('homeView'); if (!panel) return;
    chatSlot.append(panel);
  }
  function toggleBriefing() {
    const deck = document.getElementById('returnBriefingDeck');
    if (!deck) return;
    deck.hidden = !deck.hidden;
    const btn = document.getElementById('tmBriefingBtn');
    if (btn) btn.setAttribute('aria-pressed', String(!deck.hidden));
    if (!deck.hidden) deck.focus?.();
  }
  function toggleLive() {
    state.live = !state.live;
    $('#tmLive').setAttribute('aria-pressed', String(state.live));
    $('#tmLive .tm-status').className = `tm-status ${state.live ? 'on' : 'idle'}`;
    if (state.live) startPolling(); else stopPolling();
  }
  function togglePause() {
    state.paused = !state.paused;
    const b = $('#tmPause');
    b.setAttribute('aria-pressed', String(state.paused));
    b.setAttribute('aria-label', state.paused ? 'Resume animation' : 'Pause animation');
    b.textContent = state.paused ? '▶' : '❚❚';
  }
  function ask(e) {
    e.preventDefault();
    const input = $('#tmAskInput');
    const text = input.value.trim();
    if (!text) return;
    const match = STATIONS.find((s) => s.name.toLowerCase() === text.toLowerCase().replace(/^(open|go to|show)\s+/, ''));
    if (match) { input.value = ''; openStation(match.id, input); return; }
    const composerInput = document.getElementById('textInput'), form = document.getElementById('textForm');
    if (composerInput && form) {
      setTab('chat');
      composerInput.value = text;
      form.requestSubmit();
      log('dialogue', `You asked: ${text.length > 80 ? `${text.slice(0, 80)}…` : text}`);
      input.value = '';
      composerInput.focus();
    }
  }
  function openSettings() {
    if (window.openSheet && window.contextSheet) window.openSheet(window.contextSheet);
    else (document.getElementById('settingsOpen') || document.getElementById('settingsRail'))?.click();
  }

  // ------------------------------------------------------------------ routing
  function apply() {
    if (state.on) return;
    state.on = true;
    build();
    root.hidden = false;
    document.documentElement.classList.add('tm-active');
    showChat();
    setMode(state.mode);
    requestAnimationFrame(() => { if (state.on) { fit(); select(state.selected); } });
    lastFrame = 0; requestAnimationFrame(frame);
    if (state.live) startPolling();
  }
  function route(page) {
    apply();
    if (page === 'classic') {
      if (window.sailNavigate) window.sailNavigate('home');
      else location.hash = '#home';
      return;
    }
    if (page === 'home') { state.pending = null; hidePane(); return; }
    if (page === 'entertainment') {
      state.pending = null; showEntertainment();
      return;
    }
    if (page === 'tracker') {
      const requested = !!state.trigger;
      state.pending = null; showTracker();
      if (requested) $('#tmTracker').focus({preventScroll: true});
      return;
    }
    // Two stations can share a page (Memory and Arena); keep the one that was chosen.
    const pending = state.pending && byId[state.pending].page === page ? state.pending : null;
    const shown = !pane.hidden && state.popup && byId[state.popup].page === page ? state.popup : null;
    state.pending = null;
    showStation(pending || shown || PAGE_STATION[page] || 'core', true);
  }
  const show = () => window.sailNavigate?.('home');
  const hide = () => window.sailNavigate?.('home');
  window.sailToolsMap = {show, hide, open: (id) => openStation(id)};
  window.sailFactory = window.sailToolsMap;
  window.addEventListener('sail:page', (event) => route(event.detail));
  // A fresh briefing marks its view; closing the briefing returns to the map.
  window.addEventListener('sail:briefing', (event) => {
    state.awayFresh = !!event.detail?.fresh;
    const dot = document.getElementById('tmAwayDot'); if (dot) dot.hidden = !state.awayFresh;
    if (event.detail?.closed && state.mode === 'away' && root) setMode('map');
  });
  document.getElementById('openFactorySettings')?.addEventListener('click', show);
  // Retire the browser-wide style preference; the map is now the workspace.
  try { localStorage.removeItem('sail_ui_style'); } catch (_) {}
  if (window.sailCurrentPage) route(window.sailCurrentPage());
})();

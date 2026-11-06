'use strict';
// SAIL ambient state contract (DESIGN.md §3, issue #364 §3).
//
// One typed projection of real runtime events into the single shell ambient state.
// Every view reads it; nothing else may set `--sail-light-*` or `data-sail-state`.
//
// Inputs are events that already exist: the /ws/voice connection and its messages,
// microphone capture, consented listen-only context, Guardian approvals, foreground
// tool receipts and the Arena Learn session. The reducer is pure (no DOM, no clock):
// callers pass `at` (ms) so tests are deterministic.
//
// Precedence: disconnected > error > approval_required > speaking > recording >
// listening > executing > thinking > learning > warning > completed > idle.
// Privacy (recording), approvals and errors are also published as indicators that
// stay visible whatever the shell colour is. Background jobs (Forge, Flow, Life) are
// counted but never change the shell state; they tint only their own node or card.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.SailAmbient = api;
})(typeof self !== 'undefined' ? self : this, function () {
  // Canonical states, highest shell precedence first. Colours are DESIGN.md §3.
  // light = [a, b, c]: primary light, secondary light, deep tint.
  const STATES = Object.freeze({
    disconnected:      { label: 'Offline',          light: ['#566078', '#334155', '#0f172a'] },
    error:             { label: 'Attention needed', light: ['#ed7275', '#834c72', '#3b1d24'] },
    approval_required: { label: 'Approval needed',  light: ['#e5be79', '#b28900', '#1e1b18'] },
    speaking:          { label: 'Speaking',         light: ['#ff7a45', '#df6c90', '#78350f'] },
    recording:         { label: 'Recording',        light: ['#f56b66', '#c98a4b', '#3a1716'] },
    listening:         { label: 'Listening',        light: ['#19b5bc', '#104f55', '#0a2a2f'] },
    executing:         { label: 'Executing',        light: ['#4e91fc', '#1fb5a8', '#172554'] },
    thinking:          { label: 'Thinking',         light: ['#8567ff', '#4834d4', '#2c1b4d'] },
    learning:          { label: 'Learning',         light: ['#9b7dff', '#3fc1d9', '#1d1838'] },
    warning:           { label: 'Warning',          light: ['#f3b05a', '#8a5a1c', '#1f1810'] },
    completed:         { label: 'Completed',        light: ['#5ccda4', '#c9a94a', '#0f241d'] },
    idle:              { label: 'Ready',            light: ['#496f9d', '#384257', '#0e121a'] },
  });
  const ORDER = Object.freeze(Object.keys(STATES));

  // Fine-grained voice phases (what voice.js and the Orb use) → the ambient state they
  // can claim. A phase alone never claims listening: that also needs real capture.
  const PHASES = Object.freeze({
    idle: 'idle', holding: 'idle', interrupted: 'idle',
    listening: 'listening',
    transcribing: 'thinking', thinking: 'thinking', generating: 'thinking',
    seeing: 'thinking', watching: 'thinking',
    working: 'executing',
    speaking: 'speaking',
    // Carried by their own inputs (approval, error, connection), not by the phase.
    approval_required: 'idle', error: 'idle', disconnected: 'idle',
  });

  const ERROR_MS = 1700;      // error light; the message itself stays in #error longer
  const WARNING_MS = 8000;
  const COMPLETED_MS = 1600;  // one confirmation, then settle

  function initial() {
    return {
      socket: 0,           // connection generation; events from older sockets are dropped
      connected: false,
      session: null,
      epoch: 0,            // server turn epoch; older voice events are dropped
      phase: 'idle',
      capture: false,      // microphone frames are really being captured and sent
      micDenied: false,
      listenOnly: false,   // owner-consented listening context (listen without reply)
      tools: [],           // foreground tools with an "executing" receipt
      approvals: [],       // outstanding Guardian approval sessions
      learn: null,         // active Arena Learn session id
      background: [],      // background job ids: counted, never drive the shell
      error: null,         // { message, until }
      warning: null,       // { message, until }
      completed: null,     // { until }
    };
  }

  const without = (list, item) => list.filter(value => value !== item);
  const withItem = (list, item) => (item == null || list.includes(item) ? list : [...list, item]);

  // Returns the next model, or the same object when the event is dropped/ignored.
  function reduce(model, event) {
    if (!event || typeof event.type !== 'string') return model;
    const at = Number.isFinite(event.at) ? event.at : 0;
    // Socket-scoped events from a connection that is no longer current are stale.
    if (event.socket != null && event.type !== 'connect' && event.socket !== model.socket) return model;
    // Turn-scoped server events from an older epoch must not resurrect a past turn.
    if (event.epoch != null && event.epoch < model.epoch) return model;
    const next = { ...model };
    if (event.epoch != null && event.epoch > model.epoch) next.epoch = event.epoch;
    switch (event.type) {
      case 'connect':
        if (!(event.socket > model.socket)) return model;
        return { ...initial(), socket: event.socket, connected: true, micDenied: model.micDenied,
          learn: model.learn, background: model.background, approvals: model.approvals };
      case 'disconnect':
        // Everything tied to the live session is unknown until the next snapshot.
        return { ...initial(), socket: model.socket, connected: false, micDenied: model.micDenied,
          learn: model.learn, background: model.background, approvals: model.approvals };
      case 'snapshot':
        // Authoritative state after (re)connect: a new session starts from its snapshot.
        return { ...next, session: event.session ?? null, epoch: event.epoch ?? 0, phase: 'idle',
          tools: [], completed: null, capture: false,
          listenOnly: event.listenOnly === true,
          approvals: Array.isArray(event.approvals) ? [...event.approvals] : model.approvals };
      case 'phase':
        if (!(event.phase in PHASES)) return model;
        next.phase = event.phase;
        if (event.phase === 'idle' || event.phase === 'interrupted') next.tools = [];
        // The turn moved on: a confirmation shown later would describe the past.
        if (PHASES[event.phase] !== 'idle') next.completed = null;
        return next;
      case 'capture':
        next.capture = event.active === true && !model.micDenied;
        return next;
      case 'mic_denied':
        next.micDenied = event.denied !== false;
        if (next.micDenied) next.capture = false;
        return next;
      case 'listen_only':
        next.listenOnly = event.enabled === true;
        return next;
      case 'tool': {
        const name = String(event.tool || '');
        if (!name) return model;
        if (event.state === 'executing') next.tools = withItem(model.tools, name);
        else if (['completed', 'failed', 'cancelled'].includes(event.state)) {
          next.tools = without(model.tools, name);
          if (event.state === 'completed') next.completed = { until: at + COMPLETED_MS };
        } else return model;
        return next;
      }
      case 'approval':
        if (event.session == null) return model;
        next.approvals = event.pending === false ? without(model.approvals, event.session) : withItem(model.approvals, event.session);
        return next;
      case 'approvals':
        next.approvals = Array.isArray(event.pending) ? [...new Set(event.pending)] : [];
        return next;
      case 'learn':
        next.learn = event.session ?? null;
        return next;
      case 'background':
        if (event.id == null) return model;
        next.background = event.active === false ? without(model.background, event.id) : withItem(model.background, event.id);
        return next;
      case 'error':
        next.error = { message: String(event.message || 'Error'), until: at + (event.ms ?? ERROR_MS) };
        return next;
      case 'warning':
        next.warning = { message: String(event.message || 'Warning'), until: at + (event.ms ?? WARNING_MS) };
        return next;
      default:
        return model;
    }
  }

  // The live claims, before precedence. Pure function of model and time.
  function claims(model, now) {
    const on = new Set();
    if (!model.connected) on.add('disconnected');
    if (model.error && model.error.until > now) on.add('error');
    if (model.approvals.length) on.add('approval_required');
    const phase = PHASES[model.phase] || 'idle';
    if (phase === 'speaking') on.add('speaking');
    // Listening needs genuine capture; denied microphone can never claim it.
    if (model.capture && !model.micDenied) on.add(model.listenOnly ? 'recording' : 'listening');
    if (model.tools.length || phase === 'executing') on.add('executing');
    if (phase === 'thinking') on.add('thinking');
    if (model.learn) on.add('learning');
    if (model.warning && model.warning.until > now) on.add('warning');
    if (model.completed && model.completed.until > now) on.add('completed');
    return on;
  }

  function project(model, now = 0) {
    const on = claims(model, now);
    const state = model.connected ? ORDER.find(name => on.has(name)) || 'idle' : 'disconnected';
    const meta = STATES[state];
    const expiries = [model.error, model.warning, model.completed]
      .map(item => item && item.until).filter(until => Number.isFinite(until) && until > now);
    return {
      state,
      label: !model.connected && model.socket === 0 ? 'Connecting' : meta.label,
      light: meta.light,
      indicators: {
        connected: model.connected,
        recording: model.connected && model.capture && model.listenOnly && !model.micDenied,
        approvals: model.approvals.length,
        error: model.error && model.error.until > now ? model.error.message : null,
        micDenied: model.micDenied,
        background: model.background.length,
      },
      nextChangeAt: expiries.length ? Math.min(...expiries) : null,
    };
  }

  // Browser binding: one store per page, writing only presentation outputs.
  function createStore({ now = () => Date.now(), target = null } = {}) {
    let model = initial();
    let last = null, timer = null;
    const listeners = new Set();
    function apply(view) {
      const root = target || (typeof document !== 'undefined' ? document.documentElement : null);
      if (!root) return;
      const [a, b, c] = view.light;
      root.dataset.sailState = view.state;
      root.dataset.sailRecording = String(view.indicators.recording);
      root.dataset.sailApproval = String(view.indicators.approvals);
      root.dataset.sailConnected = String(view.indicators.connected);
      for (const [name, value] of [['--sail-light-a', a], ['--sail-light-b', b], ['--sail-light-c', c],
        // Legacy aliases still read by app.css; same values, no second colour source.
        ['--ambient-one', a], ['--ambient-two', b], ['--ambient-three', c]]) root.style.setProperty(name, value);
    }
    function render() {
      const t = now();
      const view = project(model, t);
      if (timer != null) { clearTimeout(timer); timer = null; }
      // Re-render when a transient (error, warning, completed) expires.
      if (view.nextChangeAt != null) timer = setTimeout(render, Math.max(0, view.nextChangeAt - t) + 1);
      const key = JSON.stringify(view.state) + JSON.stringify(view.indicators) + view.label;
      if (key === last) return view;
      last = key;
      apply(view);
      for (const listener of listeners) {
        try { listener(view); } catch (error) { console.error('ambient listener failed', error); }
      }
      return view;
    }
    return {
      dispatch(event) {
        const next = reduce(model, { at: now(), ...event });
        if (next === model) return null;
        model = next;
        return render();
      },
      get view() { return project(model, now()); },
      get model() { return model; },
      subscribe(listener) { listeners.add(listener); listener(project(model, now())); return () => listeners.delete(listener); },
      render,
    };
  }

  const api = { STATES, ORDER, PHASES, initial, reduce, project, createStore };
  if (typeof document !== 'undefined') {
    api.store = createStore();
    api.dispatch = event => api.store.dispatch(event);
    api.subscribe = listener => api.store.subscribe(listener);
    api.store.render();
  }
  return Object.freeze(api);
});

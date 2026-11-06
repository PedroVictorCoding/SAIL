// Run: node --test scripts/ui/*.test.js
// Transitions, precedence and staleness of the SAIL ambient state contract (#364 §3).
'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const A = require(path.join(__dirname, '../../crates/sail-dialogue/web/ambient-state.js'));

const run = (events, start = A.initial()) => events.reduce(A.reduce, start);
const stateOf = (model, now = 0) => A.project(model, now).state;
const connected = () => run([{ type: 'connect', socket: 1 }, { type: 'snapshot', socket: 1, session: 's1' }]);

test('canonical list is the documented one and every state has a label and three lights', () => {
  assert.deepEqual(A.ORDER, ['disconnected', 'error', 'approval_required', 'speaking', 'recording', 'listening',
    'executing', 'thinking', 'learning', 'warning', 'completed', 'idle']);
  for (const name of A.ORDER) {
    assert.ok(A.STATES[name].label, name);
    assert.equal(A.STATES[name].light.length, 3, name);
    for (const colour of A.STATES[name].light) assert.match(colour, /^#[0-9a-f]{6}$/, name);
  }
});

test('DESIGN.md §3 table lists exactly the canonical states with the same primary colours', () => {
  const design = fs.readFileSync(path.join(__dirname, '../../DESIGN.md'), 'utf8');
  const section = design.slice(design.indexOf('## 3.'), design.indexOf('### Arbitration'));
  const rows = [...section.matchAll(/^\| `([a-z_]+)` \|.*?`(#[0-9a-f]{6})`/gm)].map(m => [m[1], m[2]]);
  assert.deepEqual(rows.map(r => r[0]).sort(), [...A.ORDER].sort());
  for (const [name, colour] of rows) assert.equal(colour, A.STATES[name].light[0], `${name} colour in DESIGN.md`);
});

test('before the first connection the shell is disconnected and labelled Connecting', () => {
  const view = A.project(A.initial());
  assert.equal(view.state, 'disconnected');
  assert.equal(view.label, 'Connecting');
});

test('ready → listening → thinking → speaking → ready', () => {
  let m = connected();
  assert.equal(stateOf(m), 'idle');
  m = run([{ type: 'capture', active: true }, { type: 'phase', phase: 'listening' }], m);
  assert.equal(stateOf(m), 'listening');
  m = run([{ type: 'capture', active: false }, { type: 'phase', phase: 'transcribing', epoch: 1 }], m);
  assert.equal(stateOf(m), 'thinking');
  m = run([{ type: 'phase', phase: 'generating', epoch: 1 }], m);
  assert.equal(stateOf(m), 'thinking');
  m = run([{ type: 'phase', phase: 'speaking', epoch: 1 }], m);
  assert.equal(stateOf(m), 'speaking');
  m = run([{ type: 'phase', phase: 'idle' }], m);
  assert.equal(stateOf(m), 'idle');
});

test('a listening phase without real capture never claims listening', () => {
  const m = run([{ type: 'phase', phase: 'listening' }], connected());
  assert.equal(stateOf(m), 'idle');
});

test('denied microphone cannot show listening, even if capture is reported later', () => {
  let m = run([{ type: 'mic_denied' }, { type: 'capture', active: true }, { type: 'phase', phase: 'listening' }], connected());
  assert.equal(stateOf(m), 'idle');
  assert.equal(A.project(m).indicators.micDenied, true);
  m = run([{ type: 'mic_denied', denied: false }, { type: 'capture', active: true }], m);
  assert.equal(stateOf(m), 'listening');
});

test('consented listen-only capture shows recording and a persistent REC indicator', () => {
  let m = run([{ type: 'listen_only', enabled: true }, { type: 'capture', active: true }], connected());
  assert.equal(stateOf(m), 'recording');
  assert.equal(A.project(m).indicators.recording, true);
  // A typed question answered aloud: speaking lights the shell, REC stays visible.
  m = run([{ type: 'phase', phase: 'speaking', epoch: 2 }], m);
  assert.equal(stateOf(m), 'speaking');
  assert.equal(A.project(m).indicators.recording, true);
  m = run([{ type: 'listen_only', enabled: false }], m);
  assert.equal(A.project(m).indicators.recording, false);
});

test('mic on without consent is listening, never recording', () => {
  const m = run([{ type: 'capture', active: true }], connected());
  assert.equal(stateOf(m), 'listening');
  assert.equal(A.project(m).indicators.recording, false);
});

test('overlapping capture, TTS and Forge: foreground voice wins, background never drives the shell', () => {
  let m = run([{ type: 'background', id: 'forge:job-1' }, { type: 'background', id: 'forge:job-2' }], connected());
  assert.equal(stateOf(m), 'idle');
  assert.equal(A.project(m).indicators.background, 2);
  m = run([{ type: 'capture', active: true }], m);
  assert.equal(stateOf(m), 'listening');
  m = run([{ type: 'phase', phase: 'speaking', epoch: 3 }], m);
  assert.equal(stateOf(m), 'speaking', 'TTS playback outranks open microphone');
  m = run([{ type: 'phase', phase: 'idle' }], m);
  assert.equal(stateOf(m), 'listening');
  m = run([{ type: 'capture', active: false }, { type: 'background', id: 'forge:job-1', active: false }], m);
  assert.equal(stateOf(m), 'idle');
  assert.equal(A.project(m).indicators.background, 1);
});

test('foreground tool receipts drive executing, then one brief completed, then ready', () => {
  let m = run([{ type: 'tool', tool: 'fs:read', state: 'executing', at: 0 }], connected());
  assert.equal(stateOf(m, 0), 'executing');
  m = run([{ type: 'tool', tool: 'fs:read', state: 'completed', at: 100 }], m);
  assert.equal(stateOf(m, 200), 'completed');
  const view = A.project(m, 200);
  assert.ok(view.nextChangeAt > 200);
  assert.equal(stateOf(m, view.nextChangeAt), 'idle', 'completed is transient, not a permanent green shell');
  m = run([{ type: 'tool', tool: 'fs:read', state: 'executing', at: 5000 }, { type: 'tool', tool: 'fs:read', state: 'failed', at: 5100 }], m);
  assert.equal(stateOf(m, 5200), 'idle', 'a failed tool is not a completion');
});

test('thinking outranks a completed tool inside the same turn', () => {
  const m = run([{ type: 'phase', phase: 'thinking', epoch: 1 }, { type: 'tool', tool: 'os:status', state: 'executing' },
    { type: 'tool', tool: 'os:status', state: 'completed', at: 10 }], connected());
  assert.equal(stateOf(m, 20), 'thinking');
});

test('a confirmation is consumed when the turn moves on, so it never reappears afterwards', () => {
  let m = run([{ type: 'phase', phase: 'thinking', epoch: 1 }, { type: 'tool', tool: 'fs:read', state: 'executing' },
    { type: 'tool', tool: 'fs:read', state: 'completed', at: 0 }, { type: 'phase', phase: 'speaking', epoch: 1 }], connected());
  m = run([{ type: 'phase', phase: 'idle', epoch: 1 }], m);
  assert.equal(stateOf(m, 100), 'idle');
});

test('approval stays visible over speaking and clears when resolved', () => {
  let m = run([{ type: 'phase', phase: 'speaking', epoch: 1 }, { type: 'approval', session: 'a1' }], connected());
  assert.equal(stateOf(m), 'approval_required');
  assert.equal(A.project(m).indicators.approvals, 1);
  m = run([{ type: 'approval', session: 'a1', pending: false }], m);
  assert.equal(stateOf(m), 'speaking');
  m = run([{ type: 'approvals', pending: ['b', 'c', 'b'] }], m);
  assert.equal(A.project(m).indicators.approvals, 2);
});

test('error is always visible, transient, and outranks everything but disconnection', () => {
  let m = run([{ type: 'capture', active: true }, { type: 'approval', session: 'a1' }, { type: 'error', message: 'boom', at: 0 }], connected());
  assert.equal(stateOf(m, 10), 'error');
  assert.equal(A.project(m, 10).indicators.error, 'boom');
  assert.equal(stateOf(m, 5000), 'approval_required');
  m = run([{ type: 'disconnect', socket: 1 }], m);
  assert.equal(stateOf(m, 10), 'disconnected');
});

test('warning is shown only while nothing more urgent is active', () => {
  let m = run([{ type: 'warning', message: 'Recording limit reached', at: 0 }], connected());
  assert.equal(stateOf(m, 1), 'warning');
  m = run([{ type: 'phase', phase: 'thinking', epoch: 1 }], m);
  assert.equal(stateOf(m, 1), 'thinking');
});

test('learning is a low-priority foreground session state', () => {
  let m = run([{ type: 'learn', session: 'L1' }], connected());
  assert.equal(stateOf(m), 'learning');
  m = run([{ type: 'capture', active: true }], m);
  assert.equal(stateOf(m), 'listening', 'voice inside a lesson still lights the shell');
  m = run([{ type: 'capture', active: false }, { type: 'learn', session: null }], m);
  assert.equal(stateOf(m), 'idle');
});

test('stale epochs cannot resurrect speaking or listening', () => {
  let m = run([{ type: 'phase', phase: 'speaking', epoch: 4 }, { type: 'phase', phase: 'idle', epoch: 5 }], connected());
  assert.equal(stateOf(m), 'idle');
  const before = m;
  m = run([{ type: 'phase', phase: 'speaking', epoch: 4 }, { type: 'phase', phase: 'listening', epoch: 3 }], m);
  assert.equal(m, before, 'out-of-order events are dropped without changing the model');
});

test('events from a previous socket are dropped after reconnect', () => {
  let m = run([{ type: 'disconnect', socket: 1 }, { type: 'connect', socket: 2 }, { type: 'snapshot', socket: 2, session: 's2' }], connected());
  const before = m;
  m = run([{ type: 'phase', phase: 'speaking', socket: 1 }, { type: 'disconnect', socket: 1 }], m);
  assert.equal(m, before);
  assert.equal(stateOf(m), 'idle');
  assert.equal(run([{ type: 'connect', socket: 1 }], m), m, 'an older connect is ignored');
});

test('disconnect clears live claims; reconnect resyncs from the snapshot', () => {
  let m = run([{ type: 'listen_only', enabled: true }, { type: 'capture', active: true }, { type: 'phase', phase: 'speaking', epoch: 9 },
    { type: 'tool', tool: 'fs:read', state: 'executing' }, { type: 'learn', session: 'L1' }], connected());
  m = run([{ type: 'disconnect', socket: 1 }], m);
  const offline = A.project(m);
  assert.equal(offline.state, 'disconnected');
  assert.equal(offline.label, 'Offline');
  assert.equal(offline.indicators.recording, false, 'no REC claim without a live session');
  m = run([{ type: 'connect', socket: 2 }], m);
  assert.equal(stateOf(m), 'learning', 'session-independent Learn survives; voice does not');
  m = run([{ type: 'snapshot', socket: 2, session: 's2', epoch: 0 }, { type: 'phase', phase: 'thinking', epoch: 1 }], m);
  assert.equal(m.epoch, 1, 'the new session starts its own epoch sequence');
  assert.equal(stateOf(m), 'thinking');
});

test('pause/resume: interrupt returns to ready, and a resumed turn claims again with a newer epoch', () => {
  let m = run([{ type: 'phase', phase: 'speaking', epoch: 2 }, { type: 'tool', tool: 'web:read', state: 'executing' }], connected());
  m = run([{ type: 'phase', phase: 'interrupted', epoch: 3 }], m);
  assert.equal(stateOf(m), 'idle', 'interruption clears foreground tools');
  m = run([{ type: 'phase', phase: 'speaking', epoch: 2 }], m);
  assert.equal(stateOf(m), 'idle', 'the interrupted turn cannot resume itself');
  m = run([{ type: 'phase', phase: 'thinking', epoch: 4 }], m);
  assert.equal(stateOf(m), 'thinking');
});

test('unknown events and phases are ignored', () => {
  const m = connected();
  assert.equal(A.reduce(m, { type: 'phase', phase: 'creating' }), m);
  assert.equal(A.reduce(m, { type: 'nope' }), m);
  assert.equal(A.reduce(m, null), m);
});

test('store writes data-sail-state and the light variables only on change', () => {
  const props = new Map();
  const target = { dataset: {}, style: { setProperty: (k, v) => props.set(k, v) } };
  let t = 0;
  const store = A.createStore({ now: () => t, target });
  const seen = [];
  store.subscribe(view => seen.push(view.state));
  store.dispatch({ type: 'connect', socket: 1 });
  assert.equal(target.dataset.sailState, 'idle');
  assert.equal(props.get('--sail-light-a'), A.STATES.idle.light[0]);
  store.dispatch({ type: 'capture', active: true });
  assert.equal(target.dataset.sailState, 'listening');
  assert.equal(props.get('--sail-light-a'), A.STATES.listening.light[0]);
  assert.equal(props.get('--ambient-one'), A.STATES.listening.light[0]);
  store.dispatch({ type: 'listen_only', enabled: true });
  assert.equal(target.dataset.sailRecording, 'true');
  assert.deepEqual(seen, ['disconnected', 'idle', 'listening', 'recording']);
});

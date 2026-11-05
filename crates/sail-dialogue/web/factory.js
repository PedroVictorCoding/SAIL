'use strict';
// SAIL Factory Floor — the room is the index.
//
// Every subsystem is a fixed physical object on one isometric floor. Nothing is
// hidden behind tabs and nothing floats detached: a station is where it is, and the
// camera only moves because the person moved it. Expansion is an overlay on the same
// room, never a route change.
//
// Mounted by `subsystem-pages.js` through the `sail:page` event (route `factory`).

const FACTORY = {
  // One palette, one hue per state. Accent belongs to selection and alarm only.
  palette: {
    floor: '#17191D', floorDot: '#2A2D33', structure: '#3A3E45',
    surface: '#333B43', surfaceTop: '#4A525B', surfaceSide: '#272E35',
    body: '#C9CDD4', muted: '#8A919B',
    accent: '#F26B21', ok: '#5FD3A6', alert: '#FF4438',
  },
  unit: 34,                                  // pixels per floor tile at zoom 1
  camera: { panX: 340, panY: 150, zoom: 1 },
  hover: null, selected: null, listMode: false,
  sceneIndex: 0, scenes: ['Overview', 'Ops', 'Body', 'Comms'],
  latency: null, mounted: false,
};

// Station registry: anchor tile, footprint in tiles, height in tiles.
// `zone` groups stations for the room directory; `rows` feeds the anchored card.
FACTORY.registry = [
  { id: 'arena', name: 'ARENA', zone: 'KNOWLEDGE', kind: 'tank', x: 2, y: 2, w: 2, d: 2, h: 1.6, rows: [['Propositions', '1 284'], ['Chains', '312'], ['Recall', 'ok']] },
  { id: 'mind', name: 'MIND', zone: 'COMPUTE', kind: 'tank', x: 6, y: 1, w: 2, d: 2, h: 2.1, rows: [['Model', 'qwen3-8b'], ['Context', '62%'], ['Tokens/s', '41']] },
  { id: 'dialogue', name: 'DIALOGUE', zone: 'COMPUTE', kind: 'bank', x: 0, y: 5, w: 3, d: 1, h: 1.1, rows: [['Turns', '318'], ['Voice', 'ready'], ['Latency', '310ms']] },
  { id: 'guardian', name: 'GUARDIAN', zone: 'CONTROL', kind: 'canisters', x: 9, y: 4, w: 1, d: 1, h: 1.2, rows: [['Approvals', '2 open'], ['Vault', 'locked'], ['Policy', 'strict']] },
  { id: 'flow', name: 'FLOW', zone: 'CONTROL', kind: 'valve', x: 5, y: 4, w: 1, d: 1, h: 0.9, rows: [['Queue', '3'], ['Admission', 'ok'], ['Retries', '0']] },
  { id: 'forge', name: 'FORGE', zone: 'BUILD', kind: 'press', x: 4, y: 6, w: 2, d: 2, h: 1.5, rows: [['Jobs', '1 running'], ['Worktrees', '2'], ['Review', 'idle']] },
  { id: 'pulse', name: 'PULSE', zone: 'SCHEDULE', kind: 'conveyor', x: 7, y: 7, w: 4, d: 1, h: 0.8, rows: [['Next', '18:40'], ['Routines', '4'], ['Quiet', 'off']] },
  { id: 'canvas', name: 'CANVAS', zone: 'VISUAL', kind: 'bank', x: 2, y: 7, w: 2, d: 1, h: 1.3, rows: [['Artefacts', '7'], ['Camera', 'idle'], ['Feeds', '2']] },
  { id: 'bridge', name: 'BRIDGE', zone: 'EXTERNAL', kind: 'pipe-run', x: 10, y: 2, w: 3, d: 1, h: 0.7, rows: [['Escalations', '0'], ['Providers', '2'], ['Quota', 'ok']] },
  { id: 'discovery', name: 'DISCOVERY', zone: 'KNOWLEDGE', kind: 'radar', x: 0, y: 9, w: 1, d: 1, h: 1.0, rows: [['Gaps', '5'], ['Sources', '12'], ['Cursor', 'ok']] },
  { id: 'capacity', name: 'CAPACITY', zone: 'COMPUTE', kind: 'silo', x: 8, y: 0, w: 1, d: 1, h: 2.4, rows: [['Disk', '68%'], ['RAM', '54%'], ['VRAM', '41%']] },
  { id: 'anomaly', name: 'ANOMALY', zone: 'CONTROL', kind: 'pylons', x: 0, y: 11, w: 2, d: 1, h: 0.6, rows: [['Signals', '12'], ['Deviating', '0'], ['Window', '15m']] },
];

// Pipe network: tile-space runs between stations.
FACTORY.pipes = [
  { from: [5, 4], to: [3, 3] },
  { from: [5, 4], to: [6, 2] },
  { from: [6, 2], to: [9, 1] },
  { from: [6, 2], to: [9, 4] },
  { from: [6, 2], to: [10, 2] },
  { from: [3, 3], to: [1, 5] },
  { from: [3, 3], to: [4, 7] },
  { from: [5, 4], to: [4, 7] },
  { from: [4, 7], to: [2, 8] },
  { from: [4, 7], to: [7, 7] },
  { from: [2, 8], to: [0, 9] },
  { from: [2, 8], to: [1, 11] },
];

// Decorative plant furniture: fixed, non-interactive, keeps the floor populated.
FACTORY.props = [
  { kind: 'silo', x: 4, y: 0, w: 1, d: 1, h: 1.4 },
  { kind: 'canisters', x: 2, y: 0, w: 1, d: 1, h: 0.8 },
  { kind: 'valve', x: 8, y: 5, w: 1, d: 1, h: 0.7 },
  { kind: 'silo', x: 9, y: 9, w: 1, d: 1, h: 1.2 },
  { kind: 'canisters', x: 6, y: 9, w: 1, d: 1, h: 0.7 },
];

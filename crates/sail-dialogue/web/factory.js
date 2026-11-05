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

// ---------------------------------------------------------------- projection --

const COS30 = Math.cos(Math.PI / 6);
const SIN30 = 0.5;

/** Tile space (x, y, z) to screen space. */
function isoPoint(x, y, z = 0, camera = FACTORY.camera, unit = FACTORY.unit) {
  const scale = unit * camera.zoom;
  return {
    x: camera.panX + (x - y) * COS30 * scale,
    y: camera.panY + (x + y) * SIN30 * scale - z * scale,
  };
}

/** Screen space back to the floor plane, for hit testing. */
function screenToTile(px, py, camera = FACTORY.camera, unit = FACTORY.unit) {
  const scale = unit * camera.zoom;
  const dx = (px - camera.panX) / scale;
  const dy = (py - camera.panY) / scale;
  return { x: dy / SIN30 / 2 + dx / COS30 / 2, y: dy / SIN30 / 2 - dx / COS30 / 2 };
}

/** Painter's order: things further along +x and +y sit nearer the viewer. */
function depth(record) {
  return record.x + record.y + (record.w || 0) / 2 + (record.d || 0) / 2;
}

function visibleStations(registry = FACTORY.registry) {
  return registry.slice().sort((a, b) => depth(a) - depth(b));
}

/** Below 80% zoom stations reduce to silhouette plus a status light. */
function detailForZoom(zoom = FACTORY.camera.zoom) {
  if (zoom < 0.8) return 'silhouette';
  if (zoom > 1.1) return 'full';
  return 'normal';
}

/** Frames the whole plant once, on mount. After that the camera only moves by hand. */
function fitCamera(width, height, unit = FACTORY.unit) {
  const flat = { panX: 0, panY: 0, zoom: 1 };
  const points = [];
  for (const record of FACTORY.registry) {
    for (const [x, y, z] of [
      [record.x, record.y, 0], [record.x + record.w, record.y, 0],
      [record.x, record.y + record.d, record.h], [record.x + record.w, record.y + record.d, record.h],
    ]) points.push(isoPoint(x, y, z, flat, unit));
  }
  const xs = points.map(point => point.x), ys = points.map(point => point.y);
  const minX = Math.min(...xs), maxX = Math.max(...xs);
  const minY = Math.min(...ys), maxY = Math.max(...ys);
  const zoom = Math.min((width - 140) / Math.max(1, maxX - minX), (height - 190) / Math.max(1, maxY - minY));
  FACTORY.camera.zoom = Math.min(1.5, Math.max(0.5, zoom));
  const centreX = (minX + maxX) / 2, centreY = (minY + maxY) / 2;
  FACTORY.camera.panX = width / 2 - centreX * FACTORY.camera.zoom;
  FACTORY.camera.panY = height / 2 - centreY * FACTORY.camera.zoom + 10;
  return FACTORY.camera;
}

// ------------------------------------------------------------------ drawing --

function polygon(ctx, points, fill, stroke, lineWidth = 1) {
  ctx.beginPath();
  points.forEach((point, index) => (index ? ctx.lineTo(point.x, point.y) : ctx.moveTo(point.x, point.y)));
  ctx.closePath();
  if (fill) { ctx.fillStyle = fill; ctx.fill(); }
  if (stroke) { ctx.strokeStyle = stroke; ctx.lineWidth = lineWidth; ctx.stroke(); }
}

/** Floor plate: dotted grid inside the plant's own bounds, never infinite. */
function drawFloor(ctx, width, height) {
  const { palette } = FACTORY;
  ctx.fillStyle = palette.floor;
  ctx.fillRect(0, 0, width, height);

  const tiles = FACTORY.registry.concat(FACTORY.props);
  const bounds = tiles.reduce((acc, record) => ({
    minX: Math.min(acc.minX, record.x), minY: Math.min(acc.minY, record.y),
    maxX: Math.max(acc.maxX, record.x + record.w), maxY: Math.max(acc.maxY, record.y + record.d),
  }), { minX: Infinity, minY: Infinity, maxX: -Infinity, maxY: -Infinity });
  bounds.minX -= 2; bounds.minY -= 2; bounds.maxX += 2; bounds.maxY += 2;

  ctx.fillStyle = palette.floorDot;
  const size = FACTORY.camera.zoom >= 0.8 ? 1.4 : 1;
  for (let x = bounds.minX; x <= bounds.maxX; x += 1) {
    for (let y = bounds.minY; y <= bounds.maxY; y += 1) {
      const point = isoPoint(x, y);
      if (point.x < -20 || point.x > width + 20 || point.y < -20 || point.y > height + 20) continue;
      ctx.fillRect(point.x, point.y, size, size);
    }
  }
  if (FACTORY.camera.zoom >= 0.6) {
    polygon(ctx, [
      isoPoint(bounds.minX, bounds.minY), isoPoint(bounds.maxX, bounds.minY),
      isoPoint(bounds.maxX, bounds.maxY), isoPoint(bounds.minX, bounds.maxY),
    ], null, palette.structure, 1);
  }
}


/** Pipe run with a dark jacket and a lighter core, plus junction nodes. */
function drawPipes(ctx) {
  const { palette } = FACTORY;
  const detail = detailForZoom();
  const scale = FACTORY.camera.zoom;
  for (const pipe of FACTORY.pipes) {
    const a = pipe.from, b = pipe.to;
    // Route on the floor with one elbow, the way the plant does.
    const elbow = Math.abs(a[0] - b[0]) > Math.abs(a[1] - b[1]) ? [b[0], a[1]] : [a[0], b[1]];
    const points = [a, elbow, b].map(([x, y]) => isoPoint(x, y));
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    ctx.beginPath();
    points.forEach((point, index) => (index ? ctx.lineTo(point.x, point.y) : ctx.moveTo(point.x, point.y)));
    ctx.strokeStyle = '#14171A';
    ctx.lineWidth = 8 * scale;
    ctx.stroke();
    ctx.strokeStyle = detail === 'silhouette' ? palette.structure : '#39404A';
    ctx.lineWidth = 3 * scale;
    ctx.stroke();
    if (detail === 'silhouette') continue;
    for (const [x, y] of [a, b]) {
      const point = isoPoint(x, y);
      ctx.beginPath();
      ctx.arc(point.x, point.y, 5 * scale, 0, Math.PI * 2);
      ctx.fillStyle = '#1B2026';
      ctx.fill();
      ctx.strokeStyle = palette.structure;
      ctx.lineWidth = 1;
      ctx.stroke();
    }
  }
}

/** Soft contact shadow so an object sits on the plate instead of floating over it. */
function drawContactShadow(ctx, record) {
  const centre = isoPoint(record.x + (record.w || 1) / 2, record.y + (record.d || 1) / 2, 0);
  const scale = FACTORY.unit * FACTORY.camera.zoom;
  const span = ((record.w || 1) + (record.d || 1)) / 2;
  const rx = span * scale * COS30 * 0.95;
  const ry = span * scale * SIN30 * 0.95;
  ctx.beginPath();
  ctx.ellipse(centre.x, centre.y + 2, rx, ry, 0, 0, Math.PI * 2);
  ctx.fillStyle = 'rgba(6,8,10,0.55)';
  ctx.fill();
}

/** Isometric box: two visible side faces and a top face. */
function drawBox(ctx, rect, options = {}) {
  const { palette } = FACTORY;
  const { x, y, w, d, h } = rect;
  const lineWidth = options.accent ? 2 : 1;
  const edge = options.accent ? palette.accent : palette.structure;

  const top = [isoPoint(x, y, h), isoPoint(x + w, y, h), isoPoint(x + w, y + d, h), isoPoint(x, y + d, h)];
  const rightFace = [isoPoint(x + w, y, h), isoPoint(x + w, y + d, h), isoPoint(x + w, y + d, 0), isoPoint(x + w, y, 0)];
  const leftFace = [isoPoint(x, y + d, h), isoPoint(x + w, y + d, h), isoPoint(x + w, y + d, 0), isoPoint(x, y + d, 0)];

  polygon(ctx, rightFace, options.side || palette.surfaceSide, edge, lineWidth);
  polygon(ctx, leftFace, options.front || palette.surface, edge, lineWidth);
  polygon(ctx, top, options.top || palette.surfaceTop, edge, lineWidth);
  return { top, centre: isoPoint(x + w / 2, y + d / 2, h) };
}

/** Isometric cylinder, used for tanks and silos. */
function drawTank(ctx, rect, options = {}) {
  const { palette } = FACTORY;
  const centre = isoPoint(rect.x + rect.w / 2, rect.y + rect.d / 2, 0);
  const radius = Math.min(rect.w, rect.d) / 2;
  const scale = FACTORY.unit * FACTORY.camera.zoom;
  const rx = radius * scale * COS30 * 1.15;
  const ry = radius * scale * SIN30 * 1.15;
  const topY = centre.y - rect.h * scale;
  const edge = options.accent ? palette.accent : palette.structure;
  const lineWidth = options.accent ? 2 : 1;

  const body = ctx.createLinearGradient(centre.x - rx, 0, centre.x + rx, 0);
  body.addColorStop(0, '#232830');
  body.addColorStop(0.45, '#3B424A');
  body.addColorStop(1, '#1D2228');

  ctx.beginPath();
  ctx.ellipse(centre.x, centre.y, rx, ry, 0, 0, Math.PI);
  ctx.lineTo(centre.x - rx, topY);
  ctx.ellipse(centre.x, topY, rx, ry, 0, Math.PI, 0, true);
  ctx.closePath();
  ctx.fillStyle = body;
  ctx.fill();
  ctx.strokeStyle = edge;
  ctx.lineWidth = lineWidth;
  ctx.stroke();

  ctx.beginPath();
  ctx.ellipse(centre.x, topY, rx, ry, 0, 0, Math.PI * 2);
  ctx.fillStyle = '#454D57';
  ctx.fill();
  ctx.strokeStyle = edge;
  ctx.lineWidth = lineWidth;
  ctx.stroke();

  // One banded reading on the tank wall: state, never decoration.
  if (options.band !== false && FACTORY.camera.zoom >= 0.8) {
    ctx.beginPath();
    ctx.ellipse(centre.x, topY + (centre.y - topY) * 0.45, rx, ry, 0, 0, Math.PI);
    ctx.strokeStyle = palette.accent;
    ctx.lineWidth = 2;
    ctx.stroke();
  }
  return { anchor: { x: centre.x, y: topY } };
}


/** Stacked reading bars beside a station, as in the reference plant. */
function drawGauges(ctx, anchor, values = [0.82, 0.6, 0.38]) {
  if (detailForZoom() === 'silhouette') return;
  const { palette } = FACTORY;
  const scale = FACTORY.camera.zoom;
  const width = 5 * scale;
  const height = 26 * scale;
  values.forEach((value, index) => {
    const x = anchor.x + index * (width + 3 * scale);
    const y = anchor.y - height;
    ctx.fillStyle = '#14171A';
    ctx.fillRect(x, y, width, height);
    ctx.fillStyle = palette.accent;
    ctx.fillRect(x, y + height * (1 - value), width, height * value);
    ctx.strokeStyle = palette.structure;
    ctx.lineWidth = 1;
    ctx.strokeRect(x, y, width, height);
  });
}

/** Monospace station label, uppercase, next to its object. */
function drawLabel(ctx, record, anchor, active) {
  if (detailForZoom() === 'silhouette') {
    // Silhouette mode keeps a status light instead of text.
    const point = isoPoint(record.x + record.w / 2, record.y + record.d / 2, 0);
    ctx.fillStyle = FACTORY.palette.ok;
    ctx.beginPath();
    ctx.arc(point.x, point.y, 2.5, 0, Math.PI * 2);
    ctx.fill();
    return;
  }
  const { palette } = FACTORY;
  const size = Math.min(FACTORY.camera.zoom, 1.15);
  ctx.font = `${(active ? 11 : 10) * size}px ui-monospace, "SF Mono", Menlo, Consolas, monospace`;
  ctx.textBaseline = 'alphabetic';
  ctx.fillStyle = active ? palette.accent : palette.body;
  ctx.fillText(record.name, anchor.x - 8 * FACTORY.camera.zoom, anchor.y + 18 * FACTORY.camera.zoom);
}

/** Orange wireframe around the selected station: footprint, uprights, roof. */
function drawSelection(ctx, record) {
  const { palette } = FACTORY;
  const { x, y, w, d, h } = record;
  polygon(ctx, [isoPoint(x, y), isoPoint(x + w, y), isoPoint(x + w, y + d), isoPoint(x, y + d)],
    null, palette.accent, 2);
  for (const [px, py] of [[x, y], [x + w, y], [x + w, y + d], [x, y + d]]) {
    const base = isoPoint(px, py, 0), top = isoPoint(px, py, h);
    ctx.beginPath();
    ctx.moveTo(base.x, base.y);
    ctx.lineTo(top.x, top.y);
    ctx.strokeStyle = palette.accent;
    ctx.lineWidth = 2;
    ctx.stroke();
  }
  polygon(ctx, [isoPoint(x, y, h), isoPoint(x + w, y, h), isoPoint(x + w, y + d, h), isoPoint(x, y + d, h)],
    null, palette.accent, 2);
}


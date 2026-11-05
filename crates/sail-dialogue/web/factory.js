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


/** Draws one station according to its physical kind, returns its anchor point. */
function drawStation(ctx, record, options = {}) {
  const accent = options.accent || false;
  const zoom = FACTORY.camera.zoom;
  switch (record.kind) {
    case 'tank':
      return drawTank(ctx, record, { accent });
    case 'silo': {
      const drawn = drawTank(ctx, record, { accent, band: false });
      drawGauges(ctx, { x: drawn.anchor.x + 26 * zoom, y: drawn.anchor.y + 14 * zoom }, [0.7, 0.52, 0.34]);
      return drawn;
    }
    case 'canisters': {
      const columns = Math.max(2, Math.round(record.w * 2));
      let last = null;
      for (let index = 0; index < columns; index += 1) {
        last = drawTank(ctx, {
          x: record.x + (index % columns) * (record.w / columns), y: record.y,
          w: (record.w / columns) * 0.8, d: record.d * 0.8,
          h: record.h * (0.7 + 0.1 * (index % 3)),
        }, { accent, band: false });
      }
      return last;
    }
    case 'valve': {
      const box = drawBox(ctx, { ...record, h: record.h * 0.6 }, { accent });
      const wheel = isoPoint(record.x + record.w / 2, record.y + record.d / 2, record.h);
      ctx.beginPath();
      ctx.ellipse(wheel.x, wheel.y, 9 * zoom, 4.5 * zoom, 0, 0, Math.PI * 2);
      ctx.strokeStyle = accent ? FACTORY.palette.accent : FACTORY.palette.structure;
      ctx.lineWidth = 1.5;
      ctx.stroke();
      return { anchor: box.centre };
    }
    case 'press': {
      const base = drawBox(ctx, { ...record, h: record.h * 0.3 }, { accent });
      drawBox(ctx, {
        x: record.x + 0.25, y: record.y + 0.25,
        w: record.w - 0.5, d: record.d - 0.5, h: record.h,
      }, { accent });
      return base;
    }
    case 'conveyor': {
      // The belt already reads as a timeline: packets on it are the trip's position.
      const base = drawBox(ctx, record, { accent, top: '#3D444D', front: '#2B323A', side: '#242A31' });
      if (zoom >= 0.8) {
        const steps = Math.round(record.w * 4);
        for (let index = 0; index < steps; index += 1) {
          const point = isoPoint(record.x + record.w * (index / steps), record.y + record.d / 2, record.h);
          ctx.fillStyle = FACTORY.palette.structure;
          ctx.fillRect(point.x - 1, point.y - 2, 2, 4);
        }
        const packet = isoPoint(record.x + record.w * 0.62, record.y + record.d / 2, record.h + 0.05);
        ctx.fillStyle = FACTORY.palette.accent;
        ctx.fillRect(packet.x - 3, packet.y - 5, 6, 8);
      }
      return base;
    }
    case 'pipe-run': {
      const base = drawBox(ctx, record, { accent, top: '#39404A' });
      if (zoom >= 0.8) {
        const point = isoPoint(record.x + record.w * 0.5, record.y + record.d * 0.5, record.h);
        for (let index = 0; index < 3; index += 1) {
          ctx.beginPath();
          ctx.moveTo(point.x - (12 + index * 6), point.y + index * 3);
          ctx.lineTo(point.x + (12 + index * 6), point.y + index * 3);
          ctx.strokeStyle = index === 0 ? FACTORY.palette.accent : FACTORY.palette.structure;
          ctx.lineWidth = 1;
          ctx.stroke();
        }
      }
      return base;
    }
    case 'radar': {
      const base = drawBox(ctx, record, { accent });
      const dish = isoPoint(record.x + record.w / 2, record.y + record.d / 2, record.h);
      ctx.beginPath();
      ctx.ellipse(dish.x, dish.y, 14 * zoom, 7 * zoom, 0, Math.PI, Math.PI * 2);
      ctx.strokeStyle = accent ? FACTORY.palette.accent : FACTORY.palette.structure;
      ctx.lineWidth = 1.5;
      ctx.stroke();
      return base;
    }
    case 'pylons': {
      const anchor = isoPoint(record.x + record.w / 2, record.y + record.d / 2, record.h);
      for (let index = 0; index < 6; index += 1) {
        const px = anchor.x - 30 * zoom + index * 12 * zoom;
        ctx.beginPath();
        ctx.moveTo(px, anchor.y + 8 * zoom);
        ctx.lineTo(px, anchor.y - 2 * zoom);
        ctx.strokeStyle = accent ? FACTORY.palette.accent : FACTORY.palette.structure;
        ctx.lineWidth = 2;
        ctx.stroke();
        ctx.beginPath();
        ctx.arc(px, anchor.y - 4 * zoom, 2 * zoom, 0, Math.PI * 2);
        ctx.fillStyle = index === 0 ? FACTORY.palette.ok : FACTORY.palette.surfaceTop;
        ctx.fill();
      }
      return { anchor };
    }
    case 'bank':
    default: {
      const base = drawBox(ctx, record, { accent });
      if (zoom >= 0.8) {
        // Monitor bank: a 3 x 2 grid of screens on the +y face.
        for (let row = 0; row < 2; row += 1) {
          for (let column = 0; column < 3; column += 1) {
            const screen = isoPoint(
              record.x + record.w * ((column + 0.5) / 3),
              record.y + record.d,
              record.h * (0.45 + row * 0.28),
            );
            ctx.fillStyle = accent ? FACTORY.palette.accent : '#4B545E';
            ctx.fillRect(screen.x - 4, screen.y - 3, 8, 5);
          }
        }
      }
      return base;
    }
  }
}


// ------------------------------------------------------------------- render --

/** Screen anchors of every station, for hit testing and anchored cards. */
FACTORY.anchors = new Map();

function drawScene(ctx, width, height) {
  drawFloor(ctx, width, height);
  drawPipes(ctx);

  const current = FACTORY.selected || FACTORY.hover;
  const objects = FACTORY.props.map(prop => ({ ...prop, prop: true }))
    .concat(FACTORY.registry.map(station => ({ ...station, prop: false })));
  objects.sort((a, b) => depth(a) - depth(b));

  FACTORY.anchors.clear();
  for (const object of objects) {
    const active = !object.prop && object.id === current?.id;
    drawContactShadow(ctx, object);
    const drawn = drawStation(ctx, object, { accent: active });
    if (object.prop) continue;
    const anchor = drawn?.anchor || isoPoint(object.x + object.w / 2, object.y + object.d / 2, object.h);
    FACTORY.anchors.set(object.id, {
      ...anchor,
      base: isoPoint(object.x + object.w / 2, object.y + object.d / 2, 0),
      station: object,
    });
    drawGauges(ctx, { x: anchor.x - 18 * FACTORY.camera.zoom, y: anchor.y + 6 * FACTORY.camera.zoom });
    drawLabel(ctx, object, anchor, active);
  }

  if (FACTORY.selected) drawSelection(ctx, FACTORY.selected);
  if (FACTORY.hover && FACTORY.hover.id !== FACTORY.selected?.id) {
    // Peek: a 1px accent outline on the hovered footprint. No layout shift.
    polygon(ctx, [
      isoPoint(FACTORY.hover.x, FACTORY.hover.y),
      isoPoint(FACTORY.hover.x + FACTORY.hover.w, FACTORY.hover.y),
      isoPoint(FACTORY.hover.x + FACTORY.hover.w, FACTORY.hover.y + FACTORY.hover.d),
      isoPoint(FACTORY.hover.x, FACTORY.hover.y + FACTORY.hover.d),
    ], null, FACTORY.palette.accent, 1);
  }
}

/** Station under a screen point, using its projected footprint and body. */
function hitTest(px, py) {
  const tile = screenToTile(px, py);
  for (const record of visibleStations().reverse()) {
    const lift = (record.h || 1) * 0.6;
    const insidePlate = tile.x >= record.x && tile.x <= record.x + record.w
      && tile.y >= record.y && tile.y <= record.y + record.d;
    const insideBody = tile.x >= record.x && tile.x <= record.x + record.w
      && tile.y >= record.y - lift && tile.y <= record.y + record.d;
    if (insidePlate || insideBody) return record;
  }
  return null;
}


// --------------------------------------------------------------------- DOM --

function factoryElement(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

function mountFactory(workspace) {
  if (FACTORY.mounted) return;
  FACTORY.mounted = true;

  const floor = factoryElement('div', 'factory-floor');
  floor.id = 'factoryFloor';
  floor.innerHTML = `
    <canvas id="factoryCanvas" class="factory-canvas" role="img" tabindex="0"
      aria-label="Isometric SAIL room. Every subsystem is a fixed station on the floor."></canvas>
    <div class="factory-chrome">
      <div class="factory-breadcrumb"><span>ROOM</span><i>/</i><strong id="factoryCrumb">Overview</strong></div>
      <div class="factory-status">
        <span id="factoryClock">--</span><span id="factoryDay"></span>
        <span class="factory-live"><i></i>LIVE<span id="factoryLatency">--</span></span>
      </div>
      <aside class="factory-inspector" id="factoryInspector" hidden>
        <header><h2 id="factoryInspectorTitle">STATION</h2><button type="button" class="factory-inspector-close" id="factoryInspectorClose" aria-label="Close station detail">&#10005;</button></header>
        <p class="factory-inspector-zone" id="factoryInspectorZone"></p>
        <dl class="factory-readout" id="factoryReadout"></dl>
        <h3>STRUCTURE OVERVIEW</h3>
        <canvas id="factorySpark" width="280" height="72" aria-hidden="true"></canvas>
        <h3>WORK FLOW TIME</h3>
        <ul class="factory-log" id="factoryLog"></ul>
      </aside>
      <div class="factory-scenes">
        <button type="button" class="factory-arrow" id="factoryPrev" aria-label="Previous room">&#9664;</button>
        <div class="factory-scene-list" id="factorySceneList"></div>
        <button type="button" class="factory-arrow" id="factoryNext" aria-label="Next room">&#9654;</button>
      </div>
      <div class="factory-spec" id="factorySpec" hidden></div>
      <div class="factory-minimap"><canvas id="factoryMinimap" width="140" height="140" aria-hidden="true"></canvas></div>
      <table class="factory-list" id="factoryList" hidden>
        <caption>Room directory. Same stations, same targets, keyboard first.</caption>
        <thead><tr><th scope="col">Station</th><th scope="col">Zone</th><th scope="col">Reading</th></tr></thead>
        <tbody></tbody>
      </table>
      <p class="factory-hint">Drag to pan, scroll to zoom, click a station. \\ list mode, Esc closes.</p>
    </div>`;
  workspace.append(floor);

  const crumb = floor.querySelector('#factoryCrumb');
  if (crumb) crumb.textContent = FACTORY.scenes[FACTORY.sceneIndex];
  const bounds = floor.getBoundingClientRect();
  if (bounds.width && bounds.height) fitCamera(bounds.width, bounds.height);

  bindFactoryEvents(floor);
  buildScenes(floor);
  buildList(floor);
  updateClock(floor);
  refreshLatency(floor);
  FACTORY.timers = {
    clock: setInterval(() => updateClock(floor), 1000),
    latency: setInterval(() => refreshLatency(floor), 15000),
    repaint: setInterval(() => renderFrame(floor), 5000),
  };
  renderFrame(floor);
  requestAnimationFrame(() => renderFrame(floor));
}

function unmountFactory() {
  if (!FACTORY.mounted) return;
  const floor = document.getElementById('factoryFloor');
  if (floor) floor.remove();
  if (FACTORY.timers) Object.values(FACTORY.timers).forEach(clearInterval);
  FACTORY.timers = null;
  FACTORY.mounted = false;
  FACTORY.hover = null;
  FACTORY.anchors.clear();
}

function renderFrame(floor = document.getElementById('factoryFloor')) {
  if (!floor) return;
  const canvas = floor.querySelector('#factoryCanvas');
  if (!canvas) return;
  const rect = canvas.getBoundingClientRect();
  if (!rect.width || !rect.height) return;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const width = Math.round(rect.width);
  const height = Math.round(rect.height);
  if (canvas.width !== Math.round(width * dpr) || canvas.height !== Math.round(height * dpr)) {
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
  }
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  drawScene(ctx, width, height);
  drawMinimap(floor);
  positionSpecCard(floor);
}


// ------------------------------------------------------------------- chrome --

const DAYS = ['SUNDAY', 'MONDAY', 'TUESDAY', 'WEDNESDAY', 'THURSDAY', 'FRIDAY', 'SATURDAY'];

function clockText(now = new Date()) {
  const pad = value => String(value).padStart(2, '0');
  return `${now.getFullYear()}/${pad(now.getMonth() + 1)}/${pad(now.getDate())} ${pad(now.getHours())}:${pad(now.getMinutes())}`;
}

function dayText(now = new Date()) {
  return DAYS[now.getDay()];
}

function updateClock(floor = document.getElementById('factoryFloor')) {
  if (!floor) return;
  const now = new Date();
  const clock = floor.querySelector('#factoryClock');
  const day = floor.querySelector('#factoryDay');
  if (clock) clock.textContent = clockText(now);
  if (day) day.textContent = dayText(now);
}

/** Latency is a trust reading: only ever show a measured round trip. */
async function refreshLatency(floor = document.getElementById('factoryFloor')) {
  if (!floor) return;
  const target = floor.querySelector('#factoryLatency');
  if (!target) return;
  const started = performance.now();
  try {
    const response = await fetch('/api/health', { cache: 'no-store' });
    if (!response.ok) throw new Error('offline');
    await response.json();
    FACTORY.latency = Math.round(performance.now() - started);
    target.textContent = `${FACTORY.latency}ms`;
    target.dataset.state = 'live';
  } catch (_) {
    FACTORY.latency = null;
    target.textContent = 'offline';
    target.dataset.state = 'offline';
  }
}

function buildScenes(floor) {
  const list = floor.querySelector('#factorySceneList');
  list.replaceChildren();
  FACTORY.scenes.forEach((name, index) => {
    const button = factoryElement('button', 'factory-scene', name);
    button.type = 'button';
    button.dataset.scene = String(index);
    button.setAttribute('aria-pressed', String(index === FACTORY.sceneIndex));
    button.addEventListener('click', () => setScene(index, floor));
    list.append(button);
  });
  floor.querySelector('#factoryPrev').addEventListener('click', () => setScene(FACTORY.sceneIndex - 1, floor));
  floor.querySelector('#factoryNext').addEventListener('click', () => setScene(FACTORY.sceneIndex + 1, floor));
}

function setScene(index, floor = document.getElementById('factoryFloor')) {
  FACTORY.sceneIndex = (index + FACTORY.scenes.length) % FACTORY.scenes.length;
  const crumb = floor?.querySelector('#factoryCrumb');
  if (crumb && !FACTORY.selected) crumb.textContent = FACTORY.scenes[FACTORY.sceneIndex];
  floor?.querySelectorAll('.factory-scene').forEach((button, position) => {
    button.setAttribute('aria-pressed', String(position === FACTORY.sceneIndex));
  });
  renderFrame(floor);
}

function buildList(floor) {
  const body = floor.querySelector('#factoryList tbody');
  body.replaceChildren();
  for (const station of FACTORY.registry) {
    const row = factoryElement('tr');
    row.tabIndex = 0;
    row.dataset.station = station.id;
    row.append(
      factoryElement('th', null, station.name),
      factoryElement('td', null, station.zone),
      factoryElement('td', null, `${station.rows[0][0]}: ${station.rows[0][1]}`),
    );
    const select = () => selectStation(station, floor);
    row.addEventListener('click', select);
    row.addEventListener('keydown', event => {
      if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); select(); }
    });
    body.append(row);
  }
}

function drawMinimap(floor) {
  const canvas = floor.querySelector('#factoryMinimap');
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  const tile = 8;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.fillStyle = '#17191D';
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.save();
  ctx.beginPath();
  ctx.rect(0, 0, canvas.width, canvas.height);
  ctx.clip();
  for (const station of FACTORY.registry) {
    ctx.fillStyle = station.id === FACTORY.selected?.id ? FACTORY.palette.accent : '#4A5158';
    ctx.fillRect(station.x * tile + 10, station.y * tile + 10,
      Math.max(2, station.w * tile - 2), Math.max(2, station.d * tile - 2));
  }
  // Viewport outline, projected corner by corner so the minimap stays honest.
  const rect = canvas.getBoundingClientRect();
  const corners = [
    screenToTile(0, 0), screenToTile(rect.width, 0),
    screenToTile(rect.width, rect.height), screenToTile(0, rect.height),
  ];
  ctx.beginPath();
  corners.forEach((corner, index) => {
    const px = corner.x * tile + 10, py = corner.y * tile + 10;
    if (index) ctx.lineTo(px, py); else ctx.moveTo(px, py);
  });
  ctx.closePath();
  ctx.strokeStyle = FACTORY.palette.accent;
  ctx.lineWidth = 1;
  ctx.stroke();
  ctx.restore();
}

/** The anchored spec card: three rows, mono numerals, tied to its object. */
function positionSpecCard(floor) {
  const card = floor.querySelector('#factorySpec');
  if (!card) return;
  const station = FACTORY.hover || FACTORY.selected;
  const anchor = station && FACTORY.anchors.get(station.id);
  if (!station || !anchor || FACTORY.listMode) { card.hidden = true; return; }
  card.replaceChildren();
  card.append(factoryElement('strong', null, station.name));
  for (const [label, value] of station.rows.slice(0, 3)) {
    const row = factoryElement('div', 'factory-spec-row');
    row.append(factoryElement('span', null, label), factoryElement('b', null, value));
    card.append(row);
  }
  const bounds = floor.getBoundingClientRect();
  const width = 200;
  card.style.left = `${Math.min(Math.max(anchor.x + 14, 8), bounds.width - width - 8)}px`;
  card.style.top = `${Math.max(anchor.y - 74, 8)}px`;
  card.hidden = false;
}


// ----------------------------------------------------------------- selection --

function drawSpark(canvas, seed = 0) {
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  const width = canvas.width, height = canvas.height;
  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = '#1B1F23';
  ctx.fillRect(0, 0, width, height);
  ctx.strokeStyle = '#2C3137';
  ctx.lineWidth = 1;
  for (let index = 1; index < 4; index += 1) {
    ctx.beginPath();
    ctx.moveTo(0, (height / 4) * index);
    ctx.lineTo(width, (height / 4) * index);
    ctx.stroke();
  }
  ctx.beginPath();
  for (let index = 0; index <= 24; index += 1) {
    const t = index / 24;
    const value = 0.55 + 0.3 * Math.sin(t * 5 + seed) * Math.cos(t * 2.4 + seed * 0.7);
    const x = t * width, y = height - value * height * 0.85;
    if (index === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  }
  ctx.strokeStyle = FACTORY.palette.accent;
  ctx.lineWidth = 1.5;
  ctx.stroke();
}

function selectStation(station, floor = document.getElementById('factoryFloor')) {
  if (!floor) return;
  const crumb = floor.querySelector('#factoryCrumb');
  const inspector = floor.querySelector('#factoryInspector');
  if (!station) {
    FACTORY.selected = null;
    if (inspector) inspector.hidden = true;
    if (crumb) crumb.textContent = FACTORY.scenes[FACTORY.sceneIndex];
    renderFrame(floor);
    return;
  }
  FACTORY.selected = station;
  if (crumb) crumb.textContent = station.name;
  if (inspector) {
    inspector.hidden = false;
    floor.querySelector('#factoryInspectorTitle').textContent = station.name;
    floor.querySelector('#factoryInspectorZone').textContent = station.zone;
    const readout = floor.querySelector('#factoryReadout');
    readout.replaceChildren();
    for (const [label, value] of station.rows) {
      readout.append(factoryElement('dt', null, label), factoryElement('dd', null, value));
    }
    const log = floor.querySelector('#factoryLog');
    log.replaceChildren();
    for (const line of [
      `Opened on ${FACTORY.scenes[FACTORY.sceneIndex]}`,
      'Reading comes from the running station',
      'Selection stays inside this room',
    ]) log.append(factoryElement('li', null, line));
    drawSpark(floor.querySelector('#factorySpark'), station.id.length % 5);
  }
  renderFrame(floor);
}

/** Moves the camera to a station, keeping the room itself where it is. */
function flyTo(station, floor = document.getElementById('factoryFloor')) {
  const canvas = floor?.querySelector('#factoryCanvas');
  if (!canvas) return;
  const rect = canvas.getBoundingClientRect();
  const unit = FACTORY.unit * FACTORY.camera.zoom;
  const cx = station.x + station.w / 2;
  const cy = station.y + station.d / 2;
  FACTORY.camera.panX = rect.width / 2 - (cx - cy) * COS30 * unit;
  FACTORY.camera.panY = rect.height / 2 - (cx + cy) * SIN30 * unit + 40;
  selectStation(station, floor);
}

/** List mode: the same stations as a table, for keyboard and screen readers. */
function toggleList(floor = document.getElementById('factoryFloor'), force) {
  if (!floor) return;
  FACTORY.listMode = force == null ? !FACTORY.listMode : force;
  const table = floor.querySelector('#factoryList');
  if (table) table.hidden = !FACTORY.listMode;
  if (FACTORY.listMode) table?.querySelector('tbody tr')?.focus();
  else floor.querySelector('#factoryCanvas')?.focus?.();
  renderFrame(floor);
}


// ------------------------------------------------------------------- events --

function bindFactoryEvents(floor) {
  const canvas = floor.querySelector('#factoryCanvas');
  if (!canvas) return;
  let drag = null;

  const local = event => {
    const rect = canvas.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  };

  canvas.addEventListener('pointerdown', event => {
    const point = local(event);
    drag = { ...point, panX: FACTORY.camera.panX, panY: FACTORY.camera.panY, moved: false };
    canvas.setPointerCapture?.(event.pointerId);
  });

  canvas.addEventListener('pointermove', event => {
    const point = local(event);
    if (drag) {
      const dx = point.x - drag.x, dy = point.y - drag.y;
      if (Math.abs(dx) > 3 || Math.abs(dy) > 3) drag.moved = true;
      if (drag.moved) {
        FACTORY.camera.panX = drag.panX + dx;
        FACTORY.camera.panY = drag.panY + dy;
        renderFrame(floor);
        return;
      }
    }
    const station = hitTest(point.x, point.y);
    if ((station?.id || null) !== (FACTORY.hover?.id || null)) {
      FACTORY.hover = station;
      canvas.style.cursor = station ? 'pointer' : 'grab';
      renderFrame(floor);
    }
  });

  canvas.addEventListener('pointerup', event => {
    const point = local(event);
    const moved = drag?.moved;
    drag = null;
    if (moved) return;
    selectStation(hitTest(point.x, point.y), floor);
  });

  canvas.addEventListener('pointerleave', () => {
    drag = null;
    if (FACTORY.hover) { FACTORY.hover = null; renderFrame(floor); }
  });

  canvas.addEventListener('wheel', event => {
    event.preventDefault();
    const point = local(event);
    const before = screenToTile(point.x, point.y);
    FACTORY.camera.zoom = Math.min(1.5, Math.max(0.5,
      FACTORY.camera.zoom + (event.deltaY > 0 ? -0.08 : 0.08)));
    const unit = FACTORY.unit * FACTORY.camera.zoom;
    FACTORY.camera.panX = point.x - (before.x - before.y) * COS30 * unit;
    FACTORY.camera.panY = point.y - (before.x + before.y) * SIN30 * unit;
    renderFrame(floor);
  }, { passive: false });

  floor.querySelector('#factoryInspectorClose')?.addEventListener('click', () => selectStation(null, floor));

  canvas.addEventListener('keydown', event => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (FACTORY.hover) selectStation(FACTORY.hover, floor);
    }
  });

  document.addEventListener('keydown', event => {
    if (!FACTORY.mounted) return;
    if (event.target && /INPUT|TEXTAREA|SELECT/.test(event.target.tagName)) return;
    if (event.key === 'Escape') selectStation(null, floor);
    else if (event.key === '\\') { event.preventDefault(); toggleList(floor); }
    else if (event.key === 'F' || event.key === 'f') {
      // Full-bleed is deliberately the only state that leaves the room metaphor.
      if (FACTORY.selected && (FACTORY.selected.kind === 'bank' || FACTORY.selected.id === 'canvas')) {
        canvas.classList.toggle('is-fullbleed');
        renderFrame(floor);
      }
    } else if (['1', '2', '3', '4'].includes(event.key) && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      setScene(Number(event.key) - 1, floor);
    }
  });

  window.addEventListener('resize', () => renderFrame(floor));
}


// ------------------------------------------------------------------- styles --

const FACTORY_STYLES = `
.app-shell.factory-route .dashboard-layout { position:relative; grid-template-columns:56px minmax(0,1fr); gap:0 }
.app-shell.factory-route .workspace { position:relative; padding:0; overflow:hidden }
.app-shell.factory-route #homePage,
.app-shell.factory-route .subsystem-page { display:none !important }
.app-shell.factory-route .assistant-panel { display:none }
.app-shell.factory-route .side-rail { padding:55px 0 25px; border-right:1px solid #2C3137 }
.app-shell.factory-route .side-rail .nav-item { gap:0; justify-content:center; padding:0 }
.app-shell.factory-route .side-rail .nav-item > span:last-child { display:none }
.app-shell.factory-route .side-rail:hover { position:absolute; z-index:30; top:96px; bottom:0; left:0; width:210px;
  background:#1A1D21; box-shadow:1px 0 0 #2C3137; overflow-y:auto }
.app-shell.factory-route .side-rail:hover .nav-item { gap:13px; justify-content:flex-start; padding:0 13px }
.app-shell.factory-route .side-rail:hover .nav-item > span:last-child { display:inline }
.factory-floor { position:absolute; inset:0; background:#17191D; overflow:hidden; isolation:isolate }
.factory-canvas { display:block; width:100%; height:100%; cursor:grab; touch-action:none }
.factory-canvas:focus-visible { outline:2px solid #F26B21; outline-offset:-2px }
.factory-chrome { position:absolute; inset:0; pointer-events:none }
.factory-chrome button, .factory-chrome table, .factory-inspector { pointer-events:auto }
.factory-breadcrumb { position:absolute; top:14px; left:18px; display:flex; align-items:center; gap:8px;
  font:500 10px/1 ui-monospace,"SF Mono",Menlo,Consolas,monospace; letter-spacing:.08em; color:#8A919B }
.factory-breadcrumb i { font-style:normal; color:#3A3E45 }
.factory-breadcrumb strong { color:#C9CDD4; font-weight:600 }
.factory-status { position:absolute; top:12px; left:50%; transform:translateX(-50%); display:flex; align-items:center; gap:14px;
  padding:6px 14px; border:1px solid #2C3137; border-radius:4px; background:#1B1F23E6;
  font:500 11px/1 ui-monospace,"SF Mono",Menlo,Consolas,monospace; letter-spacing:.06em; color:#C9CDD4;
  font-variant-numeric:tabular-nums }
.factory-status #factoryDay { color:#8A919B }
.factory-live { display:inline-flex; align-items:center; gap:6px; color:#5FD3A6 }
.factory-live i { width:6px; height:6px; border-radius:50%; background:#5FD3A6; box-shadow:0 0 6px #5FD3A6 }
.factory-live span[data-state="offline"] { color:#FF4438 }
.factory-scenes { position:absolute; bottom:16px; left:50%; transform:translateX(-50%); display:flex; align-items:center; gap:6px;
  padding:5px; border:1px solid #2C3137; border-radius:4px; background:#1B1F23E6 }
.factory-scene-list { display:flex; gap:4px }
.factory-scene, .factory-arrow { font:500 10px/1 ui-monospace,"SF Mono",Menlo,Consolas,monospace; letter-spacing:.08em;
  text-transform:uppercase; color:#8A919B; background:transparent; border:1px solid transparent; border-radius:3px; padding:6px 10px }
.factory-scene:hover, .factory-arrow:hover { color:#C9CDD4; border-color:#3A3E45 }
.factory-scene[aria-pressed="true"] { color:#F26B21; border-color:#F26B21; background:#F26B2114 }
.factory-scene:focus-visible, .factory-arrow:focus-visible, .factory-inspector-close:focus-visible,
.factory-list tbody tr:focus-visible { outline:2px solid #F26B21; outline-offset:2px }
.factory-spec { position:absolute; width:200px; padding:8px 10px; border:1px solid #F26B21; border-radius:4px;
  background:#1B1F23F2; color:#C9CDD4; font:11px/1.5 ui-monospace,"SF Mono",Menlo,Consolas,monospace;
  font-variant-numeric:tabular-nums }
.factory-spec strong { display:block; margin-bottom:5px; color:#F26B21; font-size:10px; letter-spacing:.1em }
.factory-spec-row { display:flex; justify-content:space-between; gap:10px }
.factory-spec-row span { color:#8A919B }
.factory-spec-row b { font-weight:500 }
.factory-inspector { position:absolute; top:0; right:0; bottom:0; width:300px; padding:16px 16px 22px;
  border-left:1px solid #2C3137; background:#1A1D21F2; color:#C9CDD4; overflow-y:auto;
  font:11px/1.5 ui-monospace,"SF Mono",Menlo,Consolas,monospace }
.factory-inspector header { display:flex; align-items:center; justify-content:space-between; gap:10px }
.factory-inspector h2 { margin:0; color:#F26B21; font-size:11px; letter-spacing:.1em }
.factory-inspector h3 { margin:18px 0 8px; color:#8A919B; font-size:10px; font-weight:500; letter-spacing:.1em }
.factory-inspector-zone { margin:3px 0 0; color:#8A919B; font-size:10px; letter-spacing:.08em }
.factory-inspector-close { border:1px solid #2C3137; border-radius:3px; background:transparent; color:#8A919B; padding:2px 7px }
.factory-readout { display:grid; grid-template-columns:1fr auto; gap:6px 12px; margin:14px 0 0; font-variant-numeric:tabular-nums }
.factory-readout dt { color:#8A919B }
.factory-readout dd { margin:0; text-align:right }
.factory-inspector canvas { display:block; width:100%; border:1px solid #2C3137; border-radius:3px }
.factory-log { display:grid; gap:7px; margin:0; padding:0; list-style:none }
.factory-log li { padding-left:10px; border-left:1px solid #3A3E45; color:#8A919B }
.factory-minimap { position:absolute; right:16px; bottom:16px; padding:5px; border:1px solid #2C3137;
  border-radius:3px; background:#1B1F23E6 }
.factory-minimap canvas { display:block }
.factory-list { position:absolute; top:56px; left:18px; min-width:340px; border-collapse:collapse; background:#1B1F23F2;
  border:1px solid #2C3137; font:11px/1.6 ui-monospace,"SF Mono",Menlo,Consolas,monospace; font-variant-numeric:tabular-nums }
.factory-list caption { padding:8px 10px; color:#8A919B; text-align:left; font-size:10px; letter-spacing:.06em }
.factory-list th, .factory-list td { padding:5px 10px; text-align:left; border-top:1px solid #2C3137 }
.factory-list thead th { color:#8A919B; font-weight:500; font-size:10px; letter-spacing:.08em }
.factory-list tbody th { color:#F26B21; font-weight:500 }
.factory-list tbody tr { cursor:pointer }
.factory-list tbody tr:hover { background:#F26B2114 }
.factory-hint { position:absolute; left:18px; bottom:56px; margin:0; color:#5A6169;
  font:10px/1.5 ui-monospace,"SF Mono",Menlo,Consolas,monospace }
.factory-canvas.is-fullbleed { position:fixed; inset:0; z-index:60; background:#17191D }
@media (max-width:1100px) { .factory-inspector { width:250px } .factory-minimap { display:none } }
@media (max-width:760px) {
  .factory-inspector { position:absolute; top:auto; left:0; right:0; bottom:0; width:auto; max-height:46%;
    border-left:0; border-top:1px solid #2C3137 }
  .factory-hint { display:none }
  .factory-scenes { bottom:8px; max-width:calc(100% - 24px); overflow-x:auto }
}
@media (prefers-reduced-motion:reduce) {
  .factory-scene, .factory-arrow, .factory-canvas { transition:none !important; animation:none !important }
}
`;

function injectFactoryStyles() {
  if (document.getElementById('factory-styles')) return;
  const style = document.createElement('style');
  style.id = 'factory-styles';
  style.textContent = FACTORY_STYLES;
  document.head.append(style);
}



// ------------------------------------------------------------------ routing --

/** Route ownership: the route decides, and the room never mounts twice. */
function showFactoryRoute() {
  const workspace = document.getElementById('workspace');
  const shell = document.getElementById('appShell');
  if (!workspace) return;
  injectFactoryStyles();
  shell?.classList.add('factory-route');
  if (location.hash !== '#factory') history.replaceState(null, '', '#factory');
  document.title = 'SAIL — Factory';
  mountFactory(workspace);
}

function hideFactoryRoute() {
  document.getElementById('appShell')?.classList.remove('factory-route');
  unmountFactory();
}

window.addEventListener('sail:page', event => {
  if (event.detail === 'factory') showFactoryRoute();
  else hideFactoryRoute();
});

window.sailFactory = {
  show: () => {
    if (typeof window.sailNavigate === 'function') window.sailNavigate('factory');
    else showFactoryRoute();
  },
  hide: hideFactoryRoute,
  state: FACTORY,
  isoPoint, screenToTile, depth, visibleStations, detailForZoom, hitTest, fitCamera,
  clockText, dayText,
};

if (typeof module !== 'undefined') {
  module.exports = {
    FACTORY, isoPoint, screenToTile, depth, visibleStations, detailForZoom,
    clockText, dayText, hitTest, fitCamera, showFactoryRoute, hideFactoryRoute,
  };
}

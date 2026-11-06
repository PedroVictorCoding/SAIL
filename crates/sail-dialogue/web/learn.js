// Arena Learn — Complete LearnHarness UI & Client Controller (#247).
// Pure helpers are exported for Node unit tests (`crates/sail-dialogue/tests/learn_ui_test.cjs`);
// DOM wiring runs only in the browser.
(function (root) {
  'use strict';

  const CONFIDENCE_STOPS = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 95, 99, 100];
  const BRAILLE_FRAMES = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

  // ---------------------------------------------------------------------------
  // Pure Helpers (Tested by learn_ui_test.cjs)
  // ---------------------------------------------------------------------------

  /** Splits an SSE text buffer into complete `{event, data}` frames and the unfinished tail. */
  function learnParseSse(buffer) {
    const events = [];
    const parts = String(buffer || '').split(/\r?\n\r?\n/);
    const rest = parts.pop() ?? '';
    for (const block of parts) {
      let event = 'message';
      const data = [];
      for (const line of block.split(/\r?\n/)) {
        if (line.startsWith('event:')) event = line.slice(6).trim();
        else if (line.startsWith('data:')) data.push(line.slice(5).trimStart());
      }
      if (data.length) events.push({event, data: data.join('\n')});
    }
    return {events, rest};
  }

  /** Computes the learning-map steps in teaching order. Every state has a text label. */
  function learnStepStates(view) {
    const plan = view && view.plan;
    if (!plan || !Array.isArray(plan.nodes)) return [];
    const byId = new Map(plan.nodes.map(n => [n.id, n]));
    const visited = new Set((view.progress && view.progress.visited) || []);
    const current = view.progress && view.progress.current;
    const order = Array.isArray(plan.order) && plan.order.length ? plan.order : plan.nodes.map(n => n.id);
    return order.map(id => {
      const node = byId.get(id) || {id, title: id};
      const state = current === id ? 'current' : visited.has(id) ? 'visited' : 'upcoming';
      const label = state === 'current' ? 'Now' : state === 'visited' ? 'Covered' : 'Next';
      return {id: node.id, title: node.title, state, label};
    });
  }

  /** Human-readable session status; never implies mastery. */
  function learnStatus(view) {
    if (!view) return {kind: 'idle', text: '', retry: false};
    if (view.paused) return {kind: 'paused', text: 'Paused. Resume when you are ready.', retry: false};
    if (view.running) {
      const who = view.mode === 'assess'
        ? 'Finding your starting point…'
        : view.mode === 'practice'
        ? 'Preparing your practice problem…'
        : 'Preparing the next step…';
      return {kind: 'running', text: who, retry: false};
    }
    if (view.last_failure) {
      return {kind: 'failed', text: `Last turn did not finish: ${view.last_failure}`, retry: true};
    }
    if (view.active_question) {
      return {kind: 'waiting', text: 'Your turn: answer the question below.', retry: false};
    }
    if (view.practice && view.practice.active_problem) {
      return {kind: 'waiting', text: 'Your turn: work through the practice problem below.', retry: false};
    }
    return {kind: 'ready', text: 'Ready.', retry: false};
  }

  /** True when the browser should ask the server to start a owed turn. */
  function learnShouldAutoTurn(view, lastAutoTurnSeq) {
    if (!view || !view.needs_turn || view.running || view.paused || view.active_question || view.last_failure) {
      return false;
    }
    if (view.practice && view.practice.active_problem) return false;
    return view.last_seq !== lastAutoTurnSeq;
  }

  /** Snaps any 0..100 value to the nearest LearnHarness confidence stop. */
  // Context meter (#254): share of the history budget the next prompt replays. Local
  // models only, so it never shows a cost estimate.
  function learnContextMeter(view) {
    const fraction = Number(view && view.context_fraction);
    const percent = Number.isFinite(fraction) ? Math.max(0, Math.min(100, Math.round(fraction * 100))) : 0;
    return {
      percent,
      label: `Context ${percent}% used`,
      warn: percent >= 85 ? 'Context nearly full; older parts will be summarised.' : '',
    };
  }

  // Practice outcomes are session evidence, not mastery (#244).
  function learnPracticeText(outcome) {
    switch (outcome) {
      case 'solved': return 'Solved.';
      case 'solved_with_hints': return 'Solved with hints.';
      case 'not_solved': return 'Not solved yet.';
      case 'moved_on': return 'Moved on.';
      case 'surrendered': return 'Solution shown.';
      case 'dismissed': return 'Dismissed.';
      default: return String(outcome || '').replace(/_/g, ' ');
    }
  }

  function learnSnapConfidence(value) {
    const num = Number(value);
    if (!Number.isFinite(num)) return 50;
    let best = CONFIDENCE_STOPS[0];
    let bestDist = Math.abs(num - best);
    for (const stop of CONFIDENCE_STOPS) {
      const d = Math.abs(num - stop);
      if (d < bestDist) {
        best = stop;
        bestDist = d;
      }
    }
    return best;
  }

  /** Maps a 0..100 confidence percentage to the 1..5 scale expected by the core validator. */
  function pctToFiveScale(pct) {
    if (pct <= 20) return 1;
    if (pct <= 40) return 2;
    if (pct <= 60) return 3;
    if (pct <= 85) return 4;
    return 5;
  }

  /** Validates and shapes a learner answer before it is sent. */
  function learnBuildAnswer(kind, raw) {
    const answer = {};
    const dontKnow = Boolean(raw && raw.dont_know);
    const otherText = String((raw && raw.other) || '').trim();

    if (dontKnow) {
      answer.dont_know = true;
    } else if (kind === 'free_recall') {
      const text = String((raw && raw.text) || '').trim();
      if (!text) return {error: 'Write your answer first.'};
      answer.text = text;
    } else if (otherText) {
      answer.other = otherText;
    } else {
      const choices = Array.isArray(raw && raw.choices) ? raw.choices.filter(Boolean) : [];
      if (!choices.length) return {error: 'Choose an option first.'};
      if (kind === 'single' && choices.length !== 1) return {error: 'Choose one option.'};
      answer.choices = choices;
    }

    if (!dontKnow) {
      const confidence = raw && raw.confidence ? Number(raw.confidence) : null;
      if (confidence !== null) {
        if (!Number.isInteger(confidence) || confidence < 1 || confidence > 5) {
          return {error: 'Confidence must be between 1 and 5.'};
        }
        answer.confidence = confidence;
      }
      if (raw && raw.confidence_pct !== undefined && raw.confidence_pct !== '' && raw.confidence_pct !== null) {
        const pct = learnSnapConfidence(raw.confidence_pct);
        answer.confidence_pct = pct;
        if (!answer.confidence) {
          answer.confidence = pctToFiveScale(pct);
        }
      }
    }

    const thinking = String((raw && raw.thinking) || '').trim();
    if (thinking) answer.thinking = thinking;
    return {answer};
  }

  /** Effective verdict key for a question (`correct`, `partial`, `incorrect`, `unclear`, `could_not_judge`, `dont_know`). */
  function learnVerdict(question) {
    if (question && question.answer && question.answer.dont_know) return 'dont_know';
    const judgment = question && question.judgment;
    if (judgment && judgment.verdict) return judgment.verdict;
    const grade = question && question.grade;
    if (grade === 'correct' || grade === 'incorrect') return grade;
    return 'pending';
  }

  /** Plain text for a question's grade or free-recall judgment; never says "mastered". */
  function learnGradeText(question) {
    if (question && question.answer && question.answer.dont_know) return "Didn't know.";
    const judgment = question && question.judgment;
    if (judgment) {
      if (judgment.verdict === 'correct') return 'Reasoned through.';
      if (judgment.verdict === 'partial') return 'Partly there.';
      if (judgment.verdict === 'incorrect') return 'Needs another pass.';
      if (judgment.verdict === 'unclear') {
        return judgment.follow_up ? 'Unclear — answer the follow-up below.' : 'Still not clear; the tutor will re-explain.';
      }
      if (judgment.verdict === 'could_not_judge') return 'SAIL could not judge this answer.';
    }
    const grade = question && question.grade;
    if (grade === 'correct') return 'Checked: matches the expected option.';
    if (grade === 'incorrect') return 'Checked: different from the expected option.';
    if (grade === 'needs_judgment') return 'Recorded for the tutor to review.';
    return '';
  }

  /** Lines of judgment detail to show under a free-recall answer (missing relation + feedback). */
  function learnJudgmentLines(question) {
    const judgment = question && question.judgment;
    if (!judgment) return [];
    const lines = [];
    if ((judgment.verdict === 'partial' || judgment.verdict === 'incorrect') && judgment.missing) {
      lines.push(`Missing: ${judgment.missing}`);
    }
    if (judgment.feedback) lines.push(judgment.feedback);
    return lines;
  }

  /** Relative timestamp formatter matching LearnHarness sidebar (`just now`, `5m ago`, `2h ago`, `3d ago`). */
  function learnRelativeTime(ms, nowMs = Date.now()) {
    if (!ms || ms <= 0) return 'just now';
    const diffSec = Math.max(0, Math.floor((nowMs - ms) / 1000));
    if (diffSec < 45) return 'just now';
    const mins = Math.floor(diffSec / 60);
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
  }

  /** Filters and fuzzy-searches sessions for the LearnHarness sidebar. */
  function learnFilterSessions(sessions, filter = 'all', query = '') {
    const q = String(query || '').trim().toLowerCase();
    return (sessions || []).filter(s => {
      const kind = s.kind || (s.mode === 'practice' ? 'practice' : 'teach');
      if (filter === 'teach' && kind !== 'teach') return false;
      if (filter === 'practice' && kind !== 'practice') return false;
      if (!q) return true;
      const hay = `${s.title || ''} ${s.goal || ''}`.toLowerCase();
      return hay.includes(q);
    });
  }

  /**
   * Computes a Sugiyama layered DAG layout (`nodes` with `{id, title, state, x, y, w, h}`,
   * `edges` with `{from, to, points}`, and overall `{width, height}`) for inline `PlanBlock`
   * and `PlanPopup` SVG rendering.
   */
  function learnLayoutPlanDag(plan, progress) {
    if (!plan || !Array.isArray(plan.nodes) || !plan.nodes.length) {
      return {nodes: [], edges: [], width: 0, height: 0};
    }
    const visited = new Set((progress && progress.visited) || []);
    const current = progress && progress.current;
    const rawEdges = Array.isArray(plan.edges) ? plan.edges : [];
    const nodeMap = new Map();
    for (const n of plan.nodes) {
      nodeMap.set(n.id, {
        id: n.id,
        title: n.title || n.id,
        state: current === n.id ? 'current' : visited.has(n.id) ? 'visited' : 'upcoming',
        layer: 0,
        order: 0,
      });
    }

    // Longest-path layering from prerequisites to goal
    const inDegree = new Map();
    const adj = new Map();
    for (const id of nodeMap.keys()) {
      inDegree.set(id, 0);
      adj.set(id, []);
    }
    for (const [u, v] of rawEdges) {
      if (nodeMap.has(u) && nodeMap.has(v) && u !== v) {
        adj.get(u).push(v);
        inDegree.set(v, (inDegree.get(v) || 0) + 1);
      }
    }
    const queue = [];
    for (const [id, deg] of inDegree.entries()) {
      if (deg === 0) queue.push(id);
    }
    while (queue.length) {
      const u = queue.shift();
      const uNode = nodeMap.get(u);
      for (const v of adj.get(u) || []) {
        const vNode = nodeMap.get(v);
        if (vNode.layer < uNode.layer + 1) vNode.layer = uNode.layer + 1;
        inDegree.set(v, inDegree.get(v) - 1);
        if (inDegree.get(v) === 0) queue.push(v);
      }
    }

    // Group by layer
    const layers = new Map();
    let maxLayer = 0;
    for (const n of nodeMap.values()) {
      if (!layers.has(n.layer)) layers.set(n.layer, []);
      layers.get(n.layer).push(n);
      if (n.layer > maxLayer) maxLayer = n.layer;
    }

    const NODE_W = 164;
    const NODE_H = 44;
    const GAP_X = 28;
    const GAP_Y = 42;
    const PAD_X = 24;
    const PAD_Y = 20;

    let maxRowCols = 1;
    for (const list of layers.values()) {
      if (list.length > maxRowCols) maxRowCols = list.length;
    }

    const totalWidth = Math.max(320, PAD_X * 2 + maxRowCols * NODE_W + (maxRowCols - 1) * GAP_X);
    const totalHeight = PAD_Y * 2 + (maxLayer + 1) * NODE_H + maxLayer * GAP_Y;

    for (let l = 0; l <= maxLayer; l++) {
      const list = layers.get(l) || [];
      const rowW = list.length * NODE_W + Math.max(0, list.length - 1) * GAP_X;
      const startX = (totalWidth - rowW) / 2;
      const y = PAD_Y + l * (NODE_H + GAP_Y);
      list.forEach((n, idx) => {
        n.x = startX + idx * (NODE_W + GAP_X);
        n.y = y;
        n.w = NODE_W;
        n.h = NODE_H;
      });
    }

    const laidOutEdges = [];
    for (const [u, v] of rawEdges) {
      const from = nodeMap.get(u);
      const to = nodeMap.get(v);
      if (!from || !to) continue;
      const x1 = from.x + from.w / 2;
      const y1 = from.y + from.h;
      const x2 = to.x + to.w / 2;
      const y2 = to.y;
      laidOutEdges.push({from: u, to: v, x1, y1, x2, y2});
    }

    return {
      nodes: Array.from(nodeMap.values()),
      edges: laidOutEdges,
      width: totalWidth,
      height: totalHeight,
    };
  }

  /** One capture at a time; invalidated operations can never fill a different dock. */
  function learnCreateDictation({capture, transcribe, changed}) {
    let serial = 0, phase = 'idle', recording = null, abort = null, target = null;
    const update = (next, message = '') => { phase = next; changed({phase, target, message}); };
    return {
      get busy() { return ['requesting', 'recording', 'transcribing'].includes(phase); },
      cancel() {
        serial++; recording?.cancel(); recording = null; abort?.abort(); abort = null;
        target = null; update('idle');
      },
      async start(field) {
        if (this.busy) return;
        const id = ++serial; target = field; abort = new AbortController(); update('requesting');
        try {
          const session = await capture(() => this.stop(), abort.signal);
          if (id !== serial) { session.cancel(); return; }
          recording = session; update('recording');
        } catch (error) {
          if (id === serial) update('error', error.name === 'NotAllowedError'
            ? 'Microphone permission denied. Allow access in browser settings or type your answer.' : error.message);
        }
      },
      async stop() {
        if (phase !== 'recording') return;
        const id = serial, field = target, session = recording;
        abort = new AbortController(); update('transcribing');
        try {
          const audio = await session.finish();
          if (id !== serial) return;
          recording = null;
          const text = String(await transcribe(audio, abort.signal)).trim();
          if (id !== serial) return;
          if (!text) throw new Error('No speech was recognized. Try again or type your answer.');
          if (!field.isConnected) { this.cancel(); return; }
          // Append to the current draft, including edits made while STT was running.
          const combined = field.value + (field.value && !/\s$/.test(field.value) ? ' ' : '') + text;
          if (field.maxLength > 0 && combined.length > field.maxLength) {
            throw new Error('The transcript is too long for this field. Try a shorter recording. Your draft is unchanged.');
          }
          field.value = combined;
          field.dispatchEvent(new Event('input', {bubbles: true}));
          update('filled', 'Transcript added. Edit it before submitting.');
          field.focus();
        } catch (error) {
          if (id === serial) { session.cancel(); recording = null; update('error', error.message); }
        }
      },
    };
  }

  /** Same resampling worklet and PCM format used by Dialogue's local speech path. */
  async function learnCaptureAudio(onLimit, signal) {
    if (!root.isSecureContext || !root.navigator.mediaDevices?.getUserMedia) {
      throw new Error('Microphone requires HTTPS or localhost. You can still type your answer.');
    }
    let stream, context, source, node, timer, flushTimer, pendingFlush;
    let frames = [], size = 0;
    const cleanup = () => {
      clearTimeout(timer); clearTimeout(flushTimer);
      signal?.removeEventListener('abort', cancel);
      stream?.getTracks().forEach(track => { track.onended = null; track.stop(); });
      if (node) { node.port.onmessage = null; node.disconnect(); }
      source?.disconnect(); context?.close().catch(() => {});
    };
    const cancel = () => { cleanup(); frames = []; pendingFlush?.(new Error('Recording cancelled.')); pendingFlush = null; };
    signal?.addEventListener('abort', cancel, {once: true});
    const checkCancelled = () => { if (signal?.aborted) throw new Error('Recording cancelled.'); };
    try {
      checkCancelled();
      stream = await root.navigator.mediaDevices.getUserMedia({audio: {channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true}});
      checkCancelled();
      context = new root.AudioContext();
      await context.resume();
      await context.audioWorklet.addModule('/capture.js');
      checkCancelled();
      source = context.createMediaStreamSource(stream);
      node = new root.AudioWorkletNode(context, 'sail-capture');
      node.port.onmessage = ({data}) => {
        if (data?.type === 'flushed') { pendingFlush?.(); pendingFlush = null; return; }
        if (!(data instanceof ArrayBuffer)) return;
        if (size + data.byteLength > 16000 * 2 * 60) { onLimit(); return; }
        frames.push(data); size += data.byteLength;
      };
      stream.getTracks().forEach(track => { track.onended = onLimit; });
      source.connect(node); node.connect(context.destination);
      // Leave headroom for the final padded 512-sample frame.
      timer = setTimeout(onLimit, 59000);
      return {
        cancel,
        async finish() {
          try {
            await new Promise((resolve, reject) => {
              pendingFlush = error => error ? reject(error) : resolve();
              flushTimer = setTimeout(() => reject(new Error('Microphone did not finish recording. Try again.')), 1500);
              node.port.postMessage({action: 'flush', request_id: 1});
            });
            return new Blob(frames, {type: 'application/octet-stream'});
          } finally { cleanup(); frames = []; }
        },
      };
    } catch (error) { cancel(); throw error; }
  }

  // A spaced-practice reminder links here with the topic to practise (#257).
  function learnPracticeLink(hash) {
    const raw = String(hash || '');
    if (!raw.startsWith('#arena')) return '';
    const query = raw.split('?')[1] || '';
    if (!query) return '';
    try {
      return (new URLSearchParams(query).get('practice') || '').trim().slice(0, 300);
    } catch (_) {
      return '';
    }
  }

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = {
      learnCaptureAudio,
      learnCreateDictation,
      learnParseSse,
      learnStepStates,
      learnStatus,
      learnShouldAutoTurn,
      learnSnapConfidence,
      learnBuildAnswer,
      learnVerdict,
      learnGradeText,
      learnJudgmentLines,
      learnRelativeTime,
      learnFilterSessions,
      learnLayoutPlanDag,
      learnContextMeter,
      learnPracticeText,
      learnPracticeLink,
    };
  }
  if (!root || !root.document) return;

  // ---------------------------------------------------------------------------
  // Browser Controller — Full LearnHarness Experience in SAIL Arena
  // ---------------------------------------------------------------------------

  const $ = id => document.getElementById(id);

  const state = {
    sessionId: null,
    view: null,
    sessions: [],
    sessionFilter: 'all',
    sessionSearch: '',
    homeMode: 'teach',
    pickedProps: [],
    graph: {
      propositions: [],
      chains: [],
      sources: [],
      positions: {},
      render_settings: {
        node_size: 1,
        edge_width: 1,
        arrow_size: 1,
        label_size: 1,
        label_zoom: 1,
        show_cluster_labels: true,
      },
    },
    graphSearch: '',
    selectedGraphNode: null,
    chainTableOpen: false,
    chainSearch: '',
    chainSort: 'created',
    chainSelectedProps: new Set(),
    streamAbort: null,
    lastAutoTurnSeq: null,
    openQuestionId: null,
    openProblemId: null,
    dockCollapsed: false,
    sidebarCollapsed: false,
    spinnerFrame: 0,
    spinnerTimer: null,
    graphTransform: {x: 0, y: 0, k: 1},
    graphDraggingNode: null,
    graphPanning: null,
    hoveredGraphNode: null,
  };

  const dictation = learnCreateDictation({
    capture: learnCaptureAudio,
    async transcribe(audio, signal) {
      const headers = {'Content-Type': 'application/octet-stream'};
      const token = ownerToken();
      if (token) headers.Authorization = `Bearer ${token}`;
      const response = await fetch('/api/speech/transcribe', {method: 'POST', headers, body: audio, signal});
      const data = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(data.error || `Transcription failed (HTTP ${response.status}). Try again or type your answer.`);
      return data.text || '';
    },
    changed({phase, target, message}) {
      document.querySelectorAll('.learn-dictate').forEach(button => {
        const active = target?.id === button.dataset.field;
        const busy = dictation.busy;
        button.disabled = busy && !(active && phase === 'recording');
        button.setAttribute('aria-pressed', String(active && phase === 'recording'));
        button.textContent = active && phase === 'recording' ? '■ Stop recording' : '🎙 Dictate';
        button.setAttribute('aria-label', `${active && phase === 'recording' ? 'Stop recording' : 'Dictate'} ${button.dataset.label}`);
        const status = $(button.dataset.status);
        if (status) {
          status.textContent = active ? ({requesting: 'Waiting for microphone permission…', recording: 'Recording… Speak, then select Stop. Stops after one minute.', transcribing: 'Transcribing on your SAIL host…'}[phase] || message) : '';
          status.dataset.phase = phase;
        }
      });
    },
  });

  function addDictation(box, field, label) {
    field.id = `learn-dictation-${field.name}`;
    field.setAttribute('aria-label', label);
    const controls = document.createElement('div');
    controls.className = 'learn-dictation-controls';
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'learn-dictate';
    button.dataset.field = field.id;
    button.dataset.label = label;
    button.dataset.status = `${field.id}-status`;
    button.textContent = '🎙 Dictate';
    button.setAttribute('aria-label', `Dictate ${label}`);
    button.setAttribute('aria-pressed', 'false');
    const status = document.createElement('span');
    status.id = button.dataset.status;
    status.className = 'learn-dictation-status';
    status.setAttribute('role', 'status');
    field.setAttribute('aria-describedby', status.id);
    button.addEventListener('click', () => {
      if (dictation.busy) dictation.stop(); else dictation.start(field);
    });
    controls.append(button, status);
    box.appendChild(controls);
  }

  function ownerToken() {
    try { return (localStorage.getItem('sail_owner_token') || '').trim(); } catch (_) { return ''; }
  }

  async function api(path, method = 'GET', body = null) {
    const headers = {};
    const token = ownerToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    if (body !== null) headers['Content-Type'] = 'application/json';
    const r = await fetch(`/api/learn${path}`, {
      method,
      headers,
      body: body !== null ? JSON.stringify(body) : undefined,
    });
    const data = await r.json().catch(() => ({}));
    if (!r.ok) {
      const err = new Error(data.error || `HTTP ${r.status}`);
      err.status = r.status;
      err.busy = Boolean(data.busy);
      throw err;
    }
    return data;
  }

  function escapeSvgText(str) {
    return String(str || '')
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  function renderPlanSvgMarkup(plan, progress, compact = false) {
    const dag = learnLayoutPlanDag(plan, progress);
    if (!dag.nodes.length) return '';
    const scale = compact ? 0.65 : 1;
    const w = Math.round(dag.width * scale);
    const h = Math.round(dag.height * scale);
    const defs = `
      <defs>
        <marker id="lhArrow" viewBox="0 0 10 10" refX="7" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
          <path d="M 0 1 L 8 5 L 0 9 z" fill="#636366"></path>
        </marker>
        <filter id="lhCurrentGlow" x="-25%" y="-25%" width="150%" height="150%">
          <feDropShadow dx="0" dy="0" stdDeviation="4" flood-color="#fbbf24" flood-opacity="0.45"/>
        </filter>
      </defs>`;

    const edgePaths = dag.edges.map(e => {
      const midY = (e.y1 + e.y2) / 2;
      const d = `M ${e.x1} ${e.y1} C ${e.x1} ${midY}, ${e.x2} ${midY}, ${e.x2} ${e.y2 - 4}`;
      return `<path d="${d}" fill="none" stroke="#52525b" stroke-width="1.6" marker-end="url(#lhArrow)"/>`;
    }).join('');

    const nodeBoxes = dag.nodes.map(n => {
      let fill = '#1c1c1c';
      let stroke = '#3f3f46';
      let textFill = '#d4d4d8';
      let filterAttr = '';
      let badge = '';
      if (n.state === 'visited') {
        fill = 'rgba(16, 185, 129, 0.14)';
        stroke = 'rgba(52, 211, 153, 0.55)';
        textFill = '#ecfdf5';
        badge = '✓ ';
      } else if (n.state === 'current') {
        fill = 'rgba(245, 158, 11, 0.16)';
        stroke = '#fbbf24';
        textFill = '#fef3c7';
        filterAttr = 'filter="url(#lhCurrentGlow)"';
        badge = '● ';
      }
      const label = (badge + n.title).slice(0, 26);
      const cx = n.x + n.w / 2;
      const cy = n.y + n.h / 2 + 4;
      return `
        <g class="lh-dag-node ${n.state}" data-node-id="${escapeSvgText(n.id)}" ${filterAttr}>
          <rect x="${n.x}" y="${n.y}" width="${n.w}" height="${n.h}" rx="8" fill="${fill}" stroke="${stroke}" stroke-width="1.5"/>
          <text x="${cx}" y="${cy}" text-anchor="middle" fill="${textFill}" font-size="12" font-family="Source Sans 3, Inter, sans-serif" font-weight="600">${escapeSvgText(label)}</text>
        </g>`;
    }).join('');

    return `<svg class="lh-plan-svg" viewBox="0 0 ${dag.width} ${dag.height}" width="${w}" height="${h}" role="img" aria-label="Lesson plan dependency graph">${defs}${edgePaths}${nodeBoxes}</svg>`;
  }

  function setStatus(text, kind = 'idle', retry = false) {
    const el = $('learnStatus');
    if (el) {
      el.textContent = text;
      el.dataset.kind = kind;
    }
    const retryBtn = $('learnRetry');
    if (retryBtn) retryBtn.hidden = !retry;
  }

  // ---------------------------------------------------------------------------
  // Sidebar & Session List Rendering
  // ---------------------------------------------------------------------------

  function updateSidebarCollapseUi() {
    const sb = $('learnSidebar');
    if (sb) sb.classList.toggle('collapsed', state.sidebarCollapsed);
    const homeOpen = $('learnHomeSidebarOpenBtn');
    if (homeOpen) homeOpen.hidden = !state.sidebarCollapsed;
    const sessOpen = $('learnSessionSidebarOpenBtn');
    if (sessOpen) sessOpen.hidden = !state.sidebarCollapsed;
  }

  function renderSidebarSessions() {
    const host = $('learnSessions');
    if (!host) return;
    host.replaceChildren();
    const countEl = $('learnSessionCount');
    if (countEl) countEl.textContent = String((state.sessions || []).length);

    const filtered = learnFilterSessions(state.sessions, state.sessionFilter, state.sessionSearch);
    if (!filtered.length) {
      const empty = document.createElement('p');
      empty.className = 'learn-hint';
      empty.style.padding = '8px 12px';
      empty.textContent = state.sessions.length ? 'No matching sessions.' : 'No sessions yet.';
      host.appendChild(empty);
      return;
    }

    const braille = BRAILLE_FRAMES[state.spinnerFrame % BRAILLE_FRAMES.length];
    for (const s of filtered) {
      const isPractice = s.kind === 'practice' || s.mode === 'practice';
      const isActive = s.id === state.sessionId;
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = `lh-session-row quiet-button learn-resume${isActive ? ' active' : ''}`;
      btn.dataset.sessionId = s.id;

      const main = document.createElement('div');
      main.className = 'lh-session-row-main';

      const titleLine = document.createElement('div');
      titleLine.className = 'lh-session-row-title-line';
      if (s.running) {
        const spin = document.createElement('span');
        spin.className = 'lh-braille-spinner';
        spin.textContent = braille;
        titleLine.appendChild(spin);
      }
      const titleSpan = document.createElement('span');
      titleSpan.className = 'lh-session-row-title';
      titleSpan.textContent = s.title || s.goal || s.id;
      titleLine.appendChild(titleSpan);
      main.appendChild(titleLine);

      const meta = document.createElement('div');
      const rel = learnRelativeTime(s.updated_ms || s.created_ms);
      if (s.waiting_question) {
        meta.className = 'lh-session-row-meta waiting';
        meta.textContent = `Waiting for input · ${rel}`;
      } else if (s.waiting_problem) {
        meta.className = 'lh-session-row-meta waiting';
        meta.textContent = `Problem waiting · ${rel}`;
      } else if (s.running) {
        meta.className = 'lh-session-row-meta waiting';
        meta.textContent = `Working… · ${rel}`;
      } else {
        meta.className = 'lh-session-row-meta';
        meta.textContent = rel;
      }
      main.appendChild(meta);

      const right = document.createElement('div');
      right.className = 'lh-session-row-right';
      if (isPractice) {
        const icon = document.createElement('span');
        icon.className = 'lh-session-kind-badge';
        icon.title = 'Practice session';
        icon.textContent = '🏋';
        right.appendChild(icon);
      } else if (s.node_count > 0) {
        const cov = document.createElement('span');
        cov.className = 'lh-session-kind-badge';
        cov.textContent = `${s.visited_count || 0}/${s.node_count}`;
        right.appendChild(cov);
      }

      btn.append(main, right);
      btn.addEventListener('click', () => openSession(s.id));
      host.appendChild(btn);
    }
  }

  async function refreshActivityDock() {
    try {
      const data = await api('/activity');
      const entries = (data && data.entries) || [];
      const sec = $('learnActivitySection');
      const list = $('learnActivityList');
      if (!sec || !list) return;
      if (!entries.length) {
        sec.hidden = true;
        list.replaceChildren();
        return;
      }
      sec.hidden = false;
      list.replaceChildren();
      const braille = BRAILLE_FRAMES[state.spinnerFrame % BRAILLE_FRAMES.length];
      for (const item of entries) {
        const row = document.createElement('div');
        row.className = 'lh-activity-item';
        const left = document.createElement('span');
        left.textContent = `${braille} ${item.label}`;
        const stopBtn = document.createElement('button');
        stopBtn.type = 'button';
        stopBtn.className = 'lh-icon-btn';
        stopBtn.textContent = '×';
        stopBtn.title = 'Abort background run';
        stopBtn.addEventListener('click', async e => {
          e.stopPropagation();
          await api(`/sessions/${encodeURIComponent(item.session_id)}/abort`, 'POST').catch(() => {});
          refreshActivityDock();
        });
        row.append(left, stopBtn);
        list.appendChild(row);
      }
    } catch (_) {}
  }

  // ---------------------------------------------------------------------------
  // Knowledge Graph 2D Canvas & Chain Table (graph-view.tsx + chain-table.tsx)
  // ---------------------------------------------------------------------------

  function ensureGraphPositions() {
    const props = state.graph.propositions || [];
    const pos = state.graph.positions || {};
    let changed = false;
    const count = props.length;
    props.forEach((p, idx) => {
      if (!pos[p.name]) {
        const angle = (2 * Math.PI * idx) / Math.max(1, count);
        const radius = Math.min(220, 70 + count * 14);
        pos[p.name] = {
          x: Math.cos(angle) * radius + ((idx % 3) - 1) * 18,
          y: Math.sin(angle) * radius + ((idx % 2) - 0.5) * 18,
        };
        changed = true;
      }
    });
    state.graph.positions = pos;
    return changed;
  }

  function renderGraphCanvas() {
    const canvas = $('learnGraphCanvas');
    const emptyEl = $('learnGraphEmpty');
    const statsPill = $('learnGraphStatsPill');
    const props = state.graph.propositions || [];
    const chains = state.graph.chains || [];
    if (statsPill) {
      statsPill.textContent = `${props.length} props · ${chains.length} chains`;
    }
    if (emptyEl) {
      emptyEl.hidden = props.length > 0;
    }
    if (!canvas) return;
    const rect = canvas.parentElement ? canvas.parentElement.getBoundingClientRect() : {width: 800, height: 520};
    const dpr = window.devicePixelRatio || 1;
    const width = Math.max(300, rect.width || 800);
    const height = Math.max(320, rect.height || 520);
    if (canvas.width !== Math.floor(width * dpr) || canvas.height !== Math.floor(height * dpr)) {
      canvas.width = Math.floor(width * dpr);
      canvas.height = Math.floor(height * dpr);
    }
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.save();
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, width, height);

    // Subtle radial dot grid background
    ctx.fillStyle = '#171717';
    ctx.fillRect(0, 0, width, height);

    if (!props.length) {
      ctx.restore();
      return;
    }

    ensureGraphPositions();
    const pos = state.graph.positions;
    const settings = state.graph.render_settings || {
      node_size: 1,
      edge_width: 1,
      arrow_size: 1,
      label_size: 1,
      show_cluster_labels: true,
    };

    const cx = width / 2 + state.graphTransform.x;
    const cy = height / 2 - 30 + state.graphTransform.y;
    const k = state.graphTransform.k;

    ctx.translate(cx, cy);
    ctx.scale(k, k);

    // 1. Cluster bounding hulls by birth source
    const clusters = new Map();
    for (const p of props) {
      const prov = (p.provenance && p.provenance[0]) || null;
      const key = prov ? (prov.session_id || prov.file_name || 'default') : 'default';
      const label = prov ? (prov.label || prov.file_name || prov.session_id || 'Session') : 'Knowledge';
      if (!clusters.has(key)) clusters.set(key, {label, points: []});
      const pt = pos[p.name];
      if (pt) clusters.get(key).points.push(pt);
    }

    const palette = [
      {fill: 'rgba(52, 211, 153, 0.06)', stroke: 'rgba(52, 211, 153, 0.28)'},
      {fill: 'rgba(251, 191, 36, 0.06)', stroke: 'rgba(251, 191, 36, 0.28)'},
      {fill: 'rgba(96, 165, 250, 0.06)', stroke: 'rgba(96, 165, 250, 0.28)'},
      {fill: 'rgba(167, 139, 250, 0.06)', stroke: 'rgba(167, 139, 250, 0.28)'},
    ];
    let cIdx = 0;
    for (const cluster of clusters.values()) {
      const pts = cluster.points;
      if (!pts.length) continue;
      const color = palette[cIdx++ % palette.length];
      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
      for (const pt of pts) {
        if (pt.x < minX) minX = pt.x;
        if (pt.x > maxX) maxX = pt.x;
        if (pt.y < minY) minY = pt.y;
        if (pt.y > maxY) maxY = pt.y;
      }
      const pad = 34;
      const rx = minX - pad;
      const ry = minY - pad;
      const rw = Math.max(68, maxX - minX + pad * 2);
      const rh = Math.max(68, maxY - minY + pad * 2);

      ctx.save();
      ctx.fillStyle = color.fill;
      ctx.strokeStyle = color.stroke;
      ctx.lineWidth = 1.2 / k;
      ctx.setLineDash([5 / k, 4 / k]);
      ctx.beginPath();
      ctx.roundRect(rx, ry, rw, rh, 18);
      ctx.fill();
      ctx.stroke();
      if (settings.show_cluster_labels) {
        ctx.fillStyle = 'rgba(250, 250, 250, 0.42)';
        ctx.font = `600 ${Math.round(10 * settings.label_size)}px "JetBrains Mono", monospace`;
        ctx.fillText(String(cluster.label).slice(0, 32).toUpperCase(), rx + 10, ry + 15);
      }
      ctx.restore();
    }

    // 2. Reasoning Chain Edges (`premises -> conclusion`)
    ctx.save();
    for (const ch of chains) {
      const toPt = pos[ch.conclusion];
      if (!toPt) continue;
      for (const prem of ch.premises || []) {
        const fromPt = pos[prem];
        if (!fromPt) continue;
        const dx = toPt.x - fromPt.x;
        const dy = toPt.y - fromPt.y;
        const dist = Math.hypot(dx, dy) || 1;
        const rNode = 8 * settings.node_size;
        const ex = toPt.x - (dx / dist) * (rNode + 3);
        const ey = toPt.y - (dy / dist) * (rNode + 3);

        ctx.strokeStyle = 'rgba(161, 161, 170, 0.52)';
        ctx.lineWidth = 1.5 * settings.edge_width;
        ctx.beginPath();
        ctx.moveTo(fromPt.x, fromPt.y);
        ctx.lineTo(ex, ey);
        ctx.stroke();

        // Arrowhead
        const angle = Math.atan2(dy, dx);
        const aLen = 7 * settings.arrow_size;
        ctx.fillStyle = 'rgba(212, 212, 216, 0.78)';
        ctx.beginPath();
        ctx.moveTo(ex, ey);
        ctx.lineTo(ex - aLen * Math.cos(angle - Math.PI / 6), ey - aLen * Math.sin(angle - Math.PI / 6));
        ctx.lineTo(ex - aLen * Math.cos(angle + Math.PI / 6), ey - aLen * Math.sin(angle + Math.PI / 6));
        ctx.closePath();
        ctx.fill();
      }
    }
    ctx.restore();

    // 3. Proposition Nodes & Labels
    const searchQ = (state.graphSearch || '').trim().toLowerCase();
    for (const p of props) {
      const pt = pos[p.name];
      if (!pt) continue;
      const isSelected = state.selectedGraphNode && state.selectedGraphNode.name === p.name;
      const isHovered = state.hoveredGraphNode && state.hoveredGraphNode.name === p.name;
      const isPicked = state.pickedProps.some(x => x.name === p.name);
      const isMatched = searchQ && (p.name.toLowerCase().includes(searchQ) || p.statement.toLowerCase().includes(searchQ));
      const r = (isSelected || isHovered ? 9.5 : 7.5) * settings.node_size;

      if (isSelected || isHovered || isMatched || isPicked) {
        ctx.save();
        ctx.beginPath();
        ctx.arc(pt.x, pt.y, r + 6, 0, Math.PI * 2);
        ctx.fillStyle = isPicked || isMatched ? 'rgba(251, 191, 36, 0.25)' : 'rgba(52, 211, 153, 0.25)';
        ctx.fill();
        ctx.restore();
      }

      ctx.beginPath();
      ctx.arc(pt.x, pt.y, r, 0, Math.PI * 2);
      ctx.fillStyle = isPicked || isMatched ? '#fbbf24' : isSelected ? '#34d399' : '#e4e4e7';
      ctx.fill();
      ctx.lineWidth = 1.5;
      ctx.strokeStyle = '#171717';
      ctx.stroke();

      ctx.fillStyle = isSelected || isHovered || isMatched ? '#fafafa' : 'rgba(250, 250, 250, 0.82)';
      ctx.font = `500 ${Math.round(11.5 * settings.label_size)}px "Source Sans 3", Inter, sans-serif`;
      ctx.textAlign = 'center';
      ctx.fillText(p.name, pt.x, pt.y + r + 13);
    }

    ctx.restore();
  }

  function showNodeDetailCard(prop) {
    state.selectedGraphNode = prop;
    const card = $('learnNodeDetailCard');
    if (!card) return;
    if (!prop) {
      card.hidden = true;
      renderGraphCanvas();
      return;
    }
    card.hidden = false;
    $('learnNodeCardName').textContent = prop.name;
    $('learnNodeCardStatement').textContent = prop.statement;
    const meta = $('learnNodeCardMeta');
    meta.replaceChildren();
    for (const prov of prop.provenance || []) {
      const chip = document.createElement('span');
      chip.className = 'lh-prop-chip';
      chip.textContent = prov.label || prov.file_name || prov.session_id || prov.kind;
      if (prov.session_id) {
        chip.title = 'Open source session';
        chip.addEventListener('click', () => openSession(prov.session_id));
      }
      meta.appendChild(chip);
    }
    renderGraphCanvas();
  }

  function renderChainTable() {
    const overlay = $('learnChainTableOverlay');
    if (!overlay || overlay.hidden) return;
    const props = state.graph.propositions || [];
    const chains = state.graph.chains || [];
    const countLabel = $('learnChainCountLabel');
    if (countLabel) {
      countLabel.textContent = `${chains.length} chains · ${props.length} nodes`;
    }

    const body = $('learnChainTableBody');
    if (!body) return;
    body.replaceChildren();

    const q = (state.chainSearch || '').trim().toLowerCase();
    const propByName = new Map(props.map(p => [p.name, p]));

    // Group propositions & chains by source
    const groups = new Map();
    for (const p of props) {
      if (q && !p.name.toLowerCase().includes(q) && !p.statement.toLowerCase().includes(q)) continue;
      const prov = (p.provenance && p.provenance[0]) || {kind: 'notes', label: 'Knowledge Graph', created_at: p.created_at};
      const gid = prov.session_id || (prov.file_name ? `notes:${prov.file_name}` : 'default');
      const label = prov.label || prov.file_name || prov.session_id || 'Knowledge Graph';
      if (!groups.has(gid)) {
        groups.set(gid, {id: gid, label, created_at: prov.created_at || p.created_at, props: [], chains: []});
      }
      groups.get(gid).props.push(p);
    }
    for (const ch of chains) {
      const gid = ch.source_id || 'default';
      if (groups.has(gid)) {
        groups.get(gid).chains.push(ch);
      }
    }

    const groupList = Array.from(groups.values());
    groupList.sort((a, b) => {
      if (state.chainSort === 'name') return a.label.localeCompare(b.label);
      if (state.chainSort === 'props') return b.props.length - a.props.length;
      return (b.created_at || 0) - (a.created_at || 0);
    });

    if (!groupList.length) {
      const empty = document.createElement('p');
      empty.className = 'learn-hint';
      empty.style.padding = '16px';
      empty.textContent = 'No propositions or chains match your filter.';
      body.appendChild(empty);
    }

    for (const g of groupList) {
      const block = document.createElement('div');
      block.className = 'lh-cluster-block';

      const head = document.createElement('div');
      head.className = 'lh-cluster-header';
      const nameCol = document.createElement('span');
      nameCol.textContent = `▾ ${g.label}`;
      const countCol = document.createElement('span');
      countCol.textContent = `${g.props.length} props`;
      const timeCol = document.createElement('span');
      timeCol.textContent = learnRelativeTime(g.created_at);
      head.append(nameCol, countCol, timeCol);

      const rows = document.createElement('div');
      rows.className = 'lh-cluster-rows';
      head.addEventListener('click', () => {
        rows.hidden = !rows.hidden;
        nameCol.textContent = `${rows.hidden ? '▸' : '▾'} ${g.label}`;
      });

      // Render reasoning chains (`A ∧ B ⟹ C`)
      for (const ch of g.chains) {
        const crow = document.createElement('div');
        crow.className = 'lh-chain-row';
        const formula = document.createElement('div');
        formula.className = 'lh-chain-formula';

        (ch.premises || []).forEach((premName, idx) => {
          if (idx > 0) {
            const andSpan = document.createElement('span');
            andSpan.className = 'lh-connective';
            andSpan.textContent = '∧';
            formula.appendChild(andSpan);
          }
          formula.appendChild(makePropChip(premName, propByName.get(premName)));
        });

        const impSpan = document.createElement('span');
        impSpan.className = 'lh-connective';
        impSpan.textContent = '⟹';
        formula.appendChild(impSpan);
        formula.appendChild(makePropChip(ch.conclusion, propByName.get(ch.conclusion)));
        crow.appendChild(formula);
        rows.appendChild(crow);
      }

      // Render propositions with statements
      for (const p of g.props) {
        const prow = document.createElement('div');
        prow.className = 'lh-chain-row';
        const top = document.createElement('div');
        top.className = 'lh-chain-formula';
        top.appendChild(makePropChip(p.name, p));
        const st = document.createElement('div');
        st.className = 'lh-prop-statement-line';
        st.textContent = p.statement;
        prow.append(top, st);
        rows.appendChild(prow);
      }

      block.append(head, rows);
      body.appendChild(block);
    }

    const selBar = $('learnChainSelectionBar');
    const selCount = $('learnChainSelectedCount');
    if (selBar && selCount) {
      const n = state.chainSelectedProps.size;
      selBar.hidden = n === 0;
      selCount.textContent = `${n} proposition${n === 1 ? '' : 's'} selected`;
    }
  }

  function makePropChip(name, propObj) {
    const chip = document.createElement('button');
    chip.type = 'button';
    const isSel = state.chainSelectedProps.has(name);
    chip.className = `lh-prop-chip${isSel ? ' selected' : ''}`;
    chip.textContent = `${isSel ? '☑ ' : '☐ '}${name}`;
    if (propObj && propObj.statement) chip.title = propObj.statement;
    chip.addEventListener('click', e => {
      e.stopPropagation();
      if (state.chainSelectedProps.has(name)) state.chainSelectedProps.delete(name);
      else state.chainSelectedProps.add(name);
      renderChainTable();
    });
    return chip;
  }

  async function refreshGraph() {
    try {
      const snap = await api('/graph');
      if (snap && Array.isArray(snap.propositions)) {
        state.graph = snap;
        renderGraphCanvas();
        renderChainTable();
      }
    } catch (_) {}
  }

  function updatePickedNodesUi() {
    const textEl = $('learnPickedNodesText');
    const pill = $('learnPickNodesBtn');
    const clearBtn = $('learnClearPickedNodesBtn');
    if (!textEl || !pill) return;
    if (state.pickedProps.length > 0) {
      textEl.textContent = `${state.pickedProps.length} proposition${state.pickedProps.length === 1 ? '' : 's'} picked`;
      pill.classList.add('has-picked');
      if (clearBtn) clearBtn.hidden = false;
    } else {
      textEl.textContent = 'Pick nodes from graph';
      pill.classList.remove('has-picked');
      if (clearBtn) clearBtn.hidden = true;
    }
  }

  function setHomeMode(mode) {
    state.homeMode = mode === 'practice' ? 'practice' : 'teach';
    const teachBtn = $('learnModeSwitchTeach');
    const pracBtn = $('learnModeSwitchPractice');
    if (teachBtn) teachBtn.classList.toggle('active', state.homeMode === 'teach');
    if (pracBtn) pracBtn.classList.toggle('active', state.homeMode === 'practice');
    const input = $('learnGoal');
    if (input) {
      input.placeholder = state.homeMode === 'practice'
        ? 'What do you want to practice?'
        : 'What do you want to learn?';
    }
  }

  // ---------------------------------------------------------------------------
  // Active Session Rendering (TopBar, Transcript, Inline Plan DAG, Docks, Stats)
  // ---------------------------------------------------------------------------

  function renderSteps(view) {
    const list = $('learnSteps');
    const empty = $('learnMapEmpty');
    const details = $('learnMapDetails');
    const diagram = $('learnDiagram');
    if (!list) return;
    const steps = learnStepStates(view);
    list.replaceChildren();
    if (empty) empty.hidden = steps.length > 0;
    for (const step of steps) {
      const li = document.createElement('li');
      li.className = `learn-step ${step.state}`;
      const stateSpan = document.createElement('span');
      stateSpan.className = 'learn-step-state';
      stateSpan.textContent = step.label;
      const title = document.createElement('span');
      title.className = 'learn-step-title';
      title.textContent = step.title;
      li.append(stateSpan, title);
      list.appendChild(li);
    }

    // Coverage pill & Plan button in TopBar
    const covBadge = $('learnCoverageBadge');
    const planPopupBtn = $('learnPlanPopupBtn');
    const marginalia = $('learnPlanMarginalia');
    if (steps.length > 0) {
      const visitedCount = steps.filter(s => s.state === 'visited' || s.state === 'current').length;
      if (covBadge) {
        covBadge.hidden = false;
        covBadge.textContent = `${visitedCount}/${steps.length} steps`;
      }
      if (planPopupBtn) planPopupBtn.hidden = false;
      if (marginalia && view.plan) {
        marginalia.hidden = false;
        marginalia.innerHTML = renderPlanSvgMarkup(view.plan, view.progress, true);
      }
    } else {
      if (covBadge) covBadge.hidden = true;
      if (planPopupBtn) planPopupBtn.hidden = true;
      if (marginalia) marginalia.hidden = true;
    }

    const mermaid = view && view.plan && view.plan.mermaid;
    if (details && diagram) {
      details.hidden = !mermaid;
      if (mermaid) {
        diagram.innerHTML = renderPlanSvgMarkup(view.plan, view.progress, false);
      }
    }
  }

  function renderAnsweredCard(q) {
    const card = document.createElement('div');
    card.className = 'lh-card';

    const header = document.createElement('div');
    header.className = 'lh-card-header';
    const eyebrow = document.createElement('span');
    eyebrow.className = 'lh-eyebrow';
    const toolLabel = (q.tool || (q.node_id ? 'check' : 'probe')).toUpperCase();
    eyebrow.textContent = q.question_type === 'predict' ? 'PREDICTION' : toolLabel;

    const vKey = learnVerdict(q);
    const badge = document.createElement('span');
    badge.className = `lh-verdict-badge learn-grade ${vKey}`;
    badge.textContent = learnGradeText(q) || vKey;
    header.append(eyebrow, badge);
    card.appendChild(header);

    const prompt = document.createElement('p');
    prompt.className = 'lh-card-prompt';
    prompt.textContent = q.prompt;
    card.appendChild(prompt);

    if (q.details) {
      const det = document.createElement('p');
      det.className = 'learn-hint';
      det.textContent = q.details;
      card.appendChild(det);
    }

    const ans = q.answer || {};
    const correctSet = new Set(q.correct || []);
    const chosenSet = new Set(ans.choices || []);

    if (Array.isArray(q.options) && q.options.length > 0) {
      const optList = document.createElement('div');
      optList.className = 'lh-answered-options';
      for (const opt of q.options) {
        const row = document.createElement('div');
        const isChosen = chosenSet.has(opt.id);
        const isRight = correctSet.has(opt.id);
        let cls = 'lh-answered-option';
        let mark = '·';
        if (isChosen && isRight) {
          cls += ' chosen-correct';
          mark = '✓';
        } else if (isChosen && !isRight) {
          cls += ' chosen-wrong';
          mark = '✗';
        } else if (!isChosen && isRight) {
          cls += ' missed-correct';
          mark = '✓';
        }
        row.className = cls;
        const mSpan = document.createElement('span');
        mSpan.className = 'lh-option-mark';
        mSpan.textContent = mark;
        const tSpan = document.createElement('span');
        tSpan.textContent = opt.text;
        row.append(mSpan, tSpan);
        optList.appendChild(row);
      }
      card.appendChild(optList);
    }

    if (ans.text || ans.other) {
      const written = document.createElement('p');
      written.className = 'learn-answer';
      written.textContent = `Your answer: ${ans.text || ans.other}`;
      card.appendChild(written);
    } else if (ans.dont_know) {
      const dk = document.createElement('p');
      dk.className = 'learn-answer';
      dk.textContent = "Selected: I don't know";
      card.appendChild(dk);
    }

    // Confidence Rail
    const pct = ans.confidence_pct !== undefined && ans.confidence_pct !== null
      ? ans.confidence_pct
      : ans.confidence
      ? ans.confidence * 20
      : null;
    if (pct !== null && !ans.dont_know) {
      const railRow = document.createElement('div');
      railRow.className = 'lh-confidence-rail-row';
      const label = document.createElement('span');
      label.textContent = `CONFIDENCE ${pct}%`;
      const rail = document.createElement('div');
      rail.className = 'lh-confidence-rail';
      const fill = document.createElement('div');
      fill.className = 'lh-confidence-rail-fill';
      fill.style.width = `${pct}%`;
      fill.style.background = pct >= 70 ? '#34d399' : pct >= 40 ? '#fbbf24' : '#f87171';
      rail.appendChild(fill);
      railRow.append(label, rail);
      card.appendChild(railRow);
    }

    if (ans.thinking) {
      const quote = document.createElement('div');
      quote.className = 'lh-thinking-quote';
      quote.textContent = `Your thinking: ${ans.thinking}`;
      card.appendChild(quote);
    }

    for (const detail of learnJudgmentLines(q)) {
      const line = document.createElement('div');
      line.className = 'lh-why-box learn-answer';
      line.textContent = detail;
      card.appendChild(line);
    }

    if (q.explanation) {
      const why = document.createElement('div');
      why.className = 'lh-why-box';
      why.textContent = `Why: ${q.explanation}`;
      card.appendChild(why);
    }

    return card;
  }

  function renderPracticeProblemEntry(entry, view) {
    const wrap = document.createElement('div');
    wrap.style.display = 'flex';
    wrap.style.flexDirection = 'column';
    wrap.style.gap = '10px';

    const probObj = view.practice && Array.isArray(view.practice.problems)
      ? view.practice.problems.find(p => p.id === entry.problem_id)
      : null;

    // 1. ProblemCard
    const pCard = document.createElement('div');
    pCard.className = 'lh-card';
    const head = document.createElement('div');
    head.className = 'lh-card-header';
    const eyebrow = document.createElement('span');
    eyebrow.className = 'lh-eyebrow';
    eyebrow.textContent = probObj && probObj.mode ? `PROBLEM · ${probObj.mode.toUpperCase()}` : 'PROBLEM';
    head.appendChild(eyebrow);
    pCard.appendChild(head);

    const stmt = document.createElement('p');
    stmt.className = 'lh-card-prompt';
    stmt.textContent = entry.statement;
    pCard.appendChild(stmt);

    if (probObj && Array.isArray(probObj.hints) && probObj.hints.length > 0) {
      for (let i = 0; i < probObj.hints.length; i++) {
        const hBox = document.createElement('div');
        hBox.className = 'lh-thinking-quote';
        hBox.textContent = `Hint ${i + 1}: ${probObj.hints[i]}`;
        pCard.appendChild(hBox);
      }
    }
    wrap.appendChild(pCard);

    // 2. Learner Thinking Cards
    if (probObj && Array.isArray(probObj.attempts)) {
      for (const att of probObj.attempts) {
        const tCard = document.createElement('div');
        tCard.className = 'lh-card';
        const tEyebrow = document.createElement('span');
        tEyebrow.className = 'lh-eyebrow';
        tEyebrow.textContent = 'YOUR THINKING';
        const tBody = document.createElement('p');
        tBody.className = 'lh-card-prompt';
        tBody.textContent = att;
        tCard.append(tEyebrow, tBody);
        wrap.appendChild(tCard);
      }
    }

    // 3. ConcludeCard if concluded
    if (probObj && probObj.outcome) {
      const cCard = document.createElement('div');
      cCard.className = 'lh-card';
      const cHead = document.createElement('div');
      cHead.className = 'lh-card-header';
      const cEyebrow = document.createElement('span');
      cEyebrow.className = 'lh-eyebrow';
      cEyebrow.textContent = 'CONCLUDED';
      const badge = document.createElement('span');
      badge.className = `lh-verdict-badge ${probObj.outcome}`;
      badge.textContent = learnPracticeText(probObj.outcome);
      cHead.append(cEyebrow, badge);
      cCard.appendChild(cHead);

      if (probObj.verdict) {
        const vP = document.createElement('p');
        vP.className = 'lh-card-prompt';
        vP.textContent = probObj.verdict;
        cCard.appendChild(vP);
      }

      if (probObj.solution) {
        const solBox = document.createElement('div');
        solBox.className = 'lh-why-box';
        const solTitle = document.createElement('div');
        solTitle.className = 'lh-eyebrow';
        solTitle.style.marginBottom = '4px';
        solTitle.textContent = 'MODEL SOLUTION';
        const solText = document.createElement('p');
        solText.textContent = probObj.solution;
        solBox.append(solTitle, solText);
        cCard.appendChild(solBox);
      }
      wrap.appendChild(cCard);
    }

    return wrap;
  }

  function renderTranscript(view) {
    const list = $('learnTranscript');
    if (!list) return;
    list.replaceChildren();
    let planRenderedInline = false;

    for (const entry of (view && view.transcript) || []) {
      const li = document.createElement('li');
      li.className = `learn-entry ${entry.type}`;

      if (entry.type === 'question') {
        if (!entry.answer && view.active_question === entry.id) {
          // Active question is shown in the persistent bottom QuestionDock; keep a light anchor in transcript
          const anchor = document.createElement('div');
          anchor.className = 'lh-phase-divider';
          anchor.textContent = `${(entry.tool || 'question').toUpperCase()} OPEN BELOW`;
          li.appendChild(anchor);
        } else {
          li.appendChild(renderAnsweredCard(entry));
        }
      } else if (entry.type === 'problem') {
        li.appendChild(renderPracticeProblemEntry(entry, view));
      } else if (entry.type === 'diagram') {
        const card = document.createElement('div');
        card.className = 'lh-card';
        const cap = document.createElement('span');
        cap.className = 'lh-eyebrow';
        cap.textContent = `DIAGRAM · ${entry.caption || entry.node_id}`;
        const pre = document.createElement('pre');
        pre.textContent = entry.mermaid;
        card.append(cap, pre);
        li.appendChild(card);
      } else if (entry.type === 'notice') {
        if (entry.text && entry.text.startsWith('Lesson plan revision') && view.plan && !planRenderedInline) {
          planRenderedInline = true;
          const pBlock = document.createElement('div');
          pBlock.className = 'lh-plan-block';
          pBlock.title = 'Click to expand Lesson Plan DAG';
          const pHead = document.createElement('div');
          pHead.className = 'lh-plan-block-head';
          const pEye = document.createElement('span');
          pEye.className = 'lh-eyebrow';
          pEye.textContent = `LESSON PLAN · ${view.plan.nodes.length} STEPS`;
          const pHint = document.createElement('span');
          pHint.className = 'lh-eyebrow';
          pHint.textContent = 'Click to expand ⤢';
          pHead.append(pEye, pHint);
          const svgWrap = document.createElement('div');
          svgWrap.className = 'lh-plan-svg-wrap';
          svgWrap.innerHTML = renderPlanSvgMarkup(view.plan, view.progress, false);
          pBlock.append(pHead, svgWrap);
          pBlock.addEventListener('click', () => openPlanModal());
          li.appendChild(pBlock);
        } else {
          const div = document.createElement('div');
          div.className = 'lh-phase-divider learn-notice';
          div.textContent = entry.text;
          li.appendChild(div);
        }
      } else if (entry.type === 'learner') {
        const bubble = document.createElement('div');
        bubble.className = 'lh-learner-bubble';
        const p = document.createElement('p');
        p.textContent = entry.text;
        bubble.appendChild(p);
        li.appendChild(bubble);
      } else {
        // Assistant or Explain prose
        const prose = document.createElement('div');
        prose.className = 'lh-prose-entry';
        if (entry.type === 'explain' && entry.node_id) {
          const who = document.createElement('span');
          who.className = 'lh-eyebrow learn-who';
          const stepTitle = (view.plan && view.plan.nodes.find(n => n.id === entry.node_id)?.title) || entry.node_id;
          who.textContent = `STEP · ${stepTitle}`;
          prose.appendChild(who);
        }
        const body = document.createElement('p');
        body.textContent = entry.text;
        prose.appendChild(body);
        li.appendChild(prose);
      }
      list.appendChild(li);
    }

    if (view && view.running) {
      const workingLi = document.createElement('li');
      workingLi.className = 'lh-phase-divider';
      const braille = BRAILLE_FRAMES[state.spinnerFrame % BRAILLE_FRAMES.length];
      workingLi.textContent = `${braille} Thinking…`;
      list.appendChild(workingLi);
    }

    const scroller = $('learnTranscriptScroll') || list;
    scroller.scrollTop = scroller.scrollHeight;
  }

  // ---------------------------------------------------------------------------
  // Persistent Bottom Dock: QuestionDock & PracticeDock
  // ---------------------------------------------------------------------------

  function findActiveQuestion(view) {
    if (!view || !view.active_question) return null;
    for (let i = (view.transcript || []).length - 1; i >= 0; i--) {
      const e = view.transcript[i];
      if (e.type === 'question' && e.id === view.active_question) return e;
    }
    return null;
  }

  function findActiveProblem(view) {
    if (!view || !view.practice || !view.practice.active_problem) return null;
    return (view.practice.problems || []).find(p => p.id === view.practice.active_problem) || null;
  }

  function renderDock(view) {
    const form = $('learnDock');
    const msgForm = $('learnMessageForm');
    if (!form) return;

    const q = findActiveQuestion(view);
    const prob = findActiveProblem(view);

    if (!q && !prob) {
      dictation.cancel();
      state.openQuestionId = null;
      state.openProblemId = null;
      form.hidden = true;
      form.replaceChildren();
      if (msgForm) msgForm.hidden = false;
      return;
    }

    if (msgForm) msgForm.hidden = true;
    form.hidden = false;

    if (q) {
      if (state.openQuestionId === q.id && form.dataset.dockMode === 'question' && form.dataset.collapsed === String(state.dockCollapsed)) {
        return;
      }
      state.openQuestionId = q.id;
      state.openProblemId = null;
      form.dataset.dockMode = 'question';
      form.dataset.collapsed = String(state.dockCollapsed);
      renderQuestionDockContents(form, q);
      return;
    }

    if (prob) {
      const hintCount = (prob.hints || []).length;
      const cacheKey = `${prob.id}:${hintCount}`;
      if (state.openProblemId === cacheKey && form.dataset.dockMode === 'practice') {
        return;
      }
      state.openProblemId = cacheKey;
      state.openQuestionId = null;
      form.dataset.dockMode = 'practice';
      renderPracticeDockContents(form, prob);
    }
  }

  function renderQuestionDockContents(form, q) {
    dictation.cancel();
    form.replaceChildren();
    if (state.dockCollapsed) {
      const bar = document.createElement('div');
      bar.className = 'lh-dock-collapsed-bar';
      const label = document.createElement('span');
      label.className = 'lh-eyebrow';
      label.textContent = `${(q.tool || 'CHECK').toUpperCase()} · ${q.prompt.slice(0, 64)} — Open ›`;
      bar.appendChild(label);
      bar.addEventListener('click', () => {
        state.dockCollapsed = false;
        form.dataset.collapsed = 'false';
        renderQuestionDockContents(form, q);
      });
      form.appendChild(bar);
      return;
    }

    const topRow = document.createElement('div');
    topRow.className = 'lh-dock-top-row';
    const eyebrow = document.createElement('span');
    eyebrow.className = 'lh-eyebrow';
    const baseTool = (q.question_type === 'predict' ? 'PREDICTION' : (q.tool || (q.node_id ? 'CHECK' : 'PROBE'))).toUpperCase();
    eyebrow.textContent = q.kind === 'multiple' ? `${baseTool} · SELECT ALL THAT APPLY` : baseTool;

    const topActions = document.createElement('div');
    topActions.className = 'lh-dock-top-actions';
    const collapseBtn = document.createElement('button');
    collapseBtn.type = 'button';
    collapseBtn.className = 'lh-btn-ghost';
    collapseBtn.textContent = 'Minimize';
    collapseBtn.addEventListener('click', () => {
      state.dockCollapsed = true;
      form.dataset.collapsed = 'true';
      renderQuestionDockContents(form, q);
    });
    const dismissBtn = document.createElement('button');
    dismissBtn.type = 'button';
    dismissBtn.className = 'lh-btn-ghost';
    dismissBtn.textContent = 'Dismiss';
    dismissBtn.addEventListener('click', async () => {
      try {
        const updated = await api(`/sessions/${encodeURIComponent(state.sessionId)}/dismiss`, 'POST');
        applyView(updated);
      } catch (err) {
        setStatus(err.message, 'failed', false);
      }
    });
    topActions.append(collapseBtn, dismissBtn);
    topRow.append(eyebrow, topActions);
    form.appendChild(topRow);

    const fieldset = document.createElement('fieldset');
    const legend = document.createElement('legend');
    legend.className = 'lh-dock-prompt';
    legend.textContent = q.prompt;
    fieldset.appendChild(legend);

    if (q.details) {
      const det = document.createElement('p');
      det.className = 'learn-hint';
      det.textContent = q.details;
      fieldset.appendChild(det);
    }

    if (q.kind === 'free_recall') {
      const box = document.createElement('div');
      box.className = 'lh-recessed-box';
      const boxEye = document.createElement('span');
      boxEye.className = 'lh-eyebrow';
      boxEye.textContent = 'YOUR ANSWER';
      const area = document.createElement('textarea');
      area.name = 'text';
      area.rows = 3;
      area.maxLength = 4000;
      area.className = 'lh-recessed-textarea';
      area.placeholder = 'Explain in your own words…';
      box.append(boxEye, area);
      addDictation(box, area, 'your answer');
      fieldset.appendChild(box);
    } else {
      const optsWrap = document.createElement('div');
      optsWrap.className = 'lh-dock-options';
      const type = q.kind === 'multiple' ? 'checkbox' : 'radio';
      for (const opt of q.options || []) {
        const label = document.createElement('label');
        label.className = 'learn-option';
        const input = document.createElement('input');
        input.type = type;
        input.name = 'choice';
        input.value = opt.id;
        const text = document.createElement('span');
        text.textContent = opt.text;
        label.append(input, text);
        optsWrap.appendChild(label);
      }
      // Built-in "I don't know" dashed option row when not already in options
      const hasIdk = (q.options || []).some(o => /don't know/i.test(o.text));
      if (!hasIdk && q.tool !== 'ask_user_question') {
        const idkLabel = document.createElement('label');
        idkLabel.className = 'learn-option lh-dont-know-option';
        const idkInput = document.createElement('input');
        idkInput.type = type;
        idkInput.name = 'dont_know';
        idkInput.value = '1';
        const idkText = document.createElement('span');
        idkText.textContent = "I don't know";
        idkLabel.append(idkInput, idkText);
        optsWrap.appendChild(idkLabel);
      }
      fieldset.appendChild(optsWrap);
    }
    form.appendChild(fieldset);

    // Snapped 0-100% Confidence Slider
    const confRow = document.createElement('div');
    confRow.className = 'lh-confidence-row';
    const confLabel = document.createElement('span');
    confLabel.className = 'lh-eyebrow';
    confLabel.textContent = 'CONFIDENCE';
    const slider = document.createElement('input');
    slider.type = 'range';
    slider.name = 'confidence_pct';
    slider.min = '0';
    slider.max = '100';
    slider.value = '70';
    slider.className = 'lh-confidence-slider';
    const readout = document.createElement('span');
    readout.className = 'lh-confidence-readout';
    readout.textContent = '70%';
    readout.style.color = '#34d399';
    slider.addEventListener('input', () => {
      const snapped = learnSnapConfidence(slider.value);
      slider.value = String(snapped);
      readout.textContent = `${snapped}%`;
      readout.style.color = snapped >= 70 ? '#34d399' : snapped >= 40 ? '#fbbf24' : '#f87171';
    });
    confRow.append(confLabel, slider, readout);
    form.appendChild(confRow);

    // Recessed YOUR THINKING sub-card
    const thinkBox = document.createElement('div');
    thinkBox.className = 'lh-recessed-box';
    const thinkEye = document.createElement('span');
    thinkEye.className = 'lh-eyebrow';
    thinkEye.textContent = 'YOUR THINKING (OPTIONAL)';
    const thinkArea = document.createElement('textarea');
    thinkArea.name = 'thinking';
    thinkArea.rows = 2;
    thinkArea.maxLength = 4000;
    thinkArea.className = 'lh-recessed-textarea';
    thinkArea.placeholder = 'Walk through your reasoning out loud…';

    const foot = document.createElement('div');
    foot.className = 'lh-recessed-foot';
    const hint = document.createElement('span');
    hint.className = 'lh-recessed-hint';
    hint.textContent = '↵ submits · ⇧↵ newline';
    const submit = document.createElement('button');
    submit.type = 'submit';
    submit.className = 'lh-btn-primary learn-primary';
    submit.textContent = 'Submit answer';
    foot.append(hint, submit);
    thinkBox.append(thinkEye, thinkArea);
    addDictation(thinkBox, thinkArea, 'your thinking');
    thinkBox.appendChild(foot);
    form.appendChild(thinkBox);

    const err = document.createElement('p');
    err.id = 'learnDockError';
    err.className = 'learn-dock-error';
    err.setAttribute('role', 'alert');
    form.appendChild(err);
  }

  function renderPracticeDockContents(form, prob) {
    dictation.cancel();
    form.replaceChildren();
    const topRow = document.createElement('div');
    topRow.className = 'lh-dock-top-row';
    const eyebrow = document.createElement('span');
    eyebrow.className = 'lh-eyebrow';
    eyebrow.textContent = prob.mode ? `PRACTICE PROBLEM · ${prob.mode.toUpperCase()}` : 'PRACTICE PROBLEM';

    const topActions = document.createElement('div');
    topActions.className = 'lh-dock-top-actions';
    const revealed = (prob.hints || []).length;
    const totalHints = prob.hint_count || 0;
    if (totalHints > revealed) {
      const hintBtn = document.createElement('button');
      hintBtn.type = 'button';
      hintBtn.className = 'lh-btn-secondary';
      hintBtn.textContent = `💡 Hint (${revealed}/${totalHints})`;
      hintBtn.addEventListener('click', async () => {
        try {
          const updated = await api(`/sessions/${encodeURIComponent(state.sessionId)}/hint`, 'POST', {
            problem_id: prob.id,
            index: revealed,
          });
          applyView(updated);
        } catch (e) {
          setStatus(e.message, 'failed', false);
        }
      });
      topActions.appendChild(hintBtn);
    }
    const dismissBtn = document.createElement('button');
    dismissBtn.type = 'button';
    dismissBtn.className = 'lh-btn-ghost';
    dismissBtn.textContent = 'Dismiss';
    dismissBtn.addEventListener('click', async () => {
      try {
        const updated = await api(`/sessions/${encodeURIComponent(state.sessionId)}/dismiss`, 'POST');
        applyView(updated);
      } catch (e) {
        setStatus(e.message, 'failed', false);
      }
    });
    topActions.appendChild(dismissBtn);
    topRow.append(eyebrow, topActions);
    form.appendChild(topRow);

    const stmt = document.createElement('p');
    stmt.className = 'lh-dock-prompt';
    stmt.textContent = prob.statement;
    form.appendChild(stmt);

    for (let i = 0; i < (prob.hints || []).length; i++) {
      const h = document.createElement('div');
      h.className = 'lh-thinking-quote';
      h.textContent = `Hint ${i + 1}: ${prob.hints[i]}`;
      form.appendChild(h);
    }

    const box = document.createElement('div');
    box.className = 'lh-recessed-box';
    const boxEye = document.createElement('span');
    boxEye.className = 'lh-eyebrow';
    boxEye.textContent = 'YOUR THINKING';
    const area = document.createElement('textarea');
    area.name = 'practice_attempt';
    area.rows = 3;
    area.maxLength = 4000;
    area.className = 'lh-recessed-textarea';
    area.placeholder = 'Work here, think out loud…';
    const foot = document.createElement('div');
    foot.className = 'lh-recessed-foot';
    const hint = document.createElement('span');
    hint.className = 'lh-recessed-hint';
    hint.textContent = '↵ submits · ⇧↵ newline';
    const submit = document.createElement('button');
    submit.type = 'submit';
    submit.className = 'lh-btn-primary learn-primary';
    submit.textContent = 'Submit thinking';
    foot.append(hint, submit);
    box.append(boxEye, area, foot);
    form.appendChild(box);

    const err = document.createElement('p');
    err.id = 'learnDockError';
    err.className = 'learn-dock-error';
    err.setAttribute('role', 'alert');
    form.appendChild(err);
  }

  function renderSessionStatsBar(view) {
    const ctxText = $('learnContextText');
    const meterFill = $('learnContextMeterFill');
    const summaryGrid = $('learnStatsSummaryGrid');
    const turnsList = $('learnStatsTurnsList');
    const roleBadge = $('learnRoleModelBadge');

    if (!view) return;
    const totalTokens = (view.input_tokens || 0) + (view.output_tokens || 0);
    const turns = Array.isArray(view.turns) ? view.turns : [];
    const meter = learnContextMeter(view);
    const pct = meter.percent;

    if (ctxText) {
      // Session totals are cumulative; the meter is what the next prompt replays.
      ctxText.textContent = `${meter.label} · ${totalTokens.toLocaleString()} tokens · ${turns.length} turn${turns.length === 1 ? '' : 's'}`;
      if (ctxText.parentElement) ctxText.parentElement.classList.toggle('warn', !!meter.warn);
    }
    if (meterFill) {
      meterFill.style.width = `${pct}%`;
      const bar = meterFill.parentElement;
      if (bar) {
        bar.setAttribute('aria-valuenow', String(pct));
        bar.setAttribute('aria-valuetext', meter.warn ? `${meter.label}. ${meter.warn}` : meter.label);
      }
    }
    if (roleBadge) {
      const roleLabel = view.mode === 'assess' ? 'Prober' : view.mode === 'practice' ? 'Practitioner' : 'Teacher';
      roleBadge.textContent = `${roleLabel} · SAIL Mind`;
    }
    if (summaryGrid) {
      summaryGrid.innerHTML = `
        <div>Prompt: <strong>${(view.input_tokens || 0).toLocaleString()}</strong></div>
        <div>Completion: <strong>${(view.output_tokens || 0).toLocaleString()}</strong></div>
        <div>Context: <strong>${pct}%</strong></div>
        <div>Events: <strong>${view.last_seq || 0}</strong></div>
      `;
    }
    if (turnsList) {
      turnsList.replaceChildren();
      for (const t of turns) {
        const row = document.createElement('div');
        const prompt = (t.input_tokens || 0).toLocaleString();
        const completion = (t.output_tokens || 0).toLocaleString();
        row.textContent = `Turn ${t.turn} (${t.role}): ${prompt} prompt + ${completion} completion`;
        turnsList.appendChild(row);
      }
    }
  }

  function reviewDueText(ms) {
    const date = new Date(Number(ms));
    return Number.isNaN(date.getTime())
      ? 'a later day'
      : date.toLocaleString(undefined, {dateStyle: 'medium', timeStyle: 'short'});
  }

  // Opt-in spaced practice (#257). The offer appears only after a practice attempt; a reminder
  // that already exists replaces it with its real due time and a cancel action.
  function renderReview(view) {
    const strip = $('learnReviewStrip');
    if (!strip) return;
    const practice = view && view.kind === 'practice' ? view.practice : null;
    const problems = practice && Array.isArray(practice.problems) ? practice.problems : [];
    const attempted = problems.some(p => p && p.outcome);
    const review = view && view.review;
    const activeProblem = Boolean(practice && practice.active_problem);
    strip.hidden = !(review || (attempted && !activeProblem));

    const offer = $('learnReviewOffer');
    const active = $('learnReviewActive');
    if (offer) offer.hidden = Boolean(review);
    if (active) active.hidden = !review;

    const text = $('learnReviewText');
    if (text) {
      text.textContent = review
        ? `Practice again on ${reviewDueText(review.due_at_ms)}.`
        : 'Practise this topic again later? You choose.';
    }
    const activeText = $('learnReviewActiveText');
    if (activeText) {
      activeText.textContent = review
        ? `Reminder set for ${reviewDueText(review.due_at_ms)}.`
        : '';
    }
  }

  function applyView(view) {
    state.view = view;
    $('learnStart').hidden = false;
    $('learnStart').style.display = 'none';
    $('learnSession').hidden = false;
    $('learnNew').hidden = false;

    const displayTitle = view.title || view.goal || 'Session';
    $('learnGoalTitle').textContent = displayTitle;

    const modeBadge = $('learnMode');
    if (modeBadge) {
      modeBadge.dataset.mode = view.mode;
      modeBadge.textContent = view.mode === 'assess'
        ? 'Probe'
        : view.mode === 'practice'
        ? '🏋 Practice'
        : 'Teach';
    }

    $('learnStop').hidden = !view.running;
    const pause = $('learnPause');
    pause.textContent = view.paused ? 'Resume' : 'Pause';
    pause.setAttribute('aria-pressed', view.paused ? 'true' : 'false');

    const status = learnStatus(view);
    setStatus(status.text, status.kind, status.retry);
    renderSteps(view);
    renderTranscript(view);
    renderDock(view);
    renderSessionStatsBar(view);
    renderReview(view);

    const msgInput = $('learnMessageInput');
    const msgSend = $('learnMessageSend');
    const blocked = Boolean(view.running || view.active_question || (view.practice && view.practice.active_problem));
    if (msgInput) msgInput.disabled = blocked;
    if (msgSend) msgSend.disabled = blocked;

    if (learnShouldAutoTurn(view, state.lastAutoTurnSeq)) {
      state.lastAutoTurnSeq = view.last_seq;
      api(`/sessions/${encodeURIComponent(view.session)}/turn`, 'POST').catch(() => {});
    }
  }

  function showHomeView() {
    dictation.cancel();
    if (state.streamAbort) state.streamAbort.abort();
    state.sessionId = null;
    state.view = null;
    state.openQuestionId = null;
    state.openProblemId = null;
    $('learnSession').hidden = true;
    $('learnStart').hidden = false;
    $('learnStart').style.display = '';
    renderSidebarSessions();
    refreshGraph();
  }

  async function connectStream(id) {
    if (state.streamAbort) state.streamAbort.abort();
    const ctrl = new AbortController();
    state.streamAbort = ctrl;
    const headers = {};
    const token = ownerToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    try {
      const r = await fetch(`/api/learn/sessions/${encodeURIComponent(id)}/stream`, {headers, signal: ctrl.signal});
      if (!r.ok || !r.body) return;
      const reader = r.body.getReader();
      const decoder = new TextDecoder();
      let buf = '';
      while (true) {
        const {value, done} = await reader.read();
        if (done) break;
        const parsed = learnParseSse(buf + decoder.decode(value, {stream: true}));
        buf = parsed.rest;
        for (const ev of parsed.events) {
          if (ev.event === 'view') {
            applyView(JSON.parse(ev.data));
            loadSessions();
          }
        }
      }
    } catch (_) {
      if (!ctrl.signal.aborted && state.sessionId === id) {
        setTimeout(() => { if (state.sessionId === id) openSession(id); }, 1500);
      }
    }
  }

  async function openSession(id) {
    dictation.cancel();
    state.openQuestionId = null;
    state.sessionId = id;
    state.lastAutoTurnSeq = null;
    try {
      applyView(await api(`/sessions/${encodeURIComponent(id)}`));
      renderSidebarSessions();
      connectStream(id);
    } catch (e) {
      setStatus(e.message, 'failed', false);
    }
  }

  async function loadSessions() {
    try {
      const data = await api('/sessions');
      state.sessions = (data && data.sessions) || [];
      renderSidebarSessions();
    } catch (_) {}
  }

  function openPlanModal() {
    const modal = $('learnPlanModal');
    const stage = $('learnPlanModalStage');
    if (!modal || !stage || !state.view || !state.view.plan) return;
    stage.innerHTML = renderPlanSvgMarkup(state.view.plan, state.view.progress, false);
    modal.hidden = false;
  }

  // ---------------------------------------------------------------------------
  // Event Listeners & Interactive Canvas Wiring
  // ---------------------------------------------------------------------------

  function initGraphCanvasEvents() {
    const canvas = $('learnGraphCanvas');
    if (!canvas) return;

    function screenToWorld(clientX, clientY) {
      const rect = canvas.getBoundingClientRect();
      const sx = clientX - rect.left;
      const sy = clientY - rect.top;
      const cx = rect.width / 2 + state.graphTransform.x;
      const cy = rect.height / 2 - 30 + state.graphTransform.y;
      return {
        x: (sx - cx) / state.graphTransform.k,
        y: (sy - cy) / state.graphTransform.k,
      };
    }

    function hitTestNode(wx, wy) {
      const props = state.graph.propositions || [];
      const pos = state.graph.positions || {};
      const r = 12 * ((state.graph.render_settings && state.graph.render_settings.node_size) || 1);
      for (let i = props.length - 1; i >= 0; i--) {
        const p = props[i];
        const pt = pos[p.name];
        if (pt && Math.hypot(wx - pt.x, wy - pt.y) <= r) return p;
      }
      return null;
    }

    canvas.addEventListener('mousedown', e => {
      const w = screenToWorld(e.clientX, e.clientY);
      const hit = hitTestNode(w.x, w.y);
      if (hit) {
        state.graphDraggingNode = {prop: hit, moved: false};
      } else {
        state.graphPanning = {
          startX: e.clientX - state.graphTransform.x,
          startY: e.clientY - state.graphTransform.y,
        };
      }
    });

    window.addEventListener('mousemove', e => {
      if (state.graphDraggingNode) {
        const w = screenToWorld(e.clientX, e.clientY);
        const name = state.graphDraggingNode.prop.name;
        state.graph.positions[name] = {x: w.x, y: w.y};
        state.graphDraggingNode.moved = true;
        renderGraphCanvas();
        return;
      }
      if (state.graphPanning) {
        state.graphTransform.x = e.clientX - state.graphPanning.startX;
        state.graphTransform.y = e.clientY - state.graphPanning.startY;
        renderGraphCanvas();
        return;
      }
      if ($('learnStart') && !$('learnStart').hidden && $('learnStart').style.display !== 'none') {
        const w = screenToWorld(e.clientX, e.clientY);
        const hit = hitTestNode(w.x, w.y);
        if (hit !== state.hoveredGraphNode) {
          state.hoveredGraphNode = hit;
          canvas.style.cursor = hit ? 'pointer' : 'grab';
          renderGraphCanvas();
        }
      }
    });

    window.addEventListener('mouseup', () => {
      if (state.graphDraggingNode) {
        const {prop, moved} = state.graphDraggingNode;
        state.graphDraggingNode = null;
        if (moved) {
          api('/graph/positions', 'PUT', {positions: state.graph.positions}).catch(() => {});
        } else {
          showNodeDetailCard(prop);
        }
      }
      state.graphPanning = null;
    });

    canvas.addEventListener('wheel', e => {
      e.preventDefault();
      const factor = e.deltaY < 0 ? 1.1 : 0.9;
      state.graphTransform.k = Math.min(3.0, Math.max(0.35, state.graphTransform.k * factor));
      renderGraphCanvas();
    }, {passive: false});
  }

  async function startSessionFromInput() {
    const input = $('learnGoal');
    if (!input) return;
    const goal = input.value.trim();
    if (!goal) return;
    try {
      const view = await api('/sessions', 'POST', {
        goal,
        kind: state.homeMode,
        graph_context: state.pickedProps,
      });
      input.value = '';
      state.pickedProps = [];
      updatePickedNodesUi();
      state.sessionId = view.session;
      applyView(view);
      loadSessions();
      connectStream(view.session);
    } catch (err) {
      setStatus(err.message, 'failed', false);
    }
  }

  // Open the practice topic carried by a spaced-practice reminder (#257).
  async function openPracticeLink() {
    const topic = learnPracticeLink(location.hash);
    if (!topic) return;
    history.replaceState(null, '', '#arena');
    setHomeMode('practice');
    showHomeView();
    const input = $('learnGoal');
    if (input) input.value = topic;
    await startSessionFromInput();
  }

  function init() {
    if (!$('learnStartForm')) return;

    // Braille spinner animation loop for active turns
    state.spinnerTimer = setInterval(() => {
      state.spinnerFrame = (state.spinnerFrame + 1) % BRAILLE_FRAMES.length;
      if (state.sessions.some(s => s.running) || (state.view && state.view.running)) {
        renderSidebarSessions();
      }
    }, 120);

    // Sidebar collapse/expand
    $('learnSidebarCloseBtn')?.addEventListener('click', () => {
      state.sidebarCollapsed = true;
      updateSidebarCollapseUi();
      setTimeout(renderGraphCanvas, 200);
    });
    $('learnHomeSidebarOpenBtn')?.addEventListener('click', () => {
      state.sidebarCollapsed = false;
      updateSidebarCollapseUi();
      setTimeout(renderGraphCanvas, 200);
    });
    $('learnSessionSidebarOpenBtn')?.addEventListener('click', () => {
      state.sidebarCollapsed = false;
      updateSidebarCollapseUi();
    });

    // Home / New Teaching / New Practice buttons
    $('learnNew')?.addEventListener('click', () => showHomeView());
    $('learnNewTeachBtn')?.addEventListener('click', () => {
      setHomeMode('teach');
      showHomeView();
      $('learnGoal')?.focus();
    });
    $('learnNewPracticeBtn')?.addEventListener('click', () => {
      setHomeMode('practice');
      showHomeView();
      $('learnGoal')?.focus();
    });

    // Home mode switchers
    $('learnModeSwitchTeach')?.addEventListener('click', () => setHomeMode('teach'));
    $('learnModeSwitchPractice')?.addEventListener('click', () => setHomeMode('practice'));

    // Sidebar session filter & search
    document.querySelectorAll('[data-learn-filter]').forEach(btn => {
      btn.addEventListener('click', () => {
        state.sessionFilter = btn.dataset.learnFilter || 'all';
        document.querySelectorAll('[data-learn-filter]').forEach(b => {
          const active = b === btn;
          b.classList.toggle('active', active);
          b.setAttribute('aria-selected', active ? 'true' : 'false');
        });
        renderSidebarSessions();
      });
    });
    $('learnSessionSearch')?.addEventListener('input', e => {
      state.sessionSearch = e.target.value || '';
      renderSidebarSessions();
    });

    // Start new session form (Teaching or Practice, with optional picked graph propositions)
    $('learnStartForm').addEventListener('submit', e => {
      e.preventDefault();
      startSessionFromInput();
    });

    // Opt-in spaced-practice reminder (#257). Off unless the learner asks for one.
    $('learnReviewScheduleBtn')?.addEventListener('click', async () => {
      if (!state.sessionId) return;
      const days = Number($('learnReviewDays')?.value || 3);
      try {
        const view = await api(`/sessions/${encodeURIComponent(state.sessionId)}/review`, 'POST', {
          days,
          timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
          utc_offset_minutes: -new Date().getTimezoneOffset(),
        });
        applyView(view);
      } catch (err) {
        setStatus(err.message, 'failed', false);
      }
    });
    $('learnReviewCancelBtn')?.addEventListener('click', async () => {
      if (!state.sessionId) return;
      try {
        const result = await api(`/sessions/${encodeURIComponent(state.sessionId)}/review`, 'DELETE');
        if (result && result.cancelled) setStatus('Practice reminder cancelled.', 'ready', false);
        applyView(await api(`/sessions/${encodeURIComponent(state.sessionId)}`));
      } catch (err) {
        setStatus(err.message, 'failed', false);
      }
    });

    // QuestionDock & PracticeDock form submission
    $('learnDock').addEventListener('submit', async e => {
      e.preventDefault();
      const view = state.view;
      if (!view) return;
      const errEl = $('learnDockError');
      if (dictation.busy) {
        if (errEl) errEl.textContent = 'Finish dictation and review the transcript before submitting.';
        return;
      }

      // Case 1: Active Practice Problem
      const prob = findActiveProblem(view);
      if (prob && !view.active_question) {
        const fd = new FormData(e.currentTarget);
        const text = String(fd.get('practice_attempt') || '').trim();
        if (!text) {
          if (errEl) errEl.textContent = 'Write your thinking first.';
          return;
        }
        if (errEl) errEl.textContent = '';
        try {
          const updated = await api(`/sessions/${encodeURIComponent(view.session)}/attempt`, 'POST', {
            problem_id: prob.id,
            text,
          });
          state.openProblemId = null;
          applyView(updated);
        } catch (err) {
          if (errEl) errEl.textContent = err.message;
        }
        return;
      }

      // Case 2: Active Question
      const q = findActiveQuestion(view);
      if (!q) return;
      const fd = new FormData(e.currentTarget);
      const built = learnBuildAnswer(q.kind, {
        choices: fd.getAll('choice'),
        text: fd.get('text'),
        dont_know: fd.get('dont_know') === '1',
        confidence_pct: fd.get('confidence_pct'),
        thinking: fd.get('thinking'),
      });
      if (built.error) {
        if (errEl) errEl.textContent = built.error;
        return;
      }
      if (errEl) errEl.textContent = '';
      try {
        await api(`/sessions/${encodeURIComponent(view.session)}/answer`, 'POST', {
          question_id: q.id,
          answer: built.answer,
        });
        state.openQuestionId = null;
        applyView(await api(`/sessions/${encodeURIComponent(view.session)}`));
      } catch (err) {
        if (errEl) errEl.textContent = err.message;
      }
    });

    // Enter-to-submit inside QuestionDock & PracticeDock textareas
    $('learnDock').addEventListener('keydown', e => {
      if (e.key === 'Enter' && !e.shiftKey && e.target && e.target.tagName === 'TEXTAREA') {
        e.preventDefault();
        $('learnDock').requestSubmit();
      }
    });

    // Open Chat Message Form
    $('learnMessageForm').addEventListener('submit', async e => {
      e.preventDefault();
      if (!state.sessionId) return;
      const input = $('learnMessageInput');
      const text = input.value.trim();
      if (!text) return;
      const key = (typeof crypto !== 'undefined' && crypto.randomUUID) ? crypto.randomUUID() : String(Date.now());
      try {
        input.value = '';
        await api(`/sessions/${encodeURIComponent(state.sessionId)}/message`, 'POST', {key, text});
        applyView(await api(`/sessions/${encodeURIComponent(state.sessionId)}`));
      } catch (err) {
        setStatus(err.message, 'failed', false);
      }
    });

    // Session TopBar Actions: Inline Rename, Export Markdown, Plan Popup, Stop, Pause, Retry, Delete
    $('learnGoalTitle')?.addEventListener('click', () => {
      if (!state.view) return;
      const h3 = $('learnGoalTitle');
      const inp = $('learnRenameInput');
      if (!h3 || !inp) return;
      inp.value = state.view.title || state.view.goal || '';
      h3.hidden = true;
      inp.hidden = false;
      inp.focus();
      inp.select();
    });
    const commitRename = async () => {
      const h3 = $('learnGoalTitle');
      const inp = $('learnRenameInput');
      if (!h3 || !inp || inp.hidden) return;
      const newTitle = inp.value.trim();
      inp.hidden = true;
      h3.hidden = false;
      if (newTitle && state.sessionId) {
        try {
          const updated = await api(`/sessions/${encodeURIComponent(state.sessionId)}`, 'PUT', {title: newTitle});
          applyView(updated);
          loadSessions();
        } catch (_) {}
      }
    };
    $('learnRenameInput')?.addEventListener('blur', commitRename);
    $('learnRenameInput')?.addEventListener('keydown', e => {
      if (e.key === 'Enter') { e.preventDefault(); commitRename(); }
      if (e.key === 'Escape') {
        $('learnRenameInput').hidden = true;
        $('learnGoalTitle').hidden = false;
      }
    });

    $('learnExportMdBtn')?.addEventListener('click', async () => {
      if (!state.sessionId) return;
      try {
        const res = await api(`/sessions/${encodeURIComponent(state.sessionId)}/markdown`);
        const blob = new Blob([res.markdown || ''], {type: 'text/markdown;charset=utf-8'});
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `${(res.title || 'session').toLowerCase().replace(/[^a-z0-9]+/g, '-')}.md`;
        document.body.appendChild(a);
        a.click();
        a.remove();
        URL.revokeObjectURL(url);
      } catch (e) {
        setStatus(e.message, 'failed', false);
      }
    });

    $('learnPlanPopupBtn')?.addEventListener('click', () => openPlanModal());
    $('learnPlanMarginalia')?.addEventListener('click', () => openPlanModal());
    $('learnPlanModalClose')?.addEventListener('click', () => { $('learnPlanModal').hidden = true; });

    $('learnStop')?.addEventListener('click', async () => {
      if (state.sessionId) await api(`/sessions/${encodeURIComponent(state.sessionId)}/abort`, 'POST').catch(() => {});
    });
    $('learnPause')?.addEventListener('click', async () => {
      if (!state.view) return;
      try {
        applyView(await api(`/sessions/${encodeURIComponent(state.view.session)}/pause`, 'POST', {paused: !state.view.paused}));
      } catch (err) { setStatus(err.message, 'failed', false); }
    });
    $('learnRetry')?.addEventListener('click', async () => {
      if (state.sessionId) {
        await api(`/sessions/${encodeURIComponent(state.sessionId)}/turn`, 'POST').catch(e => setStatus(e.message, 'failed', true));
      }
    });
    $('learnDeleteSessionBtn')?.addEventListener('click', async () => {
      if (!state.sessionId) return;
      try {
        await api(`/sessions/${encodeURIComponent(state.sessionId)}`, 'DELETE');
        await loadSessions();
        showHomeView();
      } catch (e) {
        setStatus(e.message, 'failed', false);
      }
    });

    // Session Stats Popover
    $('learnStatsBtn')?.addEventListener('click', () => {
      const pop = $('learnStatsPopover');
      if (pop) pop.hidden = !pop.hidden;
    });

    // Knowledge Graph Chrome & Settings
    $('learnGraphSearch')?.addEventListener('input', e => {
      state.graphSearch = e.target.value || '';
      renderGraphCanvas();
    });
    $('learnGraphSettingsBtn')?.addEventListener('click', () => {
      const pop = $('learnGraphSettingsPopover');
      if (pop) pop.hidden = !pop.hidden;
    });
    const syncGraphSettings = () => {
      state.graph.render_settings = {
        node_size: Number($('learnSetNodeSize')?.value || 1),
        edge_width: Number($('learnSetEdgeWidth')?.value || 1),
        arrow_size: Number($('learnSetArrowSize')?.value || 1),
        label_size: Number($('learnSetLabelSize')?.value || 1),
        label_zoom: 1,
        show_cluster_labels: Boolean($('learnSetClusterLabels')?.checked),
      };
      renderGraphCanvas();
      api('/graph/settings', 'PUT', state.graph.render_settings).catch(() => {});
    };
    ['learnSetNodeSize', 'learnSetEdgeWidth', 'learnSetArrowSize', 'learnSetLabelSize', 'learnSetClusterLabels'].forEach(id => {
      $(id)?.addEventListener('input', syncGraphSettings);
      $(id)?.addEventListener('change', syncGraphSettings);
    });

    // Node Detail Card actions
    $('learnNodeCardClose')?.addEventListener('click', () => showNodeDetailCard(null));
    $('learnNodePracticeBtn')?.addEventListener('click', () => {
      const p = state.selectedGraphNode;
      if (!p) return;
      state.pickedProps = [{name: p.name, statement: p.statement}];
      updatePickedNodesUi();
      setHomeMode('practice');
      const input = $('learnGoal');
      if (input && !input.value.trim()) input.value = `Practice ${p.name}`;
      showNodeDetailCard(null);
      input?.focus();
    });
    $('learnNodeDeleteBtn')?.addEventListener('click', async () => {
      const p = state.selectedGraphNode;
      if (!p) return;
      try {
        const snap = await api('/graph/delete-nodes', 'POST', {names: [p.name]});
        state.graph = snap;
        showNodeDetailCard(null);
      } catch (_) {}
    });

    // Chain Table Overlay controls
    const openChains = () => {
      const ov = $('learnChainTableOverlay');
      if (ov) {
        ov.hidden = false;
        renderChainTable();
      }
    };
    $('learnChainsBtn')?.addEventListener('click', openChains);
    $('learnSidebarChainsBtn')?.addEventListener('click', () => {
      showHomeView();
      openChains();
    });
    $('learnPickNodesBtn')?.addEventListener('click', openChains);
    $('learnClearPickedNodesBtn')?.addEventListener('click', () => {
      state.pickedProps = [];
      updatePickedNodesUi();
    });
    $('learnChainCloseBtn')?.addEventListener('click', () => {
      $('learnChainTableOverlay').hidden = true;
    });
    $('learnChainSearchInput')?.addEventListener('input', e => {
      state.chainSearch = e.target.value || '';
      renderChainTable();
    });
    document.querySelectorAll('[data-chain-sort]').forEach(btn => {
      btn.addEventListener('click', () => {
        state.chainSort = btn.dataset.chainSort || 'created';
        renderChainTable();
      });
    });
    $('learnChainClearSelBtn')?.addEventListener('click', () => {
      state.chainSelectedProps.clear();
      renderChainTable();
    });
    $('learnChainPracticeSelBtn')?.addEventListener('click', () => {
      const props = state.graph.propositions || [];
      state.pickedProps = props
        .filter(p => state.chainSelectedProps.has(p.name))
        .map(p => ({name: p.name, statement: p.statement}));
      updatePickedNodesUi();
      setHomeMode('practice');
      $('learnChainTableOverlay').hidden = true;
      const input = $('learnGoal');
      if (input && !input.value.trim() && state.pickedProps.length) {
        input.value = `Practice ${state.pickedProps.slice(0, 3).map(p => p.name).join(', ')}`;
      }
      input?.focus();
    });
    $('learnChainDeleteSelBtn')?.addEventListener('click', async () => {
      const names = Array.from(state.chainSelectedProps);
      if (!names.length) return;
      try {
        const snap = await api('/graph/delete-nodes', 'POST', {names});
        state.graph = snap;
        state.chainSelectedProps.clear();
        renderChainTable();
        renderGraphCanvas();
      } catch (_) {}
    });

    // Import Notes Modal
    const openImportModal = () => { $('learnImportModal').hidden = false; };
    $('learnImportNotesBtn')?.addEventListener('click', openImportModal);
    $('learnSidebarImportBtn')?.addEventListener('click', openImportModal);
    $('learnAttachNotesBtn')?.addEventListener('click', openImportModal);
    $('learnImportModalClose')?.addEventListener('click', () => { $('learnImportModal').hidden = true; });
    $('learnImportPickFileBtn')?.addEventListener('click', () => { $('learnImportFilePicker')?.click(); });
    $('learnImportFilePicker')?.addEventListener('change', async e => {
      const file = e.target.files && e.target.files[0];
      if (!file) return;
      $('learnImportFileName').value = file.name;
      $('learnImportContent').value = await file.text();
    });
    $('learnImportForm')?.addEventListener('submit', async e => {
      e.preventDefault();
      const fileName = $('learnImportFileName').value.trim();
      const content = $('learnImportContent').value.trim();
      if (!fileName || !content) return;
      try {
        const snap = await api('/graph/import', 'POST', {file_name: fileName, content});
        state.graph = snap;
        $('learnImportModal').hidden = true;
        $('learnImportFileName').value = '';
        $('learnImportContent').value = '';
        renderGraphCanvas();
        renderChainTable();
      } catch (err) {
        setStatus(err.message, 'failed', false);
      }
    });

    initGraphCanvasEvents();
    window.addEventListener('resize', () => renderGraphCanvas());
    window.addEventListener('pagehide', () => dictation.cancel());
    document.addEventListener('visibilitychange', () => { if (document.hidden) dictation.cancel(); });
    window.addEventListener('hashchange', () => {
      const arena = location.hash.startsWith('#arena');
      if (!arena) dictation.cancel();
      if (arena) {
        loadSessions();
        refreshGraph();
        refreshActivityDock();
      }
      openPracticeLink();
    });
    $('learnTab')?.addEventListener('click', () => {
      loadSessions();
      refreshGraph();
    });

    if (location.hash.startsWith('#arena')) {
      loadSessions();
      refreshGraph();
      refreshActivityDock();
    } else {
      setTimeout(() => {
        loadSessions();
        refreshGraph();
      }, 300);
    }
    openPracticeLink();
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})(typeof window !== 'undefined' ? window : null);

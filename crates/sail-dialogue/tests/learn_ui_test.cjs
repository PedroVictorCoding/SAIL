// Run with node --test crates/sail-dialogue/tests/learn_ui_test.cjs
const {test} = require('node:test');
const assert = require('node:assert/strict');
const ui = require('../web/learn.js');

test('SSE parsing keeps partial frames for the next chunk', () => {
  const first = ui.learnParseSse('event: view\ndata: {"a":1}\n\n: keep-alive\n\nevent: view\ndata: {"b"');
  assert.deepEqual(first.events, [{event: 'view', data: '{"a":1}'}]);
  const second = ui.learnParseSse(first.rest + ':2}\r\n\r\n');
  assert.deepEqual(second.events, [{event: 'view', data: '{"b":2}'}]);
  assert.equal(second.rest, '');
});

test('map steps follow teaching order and carry text labels', () => {
  const view = {plan: {nodes: [{id: 'c', title: 'C'}, {id: 'a', title: 'A'}, {id: 'b', title: 'B'}], order: ['a', 'b', 'c']},
    progress: {current: 'b', visited: ['a', 'b']}};
  assert.deepEqual(ui.learnStepStates(view).map(s => [s.id, s.state, s.label]),
    [['a', 'visited', 'Covered'], ['b', 'current', 'Now'], ['c', 'upcoming', 'Next']]);
  assert.deepEqual(ui.learnStepStates({plan: null}), []);
});

test('status reflects paused, running, failure and waiting states', () => {
  assert.equal(ui.learnStatus({paused: true, running: true}).kind, 'paused');
  assert.equal(ui.learnStatus({running: true, mode: 'assess'}).kind, 'running');
  const failed = ui.learnStatus({last_failure: 'model offline'});
  assert.equal(failed.kind, 'failed'); assert.equal(failed.retry, true); assert.match(failed.text, /model offline/);
  assert.equal(ui.learnStatus({active_question: 'q'}).kind, 'waiting');
});

test('a missed turn is started once per view revision, never over a question or pause', () => {
  const owed = {needs_turn: true, running: false, paused: false, active_question: null, last_seq: 7};
  assert.equal(ui.learnShouldAutoTurn(owed, null), true);
  assert.equal(ui.learnShouldAutoTurn(owed, 7), false);
  assert.equal(ui.learnShouldAutoTurn({...owed, running: true}, null), false);
  assert.equal(ui.learnShouldAutoTurn({...owed, paused: true}, null), false);
  assert.equal(ui.learnShouldAutoTurn({...owed, active_question: 'q'}, null), false);
});

test('answers are validated before sending; optional signals are kept', () => {
  assert.equal(ui.learnBuildAnswer('single', {choices: []}).error, 'Choose an option first.');
  assert.equal(ui.learnBuildAnswer('single', {choices: ['a', 'b']}).error, 'Choose one option.');
  assert.deepEqual(ui.learnBuildAnswer('multiple', {choices: ['a', 'b'], confidence: '', thinking: ' '}).answer, {choices: ['a', 'b']});
  assert.equal(ui.learnBuildAnswer('free_recall', {text: '  '}).error, 'Write your answer first.');
  assert.deepEqual(ui.learnBuildAnswer('free_recall', {text: ' x ', confidence: '4', thinking: 'why'}).answer,
    {text: 'x', confidence: 4, thinking: 'why'});
});

test('grade text never claims mastery', () => {
  for (const grade of ['correct', 'incorrect', 'needs_judgment']) assert.doesNotMatch(ui.learnGradeText({grade}), /master/i);
});

test('free-recall judgments show the verdict and the missing relation, never a guess', () => {
  const judged = verdict => ({grade: 'needs_judgment', judgment: {verdict, missing: 'a full queue drops packets', feedback: 'You named buffers.'}});
  assert.equal(ui.learnGradeText(judged('partial')), 'Partly there.');
  assert.equal(ui.learnVerdict(judged('partial')), 'partial');
  assert.deepEqual(ui.learnJudgmentLines(judged('incorrect')), ['Missing: a full queue drops packets', 'You named buffers.']);
  assert.deepEqual(ui.learnJudgmentLines(judged('correct')), ['You named buffers.']);
  assert.match(ui.learnGradeText({grade: 'needs_judgment', judgment: {verdict: 'unclear', follow_up: 'Why?'}}), /follow-up/);
  assert.match(ui.learnGradeText({grade: 'needs_judgment', judgment: {verdict: 'unclear'}}), /Still not clear/);
  const failed = {grade: 'needs_judgment', judgment: {verdict: 'could_not_judge'}};
  assert.match(ui.learnGradeText(failed), /could not judge/);
  assert.deepEqual(ui.learnJudgmentLines(failed), []);
  for (const verdict of ['correct', 'partial', 'incorrect', 'unclear', 'could_not_judge']) {
    assert.doesNotMatch(ui.learnGradeText(judged(verdict)), /master/i);
  }
});

test('context meter is bounded and never mentions cost', () => {
  assert.deepEqual(ui.learnContextMeter({context_fraction: 0.42}), {percent: 42, label: 'Context 42% used', warn: ''});
  assert.match(ui.learnContextMeter({context_fraction: 0.9}).warn, /nearly full/);
  assert.equal(ui.learnContextMeter({context_fraction: 5}).percent, 100);
  assert.equal(ui.learnContextMeter(null).percent, 0);
  assert.doesNotMatch(ui.learnContextMeter({context_fraction: 0.5}).label, /cost|price/i);
});

test('practice outcomes are described without mastery or scores', () => {
  assert.equal(ui.learnPracticeText('solved'), 'Solved.');
  assert.equal(ui.learnPracticeText('solved_with_hints'), 'Solved with hints.');
  assert.equal(ui.learnPracticeText('not_solved'), 'Not solved yet.');
  for (const outcome of ['solved', 'solved_with_hints', 'not_solved']) {
    assert.doesNotMatch(ui.learnPracticeText(outcome), /master|score|grade/i);
  }
});

test('diagrams render only bounded Mermaid and fall back honestly', () => {
  const good = ui.learnDiagram({mermaid: 'flowchart TD\n  a-->b', caption: 'A to B', node_id: 'ack'});
  assert.deepEqual([good.usable, good.caption, good.error], [true, 'A to B', '']);
  assert.equal(ui.learnDiagram({mermaid: 'graph LR\n  x-->y', node_id: 'ack'}).caption, 'ack');

  const failed = ui.learnDiagram({mermaid: 'graph LR\n  x-->y', error: 'strict Mermaid parse error'});
  assert.equal(failed.usable, false);
  assert.equal(failed.error, 'strict Mermaid parse error');

  const script = ui.learnDiagram({mermaid: '<script>alert(1)</script>'});
  assert.equal(script.usable, false);
  assert.match(script.error, /HTML or script/);

  const huge = ui.learnDiagram({mermaid: 'x'.repeat(8 * 1024 + 1)});
  assert.equal(huge.usable, false);
  assert.match(huge.error, /8 KiB/);

  assert.equal(ui.learnDiagram({mermaid: '   '}).usable, false);
  assert.match(ui.learnDiagram(null).error, /no source/);
});

test('LearnHarness confidence stop snapping, session filtering, and Sugiyama DAG layout work cleanly', () => {
  assert.equal(ui.learnSnapConfidence(0), 0);
  assert.equal(ui.learnSnapConfidence(22), 20);
  assert.equal(ui.learnSnapConfidence(52), 50);
  assert.equal(ui.learnSnapConfidence(78), 80);
  assert.equal(ui.learnSnapConfidence(98), 99);

  const sessions = [
    {id: '1', title: 'TCP Congestion', goal: 'TCP', kind: 'teach'},
    {id: '2', title: 'Drill: Rust lifetimes', goal: 'lifetimes', kind: 'practice'},
  ];
  assert.equal(ui.learnFilterSessions(sessions, 'teach', '').length, 1);
  assert.equal(ui.learnFilterSessions(sessions, 'practice', '').length, 1);
  assert.equal(ui.learnFilterSessions(sessions, 'all', 'rust').length, 1);

  const dag = ui.learnLayoutPlanDag({
    nodes: [{id: 'a', title: 'Node A'}, {id: 'b', title: 'Node B'}],
    edges: [['a', 'b']],
    order: ['a', 'b'],
  }, {current: 'b', visited: ['a', 'b']});
  assert.equal(dag.nodes.length, 2);
  assert.equal(dag.edges.length, 1);
  assert.ok(dag.nodes[1].y > dag.nodes[0].y);

  assert.deepEqual(
    ui.learnBuildAnswer('single', {choices: [], dont_know: true}).answer,
    {dont_know: true}
  );
  assert.deepEqual(
    ui.learnBuildAnswer('single', {choices: ['a'], confidence_pct: 78}).answer,
    {choices: ['a'], confidence: 4, confidence_pct: 80}
  );
});


function dictationHarness(overrides = {}) {
  const changes = [];
  const field = {value: 'My draft', maxLength: 4000, isConnected: true,
    dispatchEvent(event) { this.event = event.type; }, focus() { this.focused = true; }};
  let cancelled = 0;
  const controller = ui.learnCreateDictation({
    capture: async () => ({finish: async () => 'pcm', cancel: () => { cancelled++; }}),
    transcribe: async () => 'spoken answer',
    changed: change => changes.push(change), ...overrides,
  });
  return {controller, field, changes, cancelled: () => cancelled};
}

test('dictation records, transcribes and fills an editable draft without submitting', async () => {
  const h = dictationHarness();
  await h.controller.start(h.field);
  assert.equal(h.controller.busy, true);
  await h.controller.stop();
  assert.deepEqual(h.changes.map(c => c.phase), ['requesting', 'recording', 'transcribing', 'filled']);
  assert.equal(h.field.value, 'My draft spoken answer');
  assert.equal(h.field.event, 'input');
  assert.equal(h.field.focused, true);
  assert.equal(h.controller.busy, false);
});

test('microphone denial is inline, keeps the draft and allows retry', async () => {
  let denied = true;
  const h = dictationHarness({capture: async () => {
    if (denied) throw Object.assign(new Error('denied'), {name: 'NotAllowedError'});
    return {finish: async () => 'pcm', cancel() {}};
  }});
  await h.controller.start(h.field);
  assert.equal(h.changes.at(-1).phase, 'error');
  assert.match(h.changes.at(-1).message, /permission denied/);
  assert.equal(h.field.value, 'My draft');
  denied = false;
  await h.controller.start(h.field);
  await h.controller.stop();
  assert.equal(h.changes.at(-1).phase, 'filled');
});

test('STT failure, empty transcript and excessive length keep existing text', async () => {
  for (const transcribe of [async () => { throw new Error('STT offline'); }, async () => ' ', async () => 'x'.repeat(4001)]) {
    const h = dictationHarness({transcribe});
    await h.controller.start(h.field);
    await h.controller.stop();
    assert.equal(h.changes.at(-1).phase, 'error');
    assert.equal(h.field.value, 'My draft');
    assert.equal(h.cancelled(), 1);
    assert.equal(h.controller.busy, false);
  }
});

test('cancellation during microphone permission releases the late capture', async () => {
  let resolve;
  let released = false;
  const h = dictationHarness({capture: () => new Promise(r => { resolve = r; })});
  const starting = h.controller.start(h.field);
  h.controller.cancel();
  resolve({cancel() { released = true; }});
  await starting;
  assert.equal(released, true);
  assert.equal(h.changes.at(-1).phase, 'idle');
});

test('cancellation aborts STT and ignores late results after navigation', async () => {
  let resolve, signal;
  const h = dictationHarness({transcribe: (_, s) => { signal = s; return new Promise(r => { resolve = r; }); }});
  await h.controller.start(h.field);
  const stopping = h.controller.stop();
  await Promise.resolve();
  h.controller.cancel();
  assert.equal(signal.aborted, true);
  resolve('stale transcript');
  await stopping;
  assert.equal(h.field.value, 'My draft');
  assert.equal(h.changes.at(-1).phase, 'idle');
});

test('dictation accepts one capture and stop, preserves edits made while transcribing', async () => {
  let captures = 0, finish = 0, resolve;
  const h = dictationHarness({
    capture: async () => { captures++; return {finish: async () => { finish++; return 'pcm'; }, cancel() {}}; },
    transcribe: () => new Promise(r => { resolve = r; }),
  });
  await h.controller.start(h.field);
  await h.controller.start(h.field);
  const stopping = h.controller.stop();
  await h.controller.stop();
  h.field.value = 'Edited draft';
  resolve('new words');
  await stopping;
  assert.equal(captures, 1);
  assert.equal(finish, 1);
  assert.equal(h.field.value, 'Edited draft new words');
});

test('removed field never receives a transcript', async () => {
  const h = dictationHarness();
  await h.controller.start(h.field);
  h.field.isConnected = false;
  await h.controller.stop();
  assert.equal(h.field.value, 'My draft');
  assert.equal(h.changes.at(-1).phase, 'idle');
});


test('cancelled flush cannot orphan a newer recording', async () => {
  let release;
  let captures = 0, secondCancelled = 0;
  const h = dictationHarness({capture: async () => ++captures === 1
    ? {finish: () => new Promise(resolve => { release = resolve; }), cancel() {}}
    : {finish: async () => 'new audio', cancel() { secondCancelled++; }}});
  await h.controller.start(h.field);
  const stopping = h.controller.stop();
  h.controller.cancel();
  await h.controller.start(h.field);
  release('old audio');
  await stopping;
  assert.equal(h.controller.busy, true);
  h.controller.cancel();
  assert.equal(secondCancelled, 1);
  assert.equal(h.field.value, 'My draft');
});

test('dictation capture reuses capture.js, flushes final PCM and releases microphone', async () => {
  const vm = require('node:vm');
  const fs = require('node:fs');
  let stopped = false, closed = false, modulePath, processorName, processor;
  const context = {
    destination: {}, resume: async () => {}, close: async () => { closed = true; },
    audioWorklet: {addModule: async path => { modulePath = path; }},
    createMediaStreamSource: () => ({connect() {}, disconnect() {}}),
  };
  const track = {stop() { stopped = true; }};
  const sandbox = {module: {exports: {}}, Blob, ArrayBuffer, setTimeout, clearTimeout,
    document: {readyState: 'loading', addEventListener() {}},
    window: {document: {}, isSecureContext: true,
      navigator: {mediaDevices: {getUserMedia: async () => ({getTracks: () => [track]})}},
      AudioContext: function () { return context; },
      AudioWorkletNode: function (_, name) {
        processorName = name;
        processor = this;
        this.connect = () => {};
        this.disconnect = () => {};
        this.port = {postMessage: request => {
          assert.equal(request.action, 'flush');
          this.port.onmessage({data: new Uint8Array([1, 0]).buffer});
          this.port.onmessage({data: {type: 'flushed'}});
        }};
      },
    },
  };
  vm.runInNewContext(fs.readFileSync(require.resolve('../web/learn.js'), 'utf8'), sandbox);
  const capture = await sandbox.module.exports.learnCaptureAudio(() => {});
  processor.port.onmessage({data: new Uint8Array([0, 128]).buffer});
  const audio = await capture.finish();
  assert.equal(modulePath, '/capture.js');
  assert.equal(processorName, 'sail-capture');
  assert.equal(audio.type, 'application/octet-stream');
  assert.deepEqual([...new Uint8Array(await audio.arrayBuffer())], [0, 128, 1, 0]);
  assert.equal(stopped, true);
  assert.equal(closed, true);
});

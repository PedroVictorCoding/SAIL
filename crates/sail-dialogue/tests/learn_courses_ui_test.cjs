'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  CourseApi, CourseController, answerFor, gradeLabel, gradeEvidence,
  evidenceLabel, moveItem, dateLabel,
} = require('../web/courses.js');

const options = [{id: 'a', text: 'A'}, {id: 'b', text: 'B'}];
const step = {
  id: 'move', kind: 'choice', title: 'Predict the move', options,
  confidence: true, hint_count: 2, body: [{kind: 'code', text: '<script>untrusted</script>'}],
};
const grade = {
  result: 'correct', evidence_id: 'receipt-1', skill_ids: ['move'],
  validator_kind: 'choice', assistance_state: 'independent',
  provisional_vs_retained: 'provisional', review_due_at: '2026-11-07T10:00:00Z',
  gate_decision: 'continue', gate_reasons: [], revision: 1,
  feedback: {summary: 'Checked.', explanation: [], diagnostics: []}, next_step_id: 'next',
};
const lesson = {
  lesson: {id: 'moves', course_id: 'rust', title: 'Ownership', minutes: 4, steps: [step]},
  progress: {current_step_id: step.id, state: 'in_progress', completed_step_ids: []},
};

function fixture(respond = () => ({})) {
  const calls = [];
  let sequence = 0;
  const controller = new CourseController({
    async request(url, method = 'GET', body = null) {
      calls.push({url, method, body});
      return respond(url, method, body);
    },
  }, () => {}, () => `attempt-${++sequence}`);
  return {controller, calls};
}

function choose(controller, mode = 'lesson') {
  controller.setPlayer(mode, 'moves', step, {
    lesson: lesson.lesson,
    progress: lesson.progress,
  });
  controller.player.draft.choice = ['a'];
  controller.player.draft.confidence = 'low';
}

test('all answer kinds emit only contract values, never verdicts or timestamps', () => {
  assert.equal(answerFor({kind: 'explain'}), null);
  for (const kind of ['choice', 'predict']) {
    assert.deepEqual(answerFor({kind, options}, {choice: ['b'], passed: true}), {choice: ['b']});
    for (const choice of [[], ['a', 'b'], ['unknown'], ['a', 'a']]) {
      assert.throws(() => answerFor({kind, options}, {choice}));
    }
    assert.deepEqual(answerFor({kind, options, multi: true}, {choice: ['b', 'a']}), {choice: ['b', 'a']});
    assert.throws(() => answerFor({kind, options, multi: true}, {choice: ['a', 'a']}));
  }
  assert.deepEqual(answerFor({kind: 'order', items: options}, {order: ['b', 'a']}), {order: ['b', 'a']});
  for (const order of [[], ['a'], ['a', 'a'], ['a', 'other']]) {
    assert.throws(() => answerFor({kind: 'order', items: options}, {order}));
  }
  const code = 'fn main() {\n    println!("<script>");\n}\n';
  assert.deepEqual(answerFor({kind: 'rust_edit'}, {code}), {code});
  assert.deepEqual(answerFor({kind: 'fill'}, {text: '  &value  '}), {text: '  &value  '});
  assert.throws(() => answerFor({kind: 'fill'}, {text: ' '}));
  assert.throws(() => answerFor({kind: 'rust_edit'}));
  assert.throws(() => answerFor({kind: 'unknown'}));
});

test('unavailable and assisted results never display independent retained success', () => {
  assert.equal(gradeLabel({...grade, result: 'unavailable'}, step), 'Evaluation unavailable');
  assert.equal(gradeLabel({...grade, result: 'incorrect'}, step), 'Try again');
  assert.equal(gradeLabel(grade, {kind: 'explain'}), 'Exposure recorded');
  assert.match(gradeEvidence(grade), /Provisional/);
  assert.match(gradeEvidence({...grade, provisional_vs_retained: 'retained'}), /Retained/);
  for (const assistance_state of ['hinted', 'solution_seen']) {
    assert.match(gradeEvidence({...grade, assistance_state, provisional_vs_retained: 'retained'}), /Assisted work/);
  }
  assert.equal(gradeEvidence({...grade, result: 'unavailable', provisional_vs_retained: 'retained'}), 'No success certified');
  assert.match(gradeEvidence({...grade, provisional_vs_retained: 'none'}), /Exposure/);
});

test('evidence labels preserve all seven states and unknown/not-assessed cases', () => {
  const states = ['untested', 'exposure', 'assisted', 'provisional', 'due', 'retained', 'weak'];
  assert.equal(new Set(states.map(state => evidenceLabel({state}))).size, 7);
  assert.equal(evidenceLabel({state: 'untested'}), 'Untested');
  assert.equal(evidenceLabel({state: 'retained', not_assessed: true}), 'Not assessed');
  assert.equal(evidenceLabel({state: 'invented'}), 'Evidence unavailable');
  assert.equal(dateLabel(null), 'Not recorded');
  assert.equal(dateLabel('not-a-date'), 'Date unavailable');
});

test('keyboard order movement preserves every server item and respects boundaries', () => {
  const ids = ['a', 'b', 'c'];
  assert.deepEqual(moveItem(ids, 0, 1), ['b', 'a', 'c']);
  assert.deepEqual(moveItem(ids, 0, -1), ids);
  assert.deepEqual(moveItem(ids, 2, 1), ids);
  assert.deepEqual(ids, ['a', 'b', 'c']);
});

test('API reads the current owner token and only sends the supplied body', async () => {
  let token = 'first-token';
  const requests = [];
  const api = new CourseApi(async (url, options) => {
    requests.push({url, ...options});
    return {ok: true, json: async () => ({courses: []})};
  }, {getItem: name => {
    assert.equal(name, 'sail_owner_token');
    return token;
  }});
  await api.request('/courses');
  assert.equal(requests[0].url, '/api/learn/courses');
  assert.equal(requests[0].headers.Authorization, 'Bearer first-token');
  assert.equal(requests[0].body, undefined);
  token = 'updated-token';
  const body = {step_id: 'move', attempt_id: 'same-id', answer: {choice: ['a']}, confidence: 'low'};
  await api.request('/lessons/moves/attempt', 'POST', body);
  assert.equal(requests[1].headers.Authorization, 'Bearer updated-token');
  assert.deepEqual(JSON.parse(requests[1].body), body);
  const denied = new CourseApi(async () => ({ok: false, status: 401, json: async () => ({error: 'Invalid owner token.'})}), {getItem: () => token});
  await assert.rejects(denied.request('/courses'), /Invalid owner token/);
  const missing = new CourseApi(() => {throw Error('must not fetch');}, {getItem: () => ''});
  await assert.rejects(missing.request('/courses'), /owner token/);
  const offline = new CourseApi(async () => {throw Error('Network unavailable');}, {getItem: () => token});
  await assert.rejects(offline.request('/courses'), /Network unavailable/);
});

test('transport retry reuses UUID and original answer even after a changed draft', async () => {
  let attempts = 0;
  const {controller, calls} = fixture(() => {
    if (++attempts === 1) throw Error('Connection lost');
    return grade;
  });
  choose(controller);
  await controller.submit();
  assert.match(controller.error, /Connection lost/);
  controller.player.draft.choice = ['b'];
  await controller.submit();
  assert.deepEqual(calls[0].body, calls[1].body);
  assert.equal(calls[0].body.attempt_id, 'attempt-1');
  assert.deepEqual(calls[1].body.answer, {choice: ['a']});
  assert.equal(controller.pending, null);
  assert.equal(controller.player.grade, grade);
  assert.deepEqual(Object.keys(calls[0].body).sort(), ['answer', 'attempt_id', 'confidence', 'step_id']);
});

test('an unrecognized response cannot clear an unconfirmed attempt or certify success', async () => {
  let attempts = 0;
  const {controller, calls} = fixture(() => ++attempts === 1 ? {message: 'queued'} : grade);
  choose(controller);
  await controller.submit();
  assert(controller.pending);
  assert.equal(controller.player.grade, null);
  assert.match(controller.error, /did not return a checked result/);
  await controller.submit();
  assert.deepEqual(calls[0].body, calls[1].body);
  assert.equal(controller.player.grade, grade);
});

test('pending submissions cannot be lost by navigating to another activity', async () => {
  const {controller, calls} = fixture(() => {throw Error('Connection lost');});
  choose(controller);
  await controller.submit();
  await controller.catalog();
  await controller.course('rust');
  await controller.lesson('other');
  await controller.reviews();
  await controller.skills();
  assert.equal(calls.length, 1);
  assert.match(controller.error, /Confirm the submitted answer/);
});

test('server validation errors allow corrected input with a fresh attempt ID', async () => {
  let attempts = 0;
  const {controller, calls} = fixture(() => {
    if (++attempts === 1) throw Object.assign(Error('Invalid answer'), {status: 422});
    return grade;
  });
  choose(controller);
  await controller.submit();
  assert.equal(controller.pending, null);
  controller.player.draft.choice = ['b'];
  await controller.submit();
  assert.notEqual(calls[0].body.attempt_id, calls[1].body.attempt_id);
  assert.deepEqual(calls[1].body.answer, {choice: ['b']});
});

test('confidence and answer validation happen before any attempt request', async () => {
  const {controller, calls} = fixture(() => grade);
  choose(controller);
  controller.player.draft.confidence = '';
  await controller.submit();
  assert.match(controller.error, /confidence/);
  assert.equal(calls.length, 0);
});

test('explanations record exposure with a null answer, not an invented pass', async () => {
  const {controller, calls} = fixture(() => ({...grade, provisional_vs_retained: 'none'}));
  controller.setPlayer('lesson', 'moves', {id: 'read', kind: 'explain', body: []});
  await controller.submit();
  assert.equal(calls[0].body.answer, null);
  assert.equal(calls[0].body.confidence, null);
  assert.equal(calls[0].body.step_id, 'read');
});

test('lesson progression reloads authoritative progress instead of inventing completion', async () => {
  const next = {...step, id: 'next'};
  const {controller, calls} = fixture(url => {
    if (url.endsWith('/attempt')) return grade;
    return {...lesson, lesson: {...lesson.lesson, steps: [step, next]}, progress: {...lesson.progress, current_step_id: 'next'}};
  });
  choose(controller);
  await controller.submit();
  assert.equal(controller.player.step.id, 'move');
  await controller.continuePlayer();
  assert.equal(controller.player.step.id, 'next');
  assert.equal(calls[1].url, '/lessons/moves');
});

test('hints and reveal are explicit separate actions and diagnostics have neither', async () => {
  const {controller, calls} = fixture(url => url.endsWith('/hint')
    ? {level: 1, text: 'Track the owner.', hints_used: 1, hint_count: 2}
    : {solution: [{kind: 'code', text: '<script>solution</script>'}], assistance_state: 'solution_seen'});
  choose(controller);
  await controller.hint();
  assert.equal(controller.player.hints[0].text, 'Track the owner.');
  await controller.reveal();
  assert.equal(controller.player.solution[0].text, '<script>solution</script>');
  choose(controller, 'diagnostic');
  await controller.hint();
  await controller.reveal();
  assert.equal(calls.length, 2);
});

test('certification comes from the advance endpoint and preserves blocked review reasons', async () => {
  const blocked = {gate_decision: 'blocked_by_review', certified: false, gate_reasons: ['Review ownership first.'], due: []};
  const {controller, calls} = fixture(() => blocked);
  choose(controller);
  await controller.advance();
  assert.equal(calls[0].url, '/lessons/moves/advance');
  assert.equal(controller.player.gate, blocked);
  assert.equal(controller.player.complete, undefined);
  controller.browse(0);
  assert.equal(controller.player.browsing, true);
  assert.equal(calls.length, 1);
});

test('diagnostics progress after incorrect feedback and load the server report', async () => {
  const report = {summary: 'More evidence needed.', objectives: [], untested: ['borrowing']};
  const {controller, calls} = fixture(url => url.endsWith('/attempt')
    ? {...grade, result: 'incorrect', done: true, next: null}
    : report);
  choose(controller, 'diagnostic');
  await controller.submit();
  await controller.continuePlayer();
  assert.equal(controller.screen, 'report');
  assert.equal(controller.report, report);
  assert.equal(calls[1].url, '/diagnostics/moves/report');
});

test('completed diagnostic reports use the runtime session ID without starting another check', async () => {
  const {controller, calls} = fixture(url => {
    if (url === '/courses/rust') return {
      course: {id: 'rust'}, chapters: [], objectives: [],
      diagnostic: {id: 'definition-probe', session_id: 'session-old', state: 'done'},
    };
    if (url.startsWith('/next')) return {kind: 'lesson', lesson_id: 'moves', reasons: []};
    if (url === '/diagnostics/session-old/report') return {objectives: [], untested: [], summary: 'Saved report'};
    throw Error(`Unexpected request ${url}`);
  });
  await controller.course('rust');
  await controller.diagnosticReport(controller.detail.diagnostic.session_id);
  assert.equal(controller.screen, 'report');
  assert.equal(controller.report.summary, 'Saved report');
  assert(calls.every(call => call.method === 'GET'));
});

test('server-settled unavailable steps can continue without certifying an assessment', async () => {
  const settled = {...grade, result: 'unavailable', gate_decision: 'continue', next_step_id: null};
  const {controller, calls} = fixture(url => url.endsWith('/attempt') ? settled : {
    ...lesson, progress: {...lesson.progress, state: 'completed', current_step_id: null},
  });
  choose(controller);
  await controller.submit();
  assert.equal(gradeEvidence(controller.player.grade), 'No success certified');
  await controller.continuePlayer();
  assert.equal(controller.player.complete, true);
  assert.equal(calls[1].url, '/lessons/moves');
  assert.equal(calls.some(call => call.url.endsWith('/advance')), false);
});

test('review outage is unavailable, hints remain assistance, and queue is refetched', async () => {
  const queue = {items: [], catch_up: {total_due: 0, cap: 5, sessions: 0}};
  const {controller, calls} = fixture(url => {
    if (url.endsWith('/attempt')) return {...grade, result: 'unavailable', feedback: {summary: 'Sandbox unavailable.'}};
    if (url.endsWith('/hint')) return {level: 1, text: 'Track the value.', hints_used: 1, hint_count: 2};
    return queue;
  });
  choose(controller, 'review');
  controller.courseId = 'rust';
  await controller.hint();
  assert.equal(calls[0].body, null);
  await controller.submit();
  assert.equal(calls[1].body.step_id, undefined);
  assert.equal(controller.player.grade.result, 'unavailable');
  assert.equal(controller.player.draft.choice[0], 'a');
  await controller.continuePlayer();
  assert.equal(controller.screen, 'reviews');
  assert.equal(controller.queue, queue);
});

test('study-free browsing reads past an exercise without submitting evidence', async () => {
  const {controller, calls} = fixture();
  choose(controller);
  controller.player.lesson.steps = [step, {id: 'read', kind: 'explain', title: 'Borrowing', body: []}];
  controller.browse(1);
  assert.equal(controller.player.step.id, 'read');
  assert.equal(controller.player.browsing, true);
  await controller.submit();
  assert.equal(calls.length, 0);
  controller.browse(0);
  assert.equal(controller.player.step.id, step.id);
});

test('order and Rust editor answers travel to the server verbatim, without execution', async () => {
  const {controller, calls} = fixture(() => ({...grade, result: 'unavailable'}));
  controller.setPlayer('lesson', 'moves', {id: 'order', kind: 'order', items: options});
  controller.player.draft.order = ['b', 'a'];
  await controller.submit();
  assert.deepEqual(calls[0].body.answer, {order: ['b', 'a']});
  controller.setPlayer('lesson', 'moves', {id: 'edit', kind: 'rust_edit', starter: 'fn main() {}'});
  const code = 'fn main() {\n    println!("<img src=x>");\n}';
  controller.player.draft.code = code;
  await controller.submit();
  assert.deepEqual(calls[1].body.answer, {code});
});

test('evidence explanations and bounded queue are taken unchanged from server records', async () => {
  const why = '<svg onload=alert(1)>Delayed review pending.</svg>';
  const record = {title: 'Borrowing', state: 'provisional', why};
  const queue = {items: [{id: 'review'}], catch_up: {total_due: 17, cap: 5, sessions: 4}};
  const {controller} = fixture(url => url.startsWith('/skills') ? {objectives: [record]} : queue);
  controller.courseId = 'rust';
  await controller.skills();
  assert.equal(controller.evidence[0].why, why);
  await controller.reviews();
  assert.equal(controller.queue.catch_up.total_due, 17);
  assert.equal(controller.queue.items.length, 1);
});

test('one shell tab and one of each embedded asset; untrusted content has text-only sinks', () => {
  const read = file => fs.readFileSync(path.join(__dirname, '..', file), 'utf8');
  const html = read('web/index.html');
  const assets = read('src/ui_assets.rs');
  const js = read('web/courses.js');
  assert.equal((html.match(/id="coursesTab"/g) || []).length, 1);
  assert.equal((html.match(/id="coursesPanelMain"/g) || []).length, 1);
  assert.match(html, /id="coursesPanelMain" role="tabpanel" aria-labelledby="coursesTab" hidden/);
  for (const extension of ['js', 'css']) {
    assert.equal((html.match(new RegExp(`/courses\\.${extension}`, 'g')) || []).length, 1);
    assert.equal((assets.match(new RegExp(`web/courses\\.${extension}`, 'g')) || []).length, 1);
  }
  assert.doesNotMatch(js, /innerHTML|eval\(|new Function|reason_turn|\/sessions/);
  assert.match(js, /textContent = String\(text\)/);
  assert.match(js, /root\.crypto\.randomUUID\(\)/);
});

test('real backend response fixture matches navigation, input and evidence projections', {
  skip: !process.env.COURSES_API_FIXTURE,
}, async () => {
  const samples = JSON.parse(fs.readFileSync(process.env.COURSES_API_FIXTURE, 'utf8'));
  const {controller} = fixture(url => {
    if (url === '/courses') return samples['GET /courses'];
    if (url === '/courses/rust-systems') return samples['GET /courses/rust-systems'];
    if (url === '/next?course=rust-systems') return samples['GET /next'];
    if (url === '/lessons/rust-own-move') return samples['GET /lessons/:id'];
    if (url.startsWith('/skills/evidence')) return samples['GET /skills/evidence'];
    throw Error(`Unexpected fixture request: ${url}`);
  });
  await controller.catalog();
  assert.equal(controller.courses.find(course => course.id === 'rust-systems').status, 'ready');
  assert.equal(controller.courses.find(course => course.id === 'physics-engineering').status, 'planned');
  await controller.course('rust-systems');
  assert.equal(controller.error, '');
  assert.equal(controller.next.kind, samples['GET /next'].kind);
  await controller.lesson('rust-own-move');
  assert.equal(controller.error, '');
  for (const step of controller.player.lesson.steps) {
    let draft = {};
    if (step.options) draft.choice = [step.options[0].id];
    if (step.items) draft.order = step.items.map(item => item.id);
    if (step.kind === 'fill') draft.text = 'clone';
    if (step.kind === 'rust_edit') draft.code = step.starter;
    answerFor(step, draft);
  }
  const hinted = samples['POST /lessons/:id/attempt (correct, hinted)'];
  assert.match(gradeEvidence(hinted), /Assisted work/);
  assert.equal(gradeEvidence(samples['POST /lessons/:id/attempt (incorrect)']), 'No success certified');
  await controller.skills();
  assert.equal(controller.evidence[0].objective_id, 'rust.move');
  assert(controller.evidence.every(record => evidenceLabel(record)));
});

// Opt in to the lightweight isolated browser fixture; Claude also owns real backend/shell QA.
test('browser: diagnostic, lesson, hints, reveal, retry, runner outage, gate, review and evidence', {
  skip: process.env.COURSES_BROWSER !== '1',
  timeout: 60000,
}, async () => {
  const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
  const browser = await chromium.launch({headless: true, args: ['--no-sandbox']});
  try {
    const page = await browser.newPage({viewport: {width: 1280, height: 1000}});
    const errors = [];
    const requests = [];
    page.on('pageerror', error => errors.push(error.message));
    let current = 0;
    let diagnosticDone = false;
    let complete = false;
    let reviewed = false;
    let hinted = false;
    let revealed = false;
    let loseChoiceResponse = true;
    const receipts = new Map();
    const steps = [
      {id: 'read', kind: 'explain', title: 'A single owner', body: [{kind: 'p', text: 'Track where a value is available.'}], hint_count: 0},
      {...step, id: 'predict'},
      {id: 'order', kind: 'order', title: 'Arrange the statements', body: [], items: options, hint_count: 1},
      {id: 'fill', kind: 'fill', title: 'Borrow this value', body: [], template: 'inspect(____);', hint_count: 1},
      {id: 'edit', kind: 'rust_edit', title: 'Repair a borrow', body: [], starter: 'fn main() {}', limits: {wall_ms: 1000, mem_mb: 128}, hint_count: 1},
    ];
    const objective = {
      objective_id: 'move', title: 'Track moves', state: 'provisional', independent_passes: 1,
      assisted_passes: 0, failures: 1, why: '<svg onload=alert(1)>Review pending.</svg>',
      last_independent_success: null, last_delayed_success: null,
      retention_due_at: '2026-11-07T10:00:00Z', not_assessed: false, revision: 2,
    };
    const course = {
      id: 'rust', title: 'Rust & Systems Programming', summary: 'Original SAIL ownership challenges.', status: 'ready',
      lesson_count: 1, minutes: 5,
      progress: {lessons_completed: 0, provisional: 0, retained: 0, due: 1},
    };
    await page.route('**/*', async route => {
      const request = route.request();
      const url = new URL(request.url());
      assert.equal(url.origin, 'https://sail.test');
      if (url.pathname === '/') {
        return route.fulfill({contentType: 'text/html', body: '<html><head><meta charset="utf-8"><link rel="stylesheet" href="/tokens.css"><link rel="stylesheet" href="/courses.css"></head><body style="margin:0"><div id="fixture" class="courses-root"></div><script src="/courses.js"></script></body></html>'});
      }
      if (!url.pathname.startsWith('/api/learn')) {
        const file = path.join(__dirname, '../web', url.pathname.slice(1));
        assert(fs.existsSync(file), `Unexpected asset ${url.pathname}`);
        return route.fulfill({contentType: url.pathname.endsWith('.css') ? 'text/css; charset=utf-8' : 'text/javascript; charset=utf-8', body: fs.readFileSync(file)});
      }
      assert.equal(request.headers().authorization, 'Bearer owner-token');
      const endpoint = url.pathname.replace('/api/learn', '') + url.search;
      const body = request.postData() ? request.postDataJSON() : null;
      requests.push({endpoint, method: request.method(), body});
      let result;
      if (endpoint === '/courses') {
        result = {courses: [course, {id: 'logic', title: 'Philosophy & Logic', status: 'planned'}, {id: 'physics', title: 'Physics, Math & Engineering', status: 'planned'}]};
      } else if (endpoint === '/courses/rust') {
        result = {course, diagnostic: {id: 'definition-probe', session_id: diagnosticDone ? 'cold' : null, state: diagnosticDone ? 'done' : 'not_started'}, objectives: [],
          chapters: [{id: 'ownership', title: 'Ownership', lessons: [{id: 'moves', title: 'Move and borrow', minutes: 5, state: complete ? 'completed' : 'in_progress', certified: false, prerequisites: []}]}]};
      } else if (endpoint === '/next?course=rust') {
        result = diagnosticDone ? {kind: 'lesson', lesson_id: 'moves', reasons: ['Continue the current lesson.']} : {kind: 'diagnostic', reasons: ['Start with a cold diagnostic.']};
      } else if (endpoint === '/diagnostics') {
        result = {id: 'cold', step, done: false};
      } else if (endpoint === '/diagnostics/cold/attempt') {
        diagnosticDone = true;
        result = {...grade, result: 'incorrect', done: true, next: null};
      } else if (endpoint === '/diagnostics/cold/report') {
        result = {summary: 'The diagnostic found a place to start.', objectives: [{...objective, state: 'untested'}], untested: ['borrow']};
      } else if (endpoint === '/lessons/moves') {
        result = {lesson: {id: 'moves', course_id: 'rust', title: 'Move and borrow', minutes: 5, steps},
          progress: {current_step_id: steps[current].id, completed_step_ids: steps.slice(0, current).map(item => item.id), state: complete ? 'completed' : 'in_progress'}};
      } else if (endpoint === '/lessons/moves/hint' || endpoint === '/reviews/review-move/hint') {
        if (endpoint.startsWith('/reviews/')) assert.equal(body, null);
        hinted = true;
        result = {level: 1, text: '<img src=x>Follow the value.', hints_used: 1, hint_count: 2, assistance_state: 'hinted'};
      } else if (endpoint === '/lessons/moves/reveal') {
        revealed = true;
        result = {solution: [{kind: 'code', text: '<script>solution is text</script>'}], assistance_state: 'solution_seen'};
      } else if (endpoint === '/lessons/moves/attempt') {
        if (receipts.has(body.attempt_id)) {
          result = receipts.get(body.attempt_id);
        } else {
          assert.equal('passed' in body, false);
          assert.equal('state' in body, false);
          assert.equal('timestamp' in body, false);
          const active = steps.find(item => item.id === body.step_id);
          if (active.kind === 'order') assert.deepEqual(body.answer.order, ['b', 'a']);
          if (active.kind === 'fill') assert.equal(body.answer.text, '  &value  ');
          if (active.kind === 'rust_edit') assert.match(body.answer.code, /<img src=x>/);
          const unavailable = active.kind === 'rust_edit';
          result = {...grade, result: unavailable ? 'unavailable' : 'correct',
            assistance_state: revealed ? 'solution_seen' : hinted ? 'hinted' : 'independent',
            provisional_vs_retained: hinted || unavailable || active.kind === 'explain' ? 'none' : 'provisional',
            gate_decision: unavailable ? 'continue' : current === steps.length - 1 ? 'lesson_complete' : 'continue',
            next_step_id: unavailable || current === steps.length - 1 ? null : steps[current + 1].id,
            feedback: {summary: unavailable ? 'Runner is offline.' : '<img src=x onerror=alert(1)> Checked.', explanation: [], diagnostics: [], unavailable_reason: unavailable ? 'No sandbox is configured.' : null}};
          if (current === steps.length - 1) complete = true;
          else current++;
          receipts.set(body.attempt_id, result);
          if (active.kind === 'choice' && loseChoiceResponse) {
            loseChoiceResponse = false;
            return route.abort('failed');
          }
        }
      } else if (endpoint === '/lessons/moves/advance') {
        result = {gate_decision: 'blocked_by_review', certified: false, gate_reasons: ['Review ownership first.'], due: []};
      } else if (endpoint === '/reviews/due?course=rust') {
        result = {items: reviewed ? [] : [{id: 'review-move', objective_title: 'Delayed moves review', due_at: '2026-11-06T10:00:00Z', reason: 'A delayed unseen challenge is due.', step}],
          catch_up: {total_due: reviewed ? 0 : 17, cap: 5, sessions: reviewed ? 0 : 4}};
      } else if (endpoint === '/reviews/review-move/attempt') {
        assert.equal(body.step_id, undefined);
        reviewed = true;
        result = {...grade, assistance_state: 'hinted', provisional_vs_retained: 'none', next_step_id: null};
      } else if (endpoint === '/skills/evidence?course=rust') {
        result = {objectives: ['untested', 'exposure', 'assisted', 'provisional', 'due', 'retained', 'weak'].map(state => ({...objective, state, title: `Objective ${state}`}))};
      } else {
        throw Error(`Unexpected course request: ${endpoint}`);
      }
      return route.fulfill({json: result});
    });
    await page.goto('https://sail.test/');
    await page.evaluate(() => {
      localStorage.setItem('sail_owner_token', 'owner-token');
      window.courseFixture = SailCourses.mount(document.getElementById('fixture'), new SailCourses.CourseApi(fetch.bind(window), localStorage));
      return courseFixture.catalog();
    });
    const click = name => page.getByRole('button', {name, exact: true}).click();
    const answer = async confidence => {
      await page.getByLabel('A', {exact: true}).check();
      await page.getByLabel('Confidence before checking').selectOption(confidence);
      await click('Check answer');
    };
    assert.equal(await page.getByRole('button', {name: 'Open course', exact: true}).count(), 1);
    await click('Open course');
    await click('Find my starting point');
    assert.equal(await page.getByRole('button', {name: /hint|solution/i}).count(), 0);
    await answer('medium');
    await click('View diagnostic report');
    await page.getByText('Not assessed: borrow', {exact: true}).waitFor();
    await click('Course path');
    const checksStarted = requests.filter(item => item.endpoint === '/diagnostics').length;
    await click('Diagnostic report');
    await page.getByText('Not assessed: borrow', {exact: true}).waitFor();
    assert.equal(requests.filter(item => item.endpoint === '/diagnostics').length, checksStarted);
    await click('Course path');
    await click('Continue lesson');
    await click('Study freely');
    const before = requests.filter(item => item.endpoint.endsWith('/attempt')).length;
    await click('Next material');
    await click('Previous material');
    assert.equal(requests.filter(item => item.endpoint.endsWith('/attempt')).length, before);
    await click('Resume assessed step');
    await click('Continue');
    await click('Continue lesson');
    await click('Next hint (up to 2)');
    await click('Show solution');
    assert.equal(revealed, false);
    await click('Reveal solution (records assistance)');
    await answer('low');
    await page.getByText(/Failed to fetch|NetworkError/).first().waitFor();
    await click('Retry submitted answer');
    await page.getByText(/Assisted work · no independent success certified/).waitFor();
    const sent = requests.filter(item => item.endpoint === '/lessons/moves/attempt' && item.body.step_id === 'predict');
    assert.equal(sent.length, 2);
    assert.deepEqual(sent[0].body, sent[1].body);
    await click('Continue lesson');
    await page.getByRole('button', {name: 'Move down: A', exact: true}).click();
    await click('Check answer');
    await click('Continue lesson');
    await page.getByLabel('Your expression').fill('  &value  ');
    await click('Check answer');
    await click('Continue lesson');
    await page.getByLabel('Rust code').fill('fn main() {\n    println!("<img src=x>");\n}');
    await click('Check in sandbox');
    await page.getByRole('heading', {name: 'Evaluation unavailable', exact: true}).waitFor();
    await page.getByText('No sandbox is configured.', {exact: true}).waitFor();
    assert.equal(await page.locator('#fixture img, #fixture script, #fixture svg').count(), 0);
    for (const width of [1280, 390, 360]) {
      await page.setViewportSize({width, height: 1000});
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `overflow at ${width}`);
      await page.screenshot({path: `/tmp/courses-player-${width}.png`, fullPage: true});
    }
    assert.equal(await page.getByRole('button', {name: 'Check in sandbox', exact: true}).count(), 0);
    await click('Continue without assessment');
    await click('Check certified advance');
    await page.getByText('Review ownership first.', {exact: true}).waitFor();
    await click('Open due reviews');
    await page.getByText('17 due in total · up to 5 in this session · 4 catch-up sessions', {exact: true}).waitFor();
    await click('Start this review');
    assert.equal(await page.getByRole('button', {name: 'Show solution', exact: true}).count(), 0);
    await click('Next hint (up to 2)');
    await answer('high');
    await click('Return to due queue');
    await page.getByText(/No reviews are due/).waitFor();
    await click('Evidence');
    await page.getByRole('heading', {name: 'What SAIL thinks you know', exact: true}).waitFor();
    await page.getByText(objective.why, {exact: true}).first().waitFor();
    assert.equal(await page.locator('#fixture svg').count(), 0);
    await page.emulateMedia({forcedColors: 'active', reducedMotion: 'reduce'});
    await page.screenshot({path: '/tmp/courses-evidence-forced-colors.png', fullPage: true});
    assert.deepEqual(errors, []);
    assert(requests.every(item => !/reason_turn|\/sessions|\/mind/.test(item.endpoint)));
  } finally {
    await browser.close();
  }
});

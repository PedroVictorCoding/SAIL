'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const factory = require('../web/learn-factory.js');

// Lightweight jsdom-free DOM tree for exercising SailLearnFactory.mount(el, api).
class FakeElement {
  constructor(tag, ownerDocument) {
    this.tagName = String(tag).toUpperCase();
    this.ownerDocument = ownerDocument;
    this.children = [];
    this.attributes = new Map();
    this.listeners = new Map();
    this.className = '';
    this.textContent = '';
    this.type = '';
    this.name = '';
    this.value = '';
    this.placeholder = '';
    this.disabled = false;
    this.selected = false;
    this.files = [];
  }

  setAttribute(name, value) {
    this.attributes.set(String(name), String(value));
  }

  getAttribute(name) {
    return this.attributes.get(String(name)) ?? null;
  }

  addEventListener(event, handler) {
    const list = this.listeners.get(event) || [];
    list.push(handler);
    this.listeners.set(event, list);
  }

  async trigger(event, payload = {}) {
    const list = this.listeners.get(event) || [];
    for (const fn of list) {
      await fn({ preventDefault() {}, target: this, ...payload });
    }
  }

  append(...nodes) {
    for (const node of nodes) {
      if (node !== null && node !== undefined) {
        this.children.push(node);
      }
    }
  }

  replaceChildren(...nodes) {
    this.children = [];
    this.append(...nodes);
  }

  allText() {
    let out = this.textContent || '';
    for (const child of this.children) {
      const childText = child.allText();
      if (childText) out += (out ? ' ' : '') + childText;
    }
    return out;
  }

  findAll(predicate, acc = []) {
    if (predicate(this)) acc.push(this);
    for (const child of this.children) {
      child.findAll(predicate, acc);
    }
    return acc;
  }

  find(predicate) {
    return this.findAll(predicate)[0] || null;
  }

  findButton(label) {
    return this.find(
      node => node.tagName === 'BUTTON' && node.textContent === label
    );
  }
}

function createPanel() {
  const doc = {
    createElement(tag) {
      return new FakeElement(tag, doc);
    },
  };
  return doc.createElement('div');
}

test('sourceStatusLabel formats indexed, needs review, extracting pages and failed reason', () => {
  assert.equal(
    factory.sourceStatusLabel({ status: { state: 'indexed' } }),
    'indexed'
  );
  assert.equal(
    factory.sourceStatusLabel({
      status: { state: 'needs_review', reason: 'scanned; OCR not available' },
    }),
    'needs review'
  );
  assert.equal(
    factory.sourceStatusLabel({
      status: { state: 'extracting' },
      pages_done: 140,
      pages_total: 680,
    }),
    'extracting 140/680 pages'
  );
  assert.equal(
    factory.sourceStatusLabel({
      status: { state: 'failed', reason: 'encrypted PDF refused' },
    }),
    'failed: encrypted PDF refused'
  );
  assert.equal(
    factory.sourceStatusLabel({ status: { state: 'queued' } }),
    'queued'
  );
});

test('hostile source titles and lesson steps render via textContent only, never creating markup', async () => {
  const hostileTitle = '<img src=x onerror=alert(1)>The Rust Book';
  const hostileStepText = '<script>window.pwned = true;</script>Borrow checker rule';
  const panel = createPanel();
  const controller = factory.mount(panel, async path => {
    if (path === '/factory/sources') {
      return {
        sources: [
          {
            source_id: 'src-1',
            title: hostileTitle,
            source_type: 'pdf_text',
            rights: { kind: 'restricted' },
            status: { state: 'indexed' },
            access_scope: 'learning:rust',
          },
        ],
      };
    }
    if (path === '/factory/drafts/draft-hostile') {
      return {
        draft: {
          draft_id: 'draft-hostile',
          course_id: 'rust',
          lesson_plan_id: 'own-1',
          stage: 'ready',
          output_hash: 'abc123',
          output: {
            lesson: {
              id: 'own-1',
              title: hostileTitle,
              minutes: 5,
              objective_ids: ['rust.move'],
              steps: [
                {
                  id: 's1',
                  kind: 'explain',
                  title: '<svg onload=alert(2)>Move semantics',
                  body: [{ kind: 'p', text: hostileStepText }],
                },
              ],
            },
          },
        },
        receipts: [],
        citations: [],
      };
    }
    return {};
  });

  await controller.loadSources();
  await controller.loadDraft('draft-hostile');

  assert.equal(
    panel.findAll(n => ['IMG', 'SCRIPT', 'SVG', 'IFRAME'].includes(n.tagName)).length,
    0
  );
  const headings = panel.findAll(n => n.tagName === 'H3').map(n => n.textContent);
  assert(headings.includes(hostileTitle));
  assert(panel.allText().includes(hostileStepText));

  const jsSource = fs.readFileSync(
    path.join(__dirname, '../web/learn-factory.js'),
    'utf8'
  );
  assert.doesNotMatch(jsSource, /innerHTML|outerHTML|insertAdjacentHTML|eval\(|new Function/);
});

test('Publish gating only enables ready and needs_review, and shows confirm text for needs_review', async () => {
  for (const stage of [
    'queued',
    'source_retrieved',
    'draft_generated',
    'schema_validated',
    'source_grounded',
    'subject_validated',
    'exercise_tested',
    'quality_reviewed',
    'published',
    'failed',
    'cancelled',
    'blocked',
    'source_invalidated',
  ]) {
    assert.equal(factory.canPublishDraft(stage), false, `stage ${stage} must not publish`);
    assert.equal(factory.publishConfirmText(stage), '');
  }
  assert.equal(factory.canPublishDraft('ready'), true);
  assert.equal(factory.publishConfirmText('ready'), '');
  assert.equal(factory.canPublishDraft('needs_review'), true);
  assert.equal(
    factory.publishConfirmText('needs_review'),
    "You are approving a lesson the checks couldn't fully verify"
  );

  const calls = [];
  const panel = createPanel();
  let currentStage = 'schema_validated';
  const controller = factory.mount(panel, async (url, opts) => {
    calls.push({ url, opts });
    if (url === '/factory/drafts/d1') {
      return {
        draft: {
          draft_id: 'd1',
          course_id: 'rust',
          lesson_plan_id: 'l1',
          stage: currentStage,
          output_hash: 'h1',
          output: {
            lesson: { id: 'l1', title: 'Lesson 1', minutes: 4, objective_ids: [], steps: [] },
          },
        },
        receipts: [
          {
            receipt_id: 'r1',
            draft_id: 'd1',
            output_hash: 'h1',
            check: 'rubric',
            verdict: { verdict: 'needs_review', reasons: ['Rubric criteria require human check'] },
            validator_version: 'subject/1',
          },
        ],
        citations: [
          {
            fragment_id: 'f1',
            chapter_path: ['Chapter 4', 'Ownership'],
            location: { kind: 'page', page: '37', physical: 49 },
            quality: { state: 'clean' },
          },
        ],
      };
    }
    if (url === '/factory/drafts/d1/publish') {
      return { course_id: 'rust', revision: 2 };
    }
    return {};
  });

  await controller.loadDraft('d1');
  let publishBtn = panel.findButton('Publish');
  assert.equal(publishBtn.disabled, true);
  assert.equal(
    panel.find(n => n.className === 'factory-publish-confirm'),
    null
  );
  // Attempting to publish while gated does not call the API.
  await controller.publishDraft('d1');
  assert.equal(
    calls.filter(c => c.url === '/factory/drafts/d1/publish').length,
    0
  );

  // Switch stage to needs_review: Publish becomes enabled and shows the confirm text.
  currentStage = 'needs_review';
  await controller.loadDraft('d1');
  publishBtn = panel.findButton('Publish');
  assert.equal(publishBtn.disabled, false);
  const confirmNode = panel.find(n => n.className === 'factory-publish-confirm');
  assert(confirmNode, 'confirm node must be rendered for needs_review');
  assert.equal(
    confirmNode.textContent,
    "You are approving a lesson the checks couldn't fully verify"
  );
  assert(panel.allText().includes('Chapter 4 › Ownership · Page 37 · clean'));
  assert(panel.allText().includes('Rubric criteria require human check'));

  await publishBtn.trigger('click');
  assert.equal(
    calls.filter(c => c.url === '/factory/drafts/d1/publish').length,
    1
  );
});

test('import request shape defaults rights to restricted and sends query params plus raw bytes', async () => {
  const bytes = new Uint8Array([0x25, 0x50, 0x44, 0x46]);
  const built = factory.buildImportRequest({
    fileName: 'The Rust Book.pdf',
    title: 'The Rust Programming Language',
    scope: 'learning:rust',
    bytes,
  });
  assert.equal(
    built.path,
    '/factory/sources?file_name=The+Rust+Book.pdf&title=The+Rust+Programming+Language&rights=restricted&license=&scope=learning%3Arust'
  );
  assert.equal(built.options.method, 'POST');
  assert.equal(built.options.body, bytes);

  const calls = [];
  const panel = createPanel();
  const controller = factory.mount(panel, async (url, opts) => {
    calls.push({ url, opts });
    return {
      source: {
        source_id: 'src-rust',
        title: 'The Rust Programming Language',
        source_type: 'pdf_text',
        rights: { kind: 'restricted' },
        status: { state: 'extracting' },
        pages_done: 20,
        pages_total: 560,
      },
      duplicate: false,
    };
  });

  // Verify the select defaults to 'restricted'.
  const rightsSelect = panel.find(n => n.tagName === 'SELECT' && n.name === 'rights');
  assert.equal(rightsSelect.value, 'restricted');

  controller.importForm.fileName = 'rust.pdf';
  controller.importForm.title = 'Rust Book';
  controller.importForm.scope = 'learning:rust';
  controller.importForm.bytes = bytes;

  const form = panel.find(n => n.tagName === 'FORM');
  await form.trigger('submit');

  assert.equal(calls.length, 1);
  assert.equal(
    calls[0].url,
    '/factory/sources?file_name=rust.pdf&title=Rust+Book&rights=restricted&license=&scope=learning%3Arust'
  );
  assert.equal(calls[0].opts.method, 'POST');
  assert.equal(calls[0].opts.body, bytes);
  assert(panel.allText().includes('extracting 20/560 pages'));
});

test('fetch error is caught and shown in an inline alert message instead of thrown', async () => {
  const panel = createPanel();
  const controller = factory.mount(panel, async () => {
    throw new Error('Factory backend unreachable (503)');
  });

  // Must resolve without throwing.
  const res = await controller.loadSources();
  assert.equal(res, null);
  assert.equal(controller.error, 'Factory backend unreachable (503)');

  const alertBox = panel.find(n => n.getAttribute('role') === 'alert');
  assert(alertBox, 'inline error alert must be rendered');
  assert.equal(alertBox.allText(), 'Factory backend unreachable (503)');
});

test('course projects, plan review (proposed vs supported edges) and queue views wire actions', async () => {
  const calls = [];
  const panel = createPanel();
  const controller = factory.mount(panel, async (url, opts) => {
    calls.push({ url, opts });
    if (url === '/factory/courses') {
      return {
        courses: [
          {
            course: {
              course_id: 'rust-101',
              title: 'Rust Ownership',
              domain: 'systems',
              status: { state: 'generating' },
            },
            progress: {
              planned: 6,
              published: 2,
              needs_review: 1,
              failed: 0,
              queued_jobs: 3,
            },
          },
        ],
      };
    }
    if (url === '/factory/courses/rust-101') {
      return {
        course: {
          course_id: 'rust-101',
          title: 'Rust Ownership',
          domain: 'systems',
          status: { state: 'awaiting_plan_approval' },
        },
        graph: {
          course_id: 'rust-101',
          revision: 1,
          concepts: [
            {
              id: 'rust.borrow',
              name: 'Borrowing',
              prerequisites: [
                {
                  requires: 'rust.move',
                  status: 'supported',
                  rationale: 'Cited fragment uses move before borrow.',
                },
                {
                  requires: 'rust.stack',
                  status: 'proposed',
                  rationale: 'Inferred from chapter order.',
                },
              ],
            },
          ],
        },
        plan: {
          course_id: 'rust-101',
          revision: 3,
          title: 'Ownership Curriculum',
          status: 'proposed',
          units: [
            {
              id: 'u1',
              title: 'Unit 1: Memory & Moves',
              lessons: [
                {
                  id: 'l1',
                  title: 'Move Semantics',
                  minutes: 5,
                  objective_ids: ['obj.move.1'],
                },
              ],
            },
          ],
        },
        lessons: [
          {
            lesson_plan_id: 'l1',
            unit_id: 'u1',
            title: 'Move Semantics',
            stage: 'ready',
            draft_id: 'draft-1',
            published_revision: null,
          },
        ],
      };
    }
    if (url === '/factory/courses/rust-101/plan/approve') {
      return { plan: { course_id: 'rust-101', revision: 3, status: 'owner_approved', units: [] } };
    }
    if (url === '/factory/courses/rust-101/next') {
      return {
        queued: {
          job: { kind: 'generate_lesson', course_id: 'rust-101', lesson_plan_id: 'l1', plan_revision: 3 },
          reasons: [{ reason: 'owner_request' }],
          priority: [0, 1],
        },
      };
    }
    if (url === '/factory/jobs') {
      return {
        paused: false,
        running: {
          job: { kind: 'generate_lesson', course_id: 'rust-101', lesson_plan_id: 'l1' },
          reasons: [{ reason: 'ahead_of_progress', lesson_plan_id: 'l1', distance: 1 }],
          priority: [4, 1],
        },
        queue: [
          {
            job: { kind: 'validate_draft', draft_id: 'draft-1' },
            reasons: [{ reason: 'pipeline' }],
            priority: [1, 2],
          },
        ],
        recent: [
          {
            job: { kind: 'extract_source', source_id: 'bad-pdf', revision: 1 },
            state: 'failed',
            error: 'encrypted PDF refused',
            at_ms: 1000,
          },
        ],
      };
    }
    if (url === '/factory/jobs/pause') {
      return { paused: true };
    }
    return {};
  });

  await controller.loadCourses();
  assert(
    panel.allText().includes('6 planned · 2 published · 1 needs review · 0 failed · 3 queued')
  );

  const nextBtn = panel.findButton('Generate next lesson now');
  assert(nextBtn, 'Generate next lesson now button must exist');
  await nextBtn.trigger('click');
  assert(calls.some(c => c.url === '/factory/courses/rust-101/next' && c.opts.method === 'POST'));

  await controller.loadCourse('rust-101');
  const text = panel.allText();
  assert(text.includes('Unit 1: Memory & Moves'));
  assert(text.includes('Move Semantics'));
  assert(text.includes('Objectives: obj.move.1'));
  const edgeBadges = panel
    .findAll(n => n.className.includes('factory-edge-badge'))
    .map(n => n.textContent);
  assert.deepEqual(edgeBadges, ['supported', 'proposed']);

  const approveBtn = panel.findButton('Approve plan');
  await approveBtn.trigger('click');
  assert(
    calls.some(
      c =>
        c.url === '/factory/courses/rust-101/plan/approve' &&
        c.opts.body.revision === 3
    )
  );

  await controller.loadJobs();
  assert(panel.allText().includes('ahead of progress (l1)'));
  assert(panel.allText().includes('Validate draft draft-1'));
  assert(panel.allText().includes('encrypted PDF refused'));

  const pauseQueueBtn = panel.findButton('Pause queue');
  await pauseQueueBtn.trigger('click');
  assert(calls.some(c => c.url === '/factory/jobs/pause'));
  assert.equal(controller.jobs.paused, true);
});

test('createFactoryApi sends owner Bearer token, raw bytes and JSON bodies, and surfaces server errors', async () => {
  const requests = [];
  const api = factory.createFactoryApi(
    async (url, opts) => {
      requests.push({ url, opts });
      return {
        ok: true,
        status: 200,
        json: async () => ({ settings: { enabled: true, idle_only: true, max_model_minutes_per_day: 30, context_tokens: 6000 } }),
      };
    },
    { getItem: key => (key === 'sail_owner_token' ? 'owner-secret' : '') }
  );

  const bytes = new Uint8Array([1, 2, 3]);
  await api('/factory/sources?file_name=a.txt&rights=restricted', { method: 'POST', body: bytes });
  assert.equal(requests[0].url, '/api/learn/factory/sources?file_name=a.txt&rights=restricted');
  assert.equal(requests[0].opts.headers.Authorization, 'Bearer owner-secret');
  assert.equal(requests[0].opts.headers['Content-Type'], undefined);
  assert.equal(requests[0].opts.body, bytes);

  const settingsPayload = { enabled: true, idle_only: true, max_model_minutes_per_day: 30, context_tokens: 6000 };
  await api('/factory/settings', { method: 'PUT', body: settingsPayload });
  assert.equal(requests[1].url, '/api/learn/factory/settings');
  assert.equal(requests[1].opts.headers['Content-Type'], 'application/json');
  assert.deepEqual(JSON.parse(requests[1].opts.body), settingsPayload);

  const missingTokenApi = factory.createFactoryApi(async () => ({ ok: true, json: async () => ({}) }), {
    getItem: () => '',
  });
  await assert.rejects(missingTokenApi('/factory/sources'), /owner token/);
});

test('course pause/resume, draft reject, graph edits and settings round-trip cleanly', async () => {
  const calls = [];
  const panel = createPanel();
  const controller = factory.mount(panel, async (url, opts) => {
    calls.push({ url, opts });
    if (url === '/factory/courses') {
      return {
        courses: [
          {
            course: { course_id: 'c1', title: 'Course 1', domain: 'logic', status: { state: 'generating' } },
            progress: { planned: 2, published: 1, needs_review: 0, failed: 0 },
          },
        ],
      };
    }
    if (url === '/factory/courses/c1/pause') {
      return { course: { course_id: 'c1', title: 'Course 1', domain: 'logic', status: { state: 'paused' } } };
    }
    if (url === '/factory/courses/c1/resume') {
      return { course: { course_id: 'c1', title: 'Course 1', domain: 'logic', status: { state: 'generating' } } };
    }
    if (url === '/factory/drafts/d1') {
      return {
        draft: {
          draft_id: 'd1',
          course_id: 'c1',
          lesson_plan_id: 'l1',
          stage: 'needs_review',
          output_hash: 'h1',
          output: { lesson: { id: 'l1', title: 'L1', minutes: 4, objective_ids: [], steps: [] } },
        },
        receipts: [],
        citations: [],
      };
    }
    if (url === '/factory/drafts/d1/reject') {
      return {
        draft: {
          draft_id: 'd1',
          course_id: 'c1',
          lesson_plan_id: 'l1',
          stage: 'failed',
          output_hash: 'h1',
        },
      };
    }
    if (url === '/factory/courses/c1/graph/edits') {
      return { graph: { course_id: 'c1', revision: 2, concepts: [] } };
    }
    if (url === '/factory/settings') {
      return { settings: { enabled: true, idle_only: true, max_model_minutes_per_day: 25, context_tokens: 6000 } };
    }
    return {};
  });

  await controller.loadCourses();
  await panel.findButton('Pause course').trigger('click');
  assert.equal(controller.courses[0].course.status.state, 'paused');
  await panel.findButton('Resume course').trigger('click');
  assert.equal(controller.courses[0].course.status.state, 'generating');

  await controller.loadDraft('d1');
  controller.rejectReason = 'Misleading distractor';
  await panel.findButton('Reject').trigger('click');
  assert(
    calls.some(
      c => c.url === '/factory/drafts/d1/reject' && c.opts.body.reason === 'Misleading distractor'
    )
  );

  await controller.editGraph('c1', 1, [{ kind: 'confirm_edge', from: 'a', to: 'b' }]);
  assert(calls.some(c => c.url === '/factory/courses/c1/graph/edits' && c.opts.body.base_revision === 1));

  await controller.loadSettings();
  assert.equal(controller.settings.max_model_minutes_per_day, 25);
});

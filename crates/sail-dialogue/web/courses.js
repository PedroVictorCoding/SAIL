// Original SAIL interactive-course client. Grading, evidence and gates belong to the server.
(function (root) {
  'use strict';

  const EVIDENCE = {
    untested: 'Untested',
    exposure: 'Exposure',
    assisted: 'Assisted',
    provisional: 'Provisional',
    due: 'Due for review',
    retained: 'Retained',
    weak: 'Needs practice',
  };

  function evidenceLabel(record) {
    if (record.not_assessed) return 'Not assessed';
    return EVIDENCE[record.state] || 'Evidence unavailable';
  }

  function gradeLabel(grade, step) {
    if (grade.result === 'unavailable') return 'Evaluation unavailable';
    if (step.kind === 'explain' && grade.result === 'correct') return 'Exposure recorded';
    if (grade.result === 'correct') return 'Correct';
    if (grade.result === 'incorrect') return 'Try again';
    return 'Result unavailable';
  }

  function gradeEvidence(grade) {
    if (grade.result !== 'correct') return 'No success certified';
    if (grade.assistance_state === 'hinted' || grade.assistance_state === 'solution_seen') {
      return 'Assisted work · no independent success certified';
    }
    if (grade.assistance_state !== 'independent') return 'Evidence not reported';
    if (grade.provisional_vs_retained === 'retained') return 'Retained evidence';
    if (grade.provisional_vs_retained === 'provisional') return 'Provisional evidence';
    return 'Exposure only · no independent success certified';
  }

  function answerFor(step, draft = {}) {
    if (step.kind === 'explain') return null;
    if (step.kind === 'choice' || step.kind === 'predict') {
      const choices = draft.choice || [];
      const ids = new Set((step.options || []).map(option => option.id));
      if (!choices.length || new Set(choices).size !== choices.length || choices.some(id => !ids.has(id)) || (!step.multi && choices.length !== 1)) {
        throw new Error('Choose an answer before checking it.');
      }
      return {choice: [...choices]};
    }
    if (step.kind === 'order') {
      const order = draft.order || [];
      const ids = (step.items || []).map(item => item.id);
      if (order.length !== ids.length || new Set(order).size !== ids.length || order.some(id => !ids.includes(id))) {
        throw new Error('Arrange each statement once before checking.');
      }
      return {order: [...order]};
    }
    if (step.kind === 'fill') {
      if (!String(draft.text || '').trim()) throw new Error('Enter an expression before checking it.');
      return {text: draft.text};
    }
    if (step.kind === 'rust_edit') {
      if (!String(draft.code || '').trim()) throw new Error('Enter the code to check in the sandbox.');
      return {code: draft.code};
    }
    throw new Error('This exercise type is not supported by this client.');
  }

  function moveItem(items, index, delta) {
    const result = [...items];
    const next = index + delta;
    if (index >= 0 && next >= 0 && index < result.length && next < result.length) {
      [result[index], result[next]] = [result[next], result[index]];
    }
    return result;
  }

  function dateLabel(value) {
    if (!value) return 'Not recorded';
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? 'Date unavailable' : date.toLocaleString();
  }

  class CourseApi {
    constructor(fetcher, storage) {
      this.fetcher = fetcher;
      this.storage = storage;
    }

    async request(path, method = 'GET', body = null) {
      let token = '';
      try {
        token = (this.storage.getItem('sail_owner_token') || '').trim();
      } catch (_) {
        // An unavailable browser store is reported through the authenticated API.
      }
      if (!token) throw new Error('Set the owner token in SAIL settings before opening courses.');
      const headers = {};
      if (token) headers.Authorization = `Bearer ${token}`;
      if (body !== null) headers['Content-Type'] = 'application/json';
      const abort = new AbortController();
      const timer = setTimeout(() => abort.abort(), 20000);
      try {
        const response = await this.fetcher(`/api/learn${path}`, {
          method,
          headers,
          cache: 'no-store',
          signal: abort.signal,
          body: body === null ? undefined : JSON.stringify(body),
        });
        const data = await response.json().catch(() => ({}));
        if (!response.ok) {
          const error = new Error(data.error || `Course request failed (${response.status}).`);
          error.status = response.status;
          throw error;
        }
        return data;
      } catch (error) {
        if (error.name === 'AbortError') {
          throw new Error('The request timed out. Retry to check the submitted attempt safely.');
        }
        throw error;
      } finally {
        clearTimeout(timer);
      }
    }
  }

  class CourseController {
    constructor(api, changed = () => {}, uuid = () => root.crypto.randomUUID()) {
      this.api = api;
      this.changed = changed;
      this.uuid = uuid;
      this.screen = 'catalog';
      this.courseId = null;
      this.courses = [];
      this.detail = null;
      this.next = null;
      this.player = null;
      this.queue = null;
      this.evidence = [];
      this.report = null;
      this.busy = false;
      this.error = '';
      this.retry = null;
      this.pending = null;
      this.loaded = false;
    }

    emit() {
      this.changed(this);
    }

    navigationBlocked() {
      if (!this.pending) return false;
      this.error = 'Confirm the submitted answer before leaving this activity.';
      this.retry = () => this.submit();
      this.emit();
      return true;
    }

    async run(action, retryable = true) {
      if (this.busy) return;
      this.busy = true;
      this.error = '';
      this.retry = null;
      this.emit();
      try {
        await action();
      } catch (error) {
        this.error = error.message || 'This course request did not finish.';
        this.retry = retryable ? () => this.run(action, retryable) : null;
      } finally {
        this.busy = false;
        this.emit();
      }
    }

    catalog() {
      if (this.navigationBlocked()) return Promise.resolve();
      return this.run(async () => {
        const data = await this.api.request('/courses');
        this.courses = data.courses || [];
        this.screen = 'catalog';
        this.loaded = true;
      });
    }

    course(id) {
      if (this.navigationBlocked()) return Promise.resolve();
      return this.run(async () => {
        const encoded = encodeURIComponent(id);
        const [detail, next] = await Promise.all([
          this.api.request(`/courses/${encoded}`),
          this.api.request(`/next?course=${encoded}`),
        ]);
        this.courseId = id;
        this.detail = detail;
        this.next = next;
        this.screen = 'path';
      });
    }

    setPlayer(mode, id, step, extra = {}) {
      this.player = {
        mode,
        id,
        step,
        grade: null,
        hints: [],
        hintsUsed: null,
        solution: null,
        confirmReveal: false,
        browsing: false,
        draft: {
          choice: [],
          order: (step?.items || []).map(item => item.id),
          text: '',
          code: step?.starter || '',
          confidence: '',
        },
        ...extra,
      };
      this.screen = 'player';
    }

    async loadLesson(id) {
      const data = await this.api.request(`/lessons/${encodeURIComponent(id)}`);
      this.courseId = data.lesson.course_id;
      const steps = data.lesson.steps || [];
      const current = steps.find(step => step.id === data.progress.current_step_id);
      this.setPlayer('lesson', id, current || steps[0] || null, {
        lesson: data.lesson,
        progress: data.progress,
        complete: data.progress.state === 'completed',
      });
    }

    lesson(id) {
      if (this.navigationBlocked()) return Promise.resolve();
      return this.run(() => this.loadLesson(id));
    }

    diagnostic() {
      if (this.navigationBlocked()) return Promise.resolve();
      return this.run(async () => {
        const data = await this.api.request('/diagnostics', 'POST', {course_id: this.courseId});
        if (data.done) {
          this.report = await this.api.request(`/diagnostics/${encodeURIComponent(data.id)}/report`);
          this.screen = 'report';
        } else {
          this.setPlayer('diagnostic', data.id, data.step);
        }
      });
    }

    diagnosticReport(id) {
      if (this.navigationBlocked()) return Promise.resolve();
      return this.run(async () => {
        this.report = await this.api.request(`/diagnostics/${encodeURIComponent(id)}/report`);
        this.screen = 'report';
      });
    }

    reviews() {
      if (this.navigationBlocked()) return Promise.resolve();
      return this.run(async () => {
        this.queue = await this.api.request(`/reviews/due?course=${encodeURIComponent(this.courseId)}`);
        this.screen = 'reviews';
      });
    }

    review(item) {
      if (this.busy || this.navigationBlocked()) return;
      this.setPlayer('review', item.id, item.step, {review: item});
      this.emit();
    }

    skills() {
      if (this.navigationBlocked()) return Promise.resolve();
      return this.run(async () => {
        const data = await this.api.request(`/skills/evidence?course=${encodeURIComponent(this.courseId)}`);
        this.evidence = data.objectives || [];
        this.screen = 'evidence';
      });
    }

    nextActivity() {
      if (this.next?.kind === 'diagnostic') return this.diagnostic();
      if (this.next?.kind === 'review') return this.reviews();
      if (this.next?.kind === 'lesson' && this.next.lesson_id) return this.lesson(this.next.lesson_id);
    }

    submit() {
      return this.run(async () => {
        const player = this.player;
        const step = player.step;
        if (!step || player.browsing) throw new Error('Resume the assessed step to submit an answer.');
        const path = player.mode === 'diagnostic'
          ? `/diagnostics/${encodeURIComponent(player.id)}/attempt`
          : player.mode === 'review'
            ? `/reviews/${encodeURIComponent(player.id)}/attempt`
            : `/lessons/${encodeURIComponent(player.id)}/attempt`;
        if (!this.pending) {
          const confidence = step.kind !== 'explain' && step.confidence ? player.draft.confidence : null;
          if (step.confidence && step.kind !== 'explain' && !['low', 'medium', 'high'].includes(confidence)) {
            throw new Error('Choose your confidence before checking the answer.');
          }
          const body = {
            attempt_id: this.uuid(),
            answer: answerFor(step, player.draft),
            confidence,
          };
          if (player.mode !== 'review') body.step_id = step.id;
          this.pending = {path, body, player};
        }
        if (this.pending.player !== player) {
          throw new Error('An earlier submission is awaiting confirmation. Resume or reload that activity.');
        }
        let grade;
        try {
          grade = await this.api.request(this.pending.path, 'POST', this.pending.body);
        } catch (error) {
          if (error.status >= 400 && error.status < 500 && error.status !== 429 && error.status !== 401) {
            this.pending = null;
          }
          throw error;
        }
        if (!grade || !['correct', 'incorrect', 'unavailable'].includes(grade.result)) {
          throw new Error('The server did not return a checked result. Retry the original submission.');
        }
        this.pending = null;
        player.grade = grade;
      });
    }

    continuePlayer() {
      const player = this.player;
      if (player.mode === 'review') return this.reviews();
      if (player.mode === 'lesson') return this.lesson(player.id);
      if (player.grade?.done) return this.diagnosticReport(player.id);
      if (player.grade?.next) {
        this.setPlayer('diagnostic', player.id, player.grade.next);
        this.emit();
      }
    }

    hint() {
      return this.run(async () => {
        const player = this.player;
        if (player.mode === 'diagnostic' || !player.step || player.browsing || this.pending) return;
        const prefix = player.mode === 'review' ? '/reviews' : '/lessons';
        const hint = await this.api.request(`${prefix}/${encodeURIComponent(player.id)}/hint`, 'POST', {step_id: player.step.id});
        player.hints.push(hint);
        player.hintsUsed = hint.hints_used;
        player.hintCount = hint.hint_count;
      }, false);
    }

    reveal() {
      return this.run(async () => {
        const player = this.player;
        if (player.mode !== 'lesson' || player.browsing || this.pending) return;
        const data = await this.api.request(`/lessons/${encodeURIComponent(player.id)}/reveal`, 'POST', {step_id: player.step.id});
        player.solution = data.solution || [];
        player.confirmReveal = false;
      }, false);
    }

    advance() {
      if (this.navigationBlocked()) return Promise.resolve();
      return this.run(async () => {
        const player = this.player;
        player.gate = await this.api.request(`/lessons/${encodeURIComponent(player.id)}/advance`, 'POST', {});
      });
    }

    browse(index) {
      if (this.pending || this.busy) return;
      const player = this.player;
      const steps = player.lesson?.steps || [];
      if (!steps[index]) return;
      this.setPlayer('lesson', player.id, steps[index], {
        lesson: player.lesson,
        progress: player.progress,
        browsing: true,
        browseIndex: index,
      });
      this.emit();
    }
  }

  function mount(panel, api) {
    const doc = panel.ownerDocument;
    const element = (tag, text, className) => {
      const node = doc.createElement(tag);
      if (text !== undefined && text !== null) node.textContent = String(text);
      if (className) node.className = className;
      return node;
    };
    const button = (text, action, className = '') => {
      const node = element('button', text, className);
      node.type = 'button';
      node.addEventListener('click', action);
      return node;
    };
    const paragraph = text => element('p', text, 'courses-muted');
    const heading = text => element('h2', text);
    const blocks = items => {
      const region = element('div', null, 'courses-blocks');
      for (const block of items || []) {
        const node = element(block.kind === 'code' ? 'pre' : block.kind === 'h' ? 'h3' : 'p', block.text);
        if (block.kind === 'note') node.className = 'courses-note';
        region.append(node);
      }
      return region;
    };
    const list = items => {
      const node = element('ul');
      for (const item of items || []) node.append(element('li', item));
      return node;
    };
    const controller = new CourseController(api, render);

    function navigation() {
      const nav = element('nav', null, 'courses-navigation');
      nav.setAttribute('aria-label', 'Course workspace');
      nav.append(button('All courses', () => controller.catalog()));
      if (controller.courseId) {
        nav.append(
          button('Course path', () => controller.course(controller.courseId)),
          button('Due reviews', () => controller.reviews()),
          button('Evidence', () => controller.skills()),
        );
      }
      if (controller.pending) {
        for (const item of nav.children) item.disabled = true;
      }
      return nav;
    }

    function catalog() {
      const view = element('section', null, 'courses-catalog');
      view.append(heading('Interactive courses'), paragraph('Small challenges, checked work, and evidence you can inspect. No reasoning model is needed for this course player.'));
      if (!controller.courses.length) {
        view.append(paragraph(controller.loaded ? 'No courses are available in this installation.' : 'The course catalog has not been loaded yet.'));
      }
      for (const course of controller.courses) {
        const card = element('article', null, 'courses-track');
        card.append(element('p', course.status === 'ready' ? 'Ready to study' : 'Planned · no lessons available', 'courses-label'), element('h3', course.title), paragraph(course.summary));
        if (course.status === 'ready') {
          card.append(paragraph(`${course.lesson_count} lessons · about ${course.minutes} minutes of content`));
          if (course.progress) {
            card.append(paragraph(`${course.progress.lessons_completed} lessons completed · ${course.progress.provisional} provisional · ${course.progress.retained} retained · ${course.progress.due} due`));
          }
          card.append(button('Open course', () => controller.course(course.id), 'courses-primary'));
        }
        view.append(card);
      }
      return view;
    }

    function path() {
      const detail = controller.detail;
      const view = element('section', null, 'courses-path');
      view.append(heading(detail.course.title), paragraph(detail.course.summary));
      const next = controller.next;
      if (next && next.kind !== 'done') {
        view.append(button(next.kind === 'review' ? 'Start due review' : next.kind === 'diagnostic' ? 'Find my starting point' : 'Continue lesson', () => controller.nextActivity(), 'courses-primary'));
      }
      view.append(list(next?.reasons));
      if (detail.diagnostic?.state === 'done') {
        view.append(button('Diagnostic report', () => controller.diagnosticReport(detail.diagnostic.id)));
      }
      view.append(paragraph('Study freely: every lesson stays readable. Certification is checked separately by the server.'));
      for (const chapter of detail.chapters || []) {
        const section = element('section', null, 'courses-chapter');
        section.append(element('h3', chapter.title));
        const lessons = element('ol', null, 'courses-lesson-path');
        for (const lesson of chapter.lessons || []) {
          const item = element('li');
          item.append(button(lesson.title, () => controller.lesson(lesson.id)), paragraph(`${lesson.minutes} min · ${String(lesson.state).replaceAll('_', ' ')} · ${lesson.certified ? 'Certified' : 'Not certified'}`));
          if (lesson.prerequisites?.length) item.append(paragraph(`Prerequisites: ${lesson.prerequisites.join(', ')}`));
          lessons.append(item);
        }
        section.append(lessons);
        view.append(section);
      }
      const objectives = element('details');
      objectives.append(element('summary', 'Course objectives'));
      objectives.append(list((detail.objectives || []).map(item => `${item.title}${item.prerequisites.length ? ` · prerequisites: ${item.prerequisites.join(', ')}` : ''}`)));
      view.append(objectives);
      return view;
    }

    function evidence(records, title) {
      const view = element('section');
      view.append(heading(title), paragraph('A lesson pass today is provisional. Retained evidence requires a delayed independent pass on an unseen variant. Assistance is recorded separately.'));
      if (!records.length) view.append(paragraph('No objective evidence is available yet.'));
      for (const record of records) {
        const item = element('article', null, 'courses-evidence');
        item.append(element('h3', record.title), element('p', evidenceLabel(record), 'courses-evidence-state'), paragraph(record.why));
        const facts = element('dl');
        for (const [label, value] of [
          ['Independent passes', record.independent_passes],
          ['Assisted passes', record.assisted_passes],
          ['Failed checks', record.failures],
          ['Last independent success', dateLabel(record.last_independent_success)],
          ['Last delayed success', dateLabel(record.last_delayed_success)],
          ['Next review', dateLabel(record.retention_due_at)],
          ['Evidence revision', record.revision],
        ]) {
          facts.append(element('dt', label), element('dd', value ?? 'Not recorded'));
        }
        item.append(facts);
        view.append(item);
      }
      return view;
    }

    function reviews() {
      const view = element('section');
      const queue = controller.queue;
      view.append(heading('Due reviews'));
      if (queue.catch_up) {
        view.append(paragraph(`${queue.catch_up.total_due} due in total · up to ${queue.catch_up.cap} in this session · ${queue.catch_up.sessions} catch-up sessions`));
      }
      if (!queue.items?.length) view.append(paragraph('No reviews are due. Return to your course path to study freely.'));
      for (const item of queue.items || []) {
        const row = element('article', null, 'courses-review-item');
        row.append(element('h3', item.objective_title), paragraph(item.reason), paragraph(`Due: ${dateLabel(item.due_at)}`), button('Start this review', () => controller.review(item)));
        view.append(row);
      }
      return view;
    }

    function gate(result) {
      const view = element('section', null, 'courses-gate');
      if (result.gate_decision === 'blocked_by_review') {
        view.append(element('h3', 'Review needed for certified advance'), list(result.gate_reasons), button('Open due reviews', () => controller.reviews()));
        view.append(paragraph('You can still read and explore all lessons.'));
      } else if (result.certified === true) {
        view.append(element('h3', 'Certified advance confirmed'), list(result.gate_reasons));
      } else {
        view.append(element('h3', 'Advance decision'), list(result.gate_reasons));
      }
      return view;
    }

    function player() {
      const p = controller.player;
      const step = p.step;
      const view = element('section', null, 'courses-player');
      view.append(element('p', p.mode === 'diagnostic' ? 'Cold diagnostic · no hints or reveal' : p.mode === 'review' ? 'Delayed review · assistance is recorded' : 'Lesson', 'courses-label'));
      view.append(heading(p.lesson?.title || p.review?.objective_title || 'Find your starting point'));
      if (!step) {
        view.append(paragraph('No active step was returned by the server.'), button('Return to course path', () => controller.course(controller.courseId)));
        return view;
      }
      if (p.mode === 'lesson') {
        const steps = p.lesson.steps || [];
        view.append(paragraph(`Step ${steps.findIndex(item => item.id === step.id) + 1} of ${steps.length} · ${p.lesson.minutes} min lesson`));
        const browse = element('div', null, 'courses-actions');
        if (p.browsing) {
          view.append(paragraph('Study freely: browsing does not record a pass or certify progress.'));
          browse.append(button('Previous material', () => controller.browse(p.browseIndex - 1)), button('Next material', () => controller.browse(p.browseIndex + 1)), button('Resume assessed step', () => controller.lesson(p.id)));
          browse.children[0].disabled = p.browseIndex === 0;
          browse.children[1].disabled = p.browseIndex >= steps.length - 1;
        } else {
          browse.append(button('Study freely', () => controller.browse(0)));
        }
        view.append(browse);
      }
      view.append(element('h3', step.title), blocks(step.body));
      if (step.code) view.append(element('pre', step.code, 'courses-code'));
      if (step.template) view.append(element('pre', step.template, 'courses-code'));
      if (p.mode === 'lesson' && p.complete && !p.browsing) {
        view.append(paragraph('The server reports this lesson completed. Completion alone does not certify retention.'), button('Check certified advance', () => controller.advance(), 'courses-primary'));
      } else if (!p.browsing && !(p.grade && p.mode !== 'lesson' && p.grade.result !== 'unavailable')) {
        view.append(exercise(p));
      }
      if (!p.browsing && !controller.pending && (!p.grade || p.grade.result === 'unavailable' || (p.mode === 'lesson' && p.grade.result === 'incorrect')) && !(p.mode === 'lesson' && p.complete)) {
        const actions = element('div', null, 'courses-actions');
        if (p.mode !== 'diagnostic' && step.hint_count > 0) {
          const count = p.hintCount ?? step.hint_count;
          const label = p.hintsUsed === null ? `Next hint (up to ${count})` : `Next hint (${p.hintsUsed}/${count})`;
          const hint = button(label, () => controller.hint());
          hint.disabled = p.hintsUsed !== null && p.hintsUsed >= count;
          actions.append(hint);
        }
        if (p.mode === 'lesson') {
          actions.append(button('Show solution', () => {
            p.confirmReveal = true;
            controller.emit();
          }));
        }
        view.append(actions);
      }
      for (const hint of p.hints) view.append(element('p', `Hint ${hint.level}: ${hint.text}`, 'courses-note'));
      if (p.confirmReveal && !p.solution) {
        const confirm = element('section', null, 'courses-note');
        confirm.append(paragraph('Revealing records assistance. A later answer will not count as independent success.'), button('Reveal solution (records assistance)', () => controller.reveal()), button('Keep trying', () => {
          p.confirmReveal = false;
          controller.emit();
        }));
        view.append(confirm);
      }
      if (p.solution) view.append(element('h3', 'Revealed solution · assistance recorded'), blocks(p.solution));
      if (p.grade) view.append(feedback(p));
      if (p.gate) view.append(gate(p.gate));
      return view;
    }

    function exercise(player) {
      const step = player.step;
      const draft = player.draft;
      const form = element('form', null, 'courses-answer');
      if (step.kind === 'choice' || step.kind === 'predict') {
        const choices = element('fieldset');
        choices.append(element('legend', step.multi ? 'Choose all that apply' : 'Choose one answer'));
        for (const option of step.options || []) {
          const label = element('label', null, 'courses-option');
          const input = element('input');
          input.type = step.multi ? 'checkbox' : 'radio';
          input.name = 'course-choice';
          input.value = option.id;
          input.checked = draft.choice.includes(option.id);
          input.addEventListener('change', () => {
            draft.choice = step.multi
              ? [...form.querySelectorAll('input[name="course-choice"]:checked')].map(node => node.value)
              : [option.id];
          });
          label.append(input, element(option.code ? 'code' : 'span', option.text));
          choices.append(label);
        }
        form.append(choices);
      } else if (step.kind === 'order') {
        form.append(paragraph('Arrange the statements using Move up and Move down.'));
        const order = element('ol', null, 'courses-order');
        draft.order.forEach((id, index) => {
          const item = step.items.find(option => option.id === id);
          const row = element('li');
          row.append(element(item.code ? 'code' : 'span', item.text));
          for (const [label, delta] of [['Move up', -1], ['Move down', 1]]) {
            const move = button(label, () => {
              draft.order = moveItem(draft.order, index, delta);
              controller.emit();
              panel.querySelectorAll('.courses-order li')[index + delta]?.querySelector('button')?.focus();
            });
            move.setAttribute('aria-label', `${label}: ${item.text}`);
            move.disabled = index + delta < 0 || index + delta >= draft.order.length;
            row.append(move);
          }
          order.append(row);
        });
        form.append(order);
      } else if (step.kind === 'fill') {
        const label = element('label', 'Your expression');
        const input = element('input');
        input.type = 'text';
        input.value = draft.text;
        input.placeholder = step.placeholder || '';
        input.addEventListener('input', () => { draft.text = input.value; });
        label.append(input);
        form.append(label);
      } else if (step.kind === 'rust_edit') {
        form.append(paragraph('Checking sends code only to the configured sandbox. If it is unavailable, your answer is not marked incorrect.'));
        if (step.limits) form.append(paragraph(`Sandbox limits: ${step.limits.wall_ms} ms · ${step.limits.mem_mb} MiB`));
        const label = element('label', 'Rust code');
        const editor = element('div', null, 'courses-editor');
        const numbers = element('pre', '', 'courses-line-numbers');
        numbers.setAttribute('aria-hidden', 'true');
        const input = element('textarea');
        input.value = draft.code;
        input.rows = 12;
        input.spellcheck = false;
        input.autocapitalize = 'off';
        input.setAttribute('aria-label', 'Rust code');
        const lineNumbers = () => {
          numbers.textContent = input.value.split('\n').map((_, index) => index + 1).join('\n');
        };
        input.addEventListener('input', () => {
          draft.code = input.value;
          lineNumbers();
        });
        input.addEventListener('scroll', () => { numbers.scrollTop = input.scrollTop; });
        lineNumbers();
        editor.append(numbers, input);
        label.append(editor);
        form.append(label);
      } else if (step.kind !== 'explain') {
        form.append(paragraph('Unsupported exercise. Reload after updating this client.'));
        return form;
      }
      if (step.confidence && step.kind !== 'explain') {
        const label = element('label', 'Confidence before checking');
        const select = element('select');
        for (const [value, text] of [['', 'Choose confidence'], ['low', 'Low'], ['medium', 'Medium'], ['high', 'High']]) {
          const option = element('option', text);
          option.value = value;
          select.append(option);
        }
        select.value = draft.confidence;
        select.addEventListener('change', () => { draft.confidence = select.value; });
        label.append(select);
        form.append(label);
      }
      const submit = element('button', controller.pending ? 'Retry submitted answer' : step.kind === 'explain' ? 'Continue' : step.kind === 'rust_edit' ? 'Check in sandbox' : 'Check answer', 'courses-primary');
      submit.type = 'submit';
      submit.disabled = player.grade?.result === 'correct';
      if (controller.pending) {
        form.querySelectorAll('input, textarea, select').forEach(node => { node.disabled = true; });
      }
      form.append(submit);
      form.addEventListener('submit', event => {
        event.preventDefault();
        controller.submit();
      });
      if (controller.pending) {
        form.append(paragraph('The previous submission is not confirmed. Retry sends its original answer and attempt ID, even if a field has since changed.'));
      }
      return form;
    }

    function feedback(player) {
      const grade = player.grade;
      const view = element('section', null, 'courses-feedback');
      view.setAttribute('aria-label', 'Checked result');
      view.append(element('h3', gradeLabel(grade, player.step)), paragraph(grade.feedback?.summary));
      if (grade.result === 'unavailable') view.append(paragraph(grade.feedback?.unavailable_reason || 'The evaluator could not run. This is not an incorrect answer.'));
      view.append(blocks(grade.feedback?.explanation));
      for (const diagnostic of grade.feedback?.diagnostics || []) {
        view.append(element('pre', `${diagnostic.level} · line ${diagnostic.line}, column ${diagnostic.column}\n${diagnostic.message}`));
      }
      if (grade.feedback?.stdout) view.append(element('pre', grade.feedback.stdout));
      const assistance = {independent: 'Independent', hinted: 'Hint used', solution_seen: 'Solution seen'};
      view.append(paragraph(`Assistance: ${assistance[grade.assistance_state] || 'Not reported'} · evidence: ${gradeEvidence(grade)} · validator: ${grade.validator_kind || 'Not reported'}`));
      if (grade.evidence_id) view.append(paragraph(`Evidence receipt: ${grade.evidence_id} · revision ${grade.revision}`));
      if (grade.review_due_at) view.append(paragraph(`Review due: ${dateLabel(grade.review_due_at)}`));
      if (grade.gate_decision === 'blocked_by_review') view.append(gate(grade));
      else view.append(list(grade.gate_reasons));
      if (player.mode === 'review') {
        view.append(button('Return to due queue', () => controller.continuePlayer()));
      } else if (player.mode === 'diagnostic' && (grade.next || grade.done)) {
        view.append(button(grade.done ? 'View diagnostic report' : 'Next diagnostic challenge', () => controller.continuePlayer(), 'courses-primary'));
      } else if (player.mode === 'lesson') {
        if (grade.result === 'correct' && (grade.next_step_id || grade.gate_decision === 'lesson_complete')) {
          view.append(button('Continue lesson', () => controller.continuePlayer(), 'courses-primary'));
        }
        if (grade.gate_decision === 'lesson_complete') {
          view.append(button('Check certified advance', () => controller.advance()));
        }
      }
      return view;
    }

    let lastScreen = '';
    let lastGrade = null;
    function render() {
      const focused = doc.activeElement;
      const focusKey = focused?.dataset.courseFocus;
      panel.replaceChildren();
      panel.setAttribute('aria-busy', String(controller.busy));
      const status = element('p', controller.busy ? 'Loading course data…' : '', 'courses-status');
      status.setAttribute('role', 'status');
      status.setAttribute('aria-live', 'polite');
      panel.append(status);
      if (controller.error) {
        const error = element('div', null, 'courses-error');
        error.setAttribute('role', 'alert');
        error.append(element('p', controller.error));
        if (controller.retry) error.append(button(controller.pending ? 'Retry original submission' : 'Retry request', controller.retry));
        panel.append(error);
      }
      const controls = element('fieldset', null, 'courses-controls');
      controls.disabled = controller.busy;
      controls.append(navigation());
      if (controller.screen === 'path') controls.append(path());
      else if (controller.screen === 'player') controls.append(player());
      else if (controller.screen === 'reviews') controls.append(reviews());
      else if (controller.screen === 'evidence') controls.append(evidence(controller.evidence, 'What SAIL thinks you know'));
      else if (controller.screen === 'report') {
        controls.append(heading('Diagnostic report'), paragraph(controller.report?.summary));
        controls.append(evidence(controller.report?.objectives || [], 'Objective evidence'));
        if (controller.report?.untested?.length) controls.append(paragraph(`Not assessed: ${controller.report.untested.join(', ')}`));
      } else controls.append(catalog());
      panel.append(controls);
      panel.querySelectorAll('button, input, textarea, select').forEach((node, index) => {
        node.dataset.courseFocus = String(index);
      });
      if (focusKey && panel.contains(focused) === false && lastScreen === controller.screen) {
        panel.querySelector(`[data-course-focus="${focusKey}"]`)?.focus({preventScroll: true});
      }
      if (!controller.busy && lastScreen !== controller.screen) {
        const title = panel.querySelector('h2');
        if (title) {
          title.tabIndex = -1;
          title.focus({preventScroll: true});
        }
      }
      const grade = controller.screen === 'player' ? controller.player?.grade : null;
      if (!controller.busy && grade && grade !== lastGrade) {
        const result = panel.querySelector('.courses-feedback h3');
        if (result) {
          result.tabIndex = -1;
          result.focus({preventScroll: true});
        }
      }
      lastGrade = grade;
      lastScreen = controller.screen;
    }

    render();
    return controller;
  }

  const exported = {CourseApi, CourseController, evidenceLabel, gradeLabel, gradeEvidence, answerFor, moveItem, dateLabel, mount};
  if (typeof module !== 'undefined') module.exports = exported;
  root.SailCourses = exported;
  if (typeof document !== 'undefined') {
    const start = () => {
      const panel = document.getElementById('coursesPanelMain');
      if (!panel) return;
      const controller = mount(panel, new CourseApi(root.fetch.bind(root), root.localStorage));
      const activate = () => {
        if (!panel.hidden && !controller.loaded && !controller.busy) controller.catalog();
      };
      new MutationObserver(activate).observe(panel, {attributes: true, attributeFilter: ['hidden']});
      document.getElementById('coursesTab')?.addEventListener('click', activate);
      activate();
    };
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start, {once: true});
    else start();
  }
})(typeof window !== 'undefined' ? window : globalThis);

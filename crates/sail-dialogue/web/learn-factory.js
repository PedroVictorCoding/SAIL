// Arena Learn Course Factory web inspector and controls (M6 / round 2).
// Untrusted source titles, fragments, claims and lesson steps are rendered via textContent only.
(function (root) {
  'use strict';

  const NEEDS_REVIEW_CONFIRM_TEXT =
    "You are approving a lesson the checks couldn't fully verify";

  const VALID_RIGHTS = ['restricted', 'owner_private', 'unknown', 'open'];

  function sourceStatusLabel(sourceOrStatus) {
    if (!sourceOrStatus) return 'unknown';
    const hasStatusField =
      typeof sourceOrStatus === 'object' &&
      sourceOrStatus !== null &&
      'status' in sourceOrStatus;
    const status = hasStatusField ? sourceOrStatus.status : sourceOrStatus;
    const pagesDone = hasStatusField
      ? sourceOrStatus.pages_done
      : sourceOrStatus.pages_done;
    const pagesTotal = hasStatusField
      ? sourceOrStatus.pages_total
      : sourceOrStatus.pages_total;

    const state =
      typeof status === 'string'
        ? status
        : status && typeof status === 'object'
          ? status.state
          : '';
    const reason =
      status && typeof status === 'object' && typeof status.reason === 'string'
        ? status.reason.trim()
        : '';

    if (state === 'indexed') return 'indexed';
    if (state === 'queued') return 'queued';
    if (state === 'removed') return 'removed';
    if (state === 'needs_review') return 'needs review';
    if (state === 'extracting') {
      if (
        typeof pagesTotal === 'number' &&
        Number.isFinite(pagesTotal) &&
        pagesTotal > 0
      ) {
        const done =
          typeof pagesDone === 'number' && Number.isFinite(pagesDone)
            ? pagesDone
            : 0;
        return `extracting ${done}/${pagesTotal} pages`;
      }
      return 'extracting';
    }
    if (state === 'failed') {
      return reason ? `failed: ${reason}` : 'failed';
    }
    return state ? String(state).replaceAll('_', ' ') : 'unknown';
  }

  function rightsBadgeLabel(rights) {
    if (!rights) return 'restricted';
    const kind = typeof rights === 'string' ? rights : rights.kind;
    if (kind === 'owner_private') return 'owner private';
    if (kind === 'open') {
      const license =
        typeof rights === 'object' && typeof rights.license === 'string'
          ? rights.license.trim()
          : '';
      return license ? `open (${license})` : 'open';
    }
    if (kind === 'unknown') return 'unknown';
    return 'restricted';
  }

  function sourceTypeLabel(sourceType) {
    const map = {
      pdf_text: 'PDF',
      pdf_scanned: 'Scanned PDF',
      epub: 'EPUB',
      markdown: 'Markdown',
      text: 'Text',
    };
    return map[sourceType] || String(sourceType || 'Document');
  }

  function buildImportRequest(input = {}) {
    const fileName = String(input.fileName ?? input.file_name ?? '').trim();
    if (!fileName) {
      throw new Error('Choose a source file before importing.');
    }
    const rights = String(input.rights || 'restricted').trim() || 'restricted';
    if (!VALID_RIGHTS.includes(rights)) {
      throw new Error(`Invalid source rights: ${rights}`);
    }
    const title = String(input.title ?? '').trim();
    const license = String(input.license ?? '').trim();
    const scope = String(input.scope ?? '').trim();
    const params = new URLSearchParams({
      file_name: fileName,
      title,
      rights,
      license,
      scope,
    });
    const body =
      input.bytes !== undefined
        ? input.bytes
        : input.body !== undefined
          ? input.body
          : new Uint8Array(0);
    return {
      path: `/factory/sources?${params.toString()}`,
      options: {
        method: 'POST',
        body,
      },
    };
  }

  function projectStatusLabel(status) {
    if (!status) return 'unknown';
    const state = typeof status === 'string' ? status : status.state;
    const reason =
      typeof status === 'object' && typeof status.reason === 'string'
        ? status.reason.trim()
        : '';
    if (state === 'awaiting_plan_approval') return 'awaiting plan approval';
    if (state === 'failed') return reason ? `failed: ${reason}` : 'failed';
    return state ? String(state).replaceAll('_', ' ') : 'unknown';
  }

  function courseProgressLabel(progress = {}) {
    const planned = Number(progress?.planned ?? 0);
    const published = Number(progress?.published ?? 0);
    const needsReview = Number(progress?.needs_review ?? 0);
    const failed = Number(progress?.failed ?? 0);
    const base = `${planned} planned · ${published} published · ${needsReview} needs review · ${failed} failed`;
    if (progress?.queued_jobs !== undefined && progress?.queued_jobs !== null) {
      return `${base} · ${Number(progress.queued_jobs)} queued`;
    }
    return base;
  }

  function edgeStatusLabel(edgeOrStatus) {
    const status =
      typeof edgeOrStatus === 'string' ? edgeOrStatus : edgeOrStatus?.status;
    if (status === 'supported') return 'supported';
    if (status === 'proposed') return 'proposed';
    if (status === 'owner_confirmed') return 'owner confirmed';
    if (status === 'owner_rejected') return 'owner rejected';
    return String(status || 'proposed').replaceAll('_', ' ');
  }

  function canPublishDraft(draftOrStage) {
    if (!draftOrStage) return false;
    const stage =
      typeof draftOrStage === 'string' ? draftOrStage : draftOrStage.stage;
    return stage === 'ready' || stage === 'needs_review';
  }

  function publishConfirmText(draftOrStage) {
    if (!draftOrStage) return '';
    const stage =
      typeof draftOrStage === 'string' ? draftOrStage : draftOrStage.stage;
    return stage === 'needs_review' ? NEEDS_REVIEW_CONFIRM_TEXT : '';
  }

  function receiptVerdictInfo(receipt) {
    const raw = receipt?.verdict;
    const verdict =
      typeof raw === 'string'
        ? raw
        : raw && typeof raw === 'object'
          ? raw.verdict
          : 'unknown';
    let reasons = [];
    if (raw && typeof raw === 'object') {
      if (Array.isArray(raw.reasons)) {
        reasons = raw.reasons.map(item => String(item));
      } else if (typeof raw.reason === 'string' && raw.reason.trim()) {
        reasons = [raw.reason.trim()];
      }
    }
    const label =
      verdict === 'needs_review'
        ? 'needs review'
        : String(verdict || 'unknown').replaceAll('_', ' ');
    return {
      check: String(receipt?.check || 'check').replaceAll('_', ' '),
      stepId: receipt?.step_id || null,
      verdict,
      label,
      reasons,
      version: receipt?.validator_version || '',
    };
  }

  function formatCitationLocation(location) {
    if (!location) return 'Location unavailable';
    if (typeof location === 'string') return location;
    if (location.kind === 'page') {
      return `Page ${location.page ?? location.physical ?? '?'}`;
    }
    if (location.kind === 'page_range') {
      return `Pages ${location.first ?? location.physical_first ?? '?'}–${location.last ?? location.physical_last ?? '?'}`;
    }
    if (location.kind === 'epub') {
      return location.anchor
        ? `${location.href}#${location.anchor}`
        : String(location.href || 'EPUB');
    }
    if (location.kind === 'lines') {
      return `Lines ${location.first ?? '?'}–${location.last ?? '?'}`;
    }
    return 'Location unavailable';
  }

  function formatQualityStatus(quality) {
    if (!quality) return 'clean';
    const state = typeof quality === 'string' ? quality : quality.state;
    const reason =
      typeof quality === 'object' && typeof quality.reason === 'string'
        ? quality.reason.trim()
        : '';
    if (state === 'clean') return 'clean';
    if (state === 'low_confidence') {
      return reason ? `low confidence (${reason})` : 'low confidence';
    }
    if (state === 'needs_validation') {
      return reason ? `needs validation (${reason})` : 'needs validation';
    }
    return String(state || 'clean').replaceAll('_', ' ');
  }

  function formatCitation(citation) {
    const path = Array.isArray(citation?.chapter_path) && citation.chapter_path.length
      ? citation.chapter_path.join(' › ')
      : 'Root';
    const loc = formatCitationLocation(citation?.location);
    const qual = formatQualityStatus(citation?.quality);
    return `${path} · ${loc} · ${qual}`;
  }

  function formatPriorityReason(reason) {
    if (!reason) return 'pipeline';
    const tag = typeof reason === 'string' ? reason : reason.reason;
    if (tag === 'owner_request') return 'owner request';
    if (tag === 'pipeline') return 'pipeline';
    if (tag === 'review_due_soon') {
      return reason.objective_id
        ? `review due soon (${reason.objective_id})`
        : 'review due soon';
    }
    if (tag === 'weak_skill') {
      return reason.objective_id
        ? `weak skill (${reason.objective_id})`
        : 'weak skill';
    }
    if (tag === 'ahead_of_progress') {
      return reason.lesson_plan_id
        ? `ahead of progress (${reason.lesson_plan_id})`
        : 'ahead of progress';
    }
    if (tag === 'missing_prerequisite') {
      return reason.objective_id
        ? `missing prerequisite (${reason.objective_id})`
        : 'missing prerequisite';
    }
    if (tag === 'active_course_assessment') return 'active course assessment';
    if (tag === 'remaining_chapter') {
      return reason.unit_index !== undefined
        ? `remaining chapter (unit ${reason.unit_index})`
        : 'remaining chapter';
    }
    if (tag === 'enrichment') return 'enrichment';
    return String(tag || 'queued').replaceAll('_', ' ');
  }

  function formatAuthoringJob(job) {
    if (!job) return 'Job';
    const kind = typeof job === 'string' ? job : job.kind;
    if (kind === 'extract_source') {
      return `Extract source ${job.source_id || ''} (rev ${job.revision ?? 1})`.trim();
    }
    if (kind === 'extract_pdf_pages') {
      return `Extract PDF pages from page ${job.first_page ?? 1} (${job.source_id || ''})`.trim();
    }
    if (kind === 'extract_concepts') {
      return `Extract concepts for ${job.course_id || 'course'}`;
    }
    if (kind === 'build_concept_graph') {
      return `Build concept graph for ${job.course_id || 'course'}`;
    }
    if (kind === 'build_curriculum') {
      return `Build curriculum for ${job.course_id || 'course'}`;
    }
    if (kind === 'generate_lesson') {
      return `Generate lesson ${job.lesson_plan_id || ''} (${job.course_id || ''})`.trim();
    }
    if (kind === 'generate_exercise_variant') {
      return `Generate variant for ${job.objective_id || ''} (${job.family || 'exercise'})`.trim();
    }
    if (kind === 'generate_review_assessment') {
      return `Generate review assessment for ${job.objective_id || ''}`.trim();
    }
    if (kind === 'validate_draft') {
      return `Validate draft ${job.draft_id || ''}`.trim();
    }
    if (kind === 'publish_course_revision') {
      return `Publish course revision for ${job.course_id || ''}`.trim();
    }
    return String(kind || 'job').replaceAll('_', ' ');
  }

  function createFactoryApi(fetcher, storage) {
    return async function factoryApi(path, options = {}) {
      let token = '';
      try {
        token = (storage?.getItem('sail_owner_token') || '').trim();
      } catch (_) {
        // Storage unavailable; reported below.
      }
      if (!token) {
        throw new Error('Set the owner token in SAIL settings before using the Course Factory.');
      }
      const method = options.method || 'GET';
      const headers = { Authorization: `Bearer ${token}` };
      let body = options.body;
      const isBinary =
        body instanceof Uint8Array ||
        (typeof ArrayBuffer !== 'undefined' && body instanceof ArrayBuffer) ||
        (typeof Blob !== 'undefined' && body instanceof Blob);
      if (body !== undefined && body !== null && !isBinary && typeof body !== 'string') {
        headers['Content-Type'] = 'application/json';
        body = JSON.stringify(body);
      }
      const cleanPath = path.startsWith('/') ? path : `/${path}`;
      const response = await fetcher(`/api/learn${cleanPath}`, {
        method,
        headers,
        cache: 'no-store',
        body: body === undefined || body === null ? undefined : body,
      });
      const data = await response.json().catch(() => ({}));
      if (!response.ok) {
        const error = new Error(data.error || `Factory request failed (${response.status}).`);
        error.status = response.status;
        throw error;
      }
      return data;
    };
  }

  function callApi(api, path, options = {}) {
    const method = options.method || 'GET';
    const body = options.body !== undefined ? options.body : undefined;
    if (typeof api === 'function') {
      return api(path, body !== undefined ? { method, body } : { method });
    }
    if (api && typeof api.request === 'function') {
      return api.request(path, method, body ?? null);
    }
    return Promise.reject(new Error('Course Factory API is not configured.'));
  }

  class FactoryController {
    constructor(api, changed = () => {}) {
      this.api = api;
      this.changed = changed;
      this.sources = [];
      this.courses = [];
      this.courseDetail = null;
      this.draftDetail = null;
      this.jobs = { paused: false, running: null, queue: [], recent: [] };
      this.settings = null;
      this.busy = false;
      this.error = '';
      this.notice = '';
      this.importForm = {
        fileName: '',
        title: '',
        rights: 'restricted',
        license: '',
        scope: 'learning:general',
        bytes: new Uint8Array(0),
      };
      this.rejectReason = '';
    }

    emit() {
      this.changed(this);
    }

    async run(action) {
      this.busy = true;
      this.error = '';
      this.notice = '';
      this.emit();
      try {
        return await action();
      } catch (err) {
        this.error =
          err && typeof err.message === 'string' && err.message
            ? err.message
            : String(err || 'Course Factory request failed.');
        return null;
      } finally {
        this.busy = false;
        this.emit();
      }
    }

    loadSources() {
      return this.run(async () => {
        const data = await callApi(this.api, '/factory/sources', { method: 'GET' });
        this.sources = Array.isArray(data?.sources) ? data.sources : [];
        return data;
      });
    }

    importSource(overrides = {}) {
      return this.run(async () => {
        const merged = { ...this.importForm, ...overrides };
        const req = buildImportRequest(merged);
        const data = await callApi(this.api, req.path, req.options);
        if (data?.source) {
          const existingIndex = this.sources.findIndex(
            item => item.source_id === data.source.source_id
          );
          if (existingIndex >= 0) {
            this.sources[existingIndex] = data.source;
          } else {
            this.sources.push(data.source);
          }
        }
        this.notice = data?.duplicate
          ? 'Source bytes already imported (idempotent).'
          : 'Source imported.';
        return data;
      });
    }

    deleteSource(sourceId) {
      return this.run(async () => {
        const encoded = encodeURIComponent(sourceId);
        const data = await callApi(this.api, `/factory/sources/${encoded}`, {
          method: 'DELETE',
        });
        this.sources = this.sources.filter(item => item.source_id !== sourceId);
        return data;
      });
    }

    loadCourses() {
      return this.run(async () => {
        const data = await callApi(this.api, '/factory/courses', { method: 'GET' });
        this.courses = Array.isArray(data?.courses) ? data.courses : [];
        return data;
      });
    }

    createCourse(payload) {
      return this.run(async () => {
        const data = await callApi(this.api, '/factory/courses', {
          method: 'POST',
          body: payload,
        });
        if (data?.course) {
          this.courses.push({
            course: data.course,
            progress: {
              planned: 0,
              published: 0,
              needs_review: 0,
              failed: 0,
              queued_jobs: 0,
            },
          });
        }
        return data;
      });
    }

    loadCourse(courseId) {
      return this.run(async () => {
        const encoded = encodeURIComponent(courseId);
        const data = await callApi(this.api, `/factory/courses/${encoded}`, {
          method: 'GET',
        });
        this.courseDetail = data;
        return data;
      });
    }

    approvePlan(courseId, revision) {
      return this.run(async () => {
        const encoded = encodeURIComponent(courseId);
        const data = await callApi(
          this.api,
          `/factory/courses/${encoded}/plan/approve`,
          {
            method: 'POST',
            body: { revision },
          }
        );
        if (this.courseDetail && data?.plan) {
          this.courseDetail.plan = data.plan;
        }
        return data;
      });
    }

    editGraph(courseId, baseRevision, edits) {
      return this.run(async () => {
        const encoded = encodeURIComponent(courseId);
        const data = await callApi(
          this.api,
          `/factory/courses/${encoded}/graph/edits`,
          {
            method: 'POST',
            body: { base_revision: baseRevision, edits },
          }
        );
        if (this.courseDetail && data?.graph) {
          this.courseDetail.graph = data.graph;
        }
        return data;
      });
    }

    loadSettings() {
      return this.run(async () => {
        const data = await callApi(this.api, '/factory/settings', {
          method: 'GET',
        });
        if (data?.settings) {
          this.settings = data.settings;
        }
        return data;
      });
    }

    saveSettings(settings) {
      return this.run(async () => {
        const data = await callApi(this.api, '/factory/settings', {
          method: 'PUT',
          body: settings,
        });
        if (data?.settings) {
          this.settings = data.settings;
        }
        this.notice = 'Factory settings saved.';
        return data;
      });
    }

    pauseCourse(courseId) {
      return this.run(async () => {
        const encoded = encodeURIComponent(courseId);
        const data = await callApi(this.api, `/factory/courses/${encoded}/pause`, {
          method: 'POST',
          body: {},
        });
        if (data?.course) {
          this.updateCourseInList(data.course);
        }
        return data;
      });
    }

    resumeCourse(courseId) {
      return this.run(async () => {
        const encoded = encodeURIComponent(courseId);
        const data = await callApi(this.api, `/factory/courses/${encoded}/resume`, {
          method: 'POST',
          body: {},
        });
        if (data?.course) {
          this.updateCourseInList(data.course);
        }
        return data;
      });
    }

    generateNextLesson(courseId) {
      return this.run(async () => {
        const encoded = encodeURIComponent(courseId);
        const data = await callApi(this.api, `/factory/courses/${encoded}/next`, {
          method: 'POST',
          body: {},
        });
        this.notice = 'Queued next lesson for generation.';
        return data;
      });
    }

    updateCourseInList(updated) {
      for (const entry of this.courses) {
        if (entry.course && entry.course.course_id === updated.course_id) {
          entry.course = updated;
        }
      }
      if (
        this.courseDetail?.course &&
        this.courseDetail.course.course_id === updated.course_id
      ) {
        this.courseDetail.course = updated;
      }
    }

    loadDraft(draftId) {
      return this.run(async () => {
        const encoded = encodeURIComponent(draftId);
        const data = await callApi(this.api, `/factory/drafts/${encoded}`, {
          method: 'GET',
        });
        this.draftDetail = data;
        return data;
      });
    }

    publishDraft(draftId) {
      const targetId = draftId || this.draftDetail?.draft?.draft_id;
      const stage = this.draftDetail?.draft?.stage;
      if (!canPublishDraft(stage)) {
        this.error = 'Only drafts in ready or needs_review stage can be published.';
        this.emit();
        return Promise.resolve(null);
      }
      return this.run(async () => {
        const encoded = encodeURIComponent(targetId);
        const data = await callApi(this.api, `/factory/drafts/${encoded}/publish`, {
          method: 'POST',
          body: {},
        });
        if (this.draftDetail?.draft && this.draftDetail.draft.draft_id === targetId) {
          this.draftDetail.draft.stage = 'published';
        }
        this.notice = `Published revision ${data?.revision ?? ''}`.trim();
        return data;
      });
    }

    rejectDraft(draftId, reason) {
      const targetId = draftId || this.draftDetail?.draft?.draft_id;
      const rejectReason = String(reason ?? this.rejectReason ?? '').trim() || 'Rejected by owner';
      return this.run(async () => {
        const encoded = encodeURIComponent(targetId);
        const data = await callApi(this.api, `/factory/drafts/${encoded}/reject`, {
          method: 'POST',
          body: { reason: rejectReason },
        });
        if (data?.draft && this.draftDetail) {
          this.draftDetail.draft = data.draft;
        }
        return data;
      });
    }

    loadJobs() {
      return this.run(async () => {
        const data = await callApi(this.api, '/factory/jobs', { method: 'GET' });
        if (data && typeof data === 'object') {
          this.jobs = {
            paused: Boolean(data.paused),
            running: data.running || null,
            queue: Array.isArray(data.queue) ? data.queue : [],
            recent: Array.isArray(data.recent) ? data.recent : [],
          };
        }
        return data;
      });
    }

    toggleJobsPause() {
      const action = this.jobs.paused ? 'resume' : 'pause';
      return this.run(async () => {
        const data = await callApi(this.api, `/factory/jobs/${action}`, {
          method: 'POST',
          body: {},
        });
        if (data && typeof data.paused === 'boolean') {
          this.jobs.paused = data.paused;
        } else {
          this.jobs.paused = action === 'pause';
        }
        return data;
      });
    }

    refreshAll() {
      return this.run(async () => {
        const [sourcesData, coursesData, jobsData] = await Promise.all([
          callApi(this.api, '/factory/sources', { method: 'GET' }),
          callApi(this.api, '/factory/courses', { method: 'GET' }),
          callApi(this.api, '/factory/jobs', { method: 'GET' }),
        ]);
        this.sources = Array.isArray(sourcesData?.sources) ? sourcesData.sources : [];
        this.courses = Array.isArray(coursesData?.courses) ? coursesData.courses : [];
        if (jobsData && typeof jobsData === 'object') {
          this.jobs = {
            paused: Boolean(jobsData.paused),
            running: jobsData.running || null,
            queue: Array.isArray(jobsData.queue) ? jobsData.queue : [],
            recent: Array.isArray(jobsData.recent) ? jobsData.recent : [],
          };
        }
      });
    }
  }

  function mount(panel, api) {
    const doc = panel.ownerDocument;
    if (panel.classList && !panel.classList.contains('factory-root')) {
      panel.classList.add('factory-root');
    } else if (!panel.className) {
      panel.className = 'factory-root';
    }

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

    const paragraph = (text, className = 'factory-muted') =>
      element('p', text, className);

    const pill = (text, className = 'factory-pill') =>
      element('span', text, className);

    const controller = new FactoryController(api, render);

    function renderLibrarySection() {
      const section = element('section', null, 'factory-section factory-library');
      section.setAttribute('aria-label', 'Source library');
      section.append(
        element('h2', 'Source Library'),
        paragraph('Books, PDFs and notes grounded with explicit source rights.')
      );

      const form = element('form', null, 'factory-form factory-import-form');
      form.append(element('h3', 'Import source'));

      const fileLabel = element('label', 'Source file');
      const fileInput = element('input');
      fileInput.type = 'file';
      fileInput.name = 'source-file';
      fileInput.setAttribute('aria-label', 'Source file');
      fileInput.addEventListener('change', async () => {
        const file = fileInput.files && fileInput.files[0];
        if (!file) return;
        controller.importForm.fileName = file.name || '';
        if (!controller.importForm.title) {
          controller.importForm.title = file.name.replace(/\.[^.]+$/, '');
        }
        if (typeof file.arrayBuffer === 'function') {
          const buf = await file.arrayBuffer();
          controller.importForm.bytes = new Uint8Array(buf);
        }
      });
      fileLabel.append(fileInput);

      const nameLabel = element('label', 'File name');
      const nameInput = element('input');
      nameInput.type = 'text';
      nameInput.name = 'file_name';
      nameInput.placeholder = 'e.g. rust-book.pdf';
      nameInput.value = controller.importForm.fileName;
      nameInput.setAttribute('aria-label', 'File name');
      nameInput.addEventListener('input', () => {
        controller.importForm.fileName = nameInput.value;
      });
      nameLabel.append(nameInput);

      const titleLabel = element('label', 'Title (optional)');
      const titleInput = element('input');
      titleInput.type = 'text';
      titleInput.name = 'title';
      titleInput.value = controller.importForm.title;
      titleInput.setAttribute('aria-label', 'Title');
      titleInput.addEventListener('input', () => {
        controller.importForm.title = titleInput.value;
      });
      titleLabel.append(titleInput);

      const rightsLabel = element('label', 'Source rights');
      const rightsSelect = element('select');
      rightsSelect.name = 'rights';
      rightsSelect.setAttribute('aria-label', 'Source rights');
      for (const [val, labelText] of [
        ['restricted', 'Restricted (grounding only, ≤25 word quotes)'],
        ['owner_private', 'Owner private'],
        ['open', 'Open license'],
        ['unknown', 'Unknown (treated as restricted)'],
      ]) {
        const opt = element('option', labelText);
        opt.value = val;
        if (val === (controller.importForm.rights || 'restricted')) {
          opt.selected = true;
        }
        rightsSelect.append(opt);
      }
      rightsSelect.value = controller.importForm.rights || 'restricted';
      rightsSelect.addEventListener('change', () => {
        controller.importForm.rights = rightsSelect.value;
      });
      rightsLabel.append(rightsSelect);

      const scopeLabel = element('label', 'Access scope');
      const scopeInput = element('input');
      scopeInput.type = 'text';
      scopeInput.name = 'scope';
      scopeInput.value = controller.importForm.scope;
      scopeInput.setAttribute('aria-label', 'Access scope');
      scopeInput.addEventListener('input', () => {
        controller.importForm.scope = scopeInput.value;
      });
      scopeLabel.append(scopeInput);

      const importBtn = element('button', 'Import source', 'factory-primary');
      importBtn.type = 'submit';
      form.append(fileLabel, nameLabel, titleLabel, rightsLabel, scopeLabel, importBtn);
      form.addEventListener('submit', event => {
        if (event && typeof event.preventDefault === 'function') {
          event.preventDefault();
        }
        controller.importSource();
      });
      section.append(form);

      if (!controller.sources.length) {
        section.append(paragraph('No sources imported yet.'));
      }
      for (const source of controller.sources) {
        const card = element('article', null, 'factory-card factory-source-card');
        card.append(element('h3', source.title || source.source_id));
        const badges = element('div', null, 'factory-badges');
        badges.append(
          pill(sourceTypeLabel(source.source_type), 'factory-pill factory-type-badge'),
          pill(rightsBadgeLabel(source.rights), 'factory-pill factory-rights-badge'),
          pill(sourceStatusLabel(source), 'factory-pill factory-status-badge')
        );
        card.append(badges);
        if (
          source.status &&
          typeof source.status === 'object' &&
          source.status.state === 'needs_review' &&
          source.status.reason
        ) {
          card.append(paragraph(`Review reason: ${source.status.reason}`));
        }
        if (source.access_scope) {
          card.append(paragraph(`Scope: ${source.access_scope}`));
        }
        const actions = element('div', null, 'factory-actions');
        actions.append(
          button('Remove source', () => controller.deleteSource(source.source_id))
        );
        card.append(actions);
        section.append(card);
      }
      return section;
    }

    function renderCoursesSection() {
      const section = element('section', null, 'factory-section factory-courses');
      section.setAttribute('aria-label', 'Course projects');
      section.append(
        element('h2', 'Course Projects'),
        paragraph('Background generation ahead of learner progress.')
      );
      if (!controller.courses.length) {
        section.append(paragraph('No course projects created yet.'));
      }
      for (const entry of controller.courses) {
        const course = entry.course || entry;
        const progress = entry.progress || {};
        const card = element('article', null, 'factory-card factory-course-card');
        card.append(element('h3', course.title || course.course_id));
        const badges = element('div', null, 'factory-badges');
        badges.append(
          pill(projectStatusLabel(course.status), 'factory-pill factory-project-status'),
          pill(String(course.domain || 'general'), 'factory-pill')
        );
        card.append(badges);
        card.append(paragraph(courseProgressLabel(progress), 'factory-progress'));
        const isPaused =
          course.status === 'paused' ||
          (course.status && course.status.state === 'paused');
        const actions = element('div', null, 'factory-actions');
        actions.append(
          button(
            isPaused ? 'Resume course' : 'Pause course',
            () =>
              isPaused
                ? controller.resumeCourse(course.course_id)
                : controller.pauseCourse(course.course_id)
          ),
          button(
            'Generate next lesson now',
            () => controller.generateNextLesson(course.course_id),
            'factory-primary'
          ),
          button('Review plan', () => controller.loadCourse(course.course_id))
        );
        card.append(actions);
        section.append(card);
      }
      return section;
    }

    function renderPlanReviewSection() {
      const detail = controller.courseDetail;
      const section = element('section', null, 'factory-section factory-plan-review');
      section.setAttribute('aria-label', 'Plan review');
      section.append(element('h2', 'Plan Review'));
      if (!detail) {
        section.append(paragraph('Select a course project to review its concept graph and curriculum plan.'));
        return section;
      }

      const plan = detail.plan;
      const graph = detail.graph;
      const lessonStatuses = new Map(
        (detail.lessons || []).map(item => [item.lesson_plan_id, item])
      );

      if (plan) {
        const planCard = element('article', null, 'factory-card');
        planCard.append(
          element('h3', plan.title || detail.course?.title || 'Curriculum Plan'),
          paragraph(`Revision ${plan.revision ?? 1} · Status: ${String(plan.status || 'proposed').replaceAll('_', ' ')}`)
        );
        const approveBtn = button(
          plan.status === 'owner_approved' ? 'Plan approved' : 'Approve plan',
          () => controller.approvePlan(detail.course?.course_id || plan.course_id, plan.revision),
          'factory-primary'
        );
        approveBtn.disabled = plan.status === 'owner_approved';
        planCard.append(approveBtn);

        const unitsList = element('div', null, 'factory-unit-list');
        for (const unit of plan.units || []) {
          const unitBox = element('section', null, 'factory-unit');
          unitBox.append(element('h4', unit.title || unit.id));
          const lessonsList = element('ol', null, 'factory-lesson-list');
          for (const lesson of unit.lessons || []) {
            const li = element('li');
            const runtime = lessonStatuses.get(lesson.id);
            const stageText = runtime?.stage
              ? String(runtime.stage).replaceAll('_', ' ')
              : 'planned';
            const objectivesText = (lesson.objective_ids || []).join(', ');
            li.append(
              element('strong', lesson.title || lesson.id),
              paragraph(
                `${lesson.minutes ?? 5} min · ${stageText}${objectivesText ? ` · Objectives: ${objectivesText}` : ''}`
              )
            );
            if (runtime?.draft_id) {
              li.append(
                button('Inspect draft', () => controller.loadDraft(runtime.draft_id))
              );
            }
            lessonsList.append(li);
          }
          unitBox.append(lessonsList);
          unitsList.append(unitBox);
        }
        planCard.append(unitsList);
        section.append(planCard);
      }

      if (graph && Array.isArray(graph.concepts)) {
        const graphCard = element('article', null, 'factory-card factory-graph-card');
        graphCard.append(element('h3', 'Concept Graph Edges'));
        const edgeList = element('ul', null, 'factory-edge-list');
        for (const concept of graph.concepts) {
          for (const edge of concept.prerequisites || []) {
            const li = element('li');
            const statusText = edgeStatusLabel(edge);
            li.append(
              element('span', `${concept.name || concept.id} requires ${edge.requires} `),
              pill(statusText, 'factory-pill factory-edge-badge')
            );
            if (edge.rationale) {
              li.append(paragraph(edge.rationale));
            }
            edgeList.append(li);
          }
        }
        graphCard.append(edgeList);
        section.append(graphCard);
      }

      return section;
    }

    function renderDraftInspectorSection() {
      const detail = controller.draftDetail;
      const section = element('section', null, 'factory-sheet factory-draft-inspector');
      section.setAttribute('aria-label', 'Draft inspector');
      section.append(element('h2', 'Draft Inspector'));
      if (!detail || !detail.draft) {
        section.append(paragraph('Select a lesson draft to inspect its steps, validation receipts and source citations.'));
        return section;
      }

      const draft = detail.draft;
      const lesson = draft.output?.lesson;
      section.append(
        element('h3', lesson?.title || draft.lesson_plan_id || draft.draft_id),
        pill(`Stage: ${String(draft.stage || 'queued').replaceAll('_', ' ')}`, 'factory-pill')
      );

      if (lesson && Array.isArray(lesson.steps)) {
        const preview = element('div', null, 'factory-lesson-preview');
        preview.append(element('h4', 'Lesson preview (plain text)'));
        for (const step of lesson.steps) {
          const stepBox = element('article', null, 'factory-card factory-step-preview');
          stepBox.append(
            element('h4', `${step.title || step.id} (${step.kind || 'step'})`)
          );
          for (const block of step.body || []) {
            stepBox.append(
              element(block.kind === 'code' ? 'pre' : 'p', block.text)
            );
          }
          if (step.code) stepBox.append(element('pre', step.code));
          if (step.starter) stepBox.append(element('pre', step.starter));
          if (Array.isArray(step.options) && step.options.length) {
            const opts = element('ul');
            for (const opt of step.options) {
              opts.append(element('li', opt.text));
            }
            stepBox.append(opts);
          }
          if (Array.isArray(step.items) && step.items.length) {
            const items = element('ol');
            for (const item of step.items) {
              items.append(element('li', item.text));
            }
            stepBox.append(items);
          }
          preview.append(stepBox);
        }
        section.append(preview);
      }

      const receiptsBox = element('div', null, 'factory-receipts');
      receiptsBox.append(element('h4', 'Validation receipts'));
      const receiptList = element('ul', null, 'factory-receipt-list');
      for (const receipt of detail.receipts || []) {
        const info = receiptVerdictInfo(receipt);
        const li = element('li');
        const checkMark = info.verdict === 'pass' ? '[PASS]' : `[${info.label.toUpperCase()}]`;
        li.append(
          element(
            'span',
            `${checkMark} ${info.check}${info.stepId ? ` (${info.stepId})` : ''} — ${info.label}`
          )
        );
        if (info.reasons.length) {
          li.append(paragraph(info.reasons.join('; ')));
        }
        receiptList.append(li);
      }
      receiptsBox.append(receiptList);
      section.append(receiptsBox);

      const citationsBox = element('div', null, 'factory-citations');
      citationsBox.append(element('h4', 'Source citations'));
      const citationList = element('ul', null, 'factory-citation-list');
      for (const citation of detail.citations || []) {
        citationList.append(element('li', formatCitation(citation)));
      }
      citationsBox.append(citationList);
      section.append(citationsBox);

      const confirmText = publishConfirmText(draft);
      if (confirmText) {
        section.append(element('p', confirmText, 'factory-publish-confirm'));
      }

      const actions = element('div', null, 'factory-actions');
      const publishBtn = button(
        'Publish',
        () => controller.publishDraft(draft.draft_id),
        'factory-primary'
      );
      publishBtn.disabled = !canPublishDraft(draft);

      const rejectInput = element('input');
      rejectInput.type = 'text';
      rejectInput.placeholder = 'Reason for rejection';
      rejectInput.value = controller.rejectReason;
      rejectInput.setAttribute('aria-label', 'Reason for rejection');
      rejectInput.addEventListener('input', () => {
        controller.rejectReason = rejectInput.value;
      });

      const rejectBtn = button('Reject', () =>
        controller.rejectDraft(draft.draft_id, controller.rejectReason)
      );

      actions.append(publishBtn, rejectInput, rejectBtn);
      section.append(actions);
      return section;
    }

    function renderQueueSection() {
      const section = element('section', null, 'factory-section factory-queue');
      section.setAttribute('aria-label', 'Generation queue');
      section.append(element('h2', 'Authoring Queue'));

      const pauseBtn = button(
        controller.jobs.paused ? 'Resume queue' : 'Pause queue',
        () => controller.toggleJobsPause()
      );
      section.append(pauseBtn);

      const runningBox = element('div', null, 'factory-card');
      runningBox.append(element('h3', 'Running job'));
      if (controller.jobs.running) {
        const running = controller.jobs.running;
        const reasons = (running.reasons || []).map(formatPriorityReason).join(', ');
        runningBox.append(
          paragraph(formatAuthoringJob(running.job)),
          paragraph(reasons ? `Reasons: ${reasons}` : 'Reasons: pipeline')
        );
      } else {
        runningBox.append(paragraph('No authoring job currently running.'));
      }
      section.append(runningBox);

      const nextBox = element('div', null, 'factory-card');
      nextBox.append(element('h3', 'Next queued jobs'));
      if (!controller.jobs.queue.length) {
        nextBox.append(paragraph('Queue is empty.'));
      } else {
        const list = element('ol', null, 'factory-queue-list');
        for (const item of controller.jobs.queue) {
          const reasons = (item.reasons || []).map(formatPriorityReason).join(', ');
          const li = element('li');
          li.append(
            element('span', formatAuthoringJob(item.job)),
            paragraph(reasons ? `Reasons: ${reasons}` : 'Reasons: pipeline')
          );
          list.append(li);
        }
        nextBox.append(list);
      }
      section.append(nextBox);

      const recentBox = element('div', null, 'factory-card');
      recentBox.append(element('h3', 'Recent failures & jobs'));
      if (!controller.jobs.recent.length) {
        recentBox.append(paragraph('No recent jobs recorded.'));
      } else {
        const list = element('ul', null, 'factory-queue-list');
        for (const rec of controller.jobs.recent) {
          const li = element('li');
          const detail = rec.error
            ? `${rec.state || 'failed'}: ${rec.error}`
            : String(rec.state || 'completed');
          li.append(element('span', `${formatAuthoringJob(rec.job)} — ${detail}`));
          list.append(li);
        }
        recentBox.append(list);
      }
      section.append(recentBox);
      return section;
    }

    function render() {
      panel.replaceChildren();
      panel.setAttribute('aria-busy', String(controller.busy));

      if (controller.error) {
        const errBox = element('div', null, 'factory-error');
        errBox.setAttribute('role', 'alert');
        errBox.append(element('p', controller.error));
        panel.append(errBox);
      }

      if (controller.notice) {
        const noteBox = element('p', controller.notice, 'factory-note');
        noteBox.setAttribute('role', 'status');
        panel.append(noteBox);
      }

      panel.append(
        renderLibrarySection(),
        renderCoursesSection(),
        renderPlanReviewSection(),
        renderDraftInspectorSection(),
        renderQueueSection()
      );
    }

    render();
    return controller;
  }

  const exported = {
    NEEDS_REVIEW_CONFIRM_TEXT,
    sourceStatusLabel,
    rightsBadgeLabel,
    sourceTypeLabel,
    buildImportRequest,
    projectStatusLabel,
    courseProgressLabel,
    edgeStatusLabel,
    canPublishDraft,
    publishConfirmText,
    receiptVerdictInfo,
    formatCitationLocation,
    formatQualityStatus,
    formatCitation,
    formatPriorityReason,
    formatAuthoringJob,
    createFactoryApi,
    FactoryController,
    mount,
  };

  if (typeof module !== 'undefined') module.exports = exported;
  root.SailLearnFactory = exported;

  if (typeof document !== 'undefined') {
    const start = () => {
      const el =
        document.getElementById('coursesFactoryMount') ||
        document.getElementById('learnFactoryPanel');
      if (!el) return;
      const api = createFactoryApi(root.fetch.bind(root), root.localStorage);
      mount(el, api);
    };
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', start, { once: true });
    } else {
      start();
    }
  }
})(typeof window !== 'undefined' ? window : globalThis);

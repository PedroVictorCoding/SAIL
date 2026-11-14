/* SAIL Agents › Schedule (round 21, Antigravity).
   Durable scheduled tasks with plain-language timing, quiet hours, and model proposals. */
'use strict';
(() => {
  const card = document.getElementById('scheduleCard');
  if (!card) return;
  const $ = s => card.querySelector(s);

  let busy = false;
  let loadedOnce = false;
  let previewTimer = null;
  let previewSeq = 0;
  let allJobs = [];
  let selectedJobId = null;
  let deleteArmedId = null;
  let skillsCache = [];
  let workflowsCache = [];

  const EXAMPLES = [
    {
      label: 'Every weekday at 9am: summarise my day',
      name: 'Morning summary',
      when: 'every weekday at 9am',
      prompt: 'Summarise my day and open tasks.',
    },
    {
      label: 'Daily at 8pm: check my open tasks',
      name: 'Evening task check',
      when: 'daily at 8pm',
      prompt: 'Check my open tasks and highlight anything urgent for tomorrow.',
    },
    {
      label: 'In 30m: remind me to stretch',
      name: 'Stretch break',
      when: 'in 30m',
      prompt: 'Remind me to take a short stretch break.',
    },
  ];

  const STATE_LABELS = {
    active: 'Active',
    paused: 'Paused',
    proposed: 'Proposed',
    done: 'Done',
  };

  const OUTCOME_LABELS = {
    running: 'Running',
    ok: 'Succeeded',
    failed: 'Failed',
    missed: 'Missed',
  };

  function el(tag, attrs = {}, ...children) {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
      else if (k === 'class') n.className = v;
      else if (k === 'text') n.textContent = v;
      else if (v !== false && v != null) n.setAttribute(k, v === true ? '' : v);
    }
    for (const c of children.flat()) {
      if (c != null && c !== false) n.append(c instanceof Node ? c : document.createTextNode(String(c)));
    }
    return n;
  }

  function status(text, error = false) {
    const node = $('#scheduleStatus');
    node.textContent = text;
    node.classList.toggle('schedule-error', error);
  }

  function showFormError(msg) {
    const node = $('#scheduleFormError');
    node.textContent = msg || '';
    node.hidden = !msg;
  }

  async function requestJson(url, options = {}) {
    const token = await getOrFetchOwnerToken();
    if (!token) throw Error('Owner authorization unavailable. Reconnect and try again.');
    const headers = { Authorization: `Bearer ${token}` };
    if (options.body !== undefined) headers['Content-Type'] = 'application/json';
    const r = await fetch(url, {
      method: options.method || (options.body !== undefined ? 'POST' : 'GET'),
      cache: 'no-store',
      headers,
      ...(options.body !== undefined ? { body: JSON.stringify(options.body) } : {}),
    });
    const data = await r.json().catch(() => ({}));
    if (!r.ok) throw Error(data.error || `Schedule unavailable (${r.status})`);
    return data;
  }

  const api = (path, options) => requestJson('/api/agents/schedule' + path, options);

  function browserTimeZone() {
    try {
      return Intl.DateTimeFormat().resolvedOptions().timeZone || '';
    } catch {
      return '';
    }
  }

  function formatLocalIso(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString();
  }

  function formatUnixTime(secs) {
    if (secs == null) return '';
    const d = new Date(secs * 1000);
    if (Number.isNaN(d.getTime())) return '';
    return d.toLocaleString();
  }

  function formatQuietUntil(iso) {
    if (!iso) return '07:00';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return '07:00';
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false });
  }

  function formatRelative(iso) {
    if (!iso) return '';
    const target = new Date(iso).getTime();
    if (Number.isNaN(target)) return '';
    const diffSecs = Math.round((target - Date.now()) / 1000);
    if (diffSecs <= 0) return 'due now';
    if (diffSecs < 60) return `in ${diffSecs}s`;
    const mins = Math.round(diffSecs / 60);
    if (mins < 60) return `in ${mins} min${mins === 1 ? '' : 's'}`;
    const hours = Math.floor(mins / 60);
    const remMins = mins % 60;
    if (hours < 24) return remMins ? `in ${hours}h ${remMins}m` : `in ${hours} hour${hours === 1 ? '' : 's'}`;
    const days = Math.round(hours / 24);
    return `in ${days} day${days === 1 ? '' : 's'}`;
  }

  function statePill(state) {
    return el('span', { class: `schedule-pill schedule-pill-${state}` },
      el('i', { 'aria-hidden': 'true' }),
      STATE_LABELS[state] || state);
  }

  function actionSummary(action) {
    if (!action) return '';
    if (action.type === 'prompt') return `Ask SAIL: ${action.text}`;
    if (action.type === 'skill') return `Use skill: ${action.name}${action.input ? ` (${action.input})` : ''}`;
    if (action.type === 'workflow') {
      const wf = workflowsCache.find(w => w.id === action.id);
      return `Run workflow: ${wf ? wf.name : action.id}`;
    }
    return '';
  }

  function renderStrip(st) {
    const strip = $('#scheduleStrip');
    if (st?.voice_active) {
      strip.textContent = "Paused while you're talking to SAIL";
      strip.hidden = false;
    } else if (st?.quiet_now) {
      strip.textContent = `Quiet hours now — results will wait until ${formatQuietUntil(st.quiet_until)}`;
      strip.hidden = false;
    } else {
      strip.textContent = '';
      strip.hidden = true;
    }
  }

  function renderProposals(proposed) {
    const wrap = $('#scheduleProposalsWrap');
    const list = $('#scheduleProposalList');
    list.replaceChildren();
    wrap.hidden = !proposed.length;
    for (const job of proposed) {
      const cardEl = el('article', { class: 'schedule-proposal', 'data-job': job.id, 'aria-label': `Proposed job ${job.name}` },
        el('header', { class: 'schedule-proposal-head' },
          el('h5', { text: job.name }),
          statePill(job.state)),
        el('p', { class: 'schedule-meta', text: job.describe || job.when_text }),
        el('p', { text: actionSummary(job.action) }),
        el('p', { class: 'schedule-meta' }, el('strong', { text: "SAIL's reason: " }), job.reason || '—'),
        el('div', { class: 'schedule-actions' },
          el('button', { type: 'button', class: 'tm-btn', onclick: () => actOnJob(job.id, 'approve', `Approved ${job.name}.`), text: 'Approve' }),
          el('button', { type: 'button', class: 'tm-btn', onclick: () => actOnJob(job.id, 'reject', `Rejected ${job.name}.`), text: 'Reject' })));
      list.append(cardEl);
    }
  }

  function renderJobs(jobs) {
    const list = $('#scheduleJobList');
    const empty = $('#scheduleEmpty');
    list.replaceChildren();
    empty.hidden = allJobs.length > 0;
    for (const job of jobs) {
      const isSelected = selectedJobId === job.id;
      const nextText = job.next_run
        ? `Next run: ${formatRelative(job.next_run)} (${formatLocalIso(job.next_run)})`
        : 'Next run: none scheduled';
      const last = job.last_run;
      const lastTime = last ? formatUnixTime(last.finished_at ?? last.started_at ?? last.slot) : '';
      const lastText = last
        ? `Last run: ${OUTCOME_LABELS[last.outcome] || last.outcome}${lastTime ? ` · ${lastTime}` : ''}`
        : 'Last run: never run yet';

      const jobEl = el('article', { class: `schedule-job${isSelected ? ' is-active' : ''}`, 'data-job': job.id },
        el('header', { class: 'schedule-job-head' },
          el('h4', { class: 'schedule-job-title' },
            el('button', {
              type: 'button',
              class: 'schedule-job-select',
              'aria-expanded': String(isSelected),
              onclick: () => openDetail(job.id),
              text: job.name,
            })),
          statePill(job.state)),
        el('p', { class: 'schedule-meta', text: job.describe || job.when_text }),
        el('p', { class: 'schedule-meta', text: nextText }),
        el('p', { class: 'schedule-meta', text: lastText }),
        job.state === 'paused' && job.failure_streak >= 3
          ? el('p', { class: 'schedule-warning', text: 'Paused after 3 failed runs' })
          : null);
      list.append(jobEl);
    }
  }

  function renderDetail(job, runs) {
    const detail = $('#scheduleDetail');
    if (!job) {
      detail.hidden = true;
      selectedJobId = null;
      deleteArmedId = null;
      return;
    }
    detail.hidden = false;
    $('#scheduleDetailName').textContent = job.name;
    $('#scheduleDetailDesc').textContent = job.describe || job.when_text;
    const deliverText = job.deliver === 'silent' ? 'Keep silently' : 'Show in SAIL';
    $('#scheduleDetailMeta').textContent = `${STATE_LABELS[job.state] || job.state} · ${deliverText} · ${actionSummary(job.action)}`;

    const actions = $('#scheduleDetailActions');
    actions.replaceChildren();
    if (job.state !== 'proposed') {
      actions.append(el('button', {
        type: 'button', class: 'tm-btn',
        onclick: () => actOnJob(job.id, 'run', `Started ${job.name}.`),
        text: 'Run now',
      }));
    }
    if (job.state === 'active') {
      actions.append(el('button', {
        type: 'button', class: 'tm-btn',
        onclick: () => actOnJob(job.id, 'pause', `Paused ${job.name}.`),
        text: 'Pause',
      }));
    } else if (job.state === 'paused') {
      actions.append(el('button', {
        type: 'button', class: 'tm-btn',
        onclick: () => actOnJob(job.id, 'resume', `Resumed ${job.name}.`),
        text: 'Resume',
      }));
    }

    if (deleteArmedId === job.id) {
      actions.append(
        el('button', {
          type: 'button', class: 'tm-btn',
          onclick: () => deleteJob(job.id, job.name),
          text: 'Confirm delete',
        }),
        el('button', {
          type: 'button', class: 'tm-btn',
          onclick: () => { deleteArmedId = null; renderDetail(job, runs); },
          text: 'Cancel',
        }));
    } else {
      actions.append(el('button', {
        type: 'button', class: 'tm-btn',
        onclick: () => { deleteArmedId = job.id; renderDetail(job, runs); },
        text: 'Delete',
      }));
    }

    const runList = $('#scheduleRunList');
    runList.replaceChildren();
    if (!runs.length) {
      runList.append(el('li', { class: 'schedule-meta', text: 'No runs recorded yet.' }));
      return;
    }
    for (const run of runs) {
      const ts = formatUnixTime(run.finished_at ?? run.started_at ?? run.slot);
      const held = run.outcome === 'ok' && !run.delivered_at;
      runList.append(el('li', { class: 'schedule-run-item' },
        el('div', { class: 'schedule-run-head' },
          el('strong', { text: OUTCOME_LABELS[run.outcome] || run.outcome }),
          el('span', { class: 'schedule-meta', text: ts })),
        run.summary ? el('p', { class: 'schedule-run-summary', text: run.summary }) : null,
        held ? el('span', { class: 'schedule-held-note', text: 'Held for quiet hours' }) : null));
    }
  }

  async function openDetail(jobId, focusHeading = true) {
    selectedJobId = jobId;
    deleteArmedId = null;
    renderJobs(allJobs.filter(j => j.state !== 'proposed'));
    const job = allJobs.find(j => j.id === jobId);
    if (!job) {
      renderDetail(null, []);
      return;
    }
    try {
      const data = await api(`/${encodeURIComponent(jobId)}/runs`);
      renderDetail(job, data.runs || []);
      if (focusHeading) $('#scheduleDetailName').focus();
    } catch (error) {
      status(error.message, true);
    }
  }

  async function actOnJob(id, action, successMessage) {
    status('Updating schedule…');
    try {
      await api(`/${encodeURIComponent(id)}/${action}`, { body: {} });
      await load();
      if (selectedJobId === id) await openDetail(id, false);
      status(successMessage);
    } catch (error) {
      status(error.message, true);
    }
  }

  async function deleteJob(id, name) {
    status(`Deleting ${name}…`);
    try {
      await api(`/${encodeURIComponent(id)}`, { method: 'DELETE' });
      if (selectedJobId === id) selectedJobId = null;
      deleteArmedId = null;
      renderDetail(null, []);
      await load();
      status(`Deleted ${name}.`);
    } catch (error) {
      status(error.message, true);
    }
  }

  function populateSelects() {
    const skillSelect = $('#scheduleSkillSelect');
    const prevSkill = skillSelect.value;
    skillSelect.replaceChildren();
    if (!skillsCache.length) {
      skillSelect.append(el('option', { value: '', text: 'No skills available' }));
    } else {
      for (const s of skillsCache) {
        skillSelect.append(el('option', { value: s.name, text: s.description ? `${s.name} — ${s.description}` : s.name }));
      }
      if (skillsCache.some(s => s.name === prevSkill)) skillSelect.value = prevSkill;
    }

    const wfSelect = $('#scheduleWorkflowSelect');
    const prevWf = wfSelect.value;
    wfSelect.replaceChildren();
    if (!workflowsCache.length) {
      wfSelect.append(el('option', { value: '', text: 'No saved workflows available' }));
    } else {
      for (const w of workflowsCache) {
        wfSelect.append(el('option', { value: w.id, text: w.name }));
      }
      if (workflowsCache.some(w => w.id === prevWf)) wfSelect.value = prevWf;
    }
  }

  async function loadChoices() {
    const [skillsRes, flowsRes] = await Promise.all([
      requestJson('/api/hermes/skills').catch(() => ({ skills: [] })),
      requestJson('/api/flow/workflows').catch(() => ({ workflows: [] })),
    ]);
    skillsCache = skillsRes.skills || [];
    workflowsCache = flowsRes.workflows || [];
    populateSelects();
  }

  async function load() {
    if (busy) return;
    busy = true;
    $('#scheduleRefresh').disabled = true;
    status(loadedOnce ? 'Refreshing…' : 'Loading schedule…');
    try {
      const [sched] = await Promise.all([api(''), loadChoices()]);
      if (!$('#scheduleTz').value.trim()) {
        $('#scheduleTz').value = browserTimeZone() || sched.status?.timezone || 'UTC';
      }
      renderStrip(sched.status);
      allJobs = sched.jobs || [];
      const proposed = allJobs.filter(j => j.state === 'proposed');
      const regular = allJobs.filter(j => j.state !== 'proposed');
      renderProposals(proposed);
      renderJobs(regular);
      if (selectedJobId) {
        const stillExists = allJobs.find(j => j.id === selectedJobId);
        if (stillExists) await openDetail(selectedJobId, false);
        else renderDetail(null, []);
      }
      const total = allJobs.length;
      status(total ? `${total} scheduled job${total === 1 ? '' : 's'}` : 'No scheduled jobs yet.');
      loadedOnce = true;
    } catch (error) {
      status(error.message, true);
    } finally {
      busy = false;
      $('#scheduleRefresh').disabled = false;
    }
  }

  async function runPreview() {
    const when = $('#scheduleWhen').value.trim();
    const tz = $('#scheduleTz').value.trim() || browserTimeZone() || 'UTC';
    const box = $('#schedulePreview');
    if (!when) {
      box.replaceChildren();
      box.classList.remove('is-error');
      box.hidden = true;
      return;
    }
    const seq = ++previewSeq;
    try {
      const data = await api('/preview', { body: { when, tz } });
      if (seq !== previewSeq) return;
      box.classList.remove('is-error');
      box.replaceChildren(
        el('p', {}, el('strong', { text: data.describe })),
        data.next?.length
          ? el('ul', {}, data.next.slice(0, 5).map(iso => el('li', { text: formatLocalIso(iso) })))
          : el('p', { class: 'schedule-meta', text: 'No upcoming runs in this window.' }));
      box.hidden = false;
    } catch (error) {
      if (seq !== previewSeq) return;
      box.classList.add('is-error');
      box.replaceChildren(el('p', { text: error.message }));
      box.hidden = false;
    }
  }

  function schedulePreviewDebounced() {
    clearTimeout(previewTimer);
    previewTimer = setTimeout(runPreview, 300);
  }

  function selectedActionKind() {
    return card.querySelector('input[name="scheduleActionKind"]:checked')?.value || 'prompt';
  }

  function syncActionPanels() {
    const kind = selectedActionKind();
    $('#scheduleActionPromptWrap').hidden = kind !== 'prompt';
    $('#scheduleActionSkillWrap').hidden = kind !== 'skill';
    $('#scheduleActionWorkflowWrap').hidden = kind !== 'workflow';
  }

  function buildActionPayload() {
    const kind = selectedActionKind();
    if (kind === 'prompt') {
      const text = $('#schedulePromptText').value.trim();
      if (!text) throw Error('Enter a prompt for SAIL.');
      return { type: 'prompt', text };
    }
    if (kind === 'skill') {
      const name = $('#scheduleSkillSelect').value;
      if (!name) throw Error('Choose a skill.');
      return { type: 'skill', name, input: $('#scheduleSkillInput').value.trim() };
    }
    const id = $('#scheduleWorkflowSelect').value;
    if (!id) throw Error('Choose a workflow.');
    return { type: 'workflow', id };
  }

  async function createJob(e) {
    e.preventDefault();
    showFormError('');
    const name = $('#scheduleName').value.trim();
    const when = $('#scheduleWhen').value.trim();
    const tz = $('#scheduleTz').value.trim() || browserTimeZone() || 'UTC';
    const deliver = $('#scheduleDeliver').value;
    if (!name) { showFormError('Enter a job name.'); return; }
    if (!when) { showFormError('Enter when the job should run.'); return; }
    let action;
    try {
      action = buildActionPayload();
    } catch (err) {
      showFormError(err.message);
      return;
    }

    const btn = $('#scheduleCreateBtn');
    btn.disabled = true;
    try {
      const res = await api('', { body: { name, when, tz, action, deliver } });
      $('#scheduleName').value = '';
      $('#scheduleWhen').value = '';
      $('#schedulePromptText').value = '';
      $('#scheduleSkillInput').value = '';
      $('#schedulePreview').hidden = true;
      await load();
      status(`Created ${res.job?.name || name}.`);
    } catch (error) {
      showFormError(error.message);
    } finally {
      btn.disabled = false;
    }
  }

  function initExamples() {
    const box = $('#scheduleExamples');
    for (const ex of EXAMPLES) {
      box.append(el('button', {
        type: 'button',
        class: 'tm-btn schedule-example-btn',
        onclick: () => {
          showFormError('');
          $('#scheduleName').value = ex.name;
          $('#scheduleWhen').value = ex.when;
          const promptRadio = card.querySelector('input[name="scheduleActionKind"][value="prompt"]');
          if (promptRadio) promptRadio.checked = true;
          syncActionPanels();
          $('#schedulePromptText').value = ex.prompt;
          runPreview();
          $('#scheduleName').focus();
        },
        text: ex.label,
      }));
    }
  }

  $('#scheduleTz').value = browserTimeZone() || 'UTC';
  $('#scheduleRefresh').onclick = load;
  $('#scheduleDetailClose').onclick = () => {
    renderDetail(null, []);
    renderJobs(allJobs.filter(j => j.state !== 'proposed'));
  };
  $('#scheduleWhen').addEventListener('input', schedulePreviewDebounced);
  $('#scheduleTz').addEventListener('input', schedulePreviewDebounced);
  for (const r of card.querySelectorAll('input[name="scheduleActionKind"]')) {
    r.addEventListener('change', syncActionPanels);
  }
  $('#scheduleForm').addEventListener('submit', createJob);
  initExamples();
  syncActionPanels();

  const shown = () => !!document.getElementById('subsystemContent')?.contains(card);
  for (const event of ['sail:page', 'sail:section']) {
    window.addEventListener(event, () => { if (shown()) load(); });
  }
  if (shown()) load();
})();

'use strict';

// Memory views for Arena's workspace. Every value shown here comes from the owner-only
// /api/memory API and is untrusted content: it is only ever inserted as text.
(() => {
  const STATUS = {
    verified: 'Verified', user_reported: 'You said', inferred: 'Inferred', unverified: 'Unverified',
    disputed: 'Disputed', superseded: 'Replaced', invalidated: 'Rejected',
  };
  const SOURCE = {
    tool_observation: 'Observed by a tool', external_artifact: 'From a file or document',
    user_statement: 'You said', model_inference: 'SAIL inferred', prior_memory: 'Restated from memory',
  };
  const KIND = {
    person: 'Person', project: 'Project', software: 'Software', device: 'Device', environment: 'Environment',
    place: 'Place', organization: 'Organization', concept: 'Idea', procedure: 'Skill', assistant: 'SAIL', other: 'Thing',
  };
  const WORK = {
    proposed: 'Proposed', approved: 'Approved', in_progress: 'In progress', implemented: 'Implemented',
    tested: 'Tested', deployed: 'Deployed', failed: 'Failed', superseded: 'Replaced',
  };
  const REVIEW = new Set(['inferred', 'unverified', 'disputed']);
  const OPS = {
    create: 'Added', assert: 'Learned', reinforce: 'Found more evidence for', set_status: 'Changed the status of',
    supersede: 'Replaced', delete: 'Forgot', merge: 'Merged into', separate: 'Separated', pin: 'Pinned',
    unpin: 'Unpinned', set_sensitivity: 'Changed the privacy of', put: 'Saved a version of', run: 'Recorded a run of',
    set_state: 'Updated', lift_tombstone: 'Allowed SAIL to learn again', alias: 'Renamed',
  };
  const FILTERS = {
    all: () => true,
    learned: op => ['create', 'assert', 'reinforce', 'put'].includes(op.op),
    review: op => ['set_status', 'set_state', 'run'].includes(op.op),
    superseded: op => op.op === 'supersede',
    forgotten: op => ['delete', 'lift_tombstone'].includes(op.op),
    merges: op => ['merge', 'separate'].includes(op.op),
    privacy: op => ['pin', 'unpin', 'set_sensitivity'].includes(op.op),
  };

  const $ = id => document.getElementById(id);
  const card = $('memoryCard');
  if (!card) return;
  const state = { loaded: false, entity: null, changes: [] };
  const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });
  const when = ms => (ms ? dateFormat.format(new Date(ms)) : '');
  const words = predicate => String(predicate || '').replace(/_/g, ' ');

  function el(tag, attrs = {}, ...children) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs)) {
      if (value === undefined || value === null || value === false) continue;
      if (key === 'class') node.className = value;
      else if (key === 'text') node.textContent = value;
      else if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
      else node.setAttribute(key, value === true ? '' : value);
    }
    for (const child of children.flat()) if (child !== null && child !== undefined && child !== false) node.append(child);
    return node;
  }
  const chip = (text, tone) => el('span', { class: `memory-chip${tone ? ` memory-chip-${tone}` : ''}`, text });
  const statusChip = status => chip(STATUS[status] || status, status);
  const button = (text, onclick, extra = {}) => el('button', { type: 'button', class: 'quiet-button', onclick, ...extra, text });
  const empty = text => el('li', { class: 'memory-empty', text });

  function setStatus(text, tone) {
    const node = $('memoryStatus');
    node.hidden = !text; node.textContent = text || '';
    node.dataset.tone = tone || '';
  }

  class MemoryError extends Error {
    constructor(message, status, body) { super(message); this.status = status; this.body = body; }
  }

  async function api(path, body) {
    const token = typeof getOrFetchOwnerToken === 'function' ? await getOrFetchOwnerToken() : '';
    let response;
    try {
      response = await fetch(`/api/memory/${path}`, {
        method: body ? 'POST' : 'GET', cache: 'no-store',
        headers: { Authorization: `Bearer ${token}`, ...(body ? { 'Content-Type': 'application/json' } : {}) },
        ...(body ? { body: JSON.stringify(body) } : {}),
      });
    } catch (_) {
      throw new MemoryError('SAIL is not reachable. Memory will load when the connection returns.', 0);
    }
    const data = await response.json().catch(() => ({}));
    if (response.ok) return data;
    const message = response.status === 401 ? 'Memory needs the owner token. Set it in Guardian preferences.'
      : response.status === 503 ? 'Owner sign-in is not configured on this SAIL server.'
      : data.error || `Memory request failed (${response.status}).`;
    throw new MemoryError(message, response.status, data);
  }

  function showTab(panel) {
    if (typeof switchCardTab === 'function') switchCardTab('memoryCard', panel);
  }

  // ---- overview ---------------------------------------------------------------------

  function entityLink(entity, extraText) {
    return el('li', {}, el('button', {
      type: 'button', class: 'memory-link', onclick: () => openEntity(entity.id),
      'aria-label': `Open ${entity.name}`,
    }, el('span', { class: 'memory-link-name', text: entity.name }),
    el('span', { class: 'memory-meta', text: extraText || KIND[entity.kind] || entity.kind }),
    entity.sensitivity !== 'normal' ? chip('Private', 'private') : null));
  }

  function claimValue(summary) {
    const claim = summary.claim;
    return claim.object.type === 'entity' ? (summary.object_name || 'something you cannot see') : claim.object.value;
  }

  function claimSummaryItem(summary, { review = false } = {}) {
    const claim = summary.claim;
    const actions = review ? el('div', { class: 'memory-actions' },
      button('Confirm', () => act(`claims/${claim.id}/confirm`, {}, 'Confirmed.')),
      button('Reject', () => rejectClaim(claim.id, `${summary.subject_name} · ${words(claim.predicate)}`)),
    ) : null;
    return el('li', { class: 'memory-claim-summary' },
      el('button', { type: 'button', class: 'memory-link', onclick: () => openEntity(claim.subject) },
        el('span', { class: 'memory-link-name', text: summary.subject_name }),
        el('span', { class: 'memory-meta', text: words(claim.predicate) })),
      el('p', { class: 'memory-value', text: claimValue(summary) }),
      el('div', { class: 'memory-chip-row' }, statusChip(claim.status),
        claim.basis ? chip(claim.basis === 'explicit' ? 'Stated' : claim.basis) : null),
      actions);
  }

  function fill(list, items, render, emptyText) {
    const node = $(list); node.replaceChildren();
    if (!items.length) node.append(empty(emptyText));
    for (const item of items) node.append(render(item));
  }

  async function loadOverview() {
    setStatus('Loading memory…');
    try {
      const data = await api('overview');
      const counts = $('memoryCounts'); counts.replaceChildren();
      for (const [label, value] of [['Things', data.counts.entities], ['Beliefs', data.counts.active_claims],
        ['Need review', data.counts.needs_review], ['Skills', data.counts.procedures], ['Project items', data.counts.project_items]]) {
        counts.append(el('div', {}, el('dt', { text: label }), el('dd', { text: String(value) })));
      }
      fill('memoryNeedsReview', data.needs_review, s => claimSummaryItem(s, { review: true }), 'Nothing is waiting for review.');
      fill('memoryProjects', data.projects, e => entityLink(e), 'No projects yet.');
      fill('memoryPreferences', data.preferences, s => claimSummaryItem(s), 'No preferences yet.');
      fill('memorySkills', data.skills, e => entityLink(e), 'No skills yet.');
      fill('memoryQuestions', data.questions, s => claimSummaryItem(s), 'No open questions.');
      fill('memoryRecent', data.recent, e => entityLink(e), 'SAIL has not learned anything yet. Teach it something below.');
      state.loaded = true;
      setStatus('');
    } catch (error) {
      setStatus(error.message, 'error');
    }
  }

  async function search(event) {
    event.preventDefault();
    const q = $('memorySearchInput').value.trim();
    const out = $('memorySearchResults'); out.replaceChildren();
    if (!q) return;
    out.append(el('p', { class: 'memory-hint', text: 'Searching…' }));
    try {
      const data = await api(`search?q=${encodeURIComponent(q)}`);
      const list = el('ul', { class: 'memory-list', 'aria-label': `Results for ${q}` });
      if (!data.entities.length) list.append(empty(`Nothing matches “${q}”.`));
      for (const entity of data.entities) list.append(entityLink(entity));
      out.replaceChildren(list);
    } catch (error) {
      out.replaceChildren(el('p', { class: 'memory-error', role: 'alert', text: error.message }));
    }
  }

  async function teach(event) {
    event.preventDefault();
    const errorNode = $('memoryTeachError'); errorNode.hidden = true;
    const body = {
      subject: $('memoryTeachSubject').value, kind: $('memoryTeachKind').value,
      predicate: $('memoryTeachPredicate').value, value: $('memoryTeachValue').value,
      category: $('memoryTeachCategory').value, scope: $('memoryTeachScope').value,
      private: $('memoryTeachPrivate').checked,
    };
    try {
      let result;
      try {
        result = await api('teach', body);
      } catch (error) {
        if (error.status !== 409 || !error.body?.candidates) throw error;
        const chosen = await chooseCandidate(body.subject, error.body.candidates);
        if (chosen === null) return;
        result = await api('teach', { ...body, subject_id: chosen });
      }
      for (const id of ['memoryTeachPredicate', 'memoryTeachValue']) $(id).value = '';
      setStatus('Remembered.');
      await loadOverview();
      await openEntity(result.entity_id);
    } catch (error) {
      errorNode.textContent = error.message; errorNode.hidden = false;
    }
  }

  // ---- dialog -------------------------------------------------------------------------

  // Resolves to the form values (or true) on confirm, null on cancel.
  function ask({ title, text, confirm = 'OK', fields = [], run }) {
    const dialog = $('memoryDialog');
    $('memoryDialogTitle').textContent = title;
    $('memoryDialogText').textContent = text || '';
    $('memoryDialogConfirm').textContent = confirm;
    const errorNode = $('memoryDialogError'); errorNode.hidden = true;
    const holder = $('memoryDialogFields'); holder.replaceChildren();
    const inputs = {};
    for (const field of fields) {
      const id = `memoryDialogField-${field.name}`;
      let input;
      if (field.options) {
        input = el('select', { class: 'memory-input', id });
        for (const [value, label] of field.options) input.append(el('option', { value, text: label }));
      } else {
        input = el('input', { class: 'memory-input', id, required: field.required, maxlength: field.max || 500, placeholder: field.placeholder || '' });
        input.value = field.value || '';
      }
      inputs[field.name] = input;
      holder.append(el('div', { class: 'memory-field' }, el('label', { for: id, text: field.label }), input));
    }
    return new Promise(resolve => {
      const form = $('memoryDialogForm');
      const finish = value => { form.onsubmit = null; $('memoryDialogCancel').onclick = null; dialog.onclose = null; if (dialog.open) dialog.close(); resolve(value); };
      form.onsubmit = async event => {
        event.preventDefault();
        const values = Object.fromEntries(Object.entries(inputs).map(([k, input]) => [k, input.value]));
        if (!run) return finish(fields.length ? values : true);
        $('memoryDialogConfirm').disabled = true;
        try { finish(await run(values)); } catch (error) {
          errorNode.textContent = error.message; errorNode.hidden = false;
        } finally { $('memoryDialogConfirm').disabled = false; }
      };
      $('memoryDialogCancel').onclick = () => finish(null);
      dialog.onclose = () => finish(null);
      dialog.showModal();
      (Object.values(inputs)[0] || $('memoryDialogConfirm')).focus();
    });
  }

  async function chooseCandidate(name, candidates) {
    const values = await ask({
      title: `Which “${name}”?`,
      text: 'SAIL knows several things with this name and keeps them separate. Choose the one you mean.',
      confirm: 'Use this one',
      fields: [{ name: 'id', label: 'Entity', options: candidates.map(c => [String(c.id), `${c.name} · ${KIND[c.kind] || c.kind} · #${c.id}`]) }],
    });
    return values ? Number(values.id) : null;
  }

  async function act(path, body, done) {
    setStatus('Saving…');
    try {
      await api(path, body);
      setStatus(done || 'Saved.');
      await refreshAll();
    } catch (error) {
      setStatus(error.message, 'error');
    }
  }

  function rejectClaim(id, label) {
    return ask({
      title: 'Reject this belief?', text: `SAIL will keep a record that “${label}” was rejected, and stop using it.`,
      confirm: 'Reject', fields: [{ name: 'reason', label: 'Why (optional)' }],
      run: values => api(`claims/${id}/invalidate`, { reason: values.reason }),
    }).then(result => { if (result) { setStatus('Rejected.'); refreshAll(); } });
  }

  // ---- entity detail -----------------------------------------------------------------

  function supportText(support) {
    const parts = [];
    if (support.direct_origins) parts.push(`${support.direct_origins} direct observation${support.direct_origins === 1 ? '' : 's'}`);
    if (support.user_origins) parts.push(`${support.user_origins} of your statements`);
    if (support.inferences) parts.push(`${support.inferences} inference${support.inferences === 1 ? '' : 's'}`);
    let text = parts.length ? `Backed by ${parts.join(', ')}.` : 'No supporting evidence.';
    if (support.refuting_origins) text += ` ${support.refuting_origins} source${support.refuting_origins === 1 ? '' : 's'} disagree.`;
    return text;
  }

  function describeOp(op, label) {
    const verb = OPS[op.op] || op.op.replace(/_/g, ' ');
    const actor = op.actor === 'user' ? 'You' : `SAIL (${op.actor.replace(/^subsystem:/, '')})`;
    let detail = '';
    if (op.detail?.from && op.detail?.to) detail = ` from ${STATUS[op.detail.from] || WORK[op.detail.from] || op.detail.from} to ${STATUS[op.detail.to] || WORK[op.detail.to] || op.detail.to}`;
    else if (op.detail?.to) detail = ` to ${op.detail.to}`;
    const target = label || (op.op === 'delete' ? `a forgotten ${op.target_kind.replace('_', ' ')}` : `${op.target_kind.replace('_', ' ')} #${op.target_id ?? ''}`);
    return `${actor} · ${verb} ${target}${detail}`;
  }

  function claimRow(item, entity) {
    const claim = item.claim;
    const value = claim.object.type === 'entity'
      ? el('button', { type: 'button', class: 'memory-inline-link', onclick: () => openEntity(claim.object.value), text: item.object_name || 'something you cannot see' })
      : el('span', { text: claim.object.value });
    const label = `${entity.name} · ${words(claim.predicate)}`;
    const active = !['superseded', 'invalidated'].includes(claim.status);
    const why = el('details', { class: 'memory-why' },
      el('summary', { text: 'Why SAIL believes this' }),
      el('p', { class: 'memory-hint', text: supportText(item.support) }),
      el('ul', { class: 'memory-evidence' }, item.evidence.length ? item.evidence.map(link => el('li', {},
        el('div', { class: 'memory-chip-row' }, chip(SOURCE[link.evidence.source_kind] || link.evidence.source_kind),
          link.stance === 'refutes' ? chip('Disagrees', 'disputed') : null,
          el('span', { class: 'memory-meta', text: when(link.evidence.observed_at) })),
        link.evidence.receipt ? el('q', { class: 'memory-receipt', text: link.evidence.receipt }) : null,
        link.evidence.source_ref ? el('span', { class: 'memory-meta', text: `Source: ${link.evidence.source_ref}` }) : null,
      )) : empty('Evidence you cannot see, or none recorded.')),
      claim.status_reason ? el('p', { class: 'memory-hint', text: `Reason: ${claim.status_reason}` }) : null,
      item.changes.length ? el('ol', { class: 'memory-history' }, item.changes.map(op => el('li', {},
        el('span', { class: 'memory-meta', text: when(op.at) }), ` ${describeOp(op, words(claim.predicate))}`,
        op.reason ? el('span', { class: 'memory-meta', text: ` — ${op.reason}` }) : null))) : null);
    const actions = active ? el('div', { class: 'memory-actions' },
      REVIEW.has(claim.status) ? button('Confirm', () => act(`claims/${claim.id}/confirm`, {}, 'Confirmed.')) : null,
      claim.object.type === 'value' ? button('Correct…', () => ask({
        title: 'Correct this belief', text: `SAIL will replace “${claim.object.value}” and keep the old value as history.`,
        confirm: 'Save correction',
        fields: [{ name: 'value', label: words(claim.predicate), value: claim.object.value, required: true, max: 2000 },
          { name: 'reason', label: 'Why (optional)' }],
        run: values => api(`claims/${claim.id}/correct`, values),
      }).then(r => { if (r) { setStatus('Corrected.'); refreshAll(); } })) : null,
      REVIEW.has(claim.status) || claim.status === 'user_reported' ? button('Reject…', () => rejectClaim(claim.id, label)) : null,
      button(claim.pinned ? 'Unpin' : 'Pin', () => act(`claims/${claim.id}/pin`, { value: !claim.pinned }, claim.pinned ? 'Unpinned.' : 'Pinned. SAIL will keep this.'),
        { 'aria-pressed': claim.pinned ? 'true' : 'false' }),
      button(claim.sensitivity === 'normal' ? 'Mark private' : 'Make visible', () => act(`claims/${claim.id}/private`, { value: claim.sensitivity === 'normal' }, 'Privacy updated.')),
      button('Forget…', () => ask({
        title: 'Forget this?', text: `SAIL will delete “${label}: ${claimValue(item)}” and its evidence, and will not relearn it unless you teach it again.`,
        confirm: 'Forget', fields: [{ name: 'reason', label: 'Why (optional)' }],
        run: values => api(`claims/${claim.id}/forget`, values),
      }).then(r => { if (r) { setStatus('Forgotten.'); refreshAll(); } })),
    ) : null;
    return el('li', { class: `memory-claim${active ? '' : ' memory-claim-past'}` },
      el('div', { class: 'memory-claim-line' }, el('span', { class: 'memory-predicate', text: words(claim.predicate) }), value),
      el('div', { class: 'memory-chip-row' }, statusChip(claim.status),
        claim.category !== 'fact' ? chip(claim.category === 'preference' ? `Preference${claim.basis ? ` · ${claim.basis === 'explicit' ? 'stated' : claim.basis}` : ''}` : claim.category) : null,
        claim.pinned ? chip('Pinned', 'pinned') : null,
        claim.sensitivity !== 'normal' ? chip('Private', 'private') : null,
        el('span', { class: 'memory-meta', text: `Learned ${when(claim.observed_at)}` }),
        claim.valid_until ? el('span', { class: 'memory-meta', text: `Until ${when(claim.valid_until)}` }) : null,
        claim.last_verified_at ? el('span', { class: 'memory-meta', text: `Checked ${when(claim.last_verified_at)}` }) : null),
      why, actions);
  }

  function renderEntity(data) {
    const { entity } = data;
    const root = $('memoryEntity'); root.replaceChildren();
    const merged = entity.merged_into !== null && entity.merged_into !== undefined;
    root.append(el('header', { class: 'memory-entity-head' },
      el('h4', { class: 'memory-entity-name', id: 'memoryEntityName', tabindex: '-1', text: entity.name }),
      el('div', { class: 'memory-chip-row' }, chip(KIND[entity.kind] || entity.kind), chip(`Area: ${entity.scope}`),
        entity.sensitivity !== 'normal' ? chip('Private', 'private') : null,
        el('span', { class: 'memory-meta', text: `Updated ${when(entity.updated_at)}` })),
      entity.aliases.length ? el('p', { class: 'memory-hint', text: `Also known as ${entity.aliases.join(', ')}` }) : null));
    if (merged) {
      root.append(el('p', { class: 'memory-notice' }, 'Merged into ',
        el('button', { type: 'button', class: 'memory-inline-link', onclick: () => openEntity(data.canonical?.id ?? entity.merged_into), text: data.canonical?.name || `#${entity.merged_into}` }),
        '. ', button('Separate again', () => act(`entities/${entity.id}/separate`, {}, 'Separated.'))));
      return;
    }
    root.append(el('div', { class: 'memory-actions' },
      button('Show in graph', () => { showTab('memoryPanelGraph'); loadGraph(entity.id); }),
      button(entity.sensitivity === 'normal' ? 'Mark private' : 'Make visible', () => act(`entities/${entity.id}/private`, { value: entity.sensitivity === 'normal' }, 'Privacy updated.')),
      button('Merge into…', () => mergeEntity(entity)),
      button('Forget everything about this…', () => ask({
        title: `Forget ${entity.name}?`, text: 'SAIL will delete this, everything it believes about it, and its connections. It will not relearn those beliefs unless you teach them again.',
        confirm: 'Forget', fields: [{ name: 'reason', label: 'Why (optional)' }],
        run: values => api(`entities/${entity.id}/forget`, values),
      }).then(r => { if (r) { state.entity = null; setStatus('Forgotten.'); $('memoryEntity').replaceChildren(el('p', { class: 'memory-empty', text: `${entity.name} was forgotten.` })); refreshAll(); } }))));

    const active = data.claims.filter(c => !['superseded', 'invalidated'].includes(c.claim.status));
    const past = data.claims.filter(c => ['superseded', 'invalidated'].includes(c.claim.status));
    const byPredicate = new Map();
    for (const item of active.filter(c => c.claim.object.type === 'value')) {
      const list = byPredicate.get(item.claim.predicate) || []; list.push(item); byPredicate.set(item.claim.predicate, list);
    }
    const conflicts = [...byPredicate.values()].filter(list => list.length > 1).flat()
      .concat(active.filter(c => c.claim.status === 'disputed' && (byPredicate.get(c.claim.predicate) || []).length <= 1));
    if (conflicts.length) {
      root.append(el('section', { class: 'memory-section memory-conflicts', 'aria-labelledby': 'memoryConflictsTitle' },
        el('h4', { id: 'memoryConflictsTitle', text: 'Conflicting or disputed' }),
        el('p', { class: 'memory-hint', text: 'SAIL holds more than one answer here, or has evidence against it. Correct, confirm, or reject to settle it.' }),
        el('ul', { class: 'memory-list' }, conflicts.map(item => claimRow(item, entity)))));
    }
    const settled = active.filter(item => !conflicts.includes(item));
    root.append(el('section', { class: 'memory-section', 'aria-labelledby': 'memoryBeliefsTitle' },
      el('h4', { id: 'memoryBeliefsTitle', text: 'What SAIL believes' }),
      el('ul', { class: 'memory-list' }, settled.length ? settled.map(item => claimRow(item, entity)) : empty('No current beliefs.'))));

    if (data.merged.length) {
      root.append(el('section', { class: 'memory-section', 'aria-labelledby': 'memoryMergedTitle' },
        el('h4', { id: 'memoryMergedTitle', text: 'Merged into this' }),
        el('ul', { class: 'memory-list' }, data.merged.map(stub => el('li', { class: 'memory-row' },
          el('span', { text: stub.name }), button(`Separate ${stub.name}`, () => act(`entities/${stub.id}/separate`, {}, 'Separated.')))))));
    }
    const current = data.edges.filter(e => !['superseded', 'invalidated'].includes(e.status));
    root.append(el('section', { class: 'memory-section', 'aria-labelledby': 'memoryRelatedTitle' },
      el('h4', { id: 'memoryRelatedTitle', text: 'Related' }),
      el('ul', { class: 'memory-list' }, current.length ? current.map(edge => edgeItem(edge, entity)) : empty('No connections yet.'))));
    if (data.project_items.length) {
      root.append(el('section', { class: 'memory-section', 'aria-labelledby': 'memoryProjectTitle' },
        el('h4', { id: 'memoryProjectTitle', text: 'Project state' }),
        el('ul', { class: 'memory-list' }, data.project_items.map(item => el('li', { class: 'memory-claim' },
          el('div', { class: 'memory-claim-line' }, el('span', { class: 'memory-predicate', text: item.kind }), el('span', { text: item.title })),
          el('div', { class: 'memory-chip-row' }, chip(WORK[item.state] || item.state, item.state),
            item.state_evidence ? el('span', { class: 'memory-meta', text: 'Backed by recorded evidence' }) : null,
            el('span', { class: 'memory-meta', text: `Updated ${when(item.updated_at)}` })),
          item.detail ? el('p', { class: 'memory-hint', text: item.detail }) : null)))));
    }
    for (const { procedure, runs } of data.procedures.slice(0, 1)) {
      const spec = procedure.spec;
      const list = (title, items) => items.length ? [el('h5', { text: title }), el('ul', { class: 'memory-plain' }, items.map(text => el('li', { text })))] : [];
      root.append(el('section', { class: 'memory-section', 'aria-labelledby': 'memoryProcedureTitle' },
        el('h4', { id: 'memoryProcedureTitle', text: `Skill · version ${procedure.version}` }),
        el('div', { class: 'memory-chip-row' }, chip(procedure.status), el('span', { class: 'memory-meta', text: `${runs.length} recorded run${runs.length === 1 ? '' : 's'}` })),
        el('p', { text: spec.summary }),
        el('p', { class: 'memory-hint', text: 'Remembering a skill does not let SAIL run it. Guardian still asks before each step that needs permission.' }),
        list('Before starting', spec.preconditions), list('Steps', spec.steps.map(s => s.description)),
        list('Afterwards', spec.postconditions), list('Needs permission for', spec.required_capabilities)));
    }
    if (past.length) {
      root.append(el('details', { class: 'memory-section memory-past' },
        el('summary', { text: `Past beliefs (${past.length})` }),
        el('ul', { class: 'memory-list' }, past.map(item => claimRow(item, entity)))));
    }
  }

  function edgeItem(edge, entity) {
    const relation = words(edge.relation);
    const text = edge.outgoing ? `${entity.name} ${relation} ${edge.other.name}` : `${edge.other.name} ${relation} ${entity.name}`;
    return el('li', {}, el('button', { type: 'button', class: 'memory-link', onclick: () => openEntity(edge.other.id), 'aria-label': `${text}. Open ${edge.other.name}` },
      el('span', { class: 'memory-link-name', text: edge.other.name }),
      el('span', { class: 'memory-meta', text: edge.outgoing ? relation : `${relation} (from ${edge.other.name})` }),
      ['superseded', 'invalidated'].includes(edge.status) ? chip('Past', 'superseded') : statusChip(edge.status)));
  }

  async function mergeEntity(entity) {
    await ask({
      title: `Merge ${entity.name} into…`,
      text: 'Only merge when both are the same thing. Everything SAIL believes about it moves to the other one; you can separate them again later.',
      confirm: 'Find and merge',
      fields: [{ name: 'target', label: 'Name of the one to keep', required: true, max: 200 }, { name: 'reason', label: 'Why (optional)' }],
      run: async values => {
        const found = (await api(`search?q=${encodeURIComponent(values.target)}`)).entities
          .filter(e => e.id !== entity.id && e.kind === entity.kind);
        if (!found.length) throw new MemoryError(`No other ${(KIND[entity.kind] || entity.kind).toLowerCase()} matches “${values.target}”.`);
        let into = found[0].id;
        if (found.length > 1) {
          // Close this dialog first; the chooser reuses it.
          setTimeout(async () => {
            const chosen = await chooseCandidate(values.target, found);
            if (chosen !== null) act(`entities/${entity.id}/merge`, { into: chosen, reason: values.reason }, 'Merged.').then(() => openEntity(chosen));
          });
          return true;
        }
        await api(`entities/${entity.id}/merge`, { into, reason: values.reason });
        setStatus('Merged.'); await refreshAll(); await openEntity(into);
        return true;
      },
    });
  }

  async function openEntity(id) {
    state.entity = id;
    showTab('memoryPanelEntity');
    $('memoryEntity').replaceChildren(el('p', { class: 'memory-hint', text: 'Loading…' }));
    try {
      const data = await api(`entities/${id}`);
      if (state.entity !== id) return;
      renderEntity(data);
      $('memoryEntityName')?.focus({ preventScroll: false });
    } catch (error) {
      $('memoryEntity').replaceChildren(el('p', { class: 'memory-error', role: 'alert', text: error.message }));
    }
  }

  // ---- graph --------------------------------------------------------------------------

  const SVG = 'http://www.w3.org/2000/svg';
  const svg = (tag, attrs = {}, text) => {
    const node = document.createElementNS(SVG, tag);
    for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const clip = (text, n) => (text.length > n ? `${text.slice(0, n - 1)}…` : text);
  const MAX_NODES = 24;

  async function loadGraph(id = state.entity) {
    const holder = $('memoryGraph'); const list = $('memoryGraphList');
    if (!id) return;
    holder.replaceChildren(el('p', { class: 'memory-hint', text: 'Loading…' }));
    try {
      const data = await api(`graph/${id}?history=${$('memoryGraphHistory').checked}`);
      const { center, edges } = data;
      state.entity = center.id;
      // One node per neighbour; several relations to the same neighbour share it.
      const neighbours = new Map();
      for (const edge of edges) {
        const entry = neighbours.get(edge.other.id) || { entity: edge.other, edges: [] };
        entry.edges.push(edge); neighbours.set(edge.other.id, entry);
      }
      const shown = [...neighbours.values()].slice(0, MAX_NODES);
      const size = 520, mid = size / 2, radius = shown.length > 12 ? 200 : 170;
      const graph = svg('svg', { viewBox: `0 0 ${size} ${size}`, role: 'group', 'aria-label': `${center.name} and ${neighbours.size} connected item${neighbours.size === 1 ? '' : 's'}`, class: 'memory-graph-svg' });
      shown.forEach((node, i) => {
        const angle = (2 * Math.PI * i) / shown.length - Math.PI / 2;
        node.x = mid + radius * Math.cos(angle); node.y = mid + radius * Math.sin(angle);
        const past = node.edges.every(e => ['superseded', 'invalidated'].includes(e.status));
        graph.append(svg('line', { x1: mid, y1: mid, x2: node.x, y2: node.y, class: `memory-edge${past ? ' memory-edge-past' : ''}` }));
        const label = node.edges.map(e => words(e.relation)).join(', ');
        graph.append(svg('text', { x: (mid + node.x) / 2, y: (mid + node.y) / 2 - 4, class: 'memory-edge-label', 'text-anchor': 'middle', 'aria-hidden': 'true' }, clip(label, 22)));
      });
      const nodeGroup = (entity, x, y, isCenter, relations) => {
        const g = svg('g', { class: `memory-node${isCenter ? ' memory-node-center' : ''}`, transform: `translate(${x} ${y})` });
        if (!isCenter) {
          g.setAttribute('tabindex', '0'); g.setAttribute('role', 'button');
          g.setAttribute('aria-label', `${entity.name}, ${relations}. Show its connections`);
          const go = () => loadGraph(entity.id);
          g.addEventListener('click', go);
          g.addEventListener('keydown', event => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); go(); } });
        }
        g.append(svg('circle', { r: isCenter ? 34 : 24 }));
        g.append(svg('text', { y: isCenter ? 52 : 40, 'text-anchor': 'middle', class: 'memory-node-label' }, clip(entity.name, 20)));
        g.append(svg('text', { y: 4, 'text-anchor': 'middle', class: 'memory-node-kind', 'aria-hidden': 'true' }, (KIND[entity.kind] || entity.kind).slice(0, 3)));
        return g;
      };
      for (const node of shown) graph.append(nodeGroup(node.entity, node.x, node.y, false, node.edges.map(e => words(e.relation)).join(', ')));
      graph.append(nodeGroup(center, mid, mid, true, ''));
      const caption = el('p', { class: 'memory-hint' }, `Centered on ${center.name}. `,
        button(`Open ${center.name}`, () => openEntity(center.id)));
      holder.replaceChildren(caption, graph);
      if (neighbours.size > shown.length) holder.append(el('p', { class: 'memory-hint', text: `${neighbours.size - shown.length} more connections are listed below.` }));
      if (!edges.length) holder.append(el('p', { class: 'memory-empty', text: `${center.name} has no connections yet.` }));
      list.replaceChildren(...(edges.length ? edges.map(edge => {
        const item = edgeItem(edge, center);
        item.querySelector('button').onclick = () => loadGraph(edge.other.id);
        return item;
      }) : [empty('No connections.')]));
    } catch (error) {
      holder.replaceChildren(el('p', { class: 'memory-error', role: 'alert', text: error.message }));
    }
  }

  // ---- changes ------------------------------------------------------------------------

  async function loadChanges() {
    try {
      state.changes = (await api('changes?limit=200')).changes;
      renderChanges();
    } catch (error) {
      $('memoryChanges').replaceChildren(el('li', { class: 'memory-error', role: 'alert', text: error.message }));
    }
  }

  function renderChanges() {
    const filter = FILTERS[$('memoryChangesFilter').value] || FILTERS.all;
    const items = state.changes.filter(c => filter(c.operation));
    $('memoryChanges').replaceChildren(...(items.length ? items.map(({ operation: op, label }) => el('li', {},
      el('span', { class: 'memory-meta', text: when(op.at) }),
      op.target_kind === 'entity' && label
        ? el('button', { type: 'button', class: 'memory-inline-link', onclick: () => openEntity(op.target_id), text: describeOp(op, label) })
        : el('span', { text: describeOp(op, label) }),
      op.reason ? el('span', { class: 'memory-meta', text: op.reason }) : null)) : [empty('No changes of this kind.')]));
  }

  // ---- wiring -------------------------------------------------------------------------

  async function refreshAll() {
    const tasks = [loadOverview(), loadChanges()];
    if (state.entity) tasks.push(openEntityQuietly(state.entity));
    await Promise.all(tasks);
  }

  async function openEntityQuietly(id) {
    try { renderEntity(await api(`entities/${id}`)); } catch (error) {
      if (error.status === 404) { state.entity = null; $('memoryEntity').replaceChildren(el('p', { class: 'memory-empty', text: 'This was forgotten or merged away.' })); }
    }
  }

  async function exportMemory() {
    try {
      const data = await api('export');
      const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
      const link = el('a', { href: URL.createObjectURL(blob), download: 'sail-memory-export.json' });
      document.body.append(link); link.click(); link.remove();
      setTimeout(() => URL.revokeObjectURL(link.href), 1000);
      setStatus('Exported. The file contains what SAIL knows, not conversations.');
    } catch (error) {
      setStatus(error.message, 'error');
    }
  }

  $('memorySearchForm').addEventListener('submit', search);
  $('memoryTeachForm').addEventListener('submit', teach);
  $('memoryRefreshBtn').addEventListener('click', refreshAll);
  $('memoryExportBtn').addEventListener('click', exportMemory);
  $('memoryChangesFilter').addEventListener('change', renderChanges);
  $('memoryGraphHistory').addEventListener('change', () => loadGraph());
  card.querySelector('[data-tab-panel="memoryPanelGraph"]').addEventListener('click', () => { if (state.entity) loadGraph(); });
  card.querySelector('[data-tab-panel="memoryPanelChanges"]').addEventListener('click', loadChanges);
  const maybeLoad = () => { if (location.hash === '#arena' && !state.loaded) refreshAll(); };
  window.addEventListener('hashchange', maybeLoad);
  document.addEventListener('DOMContentLoaded', maybeLoad);
  window.sailMemory = { openEntity, refresh: refreshAll };
})();

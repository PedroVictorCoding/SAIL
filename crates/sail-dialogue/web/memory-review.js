/* Memory › Review panel (round 22, Antigravity).
   Always-in-context profile block preview with Pin/Unpin and review queue for proposed memory changes. */
'use strict';
(() => {
  const panel = document.getElementById('memoryPanelReview');
  if (!panel) return;
  const $ = s => panel.querySelector(s);

  let busy = false;
  let currentData = null;
  let confirmBulk = null; // null | 'save' | 'discard'
  let itemErrors = new Map();

  const WHY_LABELS = {
    pinned: 'pinned',
    explicit: 'you said it',
    verified: 'verified',
    reported: 'you said it',
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

  function formatEpoch(ts) {
    if (ts == null || ts === 0) return '—';
    const ms = ts > 1e12 ? ts : ts * 1000;
    try {
      return new Date(ms).toLocaleString();
    } catch (_) {
      return String(ts);
    }
  }

  function setStatus(text, isError = false) {
    const node = $('#mrStatus');
    node.textContent = text;
    node.classList.toggle('mr-error', isError);
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
    let payload = null;
    try { payload = await r.json(); } catch (_) {}
    if (!r.ok) {
      const err = new Error(payload?.error || `Request failed (${r.status})`);
      err.status = r.status;
      throw err;
    }
    return payload;
  }

  function renderProfile(profile) {
    const chars = Number(profile?.chars ?? 0);
    const budget = Number(profile?.budget ?? 1400);
    $('#mrProfileSize').textContent = `${chars.toLocaleString('en-US')} / ${budget.toLocaleString('en-US')} characters`;

    const pre = $('#mrProfilePreview');
    pre.textContent = profile?.text ? profile.text : '(No profile lines yet. Pin or verify facts about you to include them here.)';

    const list = $('#mrProfileLines');
    list.replaceChildren();
    const lines = profile?.lines || [];
    if (!lines.length) {
      list.append(el('p', { class: 'mr-muted', text: 'No profile lines in context yet.' }));
      return;
    }
    for (const line of lines) {
      const whyLabel = WHY_LABELS[line.why] || line.why || 'verified';
      list.append(
        el('div', { class: 'mr-line-row', 'data-claim-id': String(line.claim_id) },
          el('div', { class: 'mr-line-main' },
            el('span', { class: 'mr-line-text', text: line.text }),
            el('span', { class: 'mr-why-badge', 'data-why': line.why || '', text: whyLabel }),
          ),
          el('button', {
            type: 'button',
            class: 'quiet-button',
            text: line.pinned ? 'Unpin' : 'Pin',
            onclick: () => togglePin(line.claim_id, !line.pinned),
          }),
        ),
      );
    }
  }

  function renderPending(pending) {
    const bulkWrap = $('#mrBulkActions');
    bulkWrap.replaceChildren();
    if (pending.length > 0) {
      if (confirmBulk === 'save') {
        bulkWrap.append(
          el('button', { type: 'button', class: 'quiet-button', text: `Confirm save all (${pending.length})`, onclick: () => runBulk('save') }),
          el('button', { type: 'button', class: 'quiet-button', text: 'Cancel', onclick: () => { confirmBulk = null; renderPending(pending); } }),
        );
      } else if (confirmBulk === 'discard') {
        bulkWrap.append(
          el('button', { type: 'button', class: 'quiet-button mr-danger', text: `Confirm discard all (${pending.length})`, onclick: () => runBulk('discard') }),
          el('button', { type: 'button', class: 'quiet-button', text: 'Cancel', onclick: () => { confirmBulk = null; renderPending(pending); } }),
        );
      } else {
        bulkWrap.append(
          el('button', { type: 'button', class: 'quiet-button', text: 'Save all', onclick: () => { confirmBulk = 'save'; renderPending(pending); } }),
          el('button', { type: 'button', class: 'quiet-button mr-danger', text: 'Discard all', onclick: () => { confirmBulk = 'discard'; renderPending(pending); } }),
        );
      }
    }

    const list = $('#mrPendingList');
    list.replaceChildren();
    if (!pending.length) {
      list.append(el('p', { class: 'mr-muted', text: 'No memory changes waiting for review.' }));
      return;
    }

    for (const item of pending) {
      const errMsg = itemErrors.get(item.id);
      const summaryNodes = (item.summary || []).map(line => el('li', { text: line }));
      const findings = item.findings || [];
      const findingsBox = findings.length
        ? el('div', { class: 'mr-findings', role: 'alert' },
            el('strong', { text: 'Flagged by memory safety scan:' }),
            ...findings.map(f =>
              el('div', { class: 'mr-finding-item' },
                el('span', { class: 'mr-finding-rule', text: `${f.rule}: ` }),
                el('code', { text: f.excerpt || '' }),
              ),
            ),
          )
        : null;

      list.append(
        el('article', { class: 'mr-pending-card', 'data-pending-id': item.id },
          el('div', { class: 'mr-section-head' },
            el('span', { class: 'mr-muted', text: `Session: ${item.session || '—'} · ${formatEpoch(item.proposed_at)}` }),
            el('div', { class: 'mr-actions' },
              el('button', { type: 'button', class: 'quiet-button', text: 'Save', onclick: () => resolveItem(item.id, 'save') }),
              el('button', { type: 'button', class: 'quiet-button mr-danger', text: 'Discard', onclick: () => resolveItem(item.id, 'discard') }),
            ),
          ),
          summaryNodes.length ? el('ul', { class: 'mr-summary-list' }, summaryNodes) : null,
          findingsBox,
          errMsg ? el('p', { class: 'mr-error', role: 'alert', text: errMsg }) : null,
        ),
      );
    }
  }

  function renderAll(data) {
    currentData = data;
    $('#mrEnabledToggle').checked = Boolean(data.enabled);
    renderProfile(data.profile);
    renderPending(data.pending || []);
  }

  async function refresh() {
    if (busy) return;
    busy = true;
    setStatus('Loading memory review…');
    try {
      const data = await requestJson('/api/memory/review');
      confirmBulk = null;
      renderAll(data);
      const count = (data.pending || []).length;
      setStatus(count ? `${count} memory change${count === 1 ? '' : 's'} waiting for review.` : 'Memory review up to date.');
    } catch (err) {
      setStatus(err.message || 'Could not load memory review.', true);
    } finally {
      busy = false;
    }
  }

  async function toggleEnabled(enabled) {
    try {
      const res = await requestJson('/api/memory/review/enabled', { body: { enabled } });
      if (currentData) currentData.enabled = Boolean(res.enabled);
      $('#mrEnabledToggle').checked = Boolean(res.enabled);
      setStatus(res.enabled ? 'Memory review enabled. New model proposals will wait here.' : 'Memory review turned off.');
    } catch (err) {
      $('#mrEnabledToggle').checked = !enabled;
      setStatus(err.message || 'Could not update review setting.', true);
    }
  }

  async function togglePin(claimId, pinned) {
    try {
      await requestJson('/api/memory/review/pin', { body: { claim_id: claimId, pinned } });
      await refresh();
    } catch (err) {
      setStatus(err.message || 'Could not update pin.', true);
    }
  }

  async function resolveItem(id, action) {
    itemErrors.delete(id);
    try {
      await requestJson(`/api/memory/review/${encodeURIComponent(id)}/${action}`, { body: {} });
      await refresh();
    } catch (err) {
      itemErrors.set(id, err.message || `Could not ${action} change.`);
      if (currentData) renderPending(currentData.pending || []);
      setStatus(err.message || `Could not ${action} change.`, true);
    }
  }

  async function runBulk(action) {
    const items = [...(currentData?.pending || [])];
    confirmBulk = null;
    for (const item of items) {
      try {
        await requestJson(`/api/memory/review/${encodeURIComponent(item.id)}/${action}`, { body: {} });
        itemErrors.delete(item.id);
      } catch (err) {
        itemErrors.set(item.id, err.message || `Could not ${action} change.`);
      }
    }
    await refresh();
  }

  $('#mrRefresh')?.addEventListener('click', refresh);
  $('#mrEnabledToggle')?.addEventListener('change', e => toggleEnabled(e.target.checked));
  document.getElementById('memoryReviewTab')?.addEventListener('click', refresh);
  window.addEventListener('sail:page', e => {
    if (e.detail === 'memory' && !panel.hidden) refresh();
  });
})();

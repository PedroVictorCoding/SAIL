'use strict';
// SAIL Initiative: proactive deliverable cards, inbox, digest and attention controls.
// Polls /api/initiative/deliverable at most once every 60 s and pauses when hidden.
(() => {
  const POLL_MS = 60000;
  const mount = document.getElementById('initiativePanel');
  if (!mount) return;

  const state = {
    view: 'collapsed', // 'inbox' | 'digest' | 'collapsed'
    showSettings: false,
    showAll: false,
    deliverable: [],
    inbox: [],
    digest: null,
    settings: null,
    statusText: '',
    statusError: false,
    pollTimer: null,
    lastPollAt: 0,
    loading: false,
  };

  // ------------------------------------------------------------------ helpers
  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k === 'class') node.className = v;
      else if (k === 'text') node.textContent = v;
      else if (k.startsWith('on')) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? '' : String(v));
    }
    for (const c of children.flat(Infinity)) {
      if (c === null || c === undefined || c === false) continue;
      node.append(c instanceof Node ? c : document.createTextNode(String(c)));
    }
    return node;
  }

  function formatRelative(iso) {
    if (!iso) return '';
    const ms = Date.parse(iso);
    if (!Number.isFinite(ms)) return '';
    const diffSec = Math.round((ms - Date.now()) / 1000);
    const abs = Math.abs(diffSec);
    if (abs < 90) return diffSec >= 0 ? 'in 1 min' : 'just now';
    if (abs < 3600) {
      const m = Math.round(abs / 60);
      return diffSec >= 0 ? `in ${m} min` : `${m} min ago`;
    }
    if (abs < 86400 * 2) {
      const h = Math.round(abs / 3600);
      return diffSec >= 0 ? `in ${h} h` : `${h} h ago`;
    }
    const d = Math.round(abs / 86400);
    return diffSec >= 0 ? `in ${d} d` : `${d} d ago`;
  }

  function safeLink(raw) {
    if (typeof raw !== 'string') return null;
    const trimmed = raw.trim();
    if (trimmed.startsWith('/') && !trimmed.startsWith('//')) return trimmed;
    if (trimmed.startsWith('#')) return trimmed;
    // The only external pages a signal may link to (see attention::ALLOWED_EXTERNAL_LINK_PREFIXES).
    if (trimmed.startsWith('https://github.com/') && !/[\s"'<>\\]/.test(trimmed)) return trimmed;
    return null;
  }

  function setStatus(text, isError = false) {
    state.statusText = text || '';
    state.statusError = Boolean(isError);
    render();
  }

  function laterIso(kind) {
    const now = new Date();
    if (kind === '1h') {
      return new Date(now.getTime() + 3600 * 1000).toISOString();
    }
    if (kind === 'tonight') {
      const t = new Date(now);
      t.setHours(20, 0, 0, 0);
      if (t.getTime() <= now.getTime() + 15 * 60 * 1000) {
        t.setDate(t.getDate() + 1);
      }
      return t.toISOString();
    }
    // tomorrow 09:00 local
    const tm = new Date(now);
    tm.setDate(tm.getDate() + 1);
    tm.setHours(9, 0, 0, 0);
    return tm.toISOString();
  }

  // ------------------------------------------------------------------ API
  let token = null;
  async function ownerToken(force) {
    if (!force && token) return token;
    if (!force) {
      try {
        const t = localStorage.getItem('sail_owner_token');
        if (t && t.trim().length >= 32) return (token = t.trim());
      } catch (_) {}
    }
    const r = await fetch('/api/guardian/token', { cache: 'no-store' });
    if (!r.ok) {
      throw new Error('Owner token unavailable. Open SAIL settings and enter the owner token.');
    }
    const data = await r.json();
    token = data.token;
    try {
      localStorage.setItem('sail_owner_token', token);
    } catch (_) {}
    return token;
  }

  async function request(path, { method = 'GET', body } = {}) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const t = await ownerToken(attempt > 0);
      const r = await fetch(`/api/initiative${path}`, {
        method,
        cache: 'no-store',
        headers: {
          Authorization: `Bearer ${t}`,
          ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
        },
        body: body !== undefined ? JSON.stringify(body) : undefined,
      });
      if (r.status === 401 && attempt === 0) {
        token = null;
        try {
          localStorage.removeItem('sail_owner_token');
        } catch (_) {}
        continue;
      }
      const data = await r.json().catch(() => ({}));
      if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
      return data;
    }
    throw new Error('Owner authentication failed.');
  }

  // ------------------------------------------------------------------ data
  async function pollDeliverable() {
    if (document.hidden) return;
    const now = Date.now();
    if (now - state.lastPollAt < POLL_MS - 500) return;
    state.lastPollAt = now;
    try {
      const items = await request('/deliverable');
      if (Array.isArray(items) && items.length > 0) {
        const existingIds = new Set(state.deliverable.map((x) => x.id));
        for (const item of items) {
          if (!existingIds.has(item.id)) state.deliverable.unshift(item);
        }
        if (state.view === 'collapsed') state.view = 'inbox';
      }
      state.statusText = '';
      state.statusError = false;
      render();
    } catch (err) {
      setStatus(err.message || 'Initiative unavailable.', true);
    }
  }

  async function loadAll() {
    if (state.loading) return;
    state.loading = true;
    try {
      const [inbox, digest, settings] = await Promise.all([
        request('/inbox?limit=30'),
        request('/digest'),
        request('/settings'),
      ]);
      state.inbox = Array.isArray(inbox) ? inbox : [];
      state.digest = digest || null;
      state.settings = settings || null;
      state.statusText = '';
      state.statusError = false;
    } catch (err) {
      setStatus(err.message || 'Could not load initiative state.', true);
    } finally {
      state.loading = false;
      render();
    }
  }

  async function sendFeedback(id, payload) {
    try {
      await request(`/items/${encodeURIComponent(id)}/feedback`, {
        method: 'POST',
        body: payload,
      });
      state.deliverable = state.deliverable.filter((x) => x.id !== id);
      state.inbox = state.inbox.filter((x) => x.id !== id);
      if (state.digest && Array.isArray(state.digest.items)) {
        state.digest.items = state.digest.items.filter((x) => x.id !== id);
      }
      setStatus('');
      await loadAll();
    } catch (err) {
      setStatus(err.message || 'Feedback failed.', true);
    }
  }

  async function updateSettings(patch) {
    if (!state.settings) return;
    const next = { ...state.settings, ...patch };
    try {
      const saved = await request('/settings', { method: 'POST', body: next });
      state.settings = saved;
      setStatus('Settings saved.');
    } catch (err) {
      setStatus(err.message || 'Could not save settings.', true);
    }
  }

  async function releaseDigestNow() {
    try {
      const batch = await request('/digest/release', { method: 'POST' });
      state.digest = batch || null;
      state.view = 'digest';
      setStatus(batch ? 'Digest released.' : 'No pending digest items.');
    } catch (err) {
      setStatus(err.message || 'Could not release digest.', true);
    }
  }

  // ------------------------------------------------------------------ render
  const DELIVERY_LABEL = {
    interrupt: 'Now',
    suggest: 'Worth raising',
    digest: 'Digest',
    silent: 'Prepared',
  };
  // `assessment.delivery` is `{"delivery": "interrupt"}` (serde adjacently tagged).
  function deliveryOf(item) {
    const d = item.assessment?.delivery;
    return (typeof d === 'string' ? d : d?.delivery) || 'digest';
  }
  // The inbox holds only what deserves attention now; the rest waits for the digest.
  function isInboxWorthy(item) {
    if (['snoozed', 'investigating', 'dismissed', 'done', 'digested'].includes(item.state)) {
      return false;
    }
    const d = deliveryOf(item);
    return d === 'interrupt' || d === 'suggest' || item.state === 'delivered';
  }

  // Items stored before course names were readable carry Moodle's course code as a
  // title prefix ("Deadline in DECD_03749-2026_…: Entrega"); show only the item name.
  const COURSE_CODE_PREFIX =
    /^(?:(?:Exam|Test|Quiz|Deadline|Presentation|Lab|Schedule change|Registration|School item) in )?[A-Z][A-Z0-9]*_[\w.-]+:\s*/;
  function tidyTitle(title) {
    const t = String(title || '').trim();
    return t.replace(COURSE_CODE_PREFIX, '') || t || 'Attention item';
  }
  const SOURCE_LABEL = {
    moodle: 'Moodle',
    calendar: 'Calendar',
    reminders: 'Reminders',
    git_hub: 'GitHub',
    research: 'Research',
    capital: 'Capital',
    system: 'System',
    escalation: 'Escalation',
    curiosity: 'Curiosity',
    projects: 'Forge',
  };

  let openMenuId = null;

  function renderCard(item) {
    const sig = item.signal || {};
    const delivery = deliveryOf(item);
    const link = safeLink(sig.link);
    const source = SOURCE_LABEL[sig.source] || sig.source || 'SAIL';
    const when = sig.deadline
      ? `due ${formatRelative(sig.deadline)}`
      : item.created_at
        ? formatRelative(item.created_at)
        : '';
    const meta = [source, when, sig.provenance === 'hypothesis' ? 'unverified idea' : '']
      .filter(Boolean)
      .join(' · ');

    const act = (text, payload, extra = '') =>
      el('button', {
        type: 'button',
        class: `init-act ${extra}`.trim(),
        text,
        onclick: () => sendFeedback(item.id, payload),
      });
    const menuOpen = openMenuId === item.id;
    const actions = el(
      'div',
      { class: 'init-actions' },
      link
        ? el('a', {
            class: 'init-act init-act-primary',
            href: link,
            text: 'Open',
            ...(link.startsWith('https://')
              ? { target: '_blank', rel: 'noopener noreferrer' }
              : {}),
          })
        : null,
      act('Useful', { feedback: 'useful' }, link ? '' : 'init-act-primary'),
      act('Later', { feedback: 'later', until: laterIso('1h') }),
      act('Dismiss', { feedback: 'dismiss' }),
      el('button', {
        type: 'button',
        class: 'init-act init-act-more',
        'aria-label': 'More actions',
        'aria-expanded': String(menuOpen),
        text: '⋯',
        onclick: () => {
          openMenuId = menuOpen ? null : item.id;
          render();
        },
      })
    );
    const menu = menuOpen
      ? el(
          'div',
          { class: 'init-menu' },
          act('Not now', { feedback: 'not_now' }),
          act('Tonight', { feedback: 'later', until: laterIso('tonight') }),
          act('Tomorrow', { feedback: 'later', until: laterIso('tomorrow') }),
          act('Investigate', { feedback: 'investigate' }),
          act('Mute this kind', { feedback: 'mute' }, 'init-act-danger')
        )
      : null;

    return el(
      'article',
      { class: `init-item delivery-${delivery}` },
      el(
        'div',
        { class: 'init-meta' },
        el('span', {
          class: 'init-dot',
          title: DELIVERY_LABEL[delivery] || delivery,
          'aria-label': DELIVERY_LABEL[delivery] || delivery,
        }),
        el('span', { text: meta })
      ),
      el('h3', { class: 'init-item-title', text: tidyTitle(sig.title) }),
      sig.body ? el('p', { class: 'init-item-body', text: sig.body }) : null,
      actions,
      menu
    );
  }

  function renderSettingsBar() {
    if (!state.settings) return null;
    const autonomy = state.settings.autonomy || 'balanced';

    const autonomySelect = el(
      'select',
      {
        class: 'init-select',
        'aria-label': 'Autonomy level',
        onchange: (e) => updateSettings({ autonomy: e.target.value }),
      },
      el('option', { value: 'off', selected: autonomy === 'off', text: 'Off' }),
      el('option', { value: 'quiet', selected: autonomy === 'quiet', text: 'Quiet' }),
      el('option', { value: 'balanced', selected: autonomy === 'balanced', text: 'Balanced' }),
      el('option', { value: 'proactive', selected: autonomy === 'proactive', text: 'Proactive' })
    );

    const focusSelect = el(
      'select',
      {
        class: 'init-select',
        'aria-label': 'Focus: no interruptions',
        onchange: (e) => {
          const mins = Number(e.target.value);
          if (Number.isFinite(mins) && mins > 0) {
            updateSettings({
              focus_until: new Date(Date.now() + mins * 60 * 1000).toISOString(),
            });
          }
        },
      },
      el('option', { value: '', selected: true, text: 'Off' }),
      el('option', { value: '60', text: '1 hour' }),
      el('option', { value: '120', text: '2 hours' }),
      el('option', { value: '240', text: '4 hours' })
    );

    return el(
      'div',
      { class: 'init-settings-bar' },
      el('label', { class: 'init-field' }, el('span', { text: 'Autonomy' }), autonomySelect),
      el('label', { class: 'init-field' }, el('span', { text: 'Focus' }), focusSelect)
    );
  }

  function focusUntil() {
    const until = state.settings?.focus_until;
    return until && Date.parse(until) > Date.now() ? until : null;
  }

  function renderFocusNote() {
    const until = focusUntil();
    if (!until) return null;
    const time = new Date(until).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    return el(
      'p',
      { class: 'init-focus-note' },
      `Focus until ${time} — only urgent items interrupt. `,
      el('button', {
        type: 'button',
        class: 'init-act',
        text: 'Resume',
        onclick: () => updateSettings({ focus_until: null }),
      })
    );
  }

  // Variety over volume: take items round-robin by source (keeping each source's order),
  // so one busy source can't fill the visible rows. Interrupts stay first.
  function interleaveBySource(items) {
    const urgent = items.filter((x) => deliveryOf(x) === 'interrupt');
    const rest = items.filter((x) => deliveryOf(x) !== 'interrupt');
    const queues = new Map();
    for (const item of rest) {
      const key = item.signal?.source || 'sail';
      if (!queues.has(key)) queues.set(key, []);
      queues.get(key).push(item);
    }
    const out = [...urgent];
    let added = true;
    while (added) {
      added = false;
      for (const queue of queues.values()) {
        if (queue.length) {
          out.push(queue.shift());
          added = true;
        }
      }
    }
    return out;
  }

  function render() {
    const deliverableIds = new Set(state.deliverable.map((x) => x.id));
    const mergedInbox = interleaveBySource([
      ...state.deliverable,
      ...state.inbox.filter((x) => !deliverableIds.has(x.id) && isInboxWorthy(x)),
    ]);
    const waiting = state.inbox.filter(
      (x) => !deliverableIds.has(x.id) && !isInboxWorthy(x) && x.state === 'pending'
    );
    const badgeCount = state.deliverable.length || mergedInbox.length;

    const header = el(
      'div',
      { class: 'init-header' },
      el(
        'div',
        { class: 'init-title-group' },
        el('h2', { class: 'init-title', text: 'Initiative' }),
        el('span', {
          class: `init-badge ${state.deliverable.length > 0 ? 'has-items' : ''}`,
          text: String(badgeCount),
          'aria-label': `${badgeCount} initiative items`,
        })
      ),
      el(
        'div',
        { class: 'init-controls' },
        el(
          'div',
          { class: 'init-tabs', role: 'group', 'aria-label': 'Initiative views' },
          el('button', {
            type: 'button',
            class: 'init-tab',
            'aria-pressed': String(state.view === 'inbox'),
            text: 'Inbox',
            onclick: () => {
              state.view = state.view === 'inbox' ? 'collapsed' : 'inbox';
              render();
            },
          }),
          el('button', {
            type: 'button',
            class: 'init-tab',
            'aria-pressed': String(state.view === 'digest'),
            text: 'Digest',
            onclick: () => {
              state.view = state.view === 'digest' ? 'collapsed' : 'digest';
              render();
            },
          })
        ),
        el('button', {
          type: 'button',
          class: 'init-tab init-gear',
          'aria-label': 'Initiative settings',
          'aria-pressed': String(state.showSettings),
          title: 'Settings',
          text: '⚙',
          onclick: () => {
            state.showSettings = !state.showSettings;
            render();
          },
        })
      )
    );

    const children = [header];

    if (state.showSettings) {
      const settingsBar = renderSettingsBar();
      if (settingsBar) children.push(settingsBar);
    }
    const focusNote = renderFocusNote();
    if (focusNote) children.push(focusNote);

    if (state.statusText) {
      children.push(
        el('p', {
          class: `init-status ${state.statusError ? 'error' : ''}`,
          role: 'status',
          text: state.statusText,
        })
      );
    }

    if (state.view === 'inbox') {
      const body = el('div', { class: 'init-body' });
      if (mergedInbox.length === 0) {
        body.append(el('p', { class: 'init-empty', text: 'Nothing needs attention right now.' }));
      } else {
        const shown = state.showAll ? mergedInbox.slice(0, 12) : mergedInbox.slice(0, 3);
        for (const item of shown) body.append(renderCard(item));
        if (mergedInbox.length > shown.length) {
          body.append(
            el('button', {
              type: 'button',
              class: 'init-waiting',
              text: `Show ${Math.min(mergedInbox.length, 12) - shown.length} more`,
              onclick: () => {
                state.showAll = true;
                render();
              },
            })
          );
        }
      }
      if (waiting.length > 0) {
        body.append(
          el('button', {
            type: 'button',
            class: 'init-waiting',
            text: `${waiting.length} more waiting for the next digest`,
            onclick: () => {
              state.view = 'digest';
              render();
            },
          })
        );
      }
      children.push(body);
    } else if (state.view === 'digest') {
      const body = el('div', { class: 'init-body' });
      const head = el(
        'div',
        { class: 'init-digest-head' },
        el('span', {
          text: state.digest?.released_at
            ? `Last digest ${formatRelative(state.digest.released_at)}`
            : 'No digest released yet.',
        }),
        el('button', {
          type: 'button',
          class: 'init-act',
          text: 'Release now',
          onclick: releaseDigestNow,
        })
      );
      body.append(head);
      const items = state.digest?.items || [];
      if (items.length === 0) {
        body.append(el('p', { class: 'init-empty', text: 'Digest is empty.' }));
      } else {
        for (const item of items) body.append(renderCard(item));
      }
      if (waiting.length > 0) {
        body.append(el('h3', { class: 'init-subhead', text: 'Waiting for the next digest' }));
        for (const item of waiting.slice(0, 12)) body.append(renderCard(item));
      }
      children.push(body);
    }

    mount.replaceChildren(...children);
  }

  // ------------------------------------------------------------------ lifecycle
  function startPolling() {
    if (state.pollTimer) return;
    state.pollTimer = window.setInterval(() => {
      if (!document.hidden) pollDeliverable();
    }, POLL_MS);
  }

  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && Date.now() - state.lastPollAt >= POLL_MS) {
      pollDeliverable();
    }
  });

  render();
  loadAll().then(() => pollDeliverable());
  startPolling();
})();

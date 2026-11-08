'use strict';
// SAIL School: the owner's Moodle, read from SAIL's local copy (/api/moodle). Everything
// shown here comes from the last sync; nothing on this page calls Moodle directly. Moodle
// text (course names, titles, quotes, file names) is untrusted and only inserted as text.

(() => {
  const $ = (id) => document.getElementById(id);
  // Inside the SAIL workspace the sidebar already leads back.
  if (window.top !== window) document.querySelector('.md-back')?.remove();

  const state = {status: null, courses: [], course: null, items: [], changes: [], refreshTimer: null};

  const KIND = {
    exam: 'Exam', test: 'Test', quiz: 'Quiz', deadline: 'Due', presentation: 'Presentation',
    lab: 'Lab', schedule_change: 'Class change', registration: 'Sign-up', other: 'Date',
  };
  const CONFIDENCE = {
    confirmed: {label: 'Confirmed', hint: 'From a Moodle date, or two sources agree'},
    likely: {label: 'Likely', hint: 'Read from the text below'},
    uncertain: {label: 'Check this', hint: 'Sources disagree or the wording is unclear'},
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
  function ago(iso) {
    if (!iso) return 'never';
    const s = (Date.now() - Date.parse(iso)) / 1000;
    if (!isFinite(s)) return 'never';
    if (s < 90) return 'just now';
    if (s < 3600) return `${Math.round(s / 60)} min ago`;
    if (s < 86400 * 2) return `${Math.round(s / 3600)} h ago`;
    return `${Math.round(s / 86400)} days ago`;
  }
  function until(iso) {
    const s = (Date.parse(iso) - Date.now()) / 1000;
    if (!isFinite(s) || s <= 0) return 'soon';
    if (s < 3600) return `in ${Math.max(1, Math.round(s / 60))} min`;
    return `in ${Math.round(s / 3600)} h`;
  }
  function localDate(ymd) {
    const [y, m, d] = ymd.split('-').map(Number);
    return new Date(y, m - 1, d);
  }
  function daysFromToday(ymd) {
    const today = new Date(); today.setHours(0, 0, 0, 0);
    return Math.round((localDate(ymd) - today) / 86400000);
  }
  function dayLabel(ymd) {
    const n = daysFromToday(ymd);
    const date = localDate(ymd).toLocaleDateString(undefined, {weekday: 'short', day: 'numeric', month: 'short'});
    if (n === 0) return `Today · ${date}`;
    if (n === 1) return `Tomorrow · ${date}`;
    if (n > 1 && n < 7) return `In ${n} days · ${date}`;
    return date;
  }
  function bytes(n) {
    if (!isFinite(n)) return '—';
    if (n < 1024) return `${n} B`;
    if (n < 1048576) return `${(n / 1024).toFixed(0)} KB`;
    if (n < 1073741824) return `${(n / 1048576).toFixed(1)} MB`;
    return `${(n / 1073741824).toFixed(1)} GB`;
  }
  function courseName(id) {
    const c = state.courses.find((x) => x.id === id);
    return c ? c.full_name : `Course ${id}`;
  }
  function banner(text) {
    const b = $('mdBanner'); b.textContent = text || ''; b.hidden = !text;
  }

  // ------------------------------------------------------------------ API
  let token = null;
  async function ownerToken(force) {
    if (!force && token) return token;
    if (!force) {
      try { const t = localStorage.getItem('sail_owner_token'); if (t && t.trim().length >= 32) return (token = t.trim()); } catch (_) {}
    }
    const r = await fetch('/api/guardian/token', {cache: 'no-store'});
    if (!r.ok) throw new Error('Owner token unavailable. Open SAIL settings and enter the owner token.');
    const data = await r.json();
    token = data.token;
    try { localStorage.setItem('sail_owner_token', token); } catch (_) {}
    return token;
  }
  async function request(path, {method = 'GET', body, raw} = {}) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const t = await ownerToken(attempt > 0);
      const r = await fetch(`/api/moodle${path}`, {
        method, cache: 'no-store',
        headers: {Authorization: `Bearer ${t}`, ...(body !== undefined ? {'Content-Type': 'application/json'} : {})},
        body: body !== undefined ? JSON.stringify(body) : undefined,
      });
      if (r.status === 401 && attempt === 0) { token = null; try { localStorage.removeItem('sail_owner_token'); } catch (_) {} continue; }
      if (raw) { if (!r.ok) throw new Error(`HTTP ${r.status}`); return r; }
      const data = await r.json().catch(() => ({}));
      if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
      return data;
    }
    throw new Error('Owner authentication failed.');
  }

  // ------------------------------------------------------------------ status
  function renderStatus() {
    const s = state.status;
    const pill = $('mdState');
    let mode = 'ok';
    let text = `Checked ${ago(s.last_ok)}`;
    if (!s.configured) { mode = 'off'; text = 'Not connected'; }
    else if (s.syncing) { mode = 'busy'; text = 'Checking Moodle…'; }
    else if (s.needs_sign_in) { mode = 'error'; text = 'Sign in again'; }
    else if (s.last_error && (!s.last_ok || Date.parse(s.last_attempt) > Date.parse(s.last_ok))) { mode = 'warn'; text = 'Last check failed'; }
    else if (!s.last_ok) { mode = 'busy'; text = 'First check pending'; }
    pill.dataset.state = mode;
    $('mdStateText').textContent = text;
    $('mdSite').textContent = [s.site_name || (s.site_url ? new URL(s.site_url).host : 'Moodle'), s.user_name].filter(Boolean).join(' · ');
    const connected = s.configured && !s.needs_sign_in;
    $('mdSync').hidden = !connected;
    $('mdSync').disabled = !!s.syncing;
    $('mdSettingsOpen').hidden = !s.configured;
    $('mdConnect').hidden = connected;
    $('mdWorkspace').hidden = !s.configured && !s.last_ok;
    if (s.needs_sign_in && s.configured) {
      banner('Moodle stopped accepting SAIL\'s key. Sign in again below; your saved courses stay.');
    } else if (s.last_error && mode === 'warn') {
      banner(`Last check: ${s.last_error}`);
    } else banner('');
    renderUsage();
  }

  function renderUsage() {
    const s = state.status; if (!s) return;
    const dl = $('mdUsage'); dl.replaceChildren();
    const row = (k, v) => dl.append(el('dt', {text: k}), el('dd', {text: v}));
    row('Last check', s.last_ok ? ago(s.last_ok) : 'not yet');
    row('Next check', s.next_sync ? until(s.next_sync) : '—');
    row('Checks', `every ${Math.round(s.limits.sync_every_minutes / 60 * 10) / 10} h`);
    row('Requests today', `${s.usage_today.requests} of ${s.limits.requests_per_day}`);
    row('Downloaded today', `${bytes(s.usage_today.bytes)} of ${bytes(s.limits.download_bytes_per_day)}`);
    row('Reads PDFs', s.reads_pdfs ? 'yes' : 'no (runner not set up)');
    if (s.unavailable_features?.length) row('Skipped', s.unavailable_features.join(', '));
  }

  // ------------------------------------------------------------------ courses
  function renderCourses() {
    const ul = $('mdCourses'); ul.replaceChildren();
    const item = (id, name, sub, badge) => {
      const selected = state.course === id;
      ul.append(el('li', {},
        el('button', {class: 'md-course', type: 'button', 'aria-current': selected ? 'true' : null,
          onclick: () => { state.course = id; refreshCourse(); renderCourses(); }},
        el('span', {class: 'md-course-name', text: name}),
        sub ? el('span', {class: 'md-course-sub', text: sub}) : null,
        badge ? el('span', {class: 'md-badge', text: badge, 'aria-label': `${badge} new changes`}) : null)));
    };
    const unseen = state.courses.reduce((n, c) => n + (c.unseen_changes || 0), 0);
    item(null, 'All courses', `${state.courses.length} enrolled`, unseen || null);
    for (const c of state.courses) {
      const next = c.next_item ? `${KIND[c.next_item.kind] || 'Date'} · ${dayLabel(c.next_item.when.date)}` : c.short_name;
      item(c.id, c.full_name, next, c.unseen_changes || null);
    }
    if (!state.courses.length) ul.append(el('li', {class: 'md-empty', text: state.status?.last_ok ? 'No courses on Moodle.' : 'Courses appear after the first check.'}));
  }

  // ------------------------------------------------------------------ stats
  function renderStats() {
    const box = $('mdStats'); box.replaceChildren();
    const within = (n) => state.items.filter((i) => { const d = daysFromToday(i.when.date); return d >= 0 && d < n; });
    const assess = state.items.filter((i) => ['exam', 'test', 'quiz', 'presentation'].includes(i.kind));
    const next = assess[0];
    const tile = (hue, value, label, sub) => box.append(el('div', {class: `md-stat md-hue-${hue}`},
      el('span', {class: 'md-stat-value', text: value}), el('span', {class: 'md-stat-label', text: label}),
      sub ? el('span', {class: 'md-stat-sub', text: sub}) : null));
    tile('violet', next ? dayLabel(next.when.date).split(' · ')[0] : '—', 'Next exam or test', next ? next.title : 'None found yet');
    tile('blue', String(within(7).length), 'Dated items this week', `${within(30).length} in 30 days`);
    tile('green', String(state.changes.filter((c) => !c.seen).length), 'Unseen changes', state.status?.last_ok ? `checked ${ago(state.status.last_ok)}` : '');
  }

  // ------------------------------------------------------------------ items
  function renderItems() {
    const ol = $('mdItems'); ol.replaceChildren();
    if (!state.items.length) {
      ol.append(el('li', {class: 'md-empty', text: state.status?.last_ok ? 'Nothing dated coming up.' : 'Dates appear after the first check.'}));
      return;
    }
    let lastDay = null;
    for (const item of state.items.slice(0, 80)) {
      if (item.when.date !== lastDay) {
        lastDay = item.when.date;
        ol.append(el('li', {class: 'md-day', 'aria-hidden': 'true', text: dayLabel(item.when.date)}));
      }
      const conf = CONFIDENCE[item.confidence] || CONFIDENCE.uncertain;
      const time = [item.when.time?.slice(0, 5), item.when.end_time ? `– ${item.when.end_time.slice(0, 5)}` : null].filter(Boolean).join(' ');
      const ev = item.evidence?.[0];
      ol.append(el('li', {class: `md-item md-kind-${item.kind}`, 'data-confidence': item.confidence},
        el('div', {class: 'md-item-main'},
          el('span', {class: 'md-kind', text: KIND[item.kind] || 'Date'}),
          el('h3', {class: 'md-item-title', text: item.title}),
          el('p', {class: 'md-item-meta'},
            el('span', {class: 'md-sr', text: `${dayLabel(item.when.date)}. `}),
            [time, state.course === null ? courseName(item.course_id) : null, item.location,
              item.weight_percent != null ? `${item.weight_percent}% of grade` : null].filter(Boolean).join(' · ')),
          ev ? el('details', {class: 'md-evidence'},
            el('summary', {text: item.origin === 'moodle' ? 'From Moodle' : 'Where SAIL read this'}),
            el('blockquote', {text: ev.quote}),
            el('p', {class: 'md-source', text: ev.label}),
            item.evidence.length > 1 ? el('p', {class: 'md-source', text: `Also in ${item.evidence.length - 1} other place${item.evidence.length > 2 ? 's' : ''}`}) : null) : null),
        el('div', {class: 'md-item-side'},
          el('span', {class: 'md-conf', title: conf.hint, text: conf.label}),
          el('button', {class: 'md-button md-small md-quiet', type: 'button', 'aria-label': `Hide ${item.title}`, text: 'Hide',
            onclick: async (e) => { e.currentTarget.disabled = true; try { await request(`/items/${encodeURIComponent(item.id)}/dismiss`, {method: 'POST'}); await refreshCourse(); } catch (err) { banner(err.message); } }}))));
    }
  }

  // ------------------------------------------------------------------ changes
  function renderChanges() {
    const ul = $('mdChanges'); ul.replaceChildren();
    $('mdSeenAll').hidden = !state.changes.some((c) => !c.seen);
    if (!state.changes.length) {
      ul.append(el('li', {class: 'md-empty', text: state.status?.last_ok ? 'No changes since SAIL started following this.' : 'Changes appear from the second check on.'}));
      return;
    }
    for (const c of state.changes.slice(0, 100)) {
      ul.append(el('li', {class: `md-change${c.seen ? '' : ' md-unseen'}`},
        el('span', {class: 'md-sr', text: c.seen ? '' : 'New: '}),
        el('span', {class: 'md-change-title', text: c.title}),
        el('span', {class: 'md-change-meta', text: [state.course ? null : courseName(c.course_id), c.detail, ago(c.observed_at)].filter(Boolean).join(' · ')})));
    }
  }

  // ------------------------------------------------------------------ side column
  async function renderSide() {
    const grading = $('mdGrading'); grading.replaceChildren();
    const files = $('mdFiles'); files.replaceChildren();
    if (state.course === null) {
      grading.append(el('p', {class: 'md-empty', text: 'Pick a course to see how it is graded.'}));
      files.append(el('li', {class: 'md-empty', text: 'Pick a course to see its files.'}));
      return;
    }
    const [components, list] = await Promise.all([
      request(`/assessment?course=${state.course}`).catch(() => []),
      request(`/files?course=${state.course}`).catch(() => []),
    ]);
    if (!components.length) grading.append(el('p', {class: 'md-empty', text: 'SAIL hasn\'t found grading rules for this course yet.'}));
    else {
      const ul = el('ul', {class: 'md-grading'});
      for (const a of components) {
        ul.append(el('li', {},
          el('span', {class: 'md-grade-name', text: a.name}),
          el('span', {class: 'md-grade-weight', text: a.weight_percent != null ? `${a.weight_percent}%` : '—'}),
          a.minimum || a.regime ? el('span', {class: 'md-grade-note', text: [a.regime, a.minimum ? `minimum ${a.minimum}` : null].filter(Boolean).join(' · ')}) : null,
          el('details', {class: 'md-evidence'}, el('summary', {text: 'Source'}), el('blockquote', {text: a.evidence.quote}), el('p', {class: 'md-source', text: a.evidence.label}))));
      }
      grading.append(ul);
    }
    // Newest version per file.
    const seen = new Set();
    const latest = list.filter((f) => { const k = JSON.stringify(f.key); if (seen.has(k)) return false; seen.add(k); return true; });
    if (!latest.length) files.append(el('li', {class: 'md-empty', text: 'No files saved yet.'}));
    for (const f of latest.slice(0, 200)) {
      const name = f.key.filename;
      files.append(el('li', {class: 'md-file'},
        f.sha256
          ? el('button', {class: 'md-link', type: 'button', text: name, onclick: () => download(f.sha256, name)})
          : el('span', {text: name}),
        el('span', {class: 'md-file-meta', text: f.too_large ? `${bytes(f.size)} · too large to save` : `${bytes(f.size)} · saved ${ago(f.downloaded_at)}`})));
    }
  }

  async function download(sha, name) {
    try {
      const r = await request(`/files/${sha}?name=${encodeURIComponent(name)}`, {raw: true});
      const url = URL.createObjectURL(await r.blob());
      const a = el('a', {href: url, download: name}); document.body.append(a); a.click(); a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 30000);
    } catch (err) { banner(`Couldn't open ${name}: ${err.message}`); }
  }

  // ------------------------------------------------------------------ loading
  async function refreshCourse() {
    const q = state.course === null ? '' : `course=${state.course}`;
    const [items, changes] = await Promise.all([
      request(`/items?${q}`).catch((e) => { banner(e.message); return []; }),
      request(`/changes?limit=100${q ? `&${q}` : ''}`).catch(() => []),
    ]);
    state.items = items; state.changes = changes;
    renderItems(); renderChanges(); renderStats();
    await renderSide();
  }

  async function refreshAll() {
    try {
      state.status = await request('/status');
    } catch (err) {
      $('mdState').dataset.state = 'error'; $('mdStateText').textContent = 'Unavailable';
      banner(err.message.includes('HTTP 404') ? 'This SAIL build has no Moodle connector.' : err.message);
      return;
    }
    renderStatus();
    if (!$('mdWorkspace').hidden) {
      state.courses = await request('/courses').catch(() => []);
      if (state.course !== null && !state.courses.some((c) => c.id === state.course)) state.course = null;
      renderCourses();
      await refreshCourse();
    }
    scheduleRefresh();
  }

  // Only the local API is polled: fast while a sync runs, otherwise every few minutes.
  function scheduleRefresh() {
    clearTimeout(state.refreshTimer);
    if (document.hidden) return;
    state.refreshTimer = setTimeout(refreshAll, state.status?.syncing ? 5000 : 180000);
  }
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refreshAll(); });

  // ------------------------------------------------------------------ actions
  function connectError(text) { $('mdConnectError').textContent = text || ''; }
  $('mdLaunch').addEventListener('click', async () => {
    connectError('');
    // Open the tab synchronously so the browser doesn't block it as a pop-up.
    const tab = window.open('about:blank', '_blank');
    try {
      const data = await request('/launch', {method: 'POST', body: {site_url: $('mdSiteUrl').value}});
      if (tab) { tab.opener = null; tab.location.href = data.launch_url; }
      else connectError('The pop-up was blocked. Allow pop-ups for SAIL and try again.');
      $('mdRedirect').focus();
    } catch (err) { tab?.close(); connectError(err.message); }
  });
  async function connect(body, button) {
    connectError(''); button.disabled = true;
    try {
      state.status = await request('/connect', {method: 'POST', body: {site_url: $('mdSiteUrl').value, ...body}});
      $('mdRedirect').value = ''; $('mdToken').value = '';
      await refreshAll();
    } catch (err) { connectError(err.message); } finally { button.disabled = false; }
  }
  $('mdRedirectGo').addEventListener('click', (e) => {
    const v = $('mdRedirect').value.trim();
    if (!v) { connectError('Paste the address from after the sign-in.'); return; }
    connect({redirect: v}, e.currentTarget);
  });
  $('mdTokenGo').addEventListener('click', (e) => {
    const v = $('mdToken').value.trim();
    if (!v) { connectError('Paste the security key.'); return; }
    connect({token: v}, e.currentTarget);
  });
  $('mdSync').addEventListener('click', async () => {
    const button = $('mdSync');
    button.disabled = true;
    try { await request('/sync', {method: 'POST'}); state.status.syncing = true; renderStatus(); banner(''); }
    catch (err) { banner(err.message); button.disabled = false; }
    scheduleRefresh();
  });
  $('mdSeenAll').addEventListener('click', async () => {
    const ids = state.changes.filter((c) => !c.seen).map((c) => c.id).slice(0, 200);
    try { await request('/changes/seen', {method: 'POST', body: {ids}}); await refreshAll(); } catch (err) { banner(err.message); }
  });
  $('mdSettingsOpen').addEventListener('click', () => {
    const s = state.status; const dl = $('mdConn'); dl.replaceChildren();
    const row = (k, v) => dl.append(el('dt', {text: k}), el('dd', {text: v || '—'}));
    row('Site', s.site_url); row('Account', s.user_name); row('Last check', s.last_ok ? ago(s.last_ok) : 'not yet');
    $('mdSettings').showModal();
  });
  $('mdDisconnect').addEventListener('click', async () => {
    try { await request('/connect', {method: 'DELETE'}); $('mdSettings').close(); await refreshAll(); } catch (err) { banner(err.message); }
  });

  refreshAll();
})();

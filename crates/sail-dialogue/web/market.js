/* SAIL Agents › Market: browse and install Hermes skills and plugins (round 20, Antigravity).
   Contract: /home/main/sail-worktrees/LLM_Handoff.md → "Market HTTP contract". */
'use strict';
(() => {
  const card = document.getElementById('marketCard');
  if (!card) return;
  const $ = s => card.querySelector(s);

  const PAGE_SIZE = 50;
  const TRUST_LABELS = {
    builtin: 'Hermes built-in',
    trusted: 'Trusted',
    community: 'Community',
    agent_created: 'Proposed by SAIL',
  };
  const VERDICT_LABELS = {
    safe: 'Safe',
    caution: 'Caution',
    dangerous: 'Dangerous',
  };
  const STATE_LABELS = {
    ok: 'OK',
    modified: 'Edited after install',
    missing: 'Missing',
  };
  const SEVERITIES = ['critical', 'high', 'medium', 'low'];

  let busy = false;
  let refreshing = false;
  let items = [];
  let total = 0;
  let lastStatus = null;
  let queryTimer = null;
  // Map of `${kind}:${id}` -> { loading, error, report, pluginNotice }
  const stagedByItem = new Map();
  // Set of installed keys awaiting second confirmation for modified uninstall
  const confirmUninstall = new Set();

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

  function itemKey(item) {
    return `${item.kind}:${item.id}`;
  }

  function setStatus(text, isError = false) {
    const node = $('#marketStatus');
    node.textContent = text;
    node.classList.toggle('market-error', isError);
  }

  function setBusy(on) {
    busy = on;
    card.setAttribute('aria-busy', on ? 'true' : 'false');
    $('#marketRefresh').disabled = on;
    const dl = $('#marketDownloadBtn');
    if (dl) dl.disabled = on;
  }

  async function api(path, body = undefined) {
    const token = await getOrFetchOwnerToken();
    if (!token) throw Error('Owner authorization unavailable. Reconnect and try again.');
    const r = await fetch('/api/hermes/market' + path, {
      method: body !== undefined ? 'POST' : 'GET',
      cache: 'no-store',
      headers: {
        Authorization: `Bearer ${token}`,
        ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
      },
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
    });
    const data = await r.json().catch(() => ({}));
    if (!r.ok) {
      const err = Error(data.error || `Market unavailable (${r.status})`);
      err.data = data;
      err.status = r.status;
      throw err;
    }
    return data;
  }

  function formatAgo(unixSecs) {
    if (!unixSecs) return 'never updated';
    const diff = Math.max(0, Math.floor(Date.now() / 1000) - unixSecs);
    if (diff < 60) return 'updated just now';
    if (diff < 3600) return `updated ${Math.floor(diff / 60)} m ago`;
    if (diff < 86400) return `updated ${Math.floor(diff / 3600)} h ago`;
    return `updated ${Math.floor(diff / 86400)} d ago`;
  }

  function formatBytes(bytes) {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  }

  function formatDate(isoOrUnix) {
    if (!isoOrUnix) return '—';
    const d = typeof isoOrUnix === 'number' ? new Date(isoOrUnix * 1000) : new Date(isoOrUnix);
    return Number.isNaN(d.getTime()) ? String(isoOrUnix) : d.toLocaleString();
  }

  function pill(kindClass, label) {
    return el('span', { class: `market-pill market-pill-${kindClass}` }, el('i', { 'aria-hidden': 'true' }), label);
  }

  function renderCatalogLine(s) {
    const line = $('#marketCatalogLine');
    if (!s) {
      line.textContent = '';
      return;
    }
    const parts = [
      `${Number(s.skills || 0).toLocaleString()} skill${s.skills === 1 ? '' : 's'}`,
      `${Number(s.plugins || 0).toLocaleString()} plugin${s.plugins === 1 ? '' : 's'}`,
      formatAgo(Math.max(s.skills_fetched_at || 0, s.plugins_fetched_at || 0)),
    ];
    if (refreshing || s.refreshing) parts.push('Refreshing…');
    if (s.error) parts.push(`Error: ${s.error}`);
    line.textContent = parts.join(' · ');
    line.classList.toggle('market-error', Boolean(s.error));
  }

  function isFirstRun(s) {
    return Boolean(
      s &&
      (s.skills || 0) === 0 &&
      (s.plugins || 0) === 0 &&
      (s.skills_fetched_at || 0) === 0 &&
      (s.plugins_fetched_at || 0) === 0
    );
  }

  function renderFindings(report) {
    const findings = report?.findings || [];
    const box = el('div', { class: 'market-findings' },
      el('h5', {}, 'Scan verdict: ', pill(report?.verdict || 'caution', VERDICT_LABELS[report?.verdict] || report?.verdict || 'Unknown')));
    if (!findings.length) {
      box.append(el('p', { class: 'market-meta', text: `Scanned ${report?.files_scanned ?? 0} file(s); no findings.` }));
      return box;
    }
    const grouped = new Map();
    for (const f of findings) {
      const sev = f.severity || 'low';
      if (!grouped.has(sev)) grouped.set(sev, []);
      grouped.get(sev).push(f);
    }
    for (const sev of SEVERITIES) {
      const list = grouped.get(sev);
      if (!list?.length) continue;
      box.append(el('div', { class: 'market-finding-group' },
        el('h6', { text: `${sev} (${list.length})` }),
        el('ul', { class: 'market-finding-list' }, list.map(f =>
          el('li', {},
            el('strong', { text: f.rule }),
            ' · ',
            el('code', { text: `${f.file}:${f.line}` }),
            f.excerpt ? [' — ', el('code', { text: f.excerpt })] : null
          )
        ))
      ));
    }
    return box;
  }

  function renderStagePanel(item, state) {
    const panel = el('div', { class: 'market-stage-panel' });
    if (state.loading) {
      panel.append(el('p', { class: 'market-meta', text: 'Downloading and scanning…' }));
      return panel;
    }
    if (state.error) {
      panel.append(el('p', { class: 'market-meta market-error', text: state.error }));
    }
    if (state.pluginNotice) {
      panel.append(el('div', { class: 'market-notice' },
        el('span', { text: 'Installed. It is quarantined: approve its files in Skills & plugins before it can run.' }),
        el('button', {
          type: 'button',
          class: 'tm-btn',
          onclick: () => window.sailNavigate?.('agents', 'skills'),
          text: 'Open Skills & plugins',
        })
      ));
    }
    const r = state.report;
    if (!r) return panel;

    const hash = r.content_hash || '';
    panel.append(el('div', { class: 'market-stage-meta' },
      el('span', {}, 'Revision: ', el('code', { text: r.revision || '—' })),
      el('span', {}, 'Hash: ', el('code', { title: hash, text: hash ? `${hash.slice(0, 12)}…` : '—' }))
    ));

    if (r.files?.length) {
      panel.append(el('ul', { class: 'market-files-list', 'aria-label': 'Staged files' },
        r.files.map(f => el('li', {}, el('code', { text: f.path }), ` (${formatBytes(f.bytes)})`))
      ));
    }

    panel.append(renderFindings(r.report));

    if (r.preview != null && r.preview !== '') {
      panel.append(el('div', { class: 'market-preview' },
        el('h5', { text: item.kind === 'plugin' ? 'README' : 'SKILL.md' }),
        el('pre', { text: r.preview })
      ));
    }

    const actions = el('div', { class: 'market-stage-actions' });
    const buttons = el('div', { class: 'market-stage-buttons' });

    if (r.decision === 'allow') {
      buttons.append(el('button', {
        type: 'button',
        class: 'tm-btn',
        onclick: () => installStaged(item, r.stage_id, false),
        text: 'Install',
      }));
    } else if (r.decision === 'needs_ack') {
      const ackId = `marketAck-${r.stage_id}`;
      const ack = el('input', { id: ackId, type: 'checkbox' });
      const installBtn = el('button', {
        type: 'button',
        class: 'tm-btn',
        disabled: true,
        onclick: () => installStaged(item, r.stage_id, true),
        text: 'Install anyway',
      });
      ack.addEventListener('change', () => { installBtn.disabled = !ack.checked; });
      actions.append(el('label', { class: 'market-check', for: ackId },
        ack,
        el('span', { text: 'I have read the findings and want to install it anyway' })
      ));
      buttons.append(installBtn);
    } else if (r.decision === 'block') {
      actions.append(el('p', { class: 'market-meta market-error', text: r.reason || 'Blocked by security policy.' }));
    }

    buttons.append(el('button', {
      type: 'button',
      class: 'tm-btn',
      onclick: () => discardStaged(item, r.stage_id),
      text: 'Discard',
    }));
    actions.append(buttons);
    panel.append(actions);
    return panel;
  }

  function renderResults() {
    const container = $('#marketResults');
    container.replaceChildren();

    const firstRun = isFirstRun(lastStatus);
    $('#marketEmptyFirstRun').hidden = !firstRun;
    $('#marketMoreWrap').hidden = firstRun || items.length >= total;

    if (firstRun) return;

    if (!items.length) {
      container.append(el('p', { class: 'market-meta', text: 'No matching skills or plugins.' }));
      return;
    }

    for (const item of items) {
      const key = itemKey(item);
      const stageState = stagedByItem.get(key);
      const unsupported = item.route?.type === 'unsupported';
      const badges = [
        pill(item.trust || 'community', TRUST_LABELS[item.trust] || item.trust || 'Community'),
        pill('kind', item.kind === 'plugin' ? 'Plugin' : 'Skill'),
      ];
      if (item.installed) badges.push(pill('installed', 'Installed'));

      const row = el('article', {
        class: `market-row${stageState ? ' is-active' : ''}`,
        'data-market-id': item.id,
        'data-market-kind': item.kind,
      },
        el('div', { class: 'market-row-head' },
          el('div', { class: 'market-row-title' },
            el('h4', { text: item.name }),
            el('span', { class: 'market-meta', text: [item.source, item.category].filter(Boolean).join(' · ') })
          ),
          el('div', { class: 'market-row-badges' }, badges)
        ),
        item.description ? el('p', { class: 'market-desc', text: item.description }) : null,
        el('div', { class: 'market-row-foot' },
          unsupported
            ? el('span', { class: 'market-unsupported-reason', text: item.route.reason || 'Unsupported source' })
            : el('span', { class: 'market-meta', text: item.id }),
          el('div', { class: 'market-row-actions' },
            el('button', {
              type: 'button',
              class: 'tm-btn',
              disabled: unsupported || Boolean(stageState?.loading),
              title: unsupported ? (item.route.reason || 'Unsupported') : undefined,
              onclick: () => stageItem(item),
              text: stageState?.loading ? 'Scanning…' : 'Review',
            })
          )
        )
      );

      if (stageState) {
        row.append(renderStagePanel(item, stageState));
      }
      container.append(row);
    }
  }

  function renderInstalled(list) {
    const summary = $('#marketInstalledSummary');
    const container = $('#marketInstalledList');
    summary.textContent = `Installed (${list.length})`;
    container.replaceChildren();

    if (!list.length) {
      container.append(el('p', { class: 'market-meta', text: 'Nothing installed from the market yet.' }));
      return;
    }

    for (const inst of list) {
      const e = inst.entry || {};
      const state = inst.state || 'ok';
      const stateLabel = STATE_LABELS[state] || state;
      const awaitingConfirm = confirmUninstall.has(inst.key);

      const node = el('article', { class: 'market-installed-item', 'data-installed-key': inst.key },
        el('div', { class: 'market-installed-head' },
          el('strong', { text: e.name || inst.key }),
          el('div', { class: 'market-row-badges' },
            pill('kind', e.kind === 'plugin' ? 'Plugin' : 'Skill'),
            pill(e.trust || 'community', TRUST_LABELS[e.trust] || e.trust || 'Community'),
            pill(state, stateLabel)
          )
        ),
        el('p', {
          class: 'market-meta',
          text: `Installed ${formatDate(e.installed_at)} · Updated ${formatDate(e.updated_at)}`,
        }),
        inst.removed
          ? el('p', {
              class: 'market-meta market-error',
              text: `Removed from catalog (${inst.removed.date || 'unknown date'}): ${inst.removed.reason || inst.removed.name}`,
            })
          : null,
        el('div', { class: 'market-installed-actions' },
          awaitingConfirm
            ? [
                el('span', { class: 'market-meta market-error', text: 'Files were edited after install. Uninstall anyway?' }),
                el('button', {
                  type: 'button',
                  class: 'tm-btn',
                  onclick: () => uninstallItem(inst.key, true),
                  text: 'Confirm uninstall',
                }),
                el('button', {
                  type: 'button',
                  class: 'tm-btn',
                  onclick: () => { confirmUninstall.delete(inst.key); loadInstalled(); },
                  text: 'Cancel',
                }),
              ]
            : el('button', {
                type: 'button',
                class: 'tm-btn',
                onclick: () => {
                  if (state === 'modified') {
                    confirmUninstall.add(inst.key);
                    loadInstalled();
                  } else {
                    uninstallItem(inst.key, false);
                  }
                },
                text: 'Uninstall',
              })
        )
      );
      container.append(node);
    }
  }

  function currentQueryParams(offset = 0) {
    const params = new URLSearchParams();
    const text = $('#marketQuery').value.trim();
    const kind = $('#marketKind').value;
    const source = $('#marketSource').value;
    const trustedOnly = $('#marketTrustedOnly').checked;
    if (text) params.set('text', text);
    if (kind) params.set('kind', kind);
    if (source) params.set('source', source);
    if (trustedOnly) params.set('min_trust', 'trusted');
    params.set('offset', String(offset));
    params.set('limit', String(PAGE_SIZE));
    return params.toString();
  }

  async function searchMarket(append = false) {
    setBusy(true);
    setStatus(append ? 'Loading more…' : 'Loading market…');
    try {
      const offset = append ? items.length : 0;
      const data = await api('/?' + currentQueryParams(offset));
      lastStatus = data.status;
      const page = data.page || { total: 0, items: [] };
      total = page.total || 0;
      items = append ? items.concat(page.items || []) : (page.items || []);
      renderCatalogLine(lastStatus);
      renderResults();
      setStatus(isFirstRun(lastStatus)
        ? 'The market has not been downloaded yet.'
        : `Showing ${items.length} of ${total} matching item${total === 1 ? '' : 's'}.`);
    } catch (err) {
      setStatus(err.message, true);
    } finally {
      setBusy(false);
    }
  }

  async function loadInstalled() {
    try {
      const data = await api('/installed');
      renderInstalled(data.installed || []);
    } catch (err) {
      setStatus(err.message, true);
    }
  }

  async function refreshCatalog() {
    refreshing = true;
    setBusy(true);
    renderCatalogLine(lastStatus);
    setStatus('Downloading market catalogs…');
    try {
      const data = await api('/refresh', { force: true });
      lastStatus = data.status;
      refreshing = false;
      renderCatalogLine(lastStatus);
      await Promise.all([searchMarket(false), loadInstalled()]);
    } catch (err) {
      refreshing = false;
      if (err.data?.status) lastStatus = err.data.status;
      renderCatalogLine(lastStatus);
      setStatus(err.message, true);
    } finally {
      setBusy(false);
    }
  }

  async function stageItem(item) {
    const key = itemKey(item);
    stagedByItem.set(key, { loading: true, error: null, report: null, pluginNotice: false });
    renderResults();
    setStatus(`Staging ${item.name}…`);
    try {
      const report = await api('/stage', { kind: item.kind, id: item.id });
      stagedByItem.set(key, { loading: false, error: null, report, pluginNotice: false });
      renderResults();
      setStatus(`Staged ${item.name}. Review the scan findings before installing.`);
    } catch (err) {
      stagedByItem.set(key, { loading: false, error: err.message, report: null, pluginNotice: false });
      renderResults();
      setStatus(err.message, true);
    }
  }

  async function installStaged(item, stageId, acknowledgeCaution) {
    const key = itemKey(item);
    const prev = stagedByItem.get(key) || {};
    setStatus(`Installing ${item.name}…`);
    try {
      await api('/install', { stage_id: stageId, acknowledge_caution: Boolean(acknowledgeCaution) });
      if (item.kind === 'plugin') {
        stagedByItem.set(key, { loading: false, error: null, report: null, pluginNotice: true });
      } else {
        stagedByItem.delete(key);
      }
      await Promise.all([searchMarket(false), loadInstalled()]);
      setStatus(item.kind === 'plugin'
        ? `Installed ${item.name} in quarantine. Approve its files in Skills & plugins before it can run.`
        : `Installed ${item.name}.`);
    } catch (err) {
      stagedByItem.set(key, { ...prev, error: err.message });
      renderResults();
      setStatus(err.message, true);
    }
  }

  async function discardStaged(item, stageId) {
    const key = itemKey(item);
    try {
      await api('/discard', { stage_id: stageId });
      stagedByItem.delete(key);
      renderResults();
      setStatus(`Discarded ${item.name}.`);
    } catch (err) {
      setStatus(err.message, true);
    }
  }

  async function uninstallItem(key, force) {
    setStatus(`Uninstalling ${key}…`);
    try {
      await api('/uninstall', { key, force: Boolean(force) });
      confirmUninstall.delete(key);
      await Promise.all([searchMarket(false), loadInstalled()]);
      setStatus(`Uninstalled ${key}.`);
    } catch (err) {
      setStatus(err.message, true);
    }
  }

  async function loadAll() {
    if (busy) return;
    await Promise.all([searchMarket(false), loadInstalled()]);
  }

  $('#marketRefresh').onclick = refreshCatalog;
  $('#marketDownloadBtn').onclick = refreshCatalog;
  $('#marketMoreBtn').onclick = () => searchMarket(true);
  $('#marketKind').onchange = () => searchMarket(false);
  $('#marketSource').onchange = () => searchMarket(false);
  $('#marketTrustedOnly').onchange = () => searchMarket(false);
  $('#marketQuery').oninput = () => {
    clearTimeout(queryTimer);
    queryTimer = setTimeout(() => searchMarket(false), 250);
  };

  const shown = () => !!document.getElementById('subsystemContent')?.contains(card);
  for (const event of ['sail:page', 'sail:section']) {
    window.addEventListener(event, () => { if (shown()) loadAll(); });
  }
  if (shown()) loadAll();
})();

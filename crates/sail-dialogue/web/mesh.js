'use strict';
// SAIL Mesh: paired worker PCs, health/activity status, pairing form, and
// enable/forget controls inside Settings -> Models.
(() => {
  const mount = document.getElementById('meshWorkers');
  if (!mount) return;

  const MODE_DESCRIPTIONS = {
    offload: 'Background work goes to a free worker first',
    overflow: 'Workers help only while this machine is busy',
    off: 'Off — everything runs on this machine (set SAIL_MESH)',
  };

  const STATE_LABELS = {
    ready: 'Ready',
    model_unavailable: 'Model not loaded',
    busy: 'Resting',
    unreachable: 'Unreachable',
    unknown: 'Checking…',
    disabled: 'Off',
  };

  const AUTO_REFRESH_MS = 15000;

  let token = null;
  let loading = false;
  let refreshing = false;
  let pairing = false;
  let lastStatus = null;
  let bannerMessage = null;
  let bannerTone = 'error';
  let confirmingForget = null;
  let pendingWorkerAction = null;
  let refreshTimer = null;

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

  async function ownerToken(force) {
    if (!force && token) return token;
    if (!force) {
      try {
        const t = localStorage.getItem('sail_owner_token');
        if (t && t.trim().length >= 32) return (token = t.trim());
      } catch (_) {}
    }
    let r;
    try {
      r = await fetch('/api/guardian/token', { cache: 'no-store' });
    } catch (_) {
      const err = new Error("Couldn't reach SAIL — showing the last known state");
      err.kind = 'network';
      throw err;
    }
    if (!r.ok) {
      const err = new Error('Owner sign-in needed');
      err.kind = r.status === 401 ? 'auth' : 'http';
      throw err;
    }
    const data = await r.json();
    token = data.token;
    try {
      localStorage.setItem('sail_owner_token', token);
    } catch (_) {}
    return token;
  }

  async function apiFetch(path, options = {}) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const t = await ownerToken(attempt > 0);
      const headers = {
        Authorization: `Bearer ${t}`,
        ...(options.headers || {}),
      };
      let r;
      try {
        r = await fetch(`/api/mind/mesh${path}`, {
          ...options,
          cache: 'no-store',
          headers,
        });
      } catch (_) {
        const err = new Error("Couldn't reach SAIL — showing the last known state");
        err.kind = 'network';
        throw err;
      }

      if (r.status === 401 && attempt === 0) {
        token = null;
        try {
          localStorage.removeItem('sail_owner_token');
        } catch (_) {}
        continue;
      }
      if (r.status === 401) {
        const err = new Error('Owner sign-in needed');
        err.kind = 'auth';
        throw err;
      }
      if (r.status === 503) {
        const err = new Error('Mesh is not available in this SAIL process');
        err.kind = 'unavailable';
        throw err;
      }
      if (r.status === 204) {
        return null;
      }

      const contentType = r.headers.get('content-type') || '';
      let data = null;
      let rawText = '';
      if (contentType.includes('application/json')) {
        data = await r.json().catch(() => null);
      } else {
        rawText = await r.text().catch(() => '');
        try {
          data = JSON.parse(rawText);
        } catch (_) {}
      }

      if (!r.ok) {
        const msg =
          (data && typeof data.error === 'string' && data.error) ||
          (rawText && rawText.trim()) ||
          `HTTP ${r.status}`;
        const err = new Error(msg);
        err.kind = 'http';
        err.status = r.status;
        throw err;
      }
      return data;
    }
    const err = new Error('Owner sign-in needed');
    err.kind = 'auth';
    throw err;
  }

  function formatRelTime(iso) {
    const ms = Date.parse(iso);
    if (!Number.isFinite(ms)) return '—';
    const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
    if (s < 60) return `${s} s ago`;
    if (s < 3600) return `${Math.round(s / 60)} m ago`;
    if (s < 86400 * 2) return `${Math.round(s / 3600)} h ago`;
    return `${Math.round(s / 86400)} d ago`;
  }

  function formatContext(tokens) {
    const n = Number(tokens);
    if (!Number.isFinite(n) || n <= 0) return '';
    if (n >= 1024 && n % 1024 === 0) return `${n / 1024}k ctx`;
    if (n >= 1000) return `${Math.round(n / 1000)}k ctx`;
    return `${n} ctx`;
  }

  // Persistent layout containers so 15s auto-refresh never clears the pair inputs.
  const statusRegion = el('div', { class: 'mesh-status-region' });
  const pairErrorEl = el('p', {
    class: 'mesh-error',
    id: 'meshPairError',
    role: 'alert',
    'aria-live': 'polite',
    hidden: true,
  });
  const codeInput = el('input', {
    type: 'password',
    id: 'meshCodeInput',
    class: 'mesh-input',
    placeholder: 'sailmesh://…',
    autocomplete: 'off',
    spellcheck: 'false',
    'aria-label': 'Worker pairing code',
    'aria-describedby': 'meshPairError',
  });
  const codeToggleBtn = el('button', {
    type: 'button',
    id: 'meshCodeToggle',
    class: 'quiet-button mesh-toggle-btn',
    text: 'Show',
    'aria-pressed': 'false',
    'aria-label': 'Show pairing code',
    onclick: () => {
      const showing = codeInput.type === 'text';
      codeInput.type = showing ? 'password' : 'text';
      codeToggleBtn.textContent = showing ? 'Show' : 'Hide';
      codeToggleBtn.setAttribute('aria-pressed', showing ? 'false' : 'true');
      codeToggleBtn.setAttribute(
        'aria-label',
        showing ? 'Show pairing code' : 'Hide pairing code'
      );
    },
  });
  const servesInput = el('input', {
    type: 'text',
    id: 'meshServesInput',
    class: 'mesh-input',
    placeholder: 'e.g. qwen, llama (optional)',
    autocomplete: 'off',
    'aria-label': 'Only for models',
  });
  const pairSubmitBtn = el('button', {
    type: 'submit',
    id: 'meshPairBtn',
    class: 'quiet-button mesh-pair-btn',
    text: 'Pair',
  });

  const pairForm = el(
    'form',
    {
      class: 'mesh-pair-form',
      id: 'meshPairForm',
      onsubmit: (e) => {
        e.preventDefault();
        submitPair();
      },
    },
    el('p', { class: 'mesh-section-label', text: 'Pair a worker PC' }),
    el(
      'div',
      { class: 'mesh-pair-fields' },
      el(
        'div',
        { class: 'mesh-field mesh-field-code' },
        el('label', { class: 'mesh-field-label', for: 'meshCodeInput', text: 'Pairing code' }),
        el('div', { class: 'mesh-code-row' }, codeInput, codeToggleBtn)
      ),
      el(
        'div',
        { class: 'mesh-field mesh-field-serves' },
        el('label', { class: 'mesh-field-label', for: 'meshServesInput', text: 'Only for models' }),
        servesInput
      ),
      el('div', { class: 'mesh-pair-actions' }, pairSubmitBtn)
    ),
    pairErrorEl
  );

  mount.replaceChildren(statusRegion, pairForm);

  function setPairError(message) {
    if (!message) {
      pairErrorEl.textContent = '';
      pairErrorEl.hidden = true;
    } else {
      pairErrorEl.textContent = message;
      pairErrorEl.hidden = false;
    }
  }

  function updatePairControls() {
    pairSubmitBtn.disabled = pairing;
    pairSubmitBtn.textContent = pairing ? 'Checking the worker…' : 'Pair';
    codeInput.disabled = pairing;
    servesInput.disabled = pairing;
  }

  function renderWorkerRow(w) {
    const stateKey = STATE_LABELS[w.state] ? w.state : 'unknown';
    const stateLabel = STATE_LABELS[stateKey];
    const isDisabled = stateKey === 'disabled';
    const isBusy = pendingWorkerAction === w.name;

    const identity = el(
      'div',
      { class: 'mesh-worker-id' },
      el('span', { class: 'mesh-worker-name', text: w.name }),
      el('span', { class: 'mesh-worker-addr', text: w.address }),
      w.os ? el('span', { class: 'mesh-worker-os', text: w.os }) : null
    );

    const pill = el('span', {
      class: 'mesh-pill',
      'data-state': stateKey,
      text: stateLabel,
    });

    const top = el('div', { class: 'mesh-worker-top' }, identity, pill);

    const details = [];
    if (w.model) {
      const badges = [];
      const ctxText = formatContext(w.model.context_tokens);
      if (ctxText) badges.push(el('span', { class: 'mesh-badge', text: ctxText }));
      if (w.model.tools) badges.push(el('span', { class: 'mesh-badge', text: 'Tools' }));
      if (w.model.vision) badges.push(el('span', { class: 'mesh-badge', text: 'Vision' }));

      details.push(
        el(
          'div',
          { class: 'mesh-worker-model' },
          el('span', { class: 'mesh-model-name', text: w.model.file_name }),
          badges
        )
      );
    }

    const active = Number(w.active) || 0;
    const maxParallel = Number(w.max_parallel) || 0;
    const completed = Number(w.completed) || 0;
    const failed = Number(w.failed) || 0;

    const metricParts = [
      `running ${active} of ${maxParallel}`,
      `${completed} completed · ${failed} failed`,
    ];
    if (w.last_seen) {
      const rtt = w.last_rtt_ms !== undefined && w.last_rtt_ms !== null ? ` (${w.last_rtt_ms} ms)` : '';
      metricParts.push(`last check ${formatRelTime(w.last_seen)}${rtt}`);
    } else if (w.last_rtt_ms !== undefined && w.last_rtt_ms !== null) {
      metricParts.push(`${w.last_rtt_ms} ms`);
    }

    details.push(
      el('p', {
        class: 'mesh-worker-metrics',
        text: metricParts.join(' · '),
      })
    );

    if (stateKey === 'busy' && w.busy) {
      // The worker stepped aside because its owner is using that PC.
      let when = '';
      if (w.busy.full) when = ' · back when it ends';
      else if (Number(w.busy.retry_after_secs) > 0) {
        const s = Number(w.busy.retry_after_secs);
        when = s < 90 ? ` · checks again in ${s} s` : ` · checks again in ${Math.round(s / 60)} min`;
      }
      details.push(
        el('p', {
          class: 'mesh-worker-rest',
          text: `Stepped aside: ${w.busy.reason}${when}`,
        })
      );
    } else if (w.last_error) {
      details.push(
        el('p', {
          class: 'mesh-worker-error',
          text: w.last_error,
        })
      );
    }

    const toggleBtn = el('button', {
      type: 'button',
      class: 'quiet-button',
      text: isDisabled ? 'Turn on' : 'Turn off',
      disabled: isBusy,
      onclick: () => toggleWorker(w.name, isDisabled),
    });

    let forgetControl;
    if (confirmingForget === w.name) {
      forgetControl = el(
        'span',
        { class: 'mesh-forget-confirm' },
        el('span', { class: 'mesh-confirm-text', text: `Forget ${w.name}?` }),
        el('button', {
          type: 'button',
          class: 'quiet-button mesh-danger-btn',
          text: 'Confirm forget',
          disabled: isBusy,
          onclick: () => forgetWorker(w.name),
        }),
        el('button', {
          type: 'button',
          class: 'quiet-button',
          text: 'Cancel',
          disabled: isBusy,
          onclick: () => {
            confirmingForget = null;
            renderStatus();
          },
        })
      );
    } else {
      forgetControl = el('button', {
        type: 'button',
        class: 'quiet-button',
        text: 'Forget',
        disabled: isBusy,
        onclick: () => {
          confirmingForget = w.name;
          renderStatus();
        },
      });
    }

    const actions = el('div', { class: 'mesh-worker-actions' }, toggleBtn, forgetControl);

    return el(
      'li',
      { class: 'mesh-worker-row', 'data-worker-name': w.name },
      top,
      details,
      actions
    );
  }

  function renderStatus() {
    const refreshBtn = el('button', {
      type: 'button',
      id: 'meshRefreshBtn',
      class: 'quiet-button',
      text: refreshing ? 'Refreshing…' : 'Refresh',
      disabled: refreshing || loading,
      onclick: () => refreshNow(),
    });

    const head = el(
      'div',
      { class: 'mesh-head' },
      el('label', { class: 'vscode-setting-name', text: 'Worker PCs' }),
      refreshBtn
    );

    const children = [head];

    if (bannerMessage) {
      children.push(
        el('p', {
          class: bannerTone === 'warn' ? 'mesh-banner mesh-banner-warn' : 'mesh-banner mesh-banner-error',
          id: 'meshBanner',
          role: 'status',
          'aria-live': 'polite',
          text: bannerMessage,
        })
      );
    }

    if (lastStatus) {
      const modeText = MODE_DESCRIPTIONS[lastStatus.mode] || String(lastStatus.mode || 'off');
      const offloaded = Number(lastStatus.offloaded) || 0;
      const fellBack = Number(lastStatus.fell_back) || 0;

      children.push(
        el('p', {
          class: 'mesh-summary-line',
          id: 'meshModeLine',
          text: modeText,
        }),
        el('p', {
          class: 'mesh-totals-line',
          id: 'meshTotalsLine',
          text: `${offloaded} requests ran on workers · ${fellBack} fell back to this machine`,
        })
      );

      const workers = Array.isArray(lastStatus.workers) ? lastStatus.workers : [];
      if (workers.length === 0) {
        children.push(
          el(
            'div',
            { class: 'mesh-empty', id: 'meshEmptyState' },
            el('p', {
              class: 'mesh-empty-lead',
              text: 'No worker PCs paired yet.',
            }),
            el(
              'p',
              { class: 'mesh-empty-steps' },
              'On the other PC: ',
              el('code', { text: 'sail-mesh-worker init' }),
              ', then ',
              el('code', { text: 'sail-mesh-worker serve --llama-server <path> --model <gguf>' }),
              '; paste the code it prints here.'
            )
          )
        );
      } else {
        children.push(
          el(
            'ul',
            { class: 'mesh-workers-list', id: 'meshWorkersList' },
            workers.map((w) => renderWorkerRow(w))
          )
        );
      }
    }

    statusRegion.replaceChildren(...children);
  }

  function applyError(err) {
    if (err.kind === 'auth') {
      lastStatus = null;
      bannerMessage = 'Owner sign-in needed';
      bannerTone = 'error';
    } else if (err.kind === 'unavailable') {
      lastStatus = null;
      bannerMessage = 'Mesh is not available in this SAIL process';
      bannerTone = 'error';
    } else if (err.kind === 'network') {
      bannerMessage = "Couldn't reach SAIL — showing the last known state";
      bannerTone = 'warn';
    } else {
      bannerMessage = err.message || 'Unable to load worker PCs.';
      bannerTone = 'error';
    }
    renderStatus();
  }

  async function load() {
    if (loading || refreshing) return;
    loading = true;
    renderStatus();
    try {
      const data = await apiFetch('', { method: 'GET' });
      lastStatus = data;
      bannerMessage = null;
      loading = false;
      renderStatus();
    } catch (err) {
      loading = false;
      applyError(err);
    }
  }

  async function refreshNow() {
    if (refreshing || loading) return;
    refreshing = true;
    renderStatus();
    try {
      const data = await apiFetch('/refresh', { method: 'POST' });
      lastStatus = data;
      bannerMessage = null;
      refreshing = false;
      renderStatus();
    } catch (err) {
      refreshing = false;
      applyError(err);
    }
  }

  async function submitPair() {
    if (pairing) return;
    const rawCode = codeInput.value.trim();
    if (!rawCode) {
      setPairError('Paste a sailmesh:// pairing code first.');
      codeInput.focus();
      return;
    }
    const serves = servesInput.value
      .split(',')
      .map((s) => s.trim())
      .filter((s) => s.length > 0);

    setPairError('');
    pairing = true;
    updatePairControls();

    try {
      const res = await apiFetch('/pair', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ code: rawCode, serves }),
      });

      // Never keep the pairing code in memory, storage, or the URL.
      codeInput.value = '';
      servesInput.value = '';
      codeInput.type = 'password';
      codeToggleBtn.textContent = 'Show';
      codeToggleBtn.setAttribute('aria-pressed', 'false');
      codeToggleBtn.setAttribute('aria-label', 'Show pairing code');

      if (!lastStatus) {
        lastStatus = {
          version: 1,
          mode: 'offload',
          offloaded: 0,
          fell_back: 0,
          workers: [],
        };
      }
      if (res && res.worker) {
        const list = Array.isArray(lastStatus.workers) ? [...lastStatus.workers] : [];
        const idx = list.findIndex((w) => w.name === res.worker.name);
        if (idx >= 0) list[idx] = res.worker;
        else list.push(res.worker);
        lastStatus = { ...lastStatus, workers: list };
      }
      bannerMessage = null;
      pairing = false;
      updatePairControls();
      renderStatus();
    } catch (err) {
      pairing = false;
      updatePairControls();
      if (err.kind === 'auth' || err.kind === 'unavailable' || err.kind === 'network') {
        applyError(err);
      } else {
        setPairError(err.message || 'Could not pair worker.');
      }
    }
  }

  async function toggleWorker(name, enable) {
    if (pendingWorkerAction) return;
    pendingWorkerAction = name;
    renderStatus();
    try {
      const res = await apiFetch(`/workers/${encodeURIComponent(name)}/enabled`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled: enable }),
      });
      if (lastStatus && res && res.worker) {
        const list = (lastStatus.workers || []).map((w) => (w.name === name ? res.worker : w));
        lastStatus = { ...lastStatus, workers: list };
      }
      bannerMessage = null;
      pendingWorkerAction = null;
      renderStatus();
    } catch (err) {
      pendingWorkerAction = null;
      applyError(err);
    }
  }

  async function forgetWorker(name) {
    if (pendingWorkerAction) return;
    pendingWorkerAction = name;
    renderStatus();
    try {
      await apiFetch(`/workers/${encodeURIComponent(name)}`, {
        method: 'DELETE',
      });
      confirmingForget = null;
      if (lastStatus && Array.isArray(lastStatus.workers)) {
        lastStatus = {
          ...lastStatus,
          workers: lastStatus.workers.filter((w) => w.name !== name),
        };
      }
      bannerMessage = null;
      pendingWorkerAction = null;
      renderStatus();
    } catch (err) {
      pendingWorkerAction = null;
      applyError(err);
    }
  }

  function startAutoRefresh() {
    if (refreshTimer !== null) return;
    refreshTimer = setInterval(() => {
      const panel = document.getElementById('settingsPanel-models');
      if (panel && !panel.hidden && !document.hidden) {
        load();
      }
    }, AUTO_REFRESH_MS);
  }

  function stopAutoRefresh() {
    if (refreshTimer === null) return;
    clearInterval(refreshTimer);
    refreshTimer = null;
  }

  function syncVisibility() {
    const panel = document.getElementById('settingsPanel-models');
    if (!panel || !panel.hidden) {
      load();
      startAutoRefresh();
    } else {
      stopAutoRefresh();
    }
  }

  function init() {
    renderStatus();
    const panel = document.getElementById('settingsPanel-models');
    if (panel && typeof MutationObserver === 'function') {
      new MutationObserver(() => syncVisibility()).observe(panel, {
        attributes: true,
        attributeFilter: ['hidden'],
      });
    }
    syncVisibility();
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();

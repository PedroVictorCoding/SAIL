// Settings > Models: choose the default reasoning model (GET/PUT /api/mind/models).
// SAIL keeps one resident model; a new default applies the next time SAIL starts.
// Pure helpers are exported for Node tests; DOM wiring runs only in the browser.
(function (root) {
  'use strict';

  /** The option label for one catalog entry. */
  function modelOptionLabel(model) {
    const size = `${model.size_gb} GB`;
    const active = model.active_params_b === model.total_params_b
      ? `${model.total_params_b}B`
      : `${model.total_params_b}B, ${model.active_params_b}B active`;
    const notes = [];
    if (model.running) notes.push('running');
    if (!model.installed) notes.push('not installed');
    return `${model.name} (${active} · ${size})${notes.length ? ` — ${notes.join(', ')}` : ''}`;
  }

  /** The status sentence under the picker. */
  function modelStatusText(view) {
    if (!view || !Array.isArray(view.models)) return 'Model list unavailable.';
    const byId = id => view.models.find(m => m.id === id);
    const running = byId(view.running);
    const chosen = byId(view.default);
    const runningName = running ? running.name : (view.running_file || 'an unknown model');
    if (view.env_override) {
      return `Running ${runningName}. SAIL_MODEL_PATH is set, so it overrides the saved default.`;
    }
    if (view.restart_required && chosen) {
      return `Running ${runningName}. ${chosen.name} is saved as the default and starts the next time SAIL starts.`;
    }
    return `Running ${runningName}, the saved default.`;
  }

  const exported = { modelOptionLabel, modelStatusText };
  if (typeof module !== 'undefined' && module.exports) module.exports = exported;
  if (typeof document === 'undefined') return;

  function ownerToken() {
    try { return (localStorage.getItem('sail_owner_token') || '').trim(); } catch (_) { return ''; }
  }

  async function api(method, path = '', body = null) {
    const headers = {};
    const token = ownerToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    if (body !== null) headers['Content-Type'] = 'application/json';
    const response = await fetch(`/api/mind/models${path}`, {
      method,
      headers,
      body: body !== null ? JSON.stringify(body) : undefined,
    });
    const data = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(data.error || `HTTP ${response.status}`);
    return data;
  }

  function render(view) {
    const select = document.getElementById('defaultModelSelect');
    const summary = document.getElementById('defaultModelSummary');
    const status = document.getElementById('defaultModelStatus');
    if (!select) return;
    select.replaceChildren(...view.models.map(model => {
      const option = document.createElement('option');
      option.value = model.id;
      option.textContent = modelOptionLabel(model);
      option.disabled = !model.installed;
      option.selected = model.id === view.default;
      return option;
    }));
    const describe = () => {
      const model = view.models.find(m => m.id === select.value);
      summary.textContent = model ? model.summary : '';
    };
    select.onchange = describe;
    describe();
    status.textContent = modelStatusText(view);
  }

  async function load() {
    const status = document.getElementById('defaultModelStatus');
    try {
      render(await api('GET', '/'));
    } catch (error) {
      if (status) status.textContent = `Model list unavailable: ${error.message}`;
    }
  }

  async function save() {
    const select = document.getElementById('defaultModelSelect');
    const status = document.getElementById('defaultModelStatus');
    const button = document.getElementById('defaultModelSave');
    button.disabled = true;
    status.textContent = 'Saving…';
    try {
      render(await api('PUT', '/default', { model: select.value }));
    } catch (error) {
      status.textContent = `Not saved: ${error.message}`;
    } finally {
      button.disabled = false;
    }
  }

  function init() {
    const button = document.getElementById('defaultModelSave');
    if (!button) return;
    button.addEventListener('click', save);
    // Load when the Models settings panel is first shown, and on demand after that.
    const panel = document.getElementById('settingsPanel-models');
    if (panel && typeof MutationObserver === 'function') {
      new MutationObserver(() => { if (!panel.hidden) load(); })
        .observe(panel, { attributes: true, attributeFilter: ['hidden'] });
    }
    if (!panel || !panel.hidden) load();
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
  root.SailModelPicker = { ...exported, load };
})(typeof window !== 'undefined' ? window : globalThis);

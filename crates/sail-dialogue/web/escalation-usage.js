'use strict';
// SAIL Escalation Usage: read-only status, token/cost meters, quota blocks, outcomes,
// and recent decisions inside Settings -> Models.
(() => {
  const mount = document.getElementById('escalationUsage');
  if (!mount) return;

  const DENY_REASONS = {
    disabled: 'escalation is turned off',
    voice_turn: 'live voice turns are not escalated',
    over_task_budget: 'over per-task token limit',
    not_enough_local_attempts: 'try locally first',
    over_daily_tokens: 'daily token limit reached',
    over_monthly_tokens: 'monthly token limit reached',
    over_daily_cost: 'daily cost limit reached',
    over_monthly_cost: 'monthly cost limit reached',
    cooldown: 'similar question escalated recently',
    in_flight: 'similar question already running',
    below_threshold: 'below escalation threshold',
    no_provider: 'no provider with quota available',
    invalid: 'invalid escalation request',
    redaction_failed: 'private data could not be redacted',
  };

  let token = null;
  let loading = false;

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
    const r = await fetch('/api/guardian/token', { cache: 'no-store' });
    if (!r.ok) throw new Error('Owner token unavailable.');
    const data = await r.json();
    token = data.token;
    try {
      localStorage.setItem('sail_owner_token', token);
    } catch (_) {}
    return token;
  }

  async function fetchStatus() {
    for (let attempt = 0; attempt < 2; attempt++) {
      const t = await ownerToken(attempt > 0);
      const r = await fetch('/api/mind/escalation/status', {
        method: 'GET',
        cache: 'no-store',
        headers: { Authorization: `Bearer ${t}` },
      });
      if (r.status === 401 && attempt === 0) {
        token = null;
        try {
          localStorage.removeItem('sail_owner_token');
        } catch (_) {}
        continue;
      }
      if (r.status === 404) {
        return { notRunning: true };
      }
      const data = await r.json().catch(() => ({}));
      if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
      return data;
    }
    throw new Error('Owner authentication failed.');
  }

  function formatTokens(n) {
    const num = Number(n) || 0;
    if (num >= 1_000_000) return `${(num / 1_000_000).toFixed(1)}M`;
    if (num >= 1_000) return `${(num / 1_000).toFixed(1)}k`;
    return String(num);
  }

  function formatEuro(micros) {
    return `€${((Number(micros) || 0) / 1_000_000).toFixed(2)}`;
  }

  function formatKind(kind) {
    if (!kind) return 'Task';
    const text = String(kind).replace(/_/g, ' ');
    return text.charAt(0).toUpperCase() + text.slice(1);
  }

  function formatRelTime(iso) {
    const ms = Date.parse(iso);
    if (!Number.isFinite(ms)) return '—';
    const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
    if (s < 90) return 'just now';
    if (s < 3600) return `${Math.round(s / 60)}m ago`;
    if (s < 86400 * 2) return `${Math.round(s / 3600)}h ago`;
    return `${Math.round(s / 86400)}d ago`;
  }

  function formatUntilClock(iso) {
    const ms = Date.parse(iso);
    if (!Number.isFinite(ms)) return iso;
    return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }

  function renderMeter(label, tokens, tokenCap, costMicros, costCapMicros) {
    const tokenText =
      tokenCap > 0
        ? `${formatTokens(tokens)} / ${formatTokens(tokenCap)} tokens`
        : `${formatTokens(tokens)} tokens`;
    const costText =
      costCapMicros > 0
        ? ` · ${formatEuro(costMicros)} / ${formatEuro(costCapMicros)}`
        : '';

    const children = [
      el(
        'div',
        { class: 'esc-meter-row' },
        el('span', { class: 'esc-meter-label', text: label }),
        el('span', { class: 'esc-meter-vals', text: `${tokenText}${costText}` })
      ),
    ];

    if (tokenCap > 0) {
      const pct = Math.min(100, Math.max(0, Math.round((tokens / tokenCap) * 100)));
      children.push(
        el(
          'div',
          { class: 'esc-bar', role: 'progressbar', 'aria-valuenow': pct, 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-label': `${label} token usage` },
          el('div', {
            class: `esc-bar-fill ${pct >= 80 ? 'near-cap' : ''}`,
            style: `width:${pct}%`,
          })
        )
      );
    }

    return el('div', { class: 'esc-meter' }, children);
  }

  function renderDecisionText(rec) {
    const when = formatRelTime(rec.at);
    const kind = formatKind(rec.kind);
    const dec = rec.decision || {};
    if (dec.decision === 'escalate') {
      const prov = dec.provider ? ` (${dec.provider})` : '';
      return `${when} · ${kind} · Escalated${prov}`;
    }
    if (dec.decision === 'cached') {
      return `${when} · ${kind} · Cached`;
    }
    if (dec.decision === 'deny') {
      const phrase = DENY_REASONS[dec.reason] || (dec.reason ? String(dec.reason).replace(/_/g, ' ') : 'denied');
      return `${when} · ${kind} · Denied — ${phrase}`;
    }
    return `${when} · ${kind}`;
  }

  function renderStatus(data) {
    const refreshBtn = el('button', {
      type: 'button',
      class: 'quiet-button',
      text: loading ? 'Refreshing…' : 'Refresh',
      disabled: loading,
      onclick: () => load(),
    });

    const head = el(
      'div',
      { class: 'esc-head' },
      el('label', { class: 'vscode-setting-name', text: 'Stronger-Model Escalation' }),
      refreshBtn
    );

    if (data && data.notRunning) {
      mount.replaceChildren(
        head,
        el('p', { class: 'esc-status-line', text: 'Stronger-model escalation is off.' })
      );
      return;
    }

    const providers = Array.isArray(data.providers) ? data.providers : [];
    const statusLine = data.enabled
      ? providers.length > 0
        ? `On · providers: ${providers.join(', ')}`
        : 'On · no providers configured'
      : 'Off — enable in $SAIL_DATA/mind/escalation.json';

    const usage = data.usage || {};
    const meters = el(
      'div',
      { class: 'esc-meters' },
      renderMeter(
        'Today',
        usage.tokens_today || 0,
        data.daily_token_cap || 0,
        usage.cost_today_micros || 0,
        data.daily_cost_cap_micros || 0
      ),
      renderMeter(
        'This month',
        usage.tokens_month || 0,
        data.monthly_token_cap || 0,
        usage.cost_month_micros || 0,
        data.monthly_cost_cap_micros || 0
      )
    );

    const children = [head, el('p', { class: 'esc-status-line', text: statusLine }), meters];

    const blocked = Array.isArray(usage.blocked_until)
      ? usage.blocked_until.filter((entry) => Array.isArray(entry) && Date.parse(entry[1]) > Date.now())
      : [];
    if (blocked.length > 0) {
      children.push(el('p', { class: 'esc-section-label', text: 'Out of quota' }));
      children.push(
        el(
          'ul',
          { class: 'esc-rows' },
          blocked.map(([id, iso]) =>
            el('li', {
              class: 'esc-row esc-row-warn',
              text: `${id} — until ${formatUntilClock(iso)}`,
            })
          )
        )
      );
    }

    const byKind = Array.isArray(data.outcomes?.by_kind) ? data.outcomes.by_kind : [];
    const nonEmptyKinds = byKind.filter(([, imp, notImp]) => (Number(imp) || 0) + (Number(notImp) || 0) > 0);
    if (nonEmptyKinds.length > 0) {
      children.push(el('p', { class: 'esc-section-label', text: 'Outcomes' }));
      children.push(
        el(
          'ul',
          { class: 'esc-rows' },
          nonEmptyKinds.map(([kind, imp, notImp]) => {
            const total = (Number(imp) || 0) + (Number(notImp) || 0);
            return el('li', {
              class: 'esc-row',
              text: `${formatKind(kind)} ${Number(imp) || 0}/${total} helped`,
            });
          })
        )
      );
    }

    const decisions = Array.isArray(data.recent_decisions) ? data.recent_decisions.slice(0, 8) : [];
    if (decisions.length > 0) {
      children.push(el('p', { class: 'esc-section-label', text: 'Recent decisions' }));
      children.push(
        el(
          'ul',
          { class: 'esc-rows' },
          decisions.map((rec) => el('li', { class: 'esc-row', text: renderDecisionText(rec) }))
        )
      );
    }

    mount.replaceChildren(...children);
  }

  function renderError(message) {
    mount.replaceChildren(
      el(
        'div',
        { class: 'esc-head' },
        el('label', { class: 'vscode-setting-name', text: 'Stronger-Model Escalation' }),
        el('button', {
          type: 'button',
          class: 'quiet-button',
          text: 'Refresh',
          onclick: () => load(),
        })
      ),
      el('p', { class: 'esc-error', text: message || 'Unable to load escalation status.' })
    );
  }

  async function load() {
    if (loading) return;
    loading = true;
    try {
      const data = await fetchStatus();
      loading = false;
      renderStatus(data);
    } catch (err) {
      loading = false;
      renderError(err.message);
    }
  }

  function init() {
    const panel = document.getElementById('settingsPanel-models');
    if (panel && typeof MutationObserver === 'function') {
      new MutationObserver(() => {
        if (!panel.hidden) load();
      }).observe(panel, { attributes: true, attributeFilter: ['hidden'] });
    }
    if (!panel || !panel.hidden) load();
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();

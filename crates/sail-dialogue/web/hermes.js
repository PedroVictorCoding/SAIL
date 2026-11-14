/* SAIL Agents › Skills & plugins: Hermes Agent plugins (owner approval) and Agent Skills.
   The server's /api/hermes answers are the only state; nothing here runs a plugin tool. */
'use strict';
(() => {
  const card = document.getElementById('hermesCard');
  if (!card) return;
  const $ = s => card.querySelector(s);
  let busy = false, loadedOnce = false, query = '', queryTimer = null, envPollTimer = null;

  function el(tag, attrs = {}, ...children) {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
      else if (k === 'class') n.className = v;
      else if (k === 'text') n.textContent = v;
      else if (v !== false && v != null) n.setAttribute(k, v === true ? '' : v);
    }
    for (const c of children.flat()) if (c != null && c !== false) n.append(c instanceof Node ? c : document.createTextNode(String(c)));
    return n;
  }
  function status(text, error = false) {
    $('#hermesStatus').textContent = text;
    $('#hermesStatus').classList.toggle('hermes-error', error);
  }
  async function api(path, body) {
    const token = await getOrFetchOwnerToken();
    if (!token) throw Error('Owner authorization unavailable. Reconnect and try again.');
    const r = await fetch('/api/hermes' + path, {
      method: body ? 'POST' : 'GET', cache: 'no-store',
      headers: {Authorization: `Bearer ${token}`, ...(body ? {'Content-Type': 'application/json'} : {})},
      ...(body ? {body: JSON.stringify(body)} : {}),
    });
    const data = await r.json().catch(() => ({}));
    if (!r.ok) throw Error(data.error || `Skills and plugins unavailable (${r.status})`);
    return data;
  }

  const STATES = {
    quarantined: ['Not approved', 'Its code has not run. Review it, then approve these exact files to enable it.'],
    changed: ['Changed', 'Its files changed after you approved it, so it is disabled until you review and approve it again.'],
    enabled: ['Enabled', null],
    unsupported: ['Not supported', null],
    invalid: ['Invalid', null],
  };
  const SOURCES = {hermes: 'Hermes home', sail: 'SAIL data'};
  const HOOK_DESCRIPTIONS = {
    pre_llm_call: 'Sees your message for this turn, not your history.',
    post_llm_call: "Sees your message and SAIL's reply for this turn, not your history.",
    pre_tool_call: 'Sees the tool name and arguments before the tool runs.',
    post_tool_call: 'Sees the tool name, arguments, and a short preview of its result.',
  };
  const ENV_STATES = {
    not_needed: 'Not needed',
    needed: 'Needed',
    building: 'Building…',
    ready: 'Ready',
    changed: 'Changed',
    refused: 'Refused',
    failed: 'Failed',
  };

  function pill(state) {
    return el('span', {class: `hermes-pill hermes-pill-${state}`}, el('i', {'aria-hidden': 'true'}), STATES[state]?.[0] || state);
  }
  function envPill(state) {
    return el('span', {class: `hermes-pill hermes-pill-env-${state}`}, el('i', {'aria-hidden': 'true'}), ENV_STATES[state] || state);
  }
  function names(title, items, render) {
    if (!items?.length) return null;
    return el('div', {class: 'hermes-registered'}, el('h6', {text: title}), el('ul', {}, items.map(render)));
  }

  function renderHooksSection(p) {
    if (!Array.isArray(p.hooks) || !p.hooks.length) return null;
    const list = el('ul', {class: 'hermes-hook-list'});
    for (const h of p.hooks) {
      const desc = HOOK_DESCRIPTIONS[h.name] || 'Not run in SAIL, so it sees nothing.';
      const control = h.supported
        ? (() => {
            const toggle = el('input', {
              type: 'checkbox',
              checked: Boolean(h.enabled),
              onchange: () => setHook(p.id, h.name, toggle.checked, toggle),
            });
            return el('label', {class: 'hermes-hook-toggle'}, toggle, 'Runs in SAIL');
          })()
        : el('span', {class: 'hermes-meta', text: 'Not run in SAIL'});
      list.append(el('li', {class: 'hermes-hook-item', 'data-hook': h.name},
        el('div', {class: 'hermes-hook-row'},
          el('code', {text: h.name}),
          control),
        el('span', {class: 'hermes-meta', text: desc})));
    }
    return el('div', {class: 'hermes-plugin-section'},
      el('h6', {text: 'Hooks'}),
      list);
  }

  function renderEnvironmentSection(p) {
    const env = p.environment;
    if (!env) return null;
    const st = env.state || 'not_needed';
    const section = el('div', {class: 'hermes-plugin-section', 'data-env-state': st},
      el('div', {class: 'hermes-env-head'},
        el('h6', {text: 'Python environment'}),
        envPill(st)));

    if (env.source) {
      section.append(el('p', {class: 'hermes-meta'}, 'From ', el('code', {text: env.source})));
    }
    if (env.error) {
      section.append(el('p', {class: 'hermes-note hermes-error', text: env.error}));
    }
    if (env.requirements?.length) {
      section.append(el('div', {class: 'hermes-registered'},
        el('h6', {text: 'Requirements'}),
        el('ul', {class: 'hermes-env-list'}, env.requirements.map(req => el('li', {}, el('code', {text: req}))))));
    }
    if (env.refused?.length) {
      section.append(el('div', {class: 'hermes-registered'},
        el('h6', {text: 'Refused requirements'}),
        el('ul', {class: 'hermes-env-list'}, env.refused.map(ref => el('li', {class: 'hermes-error', text: ref})))));
    }
    if (st === 'ready' && env.locked?.length) {
      section.append(el('div', {class: 'hermes-registered'},
        el('h6', {text: 'Installed packages'}),
        el('ul', {class: 'hermes-env-list'}, env.locked.map(pkg =>
          el('li', {}, el('code', {text: `${pkg.name}==${pkg.version}` }))))));
    }

    const actions = [];
    if (st === 'needed') {
      actions.push(el('button', {class: 'tm-btn', type: 'button', onclick: () => envAction(p.id, 'build'), text: 'Build environment'}));
    } else if (st === 'changed') {
      actions.push(el('button', {class: 'tm-btn', type: 'button', onclick: () => envAction(p.id, 'build'), text: 'Rebuild environment'}));
    } else if (st === 'failed') {
      actions.push(el('button', {class: 'tm-btn', type: 'button', onclick: () => envAction(p.id, 'build'), text: 'Retry build'}));
    }
    if (st === 'ready' || st === 'changed' || st === 'failed') {
      actions.push(el('button', {class: 'tm-btn', type: 'button', onclick: () => envAction(p.id, 'remove'), text: 'Remove environment'}));
    }
    if (actions.length) section.append(el('div', {class: 'hermes-actions'}, actions));
    return section;
  }

  function pluginCard(p) {
    const state = p.status.state, detail = p.status.detail;
    const m = p.manifest || {};
    const heading = el('header', {class: 'hermes-plugin-head'},
      el('h5', {}, m.name || p.id, m.version ? el('small', {text: ` v${m.version}`}) : null),
      pill(state));
    const meta = el('p', {class: 'hermes-meta'},
      `${SOURCES[p.source] || p.source} · folder ${p.id} · `,
      el('code', {title: p.digest, text: `sha256 ${p.digest.slice(0, 12)}…`}),
      p.skills_only ? ' · skills only, no code' : '');
    const body = [heading, m.description ? el('p', {text: m.description}) : null, meta];
    if (STATES[state]?.[1]) body.push(el('p', {class: 'hermes-note', text: STATES[state][1]}));
    if (detail) body.push(el('p', {class: 'hermes-note', text: detail}));
    if (state === 'enabled') {
      const a = p.approval || {};
      body.push(el('p', {class: 'hermes-meta'},
        `Approved ${a.approved_at ? new Date(a.approved_at).toLocaleString() : ''} · `,
        a.network ? 'network allowed' : 'no network',
        ` · ${a.env?.length ? 'secrets: ' + a.env.join(', ') : 'no secrets'}`));
      if (p.describe_error) body.push(el('p', {class: 'hermes-note hermes-error', text: `Could not load it: ${p.describe_error}`}));
      const r = p.registered;
      if (r) {
        const regs = [
          names('Tools', r.tools, t => el('li', {}, el('code', {text: `${p.id}.${t.name}`}), t.available ? '' : ' (unavailable)', t.description ? ` — ${t.description}` : '')),
          names('Commands', r.commands, c => el('li', {}, el('code', {text: `${p.id}./${c.name}`}), c.description ? ` — ${c.description}` : '')),
          names('Skills', Object.keys(r.skills || {}), s => el('li', {}, el('code', {text: `${p.id}:${s}`}))),
          !Array.isArray(p.hooks) ? names('Hooks (listed, not run by SAIL)', r.hooks, h => el('li', {}, el('code', {text: h}))) : null,
          names('Registrations SAIL does not support', r.unsupported, u => el('li', {}, el('code', {text: u}))),
        ].filter(Boolean);
        body.push(regs.length ? el('div', {class: 'hermes-registered-grid'}, regs)
          : el('p', {class: 'hermes-meta', text: 'It registered no tools, commands or skills.'}));
      }
      const hooksSection = renderHooksSection(p);
      if (hooksSection) body.push(hooksSection);
      const envSection = renderEnvironmentSection(p);
      if (envSection) body.push(envSection);
      body.push(el('div', {class: 'hermes-actions'},
        el('button', {class: 'tm-btn', type: 'button', onclick: () => revoke(p), text: 'Revoke approval'})));
    } else if (state === 'quarantined' || state === 'changed') {
      const envSection = renderEnvironmentSection(p);
      if (envSection) body.push(envSection);
      const netId = `hermesNet-${p.id}`, envId = `hermesEnv-${p.id}`;
      const env = el('input', {id: envId, type: 'text', maxlength: '600', value: (m.requires_env || []).join(', '), autocomplete: 'off', spellcheck: 'false'});
      const net = el('input', {id: netId, type: 'checkbox'});
      body.push(el('form', {class: 'hermes-approve', onsubmit: e => { e.preventDefault(); approve(p, net.checked, env.value); }},
        el('label', {class: 'hermes-check', for: netId}, net, 'Allow network access'),
        el('label', {for: envId}, 'Secrets it may read (names from its env file, comma-separated)', env),
        el('button', {class: 'tm-btn hermes-primary', type: 'submit', text: state === 'changed' ? 'Approve the new files' : 'Approve and enable'})));
    }
    return el('article', {class: `hermes-plugin hermes-plugin-${state}`, 'data-plugin': p.id, 'aria-label': `${m.name || p.id} plugin`}, body);
  }

  function scheduleEnvPoll(pluginsList) {
    clearTimeout(envPollTimer);
    envPollTimer = null;
    const anyBuilding = (pluginsList || []).some(p => p.environment?.state === 'building');
    if (anyBuilding && shown()) {
      envPollTimer = setTimeout(() => {
        if (shown()) load();
      }, 2000);
    }
  }

  function renderPlugins(data) {
    const list = $('#hermesPluginList');
    list.replaceChildren();
    if (!data.sandbox) list.append(el('p', {class: 'hermes-note hermes-error', text: 'Bubblewrap is not installed on the host, so no plugin can run. Install bubblewrap to use plugins.'}));
    if (!data.plugins.length) {
      list.append(el('div', {class: 'hermes-empty'},
        el('p', {text: 'No Hermes plugins found. Put a plugin folder (plugin.yaml and __init__.py) in one of these folders, then refresh:'}),
        el('ul', {}, data.roots.map(r => el('li', {}, el('code', {text: r.path}))))));
      scheduleEnvPoll([]);
      return;
    }
    for (const p of data.plugins) list.append(pluginCard(p));
    scheduleEnvPoll(data.plugins);
  }

  function renderSkills(data) {
    const list = $('#hermesSkillList');
    list.replaceChildren();
    if (!data.skills.length) {
      list.append(el('li', {class: 'hermes-empty'}, query
        ? `No skill matches “${query}”.`
        : el('span', {}, 'No skills yet. Add SKILL.md folders to ', data.roots.flatMap((r, i) => [i ? ' or ' : '', el('code', {text: r.path})]), '.')));
    }
    for (const s of data.skills) {
      list.append(el('li', {}, el('button', {type: 'button', class: 'hermes-skill', 'data-skill': s.name, onclick: () => openSkill(s.name)},
        el('strong', {text: s.name}), el('span', {class: 'hermes-meta', text: SOURCES[s.source] || s.source.replace('plugin:', 'plugin ')}),
        el('span', {text: s.description}))));
    }
    const rejected = $('#hermesRejected');
    rejected.hidden = !data.rejected.length;
    rejected.querySelector('summary').textContent = `${data.rejected.length} skill folder${data.rejected.length === 1 ? '' : 's'} not loaded`;
    rejected.querySelector('ul').replaceChildren(...data.rejected.map(r => el('li', {}, el('code', {text: r.path}), ` — ${r.reason}`)));
  }

  async function openSkill(name, file = null) {
    const view = $('#hermesSkillView');
    try {
      const data = await api(`/skill?name=${encodeURIComponent(name)}${file ? `&file=${encodeURIComponent(file)}` : ''}`);
      $('#hermesSkillName').textContent = file ? `${name} / ${file}` : name;
      $('#hermesSkillBody').textContent = data.content + (data.truncated ? '\n\n[Truncated at 64 KiB]' : '');
      const files = $('#hermesSkillFiles');
      files.replaceChildren(...(file ? [el('li', {}, el('button', {type: 'button', class: 'hermes-link', onclick: () => openSkill(name), text: 'Back to SKILL.md'}))]
        : (data.files || []).map(f => el('li', {}, el('button', {type: 'button', class: 'hermes-link', onclick: () => openSkill(name, f), text: f})))));
      view.hidden = false;
      $('#hermesSkillName').focus();
    } catch (error) { status(error.message, true); }
  }

  function renderProposals(data) {
    const wrap = $('#hermesProposalsWrap');
    const list = $('#hermesProposalList');
    const proposals = data?.proposals || [];
    list.replaceChildren();
    wrap.hidden = !proposals.length;
    if (!proposals.length) return;

    const VERDICTS = { safe: 'Safe', caution: 'Caution', dangerous: 'Dangerous' };
    for (const pr of proposals) {
      const verdict = pr.report?.verdict || 'caution';
      const findings = pr.report?.findings || [];
      const cardEl = el('article', { class: 'hermes-proposal', 'data-proposal': pr.id, 'data-proposal-name': pr.name, 'aria-label': `Proposed skill ${pr.name}` },
        el('header', { class: 'hermes-proposal-head' },
          el('h5', { text: pr.name }),
          el('span', { class: `hermes-pill hermes-pill-${verdict}` }, el('i', { 'aria-hidden': 'true' }), VERDICTS[verdict] || verdict)),
        pr.description ? el('p', { text: pr.description }) : null,
        el('p', { class: 'hermes-note' }, el('strong', { text: "SAIL's reason: " }), pr.reason || '—'));

      const fBox = el('div', { class: 'hermes-proposal-findings' },
        el('strong', { text: findings.length ? `Scan findings (${findings.length})` : 'Scan: no findings' }));
      if (findings.length) {
        fBox.append(el('ul', {}, findings.map(f =>
          el('li', {},
            el('strong', { text: f.rule }), ` (${f.severity}) · `,
            el('code', { text: `${f.file}:${f.line}` }),
            f.excerpt ? [' — ', el('code', { text: f.excerpt })] : null))));
      }
      cardEl.append(fBox);

      if (pr.body != null) {
        cardEl.append(el('div', { class: 'hermes-proposal-body' },
          el('strong', { text: 'SKILL.md' }),
          el('pre', { text: pr.body })));
      }

      if (pr.conflict) {
        cardEl.append(el('p', { class: 'hermes-note hermes-error', text: 'A skill with this name already exists.' }));
      }
      if (pr.decision === 'block') {
        cardEl.append(el('p', { class: 'hermes-note hermes-error', text: pr.decision_reason || 'Blocked by security policy.' }));
      }

      const actions = el('div', { class: 'hermes-actions' });
      if (pr.decision === 'allow') {
        actions.append(el('button', {
          type: 'button', class: 'tm-btn', disabled: Boolean(pr.conflict),
          onclick: () => approveProposal(pr.id, false), text: 'Approve',
        }));
      } else if (pr.decision === 'needs_ack') {
        const ackId = `hermesPropAck-${pr.id}`;
        const ack = el('input', { id: ackId, type: 'checkbox', disabled: Boolean(pr.conflict) });
        const btn = el('button', {
          type: 'button', class: 'tm-btn', disabled: true,
          onclick: () => approveProposal(pr.id, true), text: 'Approve anyway',
        });
        ack.addEventListener('change', () => { btn.disabled = Boolean(pr.conflict) || !ack.checked; });
        cardEl.append(el('label', { class: 'hermes-check', for: ackId }, ack, 'I have read the findings and want to approve it anyway'));
        actions.append(btn);
      }
      actions.append(el('button', {
        type: 'button', class: 'tm-btn',
        onclick: () => rejectProposal(pr.id), text: 'Reject',
      }));
      cardEl.append(actions);
      list.append(cardEl);
    }
  }

  async function approveProposal(id, acknowledgeCaution) {
    status('Approving proposed skill…');
    try {
      const res = await api(`/proposals/${encodeURIComponent(id)}/approve`, { acknowledge_caution: Boolean(acknowledgeCaution) });
      await load();
      status(`Approved proposed skill ${res.skill || id}.`);
    } catch (error) { status(error.message, true); }
  }

  async function rejectProposal(id) {
    status('Rejecting proposed skill…');
    try {
      await api(`/proposals/${encodeURIComponent(id)}/reject`, {});
      await load();
      status('Rejected proposed skill.');
    } catch (error) { status(error.message, true); }
  }

  async function loadSkills() {
    const data = await api(`/skills${query ? `?query=${encodeURIComponent(query)}` : ''}`);
    renderSkills(data);
    return data;
  }
  async function load() {
    if (busy) return;
    busy = true; $('#hermesRefresh').disabled = true;
    status(loadedOnce ? 'Refreshing…' : 'Loading plugins and skills…');
    try {
      const [plugins, skills, proposals] = await Promise.all([
        api('/plugins'),
        loadSkills(),
        api('/proposals').catch(() => ({ proposals: [] })),
      ]);
      renderPlugins(plugins);
      renderProposals(proposals);
      const enabled = plugins.plugins.filter(p => p.status.state === 'enabled').length;
      status(`${plugins.plugins.length} plugin${plugins.plugins.length === 1 ? '' : 's'} found, ${enabled} enabled · ${skills.skills.length} skill${skills.skills.length === 1 ? '' : 's'}`);
      loadedOnce = true;
    } catch (error) { status(error.message, true); }
    finally { busy = false; $('#hermesRefresh').disabled = false; }
  }
  async function approve(p, network, envText) {
    const env = envText.split(/[\s,]+/).filter(Boolean);
    status(`Approving ${p.id}…`);
    try {
      await api(`/plugins/${encodeURIComponent(p.id)}/approve`, {digest: p.digest, network, env});
      await load();
      status(`Approved ${p.id}. The model can list its tools; each call still asks you first.`);
    } catch (error) { status(error.message, true); }
  }
  async function setHook(pluginId, hook, enabled, toggleEl) {
    status(`${enabled ? 'Enabling' : 'Disabling'} hook ${hook} on ${pluginId}…`);
    try {
      await api(`/plugins/${encodeURIComponent(pluginId)}/hooks`, {hook, enabled});
      await load();
      status(`${enabled ? 'Enabled' : 'Disabled'} hook ${hook} on ${pluginId}.`);
    } catch (error) {
      if (toggleEl) toggleEl.checked = !enabled;
      status(error.message, true);
    }
  }
  async function envAction(pluginId, action) {
    status(action === 'build' ? `Building Python environment for ${pluginId}…` : `Removing Python environment for ${pluginId}…`);
    try {
      await api(`/plugins/${encodeURIComponent(pluginId)}/environment`, {action});
      await load();
    } catch (error) { status(error.message, true); }
  }
  async function revoke(p) {
    status(`Revoking ${p.id}…`);
    try {
      await api(`/plugins/${encodeURIComponent(p.id)}/revoke`, {});
      await load();
      status(`Revoked ${p.id}. It will not run again until you approve it.`);
    } catch (error) { status(error.message, true); }
  }

  $('#hermesRefresh').onclick = load;
  $('#hermesSkillClose').onclick = () => { $('#hermesSkillView').hidden = true; };
  $('#hermesSkillQuery').oninput = e => {
    clearTimeout(queryTimer);
    queryTimer = setTimeout(() => { query = e.target.value.trim(); loadSkills().catch(error => status(error.message, true)); }, 200);
  };
  const shown = () => !!document.getElementById('subsystemContent')?.contains(card);
  for (const event of ['sail:page', 'sail:section']) window.addEventListener(event, () => { if (shown()) load(); });
  if (shown()) load();
})();

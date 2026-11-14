/* SAIL Agents › Skills & plugins: Hermes Agent plugins (owner approval) and Agent Skills.
   The server's /api/hermes answers are the only state; nothing here runs a plugin tool. */
'use strict';
(() => {
  const card = document.getElementById('hermesCard');
  if (!card) return;
  const $ = s => card.querySelector(s);
  let busy = false, loadedOnce = false, query = '', queryTimer = null;

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

  function pill(state) {
    return el('span', {class: `hermes-pill hermes-pill-${state}`}, el('i', {'aria-hidden': 'true'}), STATES[state]?.[0] || state);
  }
  function names(title, items, render) {
    if (!items?.length) return null;
    return el('div', {class: 'hermes-registered'}, el('h6', {text: title}), el('ul', {}, items.map(render)));
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
          names('Hooks (listed, not run by SAIL)', r.hooks, h => el('li', {}, el('code', {text: h}))),
          names('Registrations SAIL does not support', r.unsupported, u => el('li', {}, el('code', {text: u}))),
        ].filter(Boolean);
        body.push(regs.length ? el('div', {class: 'hermes-registered-grid'}, regs)
          : el('p', {class: 'hermes-meta', text: 'It registered no tools, commands or skills.'}));
      }
      body.push(el('div', {class: 'hermes-actions'},
        el('button', {class: 'tm-btn', type: 'button', onclick: () => revoke(p), text: 'Revoke approval'})));
    } else if (state === 'quarantined' || state === 'changed') {
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

  function renderPlugins(data) {
    const list = $('#hermesPluginList');
    list.replaceChildren();
    if (!data.sandbox) list.append(el('p', {class: 'hermes-note hermes-error', text: 'Bubblewrap is not installed on the host, so no plugin can run. Install bubblewrap to use plugins.'}));
    if (!data.plugins.length) {
      list.append(el('div', {class: 'hermes-empty'},
        el('p', {text: 'No Hermes plugins found. Put a plugin folder (plugin.yaml and __init__.py) in one of these folders, then refresh:'}),
        el('ul', {}, data.roots.map(r => el('li', {}, el('code', {text: r.path}))))));
      return;
    }
    for (const p of data.plugins) list.append(pluginCard(p));
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
      const [plugins, skills] = await Promise.all([api('/plugins'), loadSkills()]);
      renderPlugins(plugins);
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

/* SAIL Agents › Messaging (round 22, Antigravity).
   Telegram gateway setup, pairing approvals, unknown-sender policy, conversations, and delivery. */
'use strict';
(() => {
  const card = document.getElementById('gatewayCard');
  if (!card) return;
  const $ = s => card.querySelector(s);

  let busy = false;
  let loadedOnce = false;
  let confirmDisconnect = false;
  let confirmRevokeId = null;

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
    const node = $('#gatewayStatus');
    node.textContent = text;
    node.classList.toggle('gateway-error', isError);
  }

  function showInlineError(selector, msg) {
    const node = $(selector);
    if (!node) return;
    node.textContent = msg || '';
    node.hidden = !msg;
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
      err.payload = payload;
      err.status = r.status;
      throw err;
    }
    return payload;
  }

  function renderStatusStrip(st) {
    const strip = $('#gatewayStatusStrip');
    const pill = $('#gatewayConnPill');
    const poll = $('#gatewayPollTime');
    const errText = $('#gatewayConnError');
    const disconnectWrap = $('#gatewayDisconnectWrap');

    const connected = Boolean(st?.connected);
    const hasErr = Boolean(st?.error);
    strip.dataset.state = hasErr ? 'error' : (connected ? 'connected' : 'disconnected');
    pill.dataset.tone = hasErr ? 'error' : (connected ? 'connected' : 'disconnected');

    if (hasErr && !connected) {
      pill.textContent = 'Error';
    } else if (connected) {
      const bot = st.bot_username ? `@${st.bot_username.replace(/^@/, '')}` : 'bot';
      pill.textContent = `Connected as ${bot}`;
    } else {
      pill.textContent = 'Not connected';
    }

    poll.textContent = st?.last_poll_at ? `Last poll: ${formatEpoch(st.last_poll_at)}` : 'Last poll: never';
    errText.textContent = st?.error || '';
    errText.hidden = !st?.error;

    disconnectWrap.replaceChildren();
    if (connected) {
      if (confirmDisconnect) {
        disconnectWrap.append(
          el('button', {
            type: 'button',
            class: 'tm-btn gateway-danger',
            text: 'Confirm disconnect',
            onclick: () => disconnectBot(),
          }),
          el('button', {
            type: 'button',
            class: 'tm-btn',
            text: 'Cancel',
            onclick: () => { confirmDisconnect = false; renderStatusStrip(st); },
          }),
        );
      } else {
        disconnectWrap.append(
          el('button', {
            type: 'button',
            class: 'tm-btn',
            text: 'Disconnect',
            onclick: () => { confirmDisconnect = true; renderStatusStrip(st); },
          }),
        );
      }
    }

    $('#gatewaySetup').hidden = connected;
  }

  function renderPairing(pairing) {
    const lockNode = $('#gatewayLockout');
    if (pairing?.locked_until) {
      lockNode.textContent = `Too many wrong codes. Approvals are locked until ${formatEpoch(pairing.locked_until)}.`;
      lockNode.hidden = false;
    } else {
      lockNode.textContent = '';
      lockNode.hidden = true;
    }

    const pendingList = $('#gatewayPendingList');
    pendingList.replaceChildren();
    const pending = pairing?.pending || [];
    if (!pending.length) {
      pendingList.append(el('p', { class: 'gateway-meta', text: 'No pending pairing requests.' }));
    } else {
      for (const req of pending) {
        pendingList.append(
          el('div', { class: 'gateway-card', 'data-pending-code': req.code },
            el('div', { class: 'gateway-card-main' },
              el('span', { class: 'gateway-card-title', text: req.name || 'Telegram user' }),
              el('span', { class: 'gateway-meta', text: `Requested ${formatEpoch(req.requested_at)}` }),
              el('code', { class: 'gateway-code', text: req.code }),
            ),
            el('div', { class: 'gateway-actions' },
              el('button', {
                type: 'button',
                class: 'tm-btn gateway-primary',
                text: 'Approve',
                onclick: () => approveCode(req.code),
              }),
              el('button', {
                type: 'button',
                class: 'tm-btn',
                text: 'Deny',
                onclick: () => denyCode(req.code),
              }),
            ),
          ),
        );
      }
    }

    const pairedList = $('#gatewayPairedList');
    pairedList.replaceChildren();
    const paired = pairing?.paired || [];
    if (!paired.length) {
      pairedList.append(el('p', { class: 'gateway-meta', text: 'No paired contacts yet.' }));
    } else {
      for (const person of paired) {
        const armed = confirmRevokeId === person.sender_id;
        const actions = armed
          ? [
              el('button', {
                type: 'button',
                class: 'tm-btn gateway-danger',
                text: 'Confirm revoke',
                onclick: () => revokeSender(person.sender_id),
              }),
              el('button', {
                type: 'button',
                class: 'tm-btn',
                text: 'Cancel',
                onclick: () => { confirmRevokeId = null; renderPairing(pairing); },
              }),
            ]
          : [
              el('button', {
                type: 'button',
                class: 'tm-btn',
                text: 'Revoke',
                onclick: () => { confirmRevokeId = person.sender_id; renderPairing(pairing); },
              }),
            ];
        pairedList.append(
          el('div', { class: 'gateway-card', 'data-paired-id': String(person.sender_id) },
            el('div', { class: 'gateway-card-main' },
              el('span', { class: 'gateway-card-title', text: person.name || 'Telegram user' }),
              el('span', { class: 'gateway-meta', text: `Paired since ${formatEpoch(person.paired_at)}` }),
            ),
            el('div', { class: 'gateway-actions' }, actions),
          ),
        );
      }
    }

    const activePolicy = pairing?.policy || 'pair';
    card.querySelectorAll('.gateway-seg-btn').forEach(btn => {
      const selected = btn.dataset.policy === activePolicy;
      btn.setAttribute('aria-pressed', selected ? 'true' : 'false');
    });
  }

  function renderChats(chats) {
    const list = $('#gatewayChatsList');
    list.replaceChildren();
    if (!chats || !chats.length) {
      list.append(el('p', { class: 'gateway-meta', text: 'No Telegram conversations yet.' }));
      return;
    }
    for (const chat of chats) {
      const turnLabel = chat.turns === 1 ? '1 turn' : `${chat.turns} turns`;
      list.append(
        el('div', { class: 'gateway-card', 'data-chat-id': String(chat.chat_id) },
          el('div', { class: 'gateway-card-main' },
            el('span', { class: 'gateway-card-title', text: chat.name || `Chat ${chat.chat_id}` }),
            el('span', { class: 'gateway-meta', text: `Last message: ${formatEpoch(chat.last_message_at)} · ${turnLabel}` }),
          ),
          el('div', { class: 'gateway-actions' },
            el('button', {
              type: 'button',
              class: 'tm-btn',
              text: 'Start a new conversation',
              onclick: () => resetChat(chat.chat_id),
            }),
          ),
        ),
      );
    }
  }

  function renderOutbox(outbox) {
    $('#gatewayOutboxQueued').textContent = String(outbox?.queued ?? 0);
    $('#gatewayOutboxFailed').textContent = String(outbox?.failed_recently ?? 0);
  }

  async function refresh() {
    if (busy) return;
    busy = true;
    setStatus('Loading Telegram gateway…');
    try {
      const data = await requestJson('/api/agents/gateway');
      loadedOnce = true;
      confirmDisconnect = false;
      confirmRevokeId = null;
      renderStatusStrip(data.status);
      renderPairing(data.pairing);
      renderChats(data.chats || []);
      renderOutbox(data.outbox);
      setStatus(data.status?.connected ? 'Telegram gateway ready.' : 'Connect a Telegram bot to start receiving messages.');
    } catch (err) {
      setStatus(err.message || 'Could not load Telegram gateway.', true);
    } finally {
      busy = false;
    }
  }

  async function connectBot(e) {
    e.preventDefault();
    const input = $('#gatewayTokenInput');
    const token = input.value.trim();
    showInlineError('#gatewayConnectError', '');
    if (!token) {
      showInlineError('#gatewayConnectError', 'Paste the bot token from @BotFather first.');
      return;
    }
    try {
      await requestJson('/api/agents/gateway/connect', { body: { token } });
      input.value = '';
      await refresh();
    } catch (err) {
      showInlineError('#gatewayConnectError', err.message || 'Could not connect bot.');
    }
  }

  async function disconnectBot() {
    try {
      await requestJson('/api/agents/gateway/disconnect', { body: {} });
      confirmDisconnect = false;
      await refresh();
    } catch (err) {
      setStatus(err.message || 'Could not disconnect bot.', true);
    }
  }

  async function approveCode(code, fromForm = false) {
    showInlineError('#gatewayCodeError', '');
    try {
      await requestJson('/api/agents/gateway/pair/approve', { body: { code } });
      if (fromForm) $('#gatewayCodeInput').value = '';
      await refresh();
    } catch (err) {
      if (fromForm) {
        const lockNote = err.payload?.locked_until
          ? ` Locked until ${formatEpoch(err.payload.locked_until)}.`
          : '';
        showInlineError('#gatewayCodeError', `${err.message || 'Invalid code.'}${lockNote}`);
      } else {
        setStatus(err.message || 'Could not approve pairing code.', true);
      }
      if (err.payload?.locked_until) {
        const lockNode = $('#gatewayLockout');
        lockNode.textContent = `Too many wrong codes. Approvals are locked until ${formatEpoch(err.payload.locked_until)}.`;
        lockNode.hidden = false;
      }
    }
  }

  async function denyCode(code) {
    try {
      await requestJson('/api/agents/gateway/pair/deny', { body: { code } });
      await refresh();
    } catch (err) {
      setStatus(err.message || 'Could not deny pairing request.', true);
    }
  }

  async function revokeSender(senderId) {
    try {
      await requestJson('/api/agents/gateway/pair/revoke', { body: { sender_id: senderId } });
      confirmRevokeId = null;
      await refresh();
    } catch (err) {
      setStatus(err.message || 'Could not revoke contact.', true);
    }
  }

  async function setPolicy(policy) {
    try {
      await requestJson('/api/agents/gateway/policy', { body: { policy } });
      await refresh();
    } catch (err) {
      setStatus(err.message || 'Could not update policy.', true);
    }
  }

  async function resetChat(chatId) {
    try {
      await requestJson(`/api/agents/gateway/chats/${encodeURIComponent(chatId)}/new`, { body: {} });
      await refresh();
      setStatus('Started a new conversation for that chat.');
    } catch (err) {
      setStatus(err.message || 'Could not start a new conversation.', true);
    }
  }

  async function retryOutbox() {
    try {
      await requestJson('/api/agents/gateway/outbox/retry', { body: {} });
      await refresh();
      setStatus('Retrying queued replies.');
    } catch (err) {
      setStatus(err.message || 'Could not retry delivery.', true);
    }
  }

  $('#gatewayRefresh')?.addEventListener('click', refresh);
  $('#gatewayConnectForm')?.addEventListener('submit', connectBot);
  $('#gatewayCodeForm')?.addEventListener('submit', e => {
    e.preventDefault();
    const code = $('#gatewayCodeInput').value.trim();
    if (!code) {
      showInlineError('#gatewayCodeError', 'Type the 8-character code shown in Telegram.');
      return;
    }
    approveCode(code, true);
  });
  card.querySelectorAll('.gateway-seg-btn').forEach(btn => {
    btn.addEventListener('click', () => setPolicy(btn.dataset.policy));
  });
  $('#gatewayRetryOutbox')?.addEventListener('click', retryOutbox);

  for (const evt of ['sail:page', 'sail:section']) {
    window.addEventListener(evt, () => {
      if (card.closest('#subsystemContent') && !loadedOnce) refresh();
    });
  }
})();

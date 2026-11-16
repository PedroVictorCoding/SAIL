'use strict';
// SAIL Announce — ambient glass announcement stack, arrival greeting line, and
// silent not-addressed Orb dim. All server text is written via textContent.
(() => {
  const MAX_VISIBLE = 3;
  let items = [];
  let showAll = false;
  let dimTimer = null;

  function isSafeLink(link) {
    return typeof link === 'string' && (link.startsWith('#') || (link.startsWith('/') && !link.startsWith('//')));
  }

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
  }

  async function sendFeedback(itemId, feedback, cardEl, errorEl) {
    const buttons = cardEl.querySelectorAll('button');
    buttons.forEach((b) => { b.disabled = true; });
    if (errorEl) {
      errorEl.hidden = true;
      errorEl.textContent = '';
    }
    try {
      const fetchFn = window.SailPresence?.fetchJson;
      const url = `/api/initiative/items/${encodeURIComponent(itemId)}/feedback`;
      const options = {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ feedback }),
      };
      if (fetchFn) {
        await fetchFn(url, options);
      } else {
        const res = await fetch(url, { ...options, cache: 'no-store' });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
      }
      items = items.filter((it) => it.item_id !== itemId);
      renderStack();
    } catch (err) {
      buttons.forEach((b) => { b.disabled = false; });
      if (errorEl) {
        errorEl.textContent = err.message || 'Could not send feedback.';
        errorEl.hidden = false;
      }
    }
  }

  function buildCard(item) {
    const card = el('article', 'announce-card');
    card.dataset.itemId = item.item_id || '';

    const head = el('div', 'announce-card-head');
    const meta = el('div', 'announce-card-meta');
    const sourceText = [item.source, item.kind].filter(Boolean).join(' · ');
    if (sourceText) meta.append(el('span', 'announce-source', sourceText));
    if (item.speak) meta.append(el('span', 'announce-spoken-badge', 'Spoken'));
    head.append(meta);

    const whyBtn = el('button', 'announce-why-btn', 'Why?');
    whyBtn.type = 'button';
    whyBtn.setAttribute('aria-expanded', 'false');
    head.append(whyBtn);
    card.append(head);

    card.append(el('h3', 'announce-title', item.title || 'Announcement'));
    if (item.body) {
      card.append(el('p', 'announce-body', item.body));
    }

    const reasonsBox = el('div', 'announce-reasons');
    reasonsBox.hidden = true;
    const reasons = Array.isArray(item.reasons) ? item.reasons : [];
    if (reasons.length > 0) {
      const ul = el('ul', 'announce-reasons-list');
      for (const r of reasons) {
        ul.append(el('li', null, String(r)));
      }
      reasonsBox.append(ul);
    } else {
      reasonsBox.append(el('p', 'announce-reasons-empty', 'No additional details.'));
    }
    card.append(reasonsBox);

    whyBtn.addEventListener('click', () => {
      const open = reasonsBox.hidden;
      reasonsBox.hidden = !open;
      whyBtn.setAttribute('aria-expanded', open ? 'true' : 'false');
    });

    const errorEl = el('p', 'announce-error');
    errorEl.hidden = true;
    errorEl.setAttribute('role', 'alert');
    card.append(errorEl);

    const actions = el('div', 'announce-actions');
    const mkBtn = (label, feedback, cls) => {
      const btn = el('button', cls ? `announce-btn ${cls}` : 'announce-btn', label);
      btn.type = 'button';
      btn.addEventListener('click', () => sendFeedback(item.item_id, feedback, card, errorEl));
      return btn;
    };

    if (isSafeLink(item.link)) {
      const openBtn = el('button', 'announce-btn announce-btn-open', 'Open');
      openBtn.type = 'button';
      openBtn.addEventListener('click', () => {
        if (item.link.startsWith('#')) window.location.hash = item.link;
        else window.location.assign(item.link);
      });
      actions.append(openBtn);
    }

    actions.append(
      mkBtn('Useful', 'useful', 'announce-btn-primary'),
      mkBtn('Not now', 'not_now'),
      mkBtn("Don't tell me about this kind", 'mute', 'announce-btn-muted')
    );
    card.append(actions);
    return card;
  }

  function renderStack() {
    const container = document.getElementById('announceStack');
    if (!container) return;
    container.replaceChildren();
    if (items.length === 0) {
      container.hidden = true;
      return;
    }
    container.hidden = false;
    const visible = showAll ? items : items.slice(0, MAX_VISIBLE);
    for (const item of visible) {
      container.append(buildCard(item));
    }
    if (items.length > MAX_VISIBLE) {
      const hiddenCount = items.length - MAX_VISIBLE;
      const toggleLabel = showAll
        ? 'Show fewer'
        : `+${hiddenCount} earlier announcement${hiddenCount === 1 ? '' : 's'}`;
      const moreBtn = el('button', 'announce-more-btn', toggleLabel);
      moreBtn.type = 'button';
      moreBtn.addEventListener('click', () => {
        showAll = !showAll;
        renderStack();
      });
      container.append(moreBtn);
    }
  }

  function push(msg) {
    if (!msg || typeof msg !== 'object' || !msg.item_id) return;
    const idx = items.findIndex((it) => it.item_id === msg.item_id);
    if (idx >= 0) items[idx] = msg;
    else items.unshift(msg);
    renderStack();
  }

  function showGreeting(msg) {
    const box = document.getElementById('presenceGreeting');
    if (!box || !msg || !msg.spoken) return;
    box.replaceChildren();

    const row = el('div', 'presence-greeting-row');
    row.append(el('p', 'presence-greeting-text', msg.spoken));

    const dismissBtn = el('button', 'presence-greeting-dismiss', '×');
    dismissBtn.type = 'button';
    dismissBtn.setAttribute('aria-label', 'Dismiss greeting');
    dismissBtn.addEventListener('click', () => {
      box.replaceChildren();
      box.hidden = true;
    });
    row.append(dismissBtn);
    box.append(row);

    const greetingItems = Array.isArray(msg.items) ? msg.items : [];
    const titled = greetingItems.filter((it) => it && typeof it.title === 'string' && it.title.trim().length > 0);
    if (titled.length > 0) {
      const chips = el('div', 'presence-greeting-chips');
      for (const it of titled) {
        chips.append(el('span', 'presence-greeting-chip', it.title));
      }
      box.append(chips);
    }
    box.hidden = false;
  }

  function onNotAddressed() {
    const orb = document.getElementById('orbWrap');
    if (!orb) return;
    orb.classList.add('orb-not-addressed-dim');
    if (dimTimer) clearTimeout(dimTimer);
    dimTimer = setTimeout(() => {
      orb.classList.remove('orb-not-addressed-dim');
      dimTimer = null;
    }, 300);
  }

  window.SailAnnounce = {
    push,
    showGreeting,
    onNotAddressed,
    clear() {
      items = [];
      showAll = false;
      renderStack();
    },
  };
})();

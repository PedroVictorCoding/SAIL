// Return Briefing & Session Staging Controller ("While Away" Intelligence Layer)
'use strict';

(function () {
  let activeBriefing = null;

  async function fetchLatestBriefing() {
    try {
      const res = await fetch('/api/briefing/latest');
      if (!res.ok) return null;
      const data = await res.json();
      return data.briefing;
    } catch (e) {
      console.warn('Could not fetch return briefing:', e);
      return null;
    }
  }

  function getOwnerToken() {
    try {
      return localStorage.getItem('sail_owner_token') || sessionStorage.getItem('sail_owner_token') || '';
    } catch (_) {
      return '';
    }
  }

  function authHeaders() {
    const token = getOwnerToken();
    const headers = { 'Content-Type': 'application/json' };
    if (token) headers['Authorization'] = `Bearer ${token}`;
    return headers;
  }

  async function playStagedMusic(uri, btn) {
    if (!uri) return;
    const originalText = btn.textContent;
    btn.disabled = true;
    btn.textContent = 'Starting playback…';

    try {
      const res = await fetch('/api/briefing/actions/play-music', {
        method: 'POST',
        headers: authHeaders(),
        body: JSON.stringify({ uri })
      });
      const data = await res.json();
      if (res.ok && data.ok) {
        btn.textContent = '✓ Playing now';
        btn.classList.add('playing');
        // Announce in voice/transcript if available
        const transcript = document.getElementById('transcript');
        if (transcript) transcript.textContent = `SAIL Staged Music: Playing ${activeBriefing?.staged_music?.name || 'audio'}.`;
      } else {
        btn.disabled = false;
        btn.textContent = originalText;
        alert(data.error || 'Could not play staged music. Check Spotify connection.');
      }
    } catch (err) {
      btn.disabled = false;
      btn.textContent = originalText;
      console.error('Play music failed:', err);
    }
  }

  async function refreshBriefing(btn) {
    if (btn) {
      btn.disabled = true;
      btn.textContent = '↻ Staging…';
    }
    try {
      const res = await fetch('/api/briefing/refresh', {
        method: 'POST',
        headers: authHeaders()
      });
      if (res.ok) {
        const data = await res.json();
        renderBriefing(data.briefing);
      }
    } catch (e) {
      console.error('Refresh briefing failed:', e);
    } finally {
      if (btn) {
        btn.disabled = false;
        btn.textContent = '↻ Refresh';
      }
    }
  }

  // Closing marks the briefing read (the Tools Map returns to the map); it stays
  // available in the "While You Were Away" view.
  async function dismissBriefing() {
    const deck = document.getElementById('returnBriefingDeck');
    if (deck) deck.hidden = true;
    document.getElementById('tmBriefingBtn')?.setAttribute('aria-pressed', 'false');
    try {
      await fetch('/api/briefing/actions/dismiss', {
        method: 'POST',
        headers: authHeaders()
      });
    } catch (_) {}
  }

  function renderBriefing(briefing) {
    activeBriefing = briefing;
    const deck = document.getElementById('returnBriefingDeck');
    if (!deck) return;

    announce({ fresh: !!briefing && !briefing.dismissed });
    if (!briefing) {
      deck.hidden = true;
      document.getElementById('tmBriefingBtn')?.setAttribute('aria-pressed', 'false');
      return;
    }

    deck.hidden = false;
    document.getElementById('tmBriefingBtn')?.setAttribute('aria-pressed', 'true');

    // Greeting & headline
    const headlineEl = document.getElementById('briefingHeadline');
    if (headlineEl) headlineEl.textContent = briefing.headline;

    const timeEl = document.getElementById('briefingLocalTime');
    if (timeEl) timeEl.textContent = briefing.local_time_str;

    // 1. Music Card
    const musicCard = document.getElementById('briefingMusicCard');
    if (musicCard && briefing.staged_music) {
      const m = briefing.staged_music;
      document.getElementById('briefingMusicSlot').textContent = m.circadian_slot;
      document.getElementById('briefingMusicTitle').textContent = m.name;
      document.getElementById('briefingMusicArtist').textContent = m.by ? `by ${m.by}` : '';
      document.getElementById('briefingMusicRationale').textContent = m.rationale;
      const playBtn = document.getElementById('briefingPlayMusicBtn');
      if (playBtn) {
        playBtn.textContent = m.is_spotify ? '▶ Play on Spotify' : '▶ Play Ambient Audio';
        playBtn.onclick = () => playStagedMusic(m.uri, playBtn);
      }
    }

    // 2. Learning Card
    const learnCard = document.getElementById('briefingLearnCard');
    if (learnCard && briefing.learning_plan) {
      const l = briefing.learning_plan;
      document.getElementById('briefingLearnDomain').textContent = l.domain || 'Arena Learn';
      document.getElementById('briefingLearnTopic').textContent = l.topic;
      document.getElementById('briefingLearnRationale').textContent = l.rationale;
      document.getElementById('briefingLearnEstimate').textContent = `~${l.estimated_minutes} min lesson`;
      const learnBtn = document.getElementById('briefingStartLearnBtn');
      if (learnBtn) {
        learnBtn.onclick = () => openPage('arena');
      }
    }

    // 3. Events Card
    const eventsList = document.getElementById('briefingEventsList');
    if (eventsList && briefing.events_to_watch) {
      eventsList.replaceChildren();
      briefing.events_to_watch.forEach(evt => {
        const li = document.createElement('li');
        li.className = `briefing-event-item urgency-${evt.urgency}`;
        li.innerHTML = `
          <div class="briefing-event-header">
            <span class="briefing-urgency-badge ${evt.urgency}">${evt.due_relative}</span>
            <strong class="briefing-event-title">${escapeHtml(evt.title)}</strong>
          </div>
        `;
        eventsList.appendChild(li);
      });
      const eventsBtn = document.getElementById('briefingEventsBtn');
      if (eventsBtn) {
        eventsBtn.onclick = () => openPage('pulse');
      }
    }

    // 4. News Radar Card
    const newsList = document.getElementById('briefingNewsList');
    if (newsList && briefing.news_radar) {
      newsList.replaceChildren();
      briefing.news_radar.forEach(news => {
        const item = document.createElement('li');
        item.className = 'briefing-news-item';
        item.innerHTML = `
          <div class="briefing-news-tag">${escapeHtml(news.relevant_interest)} · <small>${escapeHtml(news.source)}</small></div>
          <a class="briefing-news-title" href="${news.url || '#'}" target="_blank" rel="noopener">${escapeHtml(news.title)} ↗</a>
          <p class="briefing-news-summary">${escapeHtml(news.summary)}</p>
        `;
        newsList.appendChild(item);
      });
    }

    // 5. Continuity & Sentinel Card
    if (briefing.project_continuity) {
      const c = briefing.project_continuity;
      document.getElementById('briefingContinuityWorkspace').textContent = c.active_workspace;
      document.getElementById('briefingContinuityForge').textContent = c.forge_summary || 'No background Forge tasks.';
      document.getElementById('briefingContinuityAction').textContent = c.suggested_action;
    }
    if (briefing.system_sentinel) {
      const s = briefing.system_sentinel;
      document.getElementById('briefingSentinelStorage').textContent = s.storage_status;
      document.getElementById('briefingSentinelMemory').textContent = s.memory_headroom;
    }
  }

  function announce(detail) {
    window.dispatchEvent(new CustomEvent('sail:briefing', { detail }));
  }

  // Open a SAIL page (Arena, Reminders) through the shared router the Tools Map follows.
  function openPage(id) {
    if (window.sailNavigate) window.sailNavigate(id);
    else window.location.hash = `#${id}`;
  }

  function escapeHtml(str) {
    if (!str) return '';
    return String(str)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  window.sailBriefing = { refresh: () => refreshBriefing(document.getElementById('briefingRefreshBtn')) };

  // The card row scrolls sideways with a hidden scrollbar: a vertical wheel scrolls it,
  // the row is focusable for arrow keys, edge fades show what is off-screen, and a card
  // whose text overflows becomes focusable so its body can be scrolled by keyboard.
  function setupCardRow() {
    const row = document.querySelector('#returnBriefingDeck .briefing-cards-grid');
    if (!row) return;
    row.tabIndex = 0;
    row.setAttribute('role', 'region');
    row.setAttribute('aria-label', 'Briefing cards, scroll sideways');
    const edges = () => {
      row.dataset.atStart = String(row.scrollLeft <= 2);
      row.dataset.atEnd = String(row.scrollLeft + row.clientWidth >= row.scrollWidth - 2);
    };
    row.addEventListener('scroll', edges, { passive: true });
    row.addEventListener('wheel', (event) => {
      if (event.ctrlKey || Math.abs(event.deltaX) >= Math.abs(event.deltaY)) return;
      if (event.target.closest('.briefing-card-body') && overflows(event.target.closest('.briefing-card-body'))) return;
      if (row.scrollWidth <= row.clientWidth) return;
      event.preventDefault();
      row.scrollBy({ left: event.deltaY, behavior: 'auto' });
    }, { passive: false });
    row.addEventListener('keydown', (event) => {
      if (event.target !== row) return;
      const card = row.querySelector('.briefing-card');
      const step = card ? card.getBoundingClientRect().width + parseFloat(getComputedStyle(row).columnGap || 0) : row.clientWidth * 0.8;
      const moves = { ArrowRight: step, ArrowLeft: -step, PageDown: row.clientWidth, PageUp: -row.clientWidth };
      if (event.key in moves) row.scrollBy({ left: moves[event.key] });
      else if (event.key === 'Home') row.scrollTo({ left: 0 });
      else if (event.key === 'End') row.scrollTo({ left: row.scrollWidth });
      else return;
      event.preventDefault(); event.stopPropagation();
    });
    const bodies = () => row.querySelectorAll('.briefing-card-body').forEach((body) => {
      if (overflows(body)) { body.tabIndex = 0; body.setAttribute('role', 'region'); body.setAttribute('aria-label', `${body.closest('.briefing-card')?.querySelector('.briefing-category-tag')?.textContent || 'Card'} details`); }
      else { body.removeAttribute('tabindex'); body.removeAttribute('role'); body.removeAttribute('aria-label'); }
    });
    const update = () => { edges(); bodies(); };
    if (typeof ResizeObserver === 'function') new ResizeObserver(update).observe(row);
    new MutationObserver(() => requestAnimationFrame(update)).observe(row, { subtree: true, childList: true, characterData: true });
    update();
  }
  function overflows(el) { return el.scrollHeight > el.clientHeight + 1; }

  window.addEventListener('DOMContentLoaded', async () => {
    setupCardRow();
    // Bind deck controls
    document.getElementById('briefingRefreshBtn')?.addEventListener('click', (e) => refreshBriefing(e.target));
    document.getElementById('briefingDismissBtn')?.addEventListener('click', dismissBriefing);

    // Initial fetch
    const briefing = await fetchLatestBriefing();
    renderBriefing(briefing);
  });
})();

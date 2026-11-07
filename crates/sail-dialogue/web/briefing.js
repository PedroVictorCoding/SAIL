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
    if (activeBriefing) activeBriefing.dismissed = true;
    announce({fresh: false, closed: true});
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
      return;
    }

    deck.hidden = false;

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
        playBtn.disabled = false;
        playBtn.classList.remove('playing');
        playBtn.textContent = m.is_spotify ? '▶ Play on Spotify' : '▶ Play Ambient Audio';
        playBtn.onclick = () => playStagedMusic(m.uri, playBtn);
      }
    }

    if (musicCard && !briefing.staged_music) {
      document.getElementById('briefingMusicSlot').textContent = 'Listening history';
      document.getElementById('briefingMusicTitle').textContent = 'No music staged';
      document.getElementById('briefingMusicArtist').textContent = '';
      document.getElementById('briefingMusicRationale').textContent =
        'Connect or reconnect Spotify to allow recent listening history, then refresh. No track is staged when history is empty or unavailable.';
      const playBtn = document.getElementById('briefingPlayMusicBtn');
      if (playBtn) {
        playBtn.disabled = true;
        playBtn.onclick = null;
        playBtn.classList.remove('playing');
        playBtn.textContent = 'Playback unavailable';
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

  window.sailBriefing = {
    refresh: () => refreshBriefing(document.getElementById('briefingRefreshBtn')),
    show: () => { if (activeBriefing) renderBriefing(activeBriefing); }
  };

  // The cards sit in a grid. A card whose text overflows becomes focusable so its body can
  // be scrolled by keyboard.
  function setupCardRow() {
    const row = document.querySelector('#returnBriefingDeck .briefing-cards-grid');
    if (!row) return;
    const bodies = () => row.querySelectorAll('.briefing-card-body').forEach((body) => {
      if (overflows(body)) { body.tabIndex = 0; body.setAttribute('role', 'region'); body.setAttribute('aria-label', `${body.closest('.briefing-card')?.querySelector('.briefing-category-tag')?.textContent || 'Card'} details`); }
      else { body.removeAttribute('tabindex'); body.removeAttribute('role'); body.removeAttribute('aria-label'); }
    });
    const update = bodies;
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

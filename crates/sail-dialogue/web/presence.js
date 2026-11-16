'use strict';
// SAIL Presence — web heartbeat, hands-free toggle, and Settings -> Presence.
// Reports metadata only (visibility, idle seconds, voice readiness). Never reads key values.
(() => {
  const DEVICE_ID_KEY = 'sail_presence_device_id';
  const HANDS_FREE_KEY = 'sail_presence_hands_free';
  const HEARTBEAT_MS = 30000;
  const IDLE_WAKE_SECS = 60;
  const INPUT_THROTTLE_MS = 5000;
  const MAX_IDLE_SECS = 86400;

  let ownerTokenValue = null;
  let attachedWs = null;
  let lastInputAt = Date.now();
  let lastWakeHeartbeatAt = 0;
  let heartbeatInFlight = false;
  let handsFreeListeners = [];
  let currentHandsFreeState = 'off';

  function makeDeviceId() {
    const rand = Math.random().toString(36).slice(2, 10);
    return `web-${rand || 'client'}`.slice(0, 64);
  }

  function loadDeviceId() {
    try {
      const stored = localStorage.getItem(DEVICE_ID_KEY);
      if (stored && /^[A-Za-z0-9_-]{1,64}$/.test(stored)) return stored;
      const fresh = makeDeviceId();
      localStorage.setItem(DEVICE_ID_KEY, fresh);
      return fresh;
    } catch (_) {
      return makeDeviceId();
    }
  }

  function loadHandsFree() {
    try {
      return localStorage.getItem(HANDS_FREE_KEY) === 'true';
    } catch (_) {
      return false;
    }
  }

  function saveHandsFree(enabled) {
    try {
      localStorage.setItem(HANDS_FREE_KEY, enabled ? 'true' : 'false');
    } catch (_) {}
  }

  const deviceId = loadDeviceId();
  let handsFreeEnabled = loadHandsFree();

  async function ownerToken(force) {
    if (!force && ownerTokenValue) return ownerTokenValue;
    if (!force) {
      try {
        const stored = localStorage.getItem('sail_owner_token');
        if (stored && stored.trim().length >= 32) return (ownerTokenValue = stored.trim());
      } catch (_) {}
    }
    const r = await fetch('/api/guardian/token', { cache: 'no-store' });
    if (!r.ok) throw new Error('Owner token unavailable. Open SAIL settings and enter the owner token.');
    const data = await r.json();
    ownerTokenValue = data.token;
    try {
      localStorage.setItem('sail_owner_token', ownerTokenValue);
    } catch (_) {}
    return ownerTokenValue;
  }

  async function fetchJson(url, options = {}) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const token = await ownerToken(attempt > 0);
      const res = await fetch(url, {
        ...options,
        cache: 'no-store',
        headers: {
          ...(options.headers || {}),
          Authorization: `Bearer ${token}`,
        },
      });
      if (res.status === 401 && attempt === 0) {
        ownerTokenValue = null;
        try {
          localStorage.removeItem('sail_owner_token');
        } catch (_) {}
        continue;
      }
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      return data;
    }
    throw new Error('Owner authentication failed.');
  }

  function currentIdleSecs() {
    const elapsed = Math.floor((Date.now() - lastInputAt) / 1000);
    return Math.max(0, Math.min(MAX_IDLE_SECS, elapsed));
  }

  function isVoiceReady() {
    return Boolean(attachedWs && attachedWs.readyState === 1);
  }

  function sendSurface() {
    if (!isVoiceReady()) return;
    try {
      attachedWs.send(JSON.stringify({
        type: 'surface',
        device_id: deviceId,
        surface: 'web',
        hands_free: Boolean(handsFreeEnabled),
      }));
    } catch (_) {}
  }

  async function sendHeartbeat() {
    if (heartbeatInFlight) return;
    heartbeatInFlight = true;
    try {
      const body = {
        device_id: deviceId,
        surface: 'web',
        visible: document.visibilityState !== 'hidden',
        input_idle_secs: currentIdleSecs(),
        voice_ready: isVoiceReady(),
      };
      const res = await fetchJson('/api/presence/heartbeat', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (res && res.state) renderPlace(res.state);
    } catch (_) {
      // Heartbeat failures are quiet when offline or unauthenticated.
    } finally {
      heartbeatInFlight = false;
    }
  }

  function noteInput() {
    const now = Date.now();
    const wasIdleSecs = (now - lastInputAt) / 1000;
    lastInputAt = now;
    if (wasIdleSecs >= IDLE_WAKE_SECS && now - lastWakeHeartbeatAt >= INPUT_THROTTLE_MS) {
      lastWakeHeartbeatAt = now;
      sendHeartbeat();
    }
  }

  function placeLabel(place) {
    switch (place) {
      case 'desk': return "You're at the desk";
      case 'dock': return "You're at the dock";
      case 'phone': return "You're on your phone";
      case 'away': return 'Away';
      case 'asleep': return 'Quiet hours (asleep)';
      default: return 'Unknown';
    }
  }

  function renderPlace(state) {
    const el = document.getElementById('presencePlaceValue');
    if (!el || !state) return;
    el.textContent = placeLabel(state.place);
  }

  function renderHandsFreeUi() {
    const btn = document.getElementById('handsFreeBtn');
    const toggle = document.getElementById('presenceHandsFreeToggle');
    const pill = document.getElementById('handsFreeStatePill');
    const text = document.getElementById('handsFreeStateText');

    if (btn) {
      btn.setAttribute('aria-pressed', handsFreeEnabled ? 'true' : 'false');
      btn.classList.toggle('active', handsFreeEnabled);
      btn.title = handsFreeEnabled ? 'Turn off hands-free' : 'Turn on hands-free';
    }
    if (toggle) {
      toggle.checked = handsFreeEnabled;
    }
    if (pill && text) {
      if (!handsFreeEnabled || currentHandsFreeState === 'off') {
        pill.hidden = true;
        pill.dataset.hfState = 'off';
        text.textContent = '';
      } else if (currentHandsFreeState === 'follow_up') {
        pill.hidden = false;
        pill.dataset.hfState = 'follow_up';
        text.textContent = 'Go ahead';
      } else {
        pill.hidden = false;
        pill.dataset.hfState = 'listening';
        text.textContent = 'Listening for "SAIL"';
      }
    }
  }

  function setHandsFree(enabled) {
    handsFreeEnabled = Boolean(enabled);
    saveHandsFree(handsFreeEnabled);
    if (!handsFreeEnabled) currentHandsFreeState = 'off';
    sendSurface();
    renderHandsFreeUi();
    for (const cb of handsFreeListeners) {
      try { cb(handsFreeEnabled); } catch (_) {}
    }
  }

  function onHandsFreeChange(cb) {
    if (typeof cb === 'function') handsFreeListeners.push(cb);
  }

  function applySettingsToForm(s) {
    if (!s) return;
    const awayMins = document.getElementById('presenceAwayMins');
    const greetEnabled = document.getElementById('presenceGreetOnArrival');
    const greetMins = document.getElementById('presenceGreetAfterMins');
    const speakEnabled = document.getElementById('presenceSpeakAnnouncements');
    const maxSpoken = document.getElementById('presenceMaxSpoken');
    const telegramAway = document.getElementById('presenceTelegramAway');
    const speakWeb = document.getElementById('presenceSpeakWeb');
    const speakDock = document.getElementById('presenceSpeakDock');
    const speakPhone = document.getElementById('presenceSpeakPhone');

    if (awayMins && Number.isFinite(s.away_after_secs)) {
      awayMins.value = String(Math.max(1, Math.round(s.away_after_secs / 60)));
    }
    if (greetEnabled) greetEnabled.checked = s.greet_on_arrival !== false;
    if (greetMins && Number.isFinite(s.greet_after_away_secs)) {
      greetMins.value = String(Math.max(1, Math.round(s.greet_after_away_secs / 60)));
    }
    if (speakEnabled) speakEnabled.checked = s.speak_announcements !== false;
    if (maxSpoken && Number.isFinite(s.max_spoken_per_hour)) {
      maxSpoken.value = String(s.max_spoken_per_hour);
    }
    if (telegramAway) telegramAway.checked = s.telegram_when_away === true;

    const surfaces = Array.isArray(s.speak_on) ? s.speak_on : ['web', 'dock'];
    if (speakWeb) speakWeb.checked = surfaces.includes('web');
    if (speakDock) speakDock.checked = surfaces.includes('dock');
    if (speakPhone) speakPhone.checked = surfaces.includes('phone');
  }

  async function loadPresenceSettings() {
    const statusEl = document.getElementById('presenceSettingsStatus');
    try {
      const [state, settings] = await Promise.all([
        fetchJson('/api/presence').catch(() => null),
        fetchJson('/api/presence/settings'),
      ]);
      if (state) renderPlace(state);
      applySettingsToForm(settings);
      if (statusEl) {
        statusEl.textContent = '';
        statusEl.classList.remove('error');
      }
    } catch (err) {
      if (statusEl) {
        statusEl.textContent = err.message || 'Presence settings unavailable.';
        statusEl.classList.add('error');
      }
    }
  }

  async function savePresenceSettings(event) {
    if (event) event.preventDefault();
    const statusEl = document.getElementById('presenceSettingsStatus');
    const awayMins = Number(document.getElementById('presenceAwayMins')?.value || 10);
    const greetMins = Number(document.getElementById('presenceGreetAfterMins')?.value || 45);
    const maxSpoken = Number(document.getElementById('presenceMaxSpoken')?.value ?? 3);

    const speakOn = [];
    if (document.getElementById('presenceSpeakWeb')?.checked) speakOn.push('web');
    if (document.getElementById('presenceSpeakDock')?.checked) speakOn.push('dock');
    if (document.getElementById('presenceSpeakPhone')?.checked) speakOn.push('phone');

    const payload = {
      away_after_secs: Math.max(60, Math.round(awayMins * 60)),
      greet_on_arrival: Boolean(document.getElementById('presenceGreetOnArrival')?.checked),
      greet_after_away_secs: Math.max(60, Math.round(greetMins * 60)),
      speak_announcements: Boolean(document.getElementById('presenceSpeakAnnouncements')?.checked),
      speak_on: speakOn,
      max_spoken_per_hour: Number.isFinite(maxSpoken) ? Math.round(maxSpoken) : 3,
      telegram_when_away: Boolean(document.getElementById('presenceTelegramAway')?.checked),
    };

    try {
      const saved = await fetchJson('/api/presence/settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      applySettingsToForm(saved);
      if (statusEl) {
        statusEl.textContent = 'Presence preferences saved.';
        statusEl.classList.remove('error');
      }
    } catch (err) {
      if (statusEl) {
        statusEl.textContent = err.message || 'Could not save presence preferences.';
        statusEl.classList.add('error');
      }
    }
  }

  function wireDom() {
    for (const evt of ['pointerdown', 'keydown', 'wheel', 'touchstart']) {
      window.addEventListener(evt, noteInput, { passive: true });
    }
    document.addEventListener('visibilitychange', () => {
      sendHeartbeat();
      if (document.visibilityState === 'hidden' && handsFreeEnabled) {
        for (const cb of handsFreeListeners) {
          try { cb(false); } catch (_) {}
        }
      }
    });

    const btn = document.getElementById('handsFreeBtn');
    if (btn) {
      btn.addEventListener('click', () => setHandsFree(!handsFreeEnabled));
    }

    const toggle = document.getElementById('presenceHandsFreeToggle');
    if (toggle) {
      toggle.addEventListener('change', () => setHandsFree(toggle.checked));
    }

    const form = document.getElementById('presenceSettingsForm');
    if (form) {
      form.addEventListener('submit', savePresenceSettings);
    }

    const presenceTabBtn = document.querySelector('[data-settings-tab="presence"]');
    if (presenceTabBtn) {
      presenceTabBtn.addEventListener('click', () => loadPresenceSettings());
    }
    const settingsOpenBtn = document.getElementById('settingsOpen');
    if (settingsOpenBtn) {
      settingsOpenBtn.addEventListener('click', () => {
        const panel = document.getElementById('settingsPanel-presence');
        if (panel && !panel.hidden) loadPresenceSettings();
      });
    }

    renderHandsFreeUi();
    setInterval(sendHeartbeat, HEARTBEAT_MS);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', wireDom);
  } else {
    wireDom();
  }

  window.SailPresence = {
    deviceId: () => deviceId,
    isHandsFree: () => handsFreeEnabled,
    setHandsFree,
    onHandsFreeChange,
    attach(ws) {
      attachedWs = ws;
      sendSurface();
      sendHeartbeat();
    },
    onDisconnect() {
      attachedWs = null;
      currentHandsFreeState = 'off';
      renderHandsFreeUi();
    },
    onServerState(state) {
      renderPlace(state);
    },
    onServerHandsFree(state) {
      currentHandsFreeState = state === 'listening' || state === 'follow_up' ? state : 'off';
      renderHandsFreeUi();
    },
    loadSettings: loadPresenceSettings,
    fetchJson,
  };
})();

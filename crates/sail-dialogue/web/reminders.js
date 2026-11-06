'use strict';
// Pulse reminders. Classic script sharing approvals.js helpers. Reminder text is always set
// as text, never HTML. The server owns the schedule; this page is a projection of it.
(() => {
  const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  const offsetMinutes = -new Date().getTimezoneOffset();
  const shown = new Set();
  let reminders = [];
  let history = [];
  let settings = { quiet_hours_enabled:false, quiet_start_minute:1320, quiet_end_minute:420,
    spoken_reminders:false, in_app_notifications:true, timezone, utc_offset_minutes:offsetMinutes };
  let timezoneSynced = false;
  let loading = false;

  const el = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const status = (message, error = false) => {
    const box = document.getElementById('pulseStatus');
    if (!box) return;
    box.textContent = message || '';
    box.classList.toggle('error', error);
  };
  const settingStatus = (message, error = false) => {
    const box = document.getElementById('pulseSettingsStatus');
    if (!box) return;
    box.textContent = message || '';
    box.classList.toggle('error', error);
  };

  async function request(path, body = null) {
    const token = await getOrFetchOwnerToken();
    const response = await fetch(`/api/pulse${path}`, {
      method: body ? 'POST' : 'GET', cache: 'no-store',
      headers: { Authorization: `Bearer ${token}`, ...(body ? { 'Content-Type': 'application/json' } : {}) },
      ...(body ? { body: JSON.stringify(body) } : {}),
    });
    const data = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(response.status === 401 ? 'Owner token not accepted.'
        : data.error || 'Pulse is unavailable.');
    }
    return data;
  }

  function formatTime(iso, zone = timezone) {
    const date = new Date(iso);
    if (Number.isNaN(date.getTime())) return 'unknown time';
    try {
      return new Intl.DateTimeFormat(undefined, { dateStyle:'medium', timeStyle:'short', timeZone:zone }).format(date);
    } catch (_) {
      return date.toLocaleString();
    }
  }

  function localInputValue(date) {
    const pad = value => String(value).padStart(2, '0');
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
  }

  function defaultDue() {
    const input = document.getElementById('pulseDue');
    if (!input) return;
    input.value = localInputValue(new Date(Date.now() + 20 * 60 * 1000));
  }

  function stateBadge(state) {
    const labels = { pending:'Waiting', due:'Due', delivered:'Delivered', cancelled:'Cancelled' };
    return el('span', `pulse-state ${state}`, labels[state] || state);
  }

  function actionButton(label, action, danger = false) {
    const button = el('button', danger ? 'pulse-danger' : null, label);
    button.type = 'button';
    button.addEventListener('click', async () => {
      button.disabled = true;
      try { await action(); } catch (error) { status(error.message, true); }
      finally { button.disabled = false; }
    });
    return button;
  }

  function snoozeParts(reminder) {
    const select = el('select');
    select.setAttribute('aria-label', `Snooze ${reminder.text}`);
    for (const [minutes, label] of [[5,'5 min'],[10,'10 min'],[30,'30 min'],[60,'1 hour']]) {
      const option = el('option', null, label);
      option.value = String(minutes);
      if (minutes === 10) option.selected = true;
      select.append(option);
    }
    const button = actionButton('Snooze', async () => {
      await request(`/reminders/${encodeURIComponent(reminder.id)}/snooze`, { minutes:Number(select.value) });
      status('Reminder snoozed.');
      await load();
    });
    return [select, button];
  }

  function renderReminder(reminder, isHistory = false) {
    const item = el('article', `pulse-item ${reminder.status}`);
    const main = el('div', 'pulse-item-main');
    main.append(el('span', 'pulse-item-text', reminder.text));
    const meta = el('div', 'pulse-item-meta');
    meta.append(stateBadge(reminder.status));
    const time = el('span', null, formatTime(reminder.due_at, reminder.timezone));
    time.title = `${reminder.due_at} · ${reminder.timezone}`;
    meta.append(time);
    if (reminder.snooze_count) meta.append(el('span', null, `Snoozed ${reminder.snooze_count}×`));
    main.append(meta);
    item.append(main);

    const actions = el('div', 'pulse-item-actions');
    if (reminder.status === 'pending' || reminder.status === 'due') {
      actions.append(...snoozeParts(reminder));
      if (reminder.status === 'due') {
        actions.append(actionButton('Dismiss', async () => {
          await request(`/reminders/${encodeURIComponent(reminder.id)}/acknowledge`, {});
          await load();
        }));
      }
      actions.append(actionButton('Cancel', async () => {
        await request(`/reminders/${encodeURIComponent(reminder.id)}/cancel`, {});
        status('Reminder cancelled.');
        await load();
      }, true));
    } else if (reminder.status === 'delivered') {
      actions.append(...snoozeParts(reminder));
    }
    item.append(actions);
    return item;
  }

  function renderList(target, list, isHistory = false) {
    if (!target) return;
    target.replaceChildren();
    if (!list.length) {
      target.append(el('p', 'pulse-empty', isHistory ? 'Nothing delivered or cancelled yet.' : 'No upcoming reminders.'));
      return;
    }
    for (const reminder of list) target.append(renderReminder(reminder, isHistory));
  }

  async function load() {
    if (loading) return;
    loading = true;
    status('Loading...');
    try {
      const data = await request('/reminders');
      reminders = data.reminders || [];
      history = data.history || [];
      settings = data.settings || settings;
      if (!timezoneSynced && settings.timezone) {
        settings.timezone = timezone;
        settings.utc_offset_minutes = offsetMinutes;
        await request('/settings', settings);
        timezoneSynced = true;
      }
      renderList(document.getElementById('pulseList'), reminders);
      renderList(document.getElementById('pulseHistory'), history, true);
      renderSettings();
      status('');
    } catch (error) {
      status(error.message, true);
    } finally {
      loading = false;
    }
  }

  function renderSettings() {
    const startHour = Math.floor(settings.quiet_start_minute / 60);
    const startMin = settings.quiet_start_minute % 60;
    const endHour = Math.floor(settings.quiet_end_minute / 60);
    const endMin = settings.quiet_end_minute % 60;
    
    const quietStart = document.getElementById('pulseQuietStart');
    const quietEnd = document.getElementById('pulseQuietEnd');
    const quietEnabled = document.getElementById('pulseQuietEnabled');
    const spokenEnabled = document.getElementById('pulseSpokenEnabled');
    
    if (quietStart) quietStart.value = `${String(startHour).padStart(2,'0')}:${String(startMin).padStart(2,'0')}`;
    if (quietEnd) quietEnd.value = `${String(endHour).padStart(2,'0')}:${String(endMin).padStart(2,'0')}`;
    if (quietEnabled) quietEnabled.checked = settings.quiet_hours_enabled;
    if (spokenEnabled) spokenEnabled.checked = settings.spoken_reminders;
  }

  async function createReminder() {
    const textInput = document.getElementById('pulseText');
    const dueInput = document.getElementById('pulseDue');
    if (!textInput || !dueInput) return;
    
    const text = textInput.value.trim();
    if (!text) { status('Reminder text is required.', true); return; }
    
    const dueDate = new Date(dueInput.value);
    if (Number.isNaN(dueDate.getTime())) { status('Invalid due time.', true); return; }
    
    try {
      await request('/reminders', {
        text,
        due_at: dueDate.toISOString(),
        timezone,
        utc_offset_minutes: offsetMinutes
      });
      textInput.value = '';
      defaultDue();
      status('Reminder created.');
      await load();
    } catch (error) {
      status(error.message, true);
    }
  }

  async function saveSettings() {
    const quietStart = document.getElementById('pulseQuietStart')?.value;
    const quietEnd = document.getElementById('pulseQuietEnd')?.value;
    const quietEnabled = document.getElementById('pulseQuietEnabled')?.checked;
    const spokenEnabled = document.getElementById('pulseSpokenEnabled')?.checked;
    
    try {
      const parseTime = (str) => {
        const [h, m] = str.split(':').map(Number);
        return h * 60 + m;
      };
      
      const newSettings = {
        ...settings,
        quiet_hours_enabled: quietEnabled ?? settings.quiet_hours_enabled,
        quiet_start_minute: quietStart ? parseTime(quietStart) : settings.quiet_start_minute,
        quiet_end_minute: quietEnd ? parseTime(quietEnd) : settings.quiet_end_minute,
        spoken_reminders: spokenEnabled ?? settings.spoken_reminders,
        timezone,
        utc_offset_minutes: offsetMinutes
      };
      
      await request('/settings', newSettings);
      settings = newSettings;
      settingStatus('Settings saved.');
      setTimeout(() => settingStatus(''), 3000);
    } catch (error) {
      settingStatus(error.message, true);
    }
  }

  function init() {
    const form = document.getElementById('pulseForm');
    const settingsForm = document.getElementById('pulseSettingsForm');
    const presetButtons = document.querySelectorAll('[data-pulse-preset]');
    const refreshBtn = document.getElementById('pulseRefreshBtn');
    
    if (form) form.addEventListener('submit', (e) => {
      e.preventDefault();
      createReminder();
    });
    
    if (settingsForm) settingsForm.addEventListener('submit', (e) => {
      e.preventDefault();
      saveSettings();
    });
    
    presetButtons.forEach(btn => {
      btn.addEventListener('click', () => {
        const minutes = parseInt(btn.dataset.pulsePreset, 10);
        const input = document.getElementById('pulseDue');
        if (input) {
          input.value = localInputValue(new Date(Date.now() + minutes * 60 * 1000));
        }
      });
    });
    
    if (refreshBtn) refreshBtn.addEventListener('click', load);
    
    // Set default due time to 20 minutes from now
    defaultDue();
    
    // Load initial data
    load();
    
    // Refresh every 10 seconds to check for new due reminders
    setInterval(load, 10000);
    
    // Refresh when the page becomes visible
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) load();
    });
  }

  // Initialize when the page loads and when Pulse is navigated to
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
  
  window.addEventListener('sail:page', (event) => {
    if (event.detail === 'pulse') {
      setTimeout(load, 100);
    }
  });
})();

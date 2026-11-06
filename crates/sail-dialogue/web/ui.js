'use strict';
const spaces = subsystemPages.map(page => ({
  id:page.id, icon:page.icon, name:page.name, subtitle:page.subtitle, available:true,
  action:() => window.sailNavigate(page.id),
})).concat([
  { id:'vision', name:'Vision', subtitle:'Camera & desktop feeds', available:true, action:() => {
    window.sailNavigate('canvas', 'canvasPanelCamera');
  } },
  { id:'life', name:'Life', subtitle:'Personal space', available:false },
  { id:'entertainment', name:'Entertainment', subtitle:'Listen and read', available:true, action:() => window.sailNavigate('entertainment') },
  { id:'sense', name:'Sense', subtitle:'Context', available:false },
  { id:'mesh', name:'Mesh', subtitle:'Local network', available:false },
  { id:'lab', name:'Lab', subtitle:'Experiments', available:false },
]);
const spacesSheet = document.getElementById('spacesSheet');
const contextSheet = document.getElementById('contextSheet');
const scrim = document.getElementById('scrim');
const sheets = [spacesSheet, contextSheet];
let activeSheet = null;
let priorFocus = null;
function openSheet(sheet) {
  if (activeSheet) closeSheet();
  priorFocus = document.activeElement; activeSheet = sheet;
  sheet.hidden = false; scrim.hidden = false;
  requestAnimationFrame(() => scrim.classList.add('visible'));
  document.body.style.overflow = 'hidden';
  sheet.querySelector('button,input,select')?.focus();
}
function closeSheet() {
  if (!activeSheet) return;
  scrim.classList.remove('visible'); activeSheet.hidden = true;
  const wasSpaces = activeSheet === spacesSheet; activeSheet = null;
  if (wasSpaces && window.location.hash === '#spaces') history.replaceState(null, '', `#${window.sailCurrentPage?.() || 'home'}`);
  setTimeout(() => { if (!activeSheet) scrim.hidden = true; }, 240);
  document.body.style.overflow = ''; priorFocus?.focus?.();
}
function renderSpaces(filter = '') {
  const list = document.getElementById('spaceList'); list.replaceChildren();
  const query = filter.trim().toLocaleLowerCase();
  for (const space of spaces.filter(item => `${item.name} ${item.subtitle}`.toLocaleLowerCase().includes(query))) {
    const button = document.createElement('button'); button.type = 'button';
    button.className = 'quiet-button space-card'; button.dataset.space = space.id;
    button.setAttribute('aria-label', space.available ? `${space.name}: ${space.subtitle}` : `${space.name}: Not connected`);
    const icon = document.createElement('span'); icon.className = 'space-icon'; icon.setAttribute('aria-hidden', 'true');
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('aria-hidden', 'true');
    const use = document.createElementNS('http://www.w3.org/2000/svg', 'use'); use.setAttribute('href', `/icons.svg#${space.icon || space.id}`); svg.append(use); icon.append(svg);
    const text = document.createElement('span'); text.className = 'space-name'; text.textContent = space.name;
    const subtitle = document.createElement('small'); subtitle.className = 'space-subtitle'; subtitle.textContent = space.available ? space.subtitle : 'Not connected';
    text.append(subtitle); button.append(icon, text);
    button.addEventListener('click', () => {
      if (!space.available) { showError(`${space.name} is not connected in this build.`); return; }
      closeSheet(); space.action?.();
    });
    list.append(button);
  }
}
renderSpaces();
document.getElementById('spacesOpen').addEventListener('click', () => { location.hash = '#spaces'; openSheet(spacesSheet); });
document.getElementById('spacesTop').addEventListener('click', () => { location.hash = '#spaces'; openSheet(spacesSheet); });
document.getElementById('settingsOpen').addEventListener('click', () => openSheet(contextSheet));
document.getElementById('settingsRail').addEventListener('click', () => openSheet(contextSheet));

// VS Code Style Settings Tab Navigation & Search
function initVscodeSettings() {
  const navItems = document.querySelectorAll('.vscode-nav-item');
  const panels = document.querySelectorAll('.vscode-category-panel');
  const searchInput = document.getElementById('settingsSearchInput');
  const clearBtn = document.getElementById('settingsSearchClear');

  function switchTab(categoryId) {
    navItems.forEach(item => {
      const isTarget = item.dataset.settingsTab === categoryId;
      item.classList.toggle('active', isTarget);
      item.setAttribute('aria-selected', String(isTarget));
    });
    panels.forEach(panel => {
      const isTarget = panel.dataset.category === categoryId;
      panel.classList.toggle('active', isTarget);
      panel.hidden = !isTarget;
    });
  }

  navItems.forEach(item => {
    item.addEventListener('click', () => {
      const cat = item.dataset.settingsTab;
      if (cat) switchTab(cat);
    });
  });

  if (searchInput) {
    searchInput.addEventListener('input', (e) => {
      const query = e.target.value.trim().toLowerCase();
      if (clearBtn) clearBtn.hidden = !query;

      const items = document.querySelectorAll('.vscode-setting-item');
      if (!query) {
        items.forEach(el => el.classList.remove('is-hidden'));
        // Restore currently active tab
        const activeNav = document.querySelector('.vscode-nav-item.active');
        if (activeNav) switchTab(activeNav.dataset.settingsTab);
        return;
      }

      // Search across all setting items in all categories
      panels.forEach(p => { p.hidden = false; p.classList.add('active'); });
      items.forEach(item => {
        const text = (item.textContent + ' ' + (item.dataset.searchTerms || '')).toLowerCase();
        const matches = text.includes(query);
        item.classList.toggle('is-hidden', !matches);
      });
    });

    if (clearBtn) {
      clearBtn.addEventListener('click', () => {
        searchInput.value = '';
        clearBtn.hidden = true;
        const items = document.querySelectorAll('.vscode-setting-item');
        items.forEach(el => el.classList.remove('is-hidden'));
        const activeNav = document.querySelector('.vscode-nav-item.active') || navItems[0];
        if (activeNav) switchTab(activeNav.dataset.settingsTab);
        searchInput.focus();
      });
    }
  }

  document.getElementById('settingsRefreshTelemetryBtn')?.addEventListener('click', () => {
    if (typeof sendAction === 'function') sendAction('get_telemetry');
  });
  document.getElementById('settingsOsStatusBtn')?.addEventListener('click', () => {
    closeSheet();
    runKernelTool('os:status');
  });
}
initVscodeSettings();
document.getElementById('checkSystemBtn').addEventListener('click', () => runKernelTool('os:status'));
document.getElementById('modelsRefreshBtn').addEventListener('click', () => checkModels());
document.getElementById('listIssuesBtn')?.addEventListener('click', () => runKernelTool('github:issues'));
document.getElementById('forgeQuickEnqueueBtn')?.addEventListener('click', () => openForgeEnqueueDialog());
document.getElementById('forgeRefreshBtn')?.addEventListener('click', () => sendAction('forge:list'));
document.getElementById('forgeClearCompletedBtn')?.addEventListener('click', () => sendAction('forge:clear', { job_id: 'all' }));
document.getElementById('forgeCancelBtn')?.addEventListener('click', () => document.getElementById('forgeEnqueueDialog')?.close());
document.getElementById('forgeEnqueueForm')?.addEventListener('submit', (e) => {
  e.preventDefault();
  const issueVal = document.getElementById('forgeIssueNumberInput')?.value?.trim();
  const taskVal = document.getElementById('forgeTaskInput')?.value?.trim();
  if (!taskVal) return;
  const payload = { task: taskVal };
  if (issueVal) payload.issue_number = parseInt(issueVal, 10);
  sendAction('forge:enqueue', payload);
  document.getElementById('forgeEnqueueDialog')?.close();
  document.getElementById('forgeTaskInput').value = '';
  document.getElementById('forgeIssueNumberInput').value = '';
});
function openForgeEnqueueDialog() {
  const dialog = document.getElementById('forgeEnqueueDialog');
  if (dialog) dialog.showModal();
}
const cardModal = document.getElementById('cardModal');
cardModal?.addEventListener('click', (e) => {
  if (e.target === cardModal) cardModal.close();
});
document.getElementById('cardModalCloseBtn')?.addEventListener('click', () => {
  cardModal?.close();
});
document.getElementById('canvasClearBtn').addEventListener('click', () => {
  const panel = document.getElementById('canvasPanel'); panel.replaceChildren();
  const empty = document.createElement('p'); empty.id = 'canvasEmptyState'; empty.className = 'canvas-empty';
  empty.textContent = 'Diagrams, images and structured results from this conversation appear here.'; panel.append(empty);
});
function updateLocalClock() {
  const now = new Date(); const hour = now.getHours();
  document.getElementById('welcomeGreeting').textContent = hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening';
  document.getElementById('localDateTime').textContent = new Intl.DateTimeFormat(undefined, { weekday:'short', day:'numeric', month:'short', hour:'2-digit', minute:'2-digit' }).format(now);
}
updateLocalClock(); setInterval(updateLocalClock, 60_000);
document.querySelectorAll('.close-sheet').forEach(button => button.addEventListener('click', closeSheet));
scrim.addEventListener('click', closeSheet);
document.getElementById('spaceSearch').addEventListener('input', event => renderSpaces(event.target.value));
document.getElementById('attachBtn').addEventListener('click', () => document.getElementById('imageInput').click());
document.getElementById('micBtn').addEventListener('click', () => toggleMic());
document.getElementById('muteDockBtn').addEventListener('click', () => toggleMute());
document.getElementById('endTurnBtn').addEventListener('click', () => submitVoiceTurn());
document.getElementById('interruptBtn').addEventListener('click', () => interrupt());
document.getElementById('newConversationBtn').addEventListener('click', () => { resetSession(); closeSheet(); });
document.getElementById('muteBtn').addEventListener('click', () => toggleMute());
document.getElementById('tierBtn').addEventListener('click', () => cycleTier());
document.getElementById('voiceSelect').addEventListener('change', event => { sendAction('set_voice', { voice:event.target.value }); announceClientSpeech(); renderSpeechPlacement(); });
document.getElementById('speechPlacementSelect').addEventListener('change', event => {
  SailClientSpeech.setPreferred(event.target.value === 'device');
  if (event.target.value === 'device') loadClientSpeech();
  else { SailClientSpeech.stop('Speech runs on the SAIL host (chosen in settings).'); announceClientSpeech(); renderSpeechPlacement(); }
});
document.getElementById('turnModeSelect').addEventListener('change', setTurnDetection);
document.getElementById('turnPauseInput').addEventListener('change', setTurnDetection);
document.getElementById('checkModelsBtn').addEventListener('click', () => checkModels());
document.getElementById('imageInput').addEventListener('change', event => {
  const file = event.target.files?.[0];
  if (file) { document.getElementById('transcript').textContent = `Image selected · ${file.name}. Send a message to analyze it locally.`; }
});
document.getElementById('cameraStopBtn').addEventListener('click', () => stopCamera());
document.getElementById('cameraCaptureBtn').addEventListener('click', () => captureCameraFrame());
document.addEventListener('keydown', event => {
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') { event.preventDefault(); if (!activeSheet) openSheet(spacesSheet); else closeSheet(); }
  if (event.key === 'Escape' && activeSheet) closeSheet();
  if (event.key === 'Tab' && activeSheet) {
    const candidates = [...activeSheet.querySelectorAll('button:not([disabled]),input:not([disabled]),select:not([disabled]),summary')].filter(node => node.offsetParent !== null);
    if (!candidates.length) return;
    if (event.shiftKey && document.activeElement === candidates[0]) { event.preventDefault(); candidates.at(-1).focus(); }
    else if (!event.shiftKey && document.activeElement === candidates.at(-1)) { event.preventDefault(); candidates[0].focus(); }
  }
});
let touchStartY = null;
document.addEventListener('touchstart', event => { touchStartY = event.touches[0]?.clientY ?? null; }, { passive:true });
document.addEventListener('touchend', event => {
  if (touchStartY === null) return;
  const delta = event.changedTouches[0].clientY - touchStartY; touchStartY = null;
  if (!activeSheet && delta < -90 && window.scrollY < 8) openSheet(spacesSheet);
  else if (activeSheet && delta > 90 && activeSheet.scrollTop <= 0) closeSheet();
}, { passive:true });
window.addEventListener('hashchange', () => { if (location.hash === '#spaces' && !activeSheet) openSheet(spacesSheet); else if (location.hash !== '#spaces' && activeSheet === spacesSheet) closeSheet(); });

// --- Card Tabs & Cycling Manager ---
function switchCardTab(cardId, targetPanelId) {
  const card = document.getElementById(cardId);
  if (!card) return;

  const tabs = card.querySelectorAll('.card-tab');
  tabs.forEach(tab => {
    const isTarget = tab.dataset.tabPanel === targetPanelId;
    tab.classList.toggle('active', isTarget);
    tab.setAttribute('aria-selected', isTarget ? 'true' : 'false');
    tab.tabIndex = isTarget ? 0 : -1;
  });

  const panels = card.querySelectorAll('.card-tab-panel');
  panels.forEach(panel => {
    const isTarget = panel.id === targetPanelId;
    panel.hidden = !isTarget;
    panel.classList.toggle('active', isTarget);
  });

  if (cardId === 'canvasCard') {
    const iconEl = document.getElementById('canvasCardIcon');
    const titleEl = document.getElementById('canvasCardTitle');
    const subEl = document.getElementById('canvasCardSub');
    const clearBtn = document.getElementById('canvasClearBtn');

    if (targetPanelId === 'canvasPanelCamera') {
      if (iconEl) iconEl.textContent = '📷';
      if (titleEl) titleEl.textContent = 'Live Camera';
      if (subEl) subEl.textContent = 'Adaptive slow frame preview';
      if (clearBtn) clearBtn.hidden = true;
    } else if (targetPanelId === 'canvasPanelDesktop') {
      if (iconEl) iconEl.textContent = '🖥';
      if (titleEl) titleEl.textContent = 'Live Desktop';
      if (subEl) subEl.textContent = 'Adaptive screen share preview';
      if (clearBtn) clearBtn.hidden = true;
    } else {
      if (iconEl) iconEl.textContent = '▧';
      if (titleEl) titleEl.textContent = 'Canvas';
      if (subEl) subEl.textContent = 'Visuals & Guardian approvals';
      if (clearBtn) clearBtn.hidden = false;
    }
  }

  if (cardId === 'modelsCard' && targetPanelId === 'modelsPanelGovernor') {
    AdaptiveFeedEngine.updateAllBadges();
  }
}

document.querySelectorAll('.card-tabs[role="tablist"]').forEach(list => {
  const tabs = Array.from(list.querySelectorAll('.card-tab'));
  tabs.forEach(tab => {
    const active = tab.classList.contains('active');
    tab.setAttribute('role', 'tab');
    tab.setAttribute('aria-selected', String(active));
    tab.tabIndex = active ? 0 : -1;
    if (tab.dataset.tabPanel) {
      tab.setAttribute('aria-controls', tab.dataset.tabPanel);
      document.getElementById(tab.dataset.tabPanel)?.setAttribute('role', 'tabpanel');
    }
    tab.addEventListener('click', (e) => {
      e.stopPropagation();
      const cardId = tab.dataset.tabCard;
      const panelId = tab.dataset.tabPanel;
      if (cardId && panelId) switchCardTab(cardId, panelId);
    });
  });
  // Arrow keys move between tabs (WAI-ARIA tabs pattern), replacing the old cycle buttons.
  list.addEventListener('keydown', (e) => {
    const index = tabs.indexOf(document.activeElement);
    if (index < 0) return;
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    let next = step === undefined ? null : (index + step + tabs.length) % tabs.length;
    if (e.key === 'Home') next = 0;
    if (e.key === 'End') next = tabs.length - 1;
    if (next === null) return;
    e.preventDefault();
    tabs[next].click();
    tabs[next].focus();
  });
});

// --- Adaptive Feed Engine: Inversely Throttled Vision Feeds ---
const AdaptiveFeedEngine = {
  cpuUsage: 15,
  cameraActive: false,
  desktopActive: false,
  cameraStream: null,
  desktopStream: null,
  cameraFacing: 'user',
  cameraTimer: null,
  desktopTimer: null,
  telemetryTimer: null,

  updateCpuUsage(percent) {
    if (Number.isFinite(percent)) {
      this.cpuUsage = Math.max(2, Math.min(100, Math.round(percent)));
      this.updateAllBadges();
    }
  },

  calculateInterval() {
    // Inversely proportional to CPU load:
    // Low CPU (5%) -> ~240ms (~4 FPS)
    // Medium CPU (35%) -> ~650ms (~1.5 FPS)
    // High CPU (75%) -> ~1500ms (~0.66 FPS)
    // Peak CPU (95%+) -> ~2200ms (~0.45 FPS, 1 frame every 2.2s)
    const minMs = 200;
    const maxMs = 2400;
    return Math.round(minMs + (this.cpuUsage / 100) * (maxMs - minMs));
  },

  getTargetFps() {
    return (1000 / this.calculateInterval()).toFixed(1);
  },

  updateAllBadges() {
    const interval = this.calculateInterval();
    const fps = this.getTargetFps();
    const cpu = this.cpuUsage;

    const camRateBadge = document.getElementById('cameraRateBadge');
    if (camRateBadge) {
      camRateBadge.textContent = this.cameraActive
        ? `${fps} FPS (${interval}ms) · CPU ${cpu}%`
        : `Slow Mode · Idle (CPU ${cpu}%)`;
    }
    const camOverlay = document.getElementById('cameraOverlayStats');
    if (camOverlay) {
      camOverlay.textContent = `Adaptive: ${interval}ms (${fps} FPS) · Inversely matched to CPU`;
    }

    const deskRateBadge = document.getElementById('desktopRateBadge');
    if (deskRateBadge) {
      deskRateBadge.textContent = this.desktopActive
        ? `${fps} FPS (${interval}ms) · CPU ${cpu}%`
        : `Slow Mode · Idle (CPU ${cpu}%)`;
    }
    const deskOverlay = document.getElementById('desktopOverlayStats');
    if (deskOverlay) {
      deskOverlay.textContent = `Adaptive: ${interval}ms (${fps} FPS) · Inversely matched to CPU`;
    }

    const govCpuText = document.getElementById('governorCpuPercent');
    if (govCpuText) govCpuText.textContent = `${cpu}%`;
    const govCpuFill = document.getElementById('governorCpuFill');
    if (govCpuFill) {
      govCpuFill.style.width = `${cpu}%`;
      govCpuFill.style.background = cpu > 75 ? 'var(--warning)' : 'linear-gradient(90deg,#38bdf8,#818cf8)';
    }
    const govFpsText = document.getElementById('governorFpsTarget');
    if (govFpsText) govFpsText.textContent = `${fps} FPS (${interval}ms)`;
    const govFpsFill = document.getElementById('governorFpsFill');
    if (govFpsFill) govFpsFill.style.width = `${Math.min(100, Math.round((parseFloat(fps) / 5) * 100))}%`;

    const govStatus = document.getElementById('governorStatusBadge');
    if (govStatus) {
      if (cpu < 30) {
        govStatus.textContent = 'Optimal autonomy (Full FPS)';
        govStatus.className = 'good-text';
      } else if (cpu < 70) {
        govStatus.textContent = 'Moderate load (Adaptive throttle)';
        govStatus.className = 'good-text';
      } else {
        govStatus.textContent = 'High system load (Power conserve)';
        govStatus.className = 'warn-text';
      }
    }
  },

  startTelemetry() {
    if (this.telemetryTimer) return;
    this.telemetryTimer = setInterval(() => {
      if (!document.hidden && ws?.readyState === WebSocket.OPEN && !activeTools.has('os:status') && typeof runKernelTool === 'function') {
        runKernelTool('os:status');
      }
    }, 5000);
  },

  stopTelemetryIfIdle() {
    // Keep telemetry pinging smoothly for live system metrics
  },

  // --- Live Camera ---
  async toggleCamera() {
    if (this.cameraActive) {
      this.stopCamera();
    } else {
      await this.startCamera();
    }
  },

  async startCamera() {
    if (!window.isSecureContext || !navigator.mediaDevices?.getUserMedia) {
      showError('Camera access requires HTTPS or localhost in a supported browser.');
      return;
    }
    try {
      this.cameraStream = await navigator.mediaDevices.getUserMedia({
        video: { facingMode: { ideal: this.cameraFacing }, width: { ideal: 960 }, height: { ideal: 540 } },
        audio: false
      });
      const video = document.getElementById('cameraVideo');
      video.srcObject = this.cameraStream;
      await video.play();

      this.cameraActive = true;
      const placeholder = document.getElementById('cameraPlaceholder');
      if (placeholder) placeholder.hidden = true;
      const toggleBtn = document.getElementById('cameraToggleBtn');
      if (toggleBtn) {
        toggleBtn.textContent = 'Stop Camera';
        toggleBtn.classList.add('active-stop');
      }
      const captureBtn = document.getElementById('cameraCaptureBtn');
      if (captureBtn) captureBtn.disabled = false;
      const flipBtn = document.getElementById('cameraFlipBtn');
      if (flipBtn) flipBtn.disabled = false;

      const indicator = document.getElementById('cameraStatusIndicator');
      if (indicator) {
        indicator.className = 'feed-live-indicator active';
        const span = indicator.querySelector('span');
        if (span) span.textContent = 'Camera Live';
      }

      this.startTelemetry();
      this.updateAllBadges();
      this.tickCamera();
    } catch (err) {
      showError(err.name === 'NotAllowedError' ? 'Camera permission was not granted.' : `Camera error: ${err.message}`);
    }
  },

  stopCamera() {
    this.cameraActive = false;
    if (this.cameraTimer) clearTimeout(this.cameraTimer);
    this.cameraTimer = null;
    this.cameraStream?.getTracks().forEach(t => t.stop());
    this.cameraStream = null;

    const video = document.getElementById('cameraVideo');
    if (video) video.srcObject = null;
    const placeholder = document.getElementById('cameraPlaceholder');
    if (placeholder) placeholder.hidden = false;

    const toggleBtn = document.getElementById('cameraToggleBtn');
    if (toggleBtn) {
      toggleBtn.textContent = 'Start Camera';
      toggleBtn.classList.remove('active-stop');
    }
    const captureBtn = document.getElementById('cameraCaptureBtn');
    if (captureBtn) captureBtn.disabled = true;
    const flipBtn = document.getElementById('cameraFlipBtn');
    if (flipBtn) flipBtn.disabled = true;

    const indicator = document.getElementById('cameraStatusIndicator');
    if (indicator) {
      indicator.className = 'feed-live-indicator inactive';
      const span = indicator.querySelector('span');
      if (span) span.textContent = 'Camera Off';
    }

    this.updateAllBadges();
    this.stopTelemetryIfIdle();
  },

  async flipCamera() {
    this.cameraFacing = this.cameraFacing === 'user' ? 'environment' : 'user';
    if (this.cameraActive) {
      this.stopCamera();
      await this.startCamera();
    }
  },

  tickCamera() {
    if (!this.cameraActive) return;
    const video = document.getElementById('cameraVideo');
    const canvas = document.getElementById('cameraCanvas');
    if (video && canvas && video.videoWidth > 0) {
      const scale = Math.min(1, 640 / video.videoWidth, 480 / video.videoHeight);
      const w = Math.max(1, Math.round(video.videoWidth * scale));
      const h = Math.max(1, Math.round(video.videoHeight * scale));
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }
      const ctx = canvas.getContext('2d');
      ctx.drawImage(video, 0, 0, w, h);
    }
    const interval = this.calculateInterval();
    this.cameraTimer = setTimeout(() => this.tickCamera(), interval);
  },

  captureCameraFrame() {
    const canvas = document.getElementById('cameraCanvas');
    if (!this.cameraActive || !canvas.width) {
      showError('Camera preview is not active yet.');
      return;
    }
    const data = canvas.toDataURL('image/jpeg', 0.85);
    const vision = { format: 'image/jpeg', data_base64: data.split(',')[1] };
    if (typeof renderArtifact === 'function') {
      renderArtifact({ artifact_type: 'image', content: data });
    }
    if (typeof sendAction === 'function') {
      sendAction('text', { text: 'Analyze this camera frame.', vision });
      if (typeof updateState === 'function') updateState('thinking');
    }
  },

  // --- Live Desktop Screen Share ---
  async toggleDesktop() {
    if (this.desktopActive) {
      this.stopDesktop();
    } else {
      await this.startDesktop();
    }
  },

  async startDesktop() {
    if (!window.isSecureContext || !navigator.mediaDevices?.getDisplayMedia) {
      showError('Screen capture is not supported or requires HTTPS/localhost.');
      return;
    }
    try {
      this.desktopStream = await navigator.mediaDevices.getDisplayMedia({
        video: { frameRate: { ideal: 5, max: 15 } },
        audio: false
      });
      const video = document.getElementById('desktopVideo');
      video.srcObject = this.desktopStream;
      await video.play();

      const track = this.desktopStream.getVideoTracks()[0];
      if (track) track.onended = () => this.stopDesktop();

      this.desktopActive = true;
      const placeholder = document.getElementById('desktopPlaceholder');
      if (placeholder) placeholder.hidden = true;
      const toggleBtn = document.getElementById('desktopToggleBtn');
      if (toggleBtn) {
        toggleBtn.textContent = 'Stop Desktop';
        toggleBtn.classList.add('active-stop');
      }
      const captureBtn = document.getElementById('desktopCaptureBtn');
      if (captureBtn) captureBtn.disabled = false;

      const indicator = document.getElementById('desktopStatusIndicator');
      if (indicator) {
        indicator.className = 'feed-live-indicator active';
        const span = indicator.querySelector('span');
        if (span) span.textContent = 'Desktop Live';
      }

      this.startTelemetry();
      this.updateAllBadges();
      this.tickDesktop();
    } catch (err) {
      if (err.name !== 'NotAllowedError') {
        showError(`Desktop capture error: ${err.message}`);
      }
    }
  },

  stopDesktop() {
    this.desktopActive = false;
    if (this.desktopTimer) clearTimeout(this.desktopTimer);
    this.desktopTimer = null;
    this.desktopStream?.getTracks().forEach(t => t.stop());
    this.desktopStream = null;

    const video = document.getElementById('desktopVideo');
    if (video) video.srcObject = null;
    const placeholder = document.getElementById('desktopPlaceholder');
    if (placeholder) placeholder.hidden = false;

    const toggleBtn = document.getElementById('desktopToggleBtn');
    if (toggleBtn) {
      toggleBtn.textContent = 'Share Screen';
      toggleBtn.classList.remove('active-stop');
    }
    const captureBtn = document.getElementById('desktopCaptureBtn');
    if (captureBtn) captureBtn.disabled = true;

    const indicator = document.getElementById('desktopStatusIndicator');
    if (indicator) {
      indicator.className = 'feed-live-indicator inactive';
      const span = indicator.querySelector('span');
      if (span) span.textContent = 'Desktop Off';
    }

    this.updateAllBadges();
    this.stopTelemetryIfIdle();
  },

  tickDesktop() {
    if (!this.desktopActive) return;
    const video = document.getElementById('desktopVideo');
    const canvas = document.getElementById('desktopCanvas');
    if (video && canvas && video.videoWidth > 0) {
      const scale = Math.min(1, 960 / video.videoWidth, 540 / video.videoHeight);
      const w = Math.max(1, Math.round(video.videoWidth * scale));
      const h = Math.max(1, Math.round(video.videoHeight * scale));
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }
      const ctx = canvas.getContext('2d');
      ctx.drawImage(video, 0, 0, w, h);
    }
    const interval = this.calculateInterval();
    this.desktopTimer = setTimeout(() => this.tickDesktop(), interval);
  },

  captureDesktopFrame() {
    const canvas = document.getElementById('desktopCanvas');
    if (!this.desktopActive || !canvas.width) {
      showError('Screen share is not active yet.');
      return;
    }
    const data = canvas.toDataURL('image/jpeg', 0.85);
    const vision = { format: 'image/jpeg', data_base64: data.split(',')[1] };
    if (typeof renderArtifact === 'function') {
      renderArtifact({ artifact_type: 'image', content: data });
    }
    if (typeof sendAction === 'function') {
      sendAction('text', { text: 'Analyze this desktop screen.', vision });
      if (typeof updateState === 'function') updateState('thinking');
    }
  }
};
window.AdaptiveFeedEngine = AdaptiveFeedEngine;

// Wire up Feed buttons
document.getElementById('cameraToggleBtn')?.addEventListener('click', () => AdaptiveFeedEngine.toggleCamera());
document.getElementById('cameraCaptureBtn')?.addEventListener('click', () => AdaptiveFeedEngine.captureCameraFrame());
document.getElementById('cameraFlipBtn')?.addEventListener('click', () => AdaptiveFeedEngine.flipCamera());
document.getElementById('desktopToggleBtn')?.addEventListener('click', () => AdaptiveFeedEngine.toggleDesktop());
document.getElementById('desktopCaptureBtn')?.addEventListener('click', () => AdaptiveFeedEngine.captureDesktopFrame());

// Legacy helper compatibility
function startCamera() {
  switchCardTab('canvasCard', 'canvasPanelCamera');
  AdaptiveFeedEngine.startCamera();
}
function stopCamera() {
  AdaptiveFeedEngine.stopCamera();
}
function captureCameraFrame() {
  AdaptiveFeedEngine.captureCameraFrame();
}
window.addEventListener('pagehide', () => {
  AdaptiveFeedEngine.stopCamera();
  AdaptiveFeedEngine.stopDesktop();
});

// Model-requested snapshots (visual_snapshot tool). The model chooses camera or screen.
// A source the user is already sharing live answers at once from its current frame; any
// other capture needs the user's click in the dialog (screen sharing also requires
// transient user activation, so it is never started from a WebSocket callback).
// Matches what realtime voice demos send: enough for text on screen, few image tokens.
const SNAPSHOT_MAX_EDGE = 768;
const SNAPSHOT_QUALITY = 0.8;
let pendingVisualCapture = null;
const captureDialog = document.getElementById('captureDialog');

function frameFromVideo(video) {
  if (!video?.videoWidth) return null;
  const scale = Math.min(1, SNAPSHOT_MAX_EDGE / Math.max(video.videoWidth, video.videoHeight));
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.round(video.videoWidth * scale));
  canvas.height = Math.max(1, Math.round(video.videoHeight * scale));
  canvas.getContext('2d').drawImage(video, 0, 0, canvas.width, canvas.height);
  return canvas.toDataURL('image/jpeg', SNAPSHOT_QUALITY);
}
function liveSnapshot(source) {
  const engine = window.AdaptiveFeedEngine;
  if (source === 'camera' && engine?.cameraActive) return frameFromVideo(document.getElementById('cameraVideo'));
  if (source === 'screen' && engine?.desktopActive) return frameFromVideo(document.getElementById('desktopVideo'));
  return null;
}
function deliverSnapshot(request, dataUrl) {
  if (!sendAction('snapshot', { request_id:request.request_id, image:{ format:'image/jpeg', data_base64:dataUrl.split(',')[1] } })) return false;
  renderArtifact({ artifact_type:'image', content:dataUrl });
  updateState('thinking');
  return true;
}
function cancelVisualCapture() {
  pendingVisualCapture = null;
  if (captureDialog.open) captureDialog.close();
}
// The user said no: tell the waiting turn so the model can answer without the image.
function declineVisualCapture() {
  const request = pendingVisualCapture;
  cancelVisualCapture();
  if (request) sendAction('snapshot', { request_id:request.request_id, error:'The user declined the capture.' });
}
function requestVisualCapture(request) {
  cancelVisualCapture();
  const live = liveSnapshot(request.source);
  if (live && deliverSnapshot(request, live)) return;
  pendingVisualCapture = request;
  const why = request.question ? ` SAIL wants to know: “${request.question}”` : '';
  document.getElementById('captureTitle').textContent = request.source === 'screen' ? 'SAIL wants to see your screen' : 'SAIL wants to see your camera';
  document.getElementById('captureExplanation').textContent = (request.source === 'screen'
    ? 'Share a screen or window to take one screenshot. It is analyzed locally.'
    : 'Allow the camera to take one picture. It is analyzed locally.') + why;
  // The model chose a source; the user can still pick the other one or cancel.
  const cameraChoice = document.getElementById('captureCameraChoice');
  const screenChoice = document.getElementById('captureScreenChoice');
  cameraChoice.hidden = false; screenChoice.hidden = false;
  captureDialog.showModal();
  (request.source === 'screen' ? screenChoice : cameraChoice).focus();
}
async function captureRequestedVisual(source) {
  const request = pendingVisualCapture;
  if (!request) return;
  let stream;
  const buttons = [...captureDialog.querySelectorAll('button')];
  buttons.forEach(button => button.disabled = true);
  try {
    if (!window.isSecureContext) throw new Error('Capture requires HTTPS or localhost.');
    const live = liveSnapshot(source);
    if (live) { if (deliverSnapshot(request, live)) cancelVisualCapture(); return; }
    // Invoke the browser permission picker synchronously from this click.
    if (source === 'screen') {
      if (!navigator.mediaDevices?.getDisplayMedia) throw new Error('Screen capture is unavailable in this browser.');
      stream = await navigator.mediaDevices.getDisplayMedia({ video:true, audio:false });
    } else {
      if (!navigator.mediaDevices?.getUserMedia) throw new Error('Camera capture is unavailable in this browser.');
      stream = await navigator.mediaDevices.getUserMedia({ video:{ facingMode:{ ideal:'environment' } }, audio:false });
    }
    if (pendingVisualCapture !== request) return;
    const video = document.createElement('video');
    video.muted = true; video.playsInline = true; video.srcObject = stream;
    await Promise.race([
      video.play().then(() => { if (!video.videoWidth) throw new Error('No image frame was received.'); }),
      new Promise((_, reject) => setTimeout(() => reject(new Error('Timed out waiting for an image frame.')), 10000))
    ]);
    if (pendingVisualCapture !== request) return;
    const data = frameFromVideo(video);
    if (data && deliverSnapshot(request, data)) cancelVisualCapture();
  } catch (error) {
    if (pendingVisualCapture === request) showError(error.name === 'NotAllowedError'
      ? 'Capture cancelled or permission denied. Choose a source to retry, or cancel.' : error.message);
  } finally {
    stream?.getTracks().forEach(track => track.stop());
    buttons.forEach(button => button.disabled = false);
  }
}
document.getElementById('captureCameraChoice').addEventListener('click', () => captureRequestedVisual('camera'));
document.getElementById('captureScreenChoice').addEventListener('click', () => captureRequestedVisual('screen'));
document.getElementById('captureCancel').addEventListener('click', declineVisualCapture);
captureDialog.addEventListener('cancel', (event) => { event.preventDefault(); declineVisualCapture(); });
window.addEventListener('pagehide', cancelVisualCapture);

// Auto-ping live metrics & models on page startup
AdaptiveFeedEngine.startTelemetry();
if (typeof checkModels === 'function') {
  checkModels();
  setInterval(() => {
    if (!document.hidden) checkModels();
  }, 15000);
}

initializeSubsystemPages();

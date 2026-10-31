'use strict';
const spaces = [
  { id:'dialogue', name:'Dialogue', subtitle:'Talk with SAIL', available:true, action:() => document.getElementById('textInput').focus() },
  { id:'canvas', name:'Canvas', subtitle:'Visual responses', available:true, action:() => {
    const panel = document.getElementById('canvasPanel');
    if (!panel.childElementCount) { const empty = document.createElement('div'); empty.id = 'canvasEmptyState'; empty.className = 'artifact-card canvas-empty-state'; empty.textContent = 'Diagrams, images and structured results from your conversation will appear here.'; panel.append(empty); }
    panel.scrollIntoView({ behavior:'smooth', block:'center' });
  } },
  { id:'vision', name:'Vision', subtitle:'Use camera or an image', available:true, action:() => startCamera() },
  { id:'arena', name:'Arena', subtitle:'Knowledge', available:false },
  { id:'flow', name:'Flow', subtitle:'Automations', available:false },
  { id:'life', name:'Life', subtitle:'Personal space', available:false },
  { id:'entertainment', name:'Entertainment', subtitle:'Listen and read', available:false },
  { id:'mind', name:'Mind', subtitle:'Local models', available:true, action:() => document.getElementById('checkModelsBtn').click() },
  { id:'agents', name:'Agents', subtitle:'Task execution', available:false },
  { id:'guardian', name:'Guardian', subtitle:'Permissions', available:false },
  { id:'bridge', name:'Bridge', subtitle:'Device tools', available:false },
  { id:'discovery', name:'Discovery', subtitle:'Research', available:false },
  { id:'pulse', name:'Pulse', subtitle:'Goals and routines', available:false },
  { id:'sense', name:'Sense', subtitle:'Context', available:false },
  { id:'mesh', name:'Mesh', subtitle:'Local network', available:false },
  { id:'lab', name:'Lab', subtitle:'Experiments', available:false },
];
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
  if (wasSpaces && window.location.hash === '#spaces') history.replaceState(null, '', location.pathname + location.search);
  setTimeout(() => { if (!activeSheet) scrim.hidden = true; }, 240);
  document.body.style.overflow = ''; priorFocus?.focus?.();
}
function renderSpaces(filter = '') {
  const list = document.getElementById('spaceList'); list.replaceChildren();
  const query = filter.trim().toLocaleLowerCase();
  for (const space of spaces.filter(item => `${item.name} ${item.subtitle}`.toLocaleLowerCase().includes(query))) {
    const button = document.createElement('button'); button.type = 'button';
    button.className = 'space-card'; button.dataset.space = space.id;
    button.setAttribute('aria-label', space.available ? `${space.name}: ${space.subtitle}` : `${space.name}: Not connected`);
    const icon = document.createElement('span'); icon.className = 'space-icon'; icon.setAttribute('aria-hidden', 'true');
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('aria-hidden', 'true');
    const use = document.createElementNS('http://www.w3.org/2000/svg', 'use'); use.setAttribute('href', `/icons.svg#${space.id}`); svg.append(use); icon.append(svg);
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
document.getElementById('homeNav').addEventListener('click', () => document.getElementById('workspace').scrollTo({ top:0, behavior:'smooth' }));
document.getElementById('canvasNav').addEventListener('click', () => document.getElementById('canvasCard').scrollIntoView({ behavior:'smooth', block:'center' }));
document.querySelectorAll('.unavailable-nav').forEach(button => button.addEventListener('click', () => showError(`${button.dataset.space} is not connected in this build.`)));
document.getElementById('checkSystemBtn').addEventListener('click', () => runKernelTool('os:status'));
document.getElementById('modelsRefreshBtn').addEventListener('click', () => checkModels());
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
document.getElementById('endTurnBtn').addEventListener('click', () => sendAction('end_turn'));
document.getElementById('interruptBtn').addEventListener('click', () => interrupt());
document.getElementById('newConversationBtn').addEventListener('click', () => { resetSession(); closeSheet(); });
document.getElementById('muteBtn').addEventListener('click', () => toggleMute());
document.getElementById('tierBtn').addEventListener('click', () => cycleTier());
document.getElementById('voiceSelect').addEventListener('change', event => sendAction('set_voice', { voice:event.target.value }));
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

let cameraStream = null;
async function startCamera() {
  if (!window.isSecureContext || !navigator.mediaDevices?.getUserMedia) {
    showError('Camera access requires HTTPS or localhost in a supported browser.'); return;
  }
  try {
    cameraStream = await navigator.mediaDevices.getUserMedia({ video:{ facingMode:{ ideal:'environment' } }, audio:false });
    const video = document.getElementById('cameraPreview'); video.srcObject = cameraStream;
    document.getElementById('cameraPanel').hidden = false;
    document.getElementById('appShell').dataset.camera = 'on';
    updateState('seeing');
    video.focus?.();
  } catch (error) {
    showError(error.name === 'NotAllowedError' ? 'Camera permission was not granted.' : `Camera: ${error.message}`);
  }
}
function stopCamera() {
  cameraStream?.getTracks().forEach(track => track.stop()); cameraStream = null;
  const video = document.getElementById('cameraPreview'); video.srcObject = null;
  document.getElementById('cameraPanel').hidden = true;
  delete document.getElementById('appShell').dataset.camera;
  if (orbState === 'seeing') updateState(isRecording ? 'listening' : 'idle');
}
async function captureCameraFrame() {
  const video = document.getElementById('cameraPreview');
  if (!cameraStream || !video.videoWidth) { showError('Camera preview is not ready yet.'); return; }
  const scale = Math.min(1, 960 / video.videoWidth, 720 / video.videoHeight);
  const canvas = document.createElement('canvas'); canvas.width = Math.max(1, Math.round(video.videoWidth * scale)); canvas.height = Math.max(1, Math.round(video.videoHeight * scale));
  canvas.getContext('2d').drawImage(video, 0, 0, canvas.width, canvas.height);
  const data = canvas.toDataURL('image/jpeg', .86);
  const vision = { format:'image/jpeg', data_base64:data.split(',')[1] };
  renderArtifact({ artifact_type:'image', content:data });
  if (sendAction('text', { text:'Analyze this camera frame.', vision })) {
    updateState('thinking');
  }
}
window.addEventListener('pagehide', stopCamera);

// Screen sharing requires transient user activation: never invoke it from a
// WebSocket callback. Preserve the original request until a frame is attached.
let pendingVisualCapture = null;
const captureDialog = document.getElementById('captureDialog');
function cancelVisualCapture() {
  pendingVisualCapture = null;
  if (captureDialog.open) captureDialog.close();
}
function requestVisualCapture(request) {
  cancelVisualCapture();
  pendingVisualCapture = request;
  document.getElementById('captureExplanation').textContent = request.source === 'screen'
    ? 'Share a screen or window to take one screenshot for local analysis.'
    : request.source === 'camera'
      ? 'Allow camera access to take one picture for local analysis.'
      : 'Camera picture or screenshot? Choose what SAIL should see for local analysis.';
  document.getElementById('captureCameraChoice').hidden = request.source === 'screen';
  document.getElementById('captureScreenChoice').hidden = request.source === 'camera';
  updateState('idle');
  captureDialog.showModal();
}
async function captureRequestedVisual(source) {
  const request = pendingVisualCapture;
  if (!request) return;
  let stream;
  const buttons = [...captureDialog.querySelectorAll('button')];
  buttons.forEach(button => button.disabled = true);
  try {
    if (!window.isSecureContext) throw new Error('Capture requires HTTPS or localhost.');
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
    const scale = Math.min(1, 960 / video.videoWidth, 720 / video.videoHeight);
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.round(video.videoWidth * scale));
    canvas.height = Math.max(1, Math.round(video.videoHeight * scale));
    canvas.getContext('2d').drawImage(video, 0, 0, canvas.width, canvas.height);
    const data = canvas.toDataURL('image/jpeg', .86);
    if (sendAction('text', { text:request.prompt, vision:{ format:'image/jpeg', data_base64:data.split(',')[1] } })) {
      renderArtifact({ artifact_type:'image', content:data });
      cancelVisualCapture(); updateState('thinking');
    }
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
document.getElementById('captureCancel').addEventListener('click', cancelVisualCapture);
captureDialog.addEventListener('cancel', cancelVisualCapture);
window.addEventListener('pagehide', cancelVisualCapture);

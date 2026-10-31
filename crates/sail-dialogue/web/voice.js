'use strict';
let ws, audioCtx, micStream, processor, micSource;
let workletLoaded=false;
const playbackTimers=new Set();
let isRecording = false, isMuted = false, orbState = 'idle';
let captureHeld = false;
let maxRecordingMs = 180000;
let pendingCaptureFlush = null, captureFlushSerial = 0, submittingVoiceTurn = false;
let currentAmplitude = 0, wavePhase = 0, spectrum8 = Array(8).fill(0), orbFrozen = false;
const activeTools = new Map();
const tiers = ['Balanced', 'HighPerformance', 'LowPower']; let tierIdx = 0;
const ambientPalettes = {
  idle:{ primary:'#9180E8', secondary:'#D080AC', tertiary:'#E7A290', glow:'rgba(208,128,172,.30)' },
  listening:{ primary:'#28B7AC', secondary:'#58BFD1', tertiary:'#6488CE', glow:'rgba(40,183,172,.34)' },
  understanding:{ primary:'#5167C9', secondary:'#8268CC', tertiary:'#A172B3', glow:'rgba(81,103,201,.34)' },
  thinking:{ primary:'#5167C9', secondary:'#8268CC', tertiary:'#A172B3', glow:'rgba(81,103,201,.38)' },
  tool_execution:{ primary:'#557FD1', secondary:'#66B4C9', tertiary:'#BA9672', glow:'rgba(85,127,209,.36)' },
  searching:{ primary:'#37AAB5', secondary:'#549CCB', tertiary:'#478FA2', glow:'rgba(55,170,181,.34)' },
  speaking:{ primary:'#E58B9C', secondary:'#E3A17C', tertiary:'#BC74B5', glow:'rgba(229,139,156,.42)' },
  creating:{ primary:'#916BD4', secondary:'#CA80BA', tertiary:'#D6A073', glow:'rgba(145,107,212,.38)' },
  success:{ primary:'#4FAF9C', secondary:'#83B999', tertiary:'#74AAB4', glow:'rgba(79,175,156,.34)' },
  approval:{ primary:'#C59C62', secondary:'#AA82AC', tertiary:'#8875BA', glow:'rgba(197,156,98,.34)' },
  error:{ primary:'#B45A69', secondary:'#834C72', tertiary:'#6A486F', glow:'rgba(180,90,105,.34)' },
  offline:{ primary:'#566078', secondary:'#424C63', tertiary:'#4B4C60', glow:'rgba(86,96,120,.24)' },
};
const statePalette = state => ambientPalettes[state] || ambientPalettes.idle;
const paletteState = state => ({ transcribing:'understanding', seeing:'searching', watching:'searching', disconnected:'offline', approval_required:'approval', interrupted:'idle', working:'tool_execution' })[state] || state;
const orbCanvas = document.getElementById('orbCanvas'), ctx = orbCanvas.getContext('2d');
const statusBadge = document.getElementById('statusBadge'), transcriptBox = document.getElementById('transcript');
const micBtn = document.getElementById('micBtn'), muteBtn = document.getElementById('muteBtn'), tierBtn = document.getElementById('tierBtn');
const errorBox = document.getElementById('error');
let playbackGeneration = 0, nextAudioTime = 0, playbackQueue = Promise.resolve(), pendingAudio = 0;
let responseNode, responseText = '', turnFinished = false, turnInProgress = false, artifactSerial = 0;
let visionSelectionPending = false;
const playingSources = new Set();
let errorTimer, errorStateTimer;
function showError(error) {
  errorBox.textContent = String(error); errorBox.hidden = false; clearTimeout(errorTimer); errorTimer = setTimeout(() => { errorBox.hidden = true; }, 8000);
  const priorState = orbState; updateState('error'); clearTimeout(errorStateTimer);
  errorStateTimer = setTimeout(() => { if (orbState === 'error') updateState(priorState === 'error' ? 'idle' : priorState); }, 1700);
}
function normalizeToolName(name) { return String(name || '').replaceAll('_', ':'); }
function toolCard(tool) {
  const name = normalizeToolName(tool);
  if (name.startsWith('vision:')) return document.getElementById('canvasCard');
  if (name === 'os:status') return document.getElementById('systemCard');
  if (name.startsWith('proc:') || name.startsWith('fs:')) return document.getElementById('toolsCard');
  return null;
}
function updateToolActivity(tool, active) {
  const name = normalizeToolName(tool); const card = toolCard(name);
  if (active) activeTools.set(name, true); else activeTools.delete(name);
  if (card) {
    if ([...activeTools.keys()].some(item => toolCard(item) === card)) card.setAttribute('data-state', 'active');
    else card.removeAttribute('data-state');
  }
  syncActivityPill();
}
function syncActivityPill() {
  const activity = document.getElementById('activityPill');
  const stateLabels = { thinking:'Thinking', working:'Working', transcribing:'Transcribing' };
  activity.hidden = activeTools.size === 0 && !stateLabels[orbState];
  const labels = { 'os:status':'Checking system', 'proc:list':'Checking processes', 'fs:query':'Looking in workspace', 'vision:frame':'Analyzing image' };
  document.getElementById('activityText').textContent = activeTools.size ? labels[[...activeTools.keys()].at(-1)] || 'Using a local tool' : stateLabels[orbState] || '';
}
function sendAction(action, fields = {}) {
  if (!ws || ws.readyState !== WebSocket.OPEN) { showError('Connection unavailable. Wait for reconnection.'); return false; }
  ws.send(JSON.stringify({ action, ...fields })); return true;
}
function connectWs() {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  ws = new WebSocket(`${proto}//${location.host}/ws/voice`);
  ws.onopen = () => { setConnection('Connected · local session', true); micBtn.disabled = false; if (orbState === 'disconnected') updateState('idle'); };
  ws.onerror = () => showError('Voice connection failed. Check the local server and HTTPS certificate.');
  ws.onclose = () => {
    cancelVisualCapture(); captureHeld = false; turnInProgress = false; stopMic(); stopPlayback(); for (const tool of activeTools.keys()) updateToolActivity(tool, false); setConnection('Disconnected · reconnecting…', false); updateState('disconnected'); micBtn.disabled = true;
    setTimeout(connectWs, 2000);
  };
  ws.onmessage = (event) => {
    let data;
    try { data = JSON.parse(event.data); } catch { showError('Invalid server message'); return; }
    switch (data.type) {
      case 'capture_required':
        setCaptureHeld(false); turnInProgress = false; stopPlayback(); requestVisualCapture(data); break;
      case 'session':
        cancelVisualCapture();
        document.getElementById('conversation').replaceChildren(); responseNode = null;
        applyControl(data.control); break;
      case 'telemetry':
        document.getElementById('sessionConfidence').textContent = `${(data.confidence * 100).toFixed(0)}%`;
        document.getElementById('sessionRms').textContent = Number(data.rms).toFixed(3);
        const duration = `${formatCaptureTime(data.captured_ms || 0)} / ${formatCaptureTime(maxRecordingMs)}`;
        if (document.getElementById('sessionDuration').textContent !== duration) document.getElementById('sessionDuration').textContent = duration;
        if (!playingSources.size) {
          currentAmplitude = data.rms; wavePhase = data.phase; spectrum8 = data.spectrum;
          if (!captureHeld && !turnInProgress && pendingAudio === 0 && !playingSources.size) updateState(data.state);
        }
        document.getElementById('telVad').textContent = `VAD inference: ${(data.vad_us / 1000).toFixed(2)}ms · round trip: ${(data.vad_roundtrip_us / 1000).toFixed(2)}ms`;
        document.getElementById('telConf').textContent = `Confidence: ${(data.confidence * 100).toFixed(0)}%`;
        document.getElementById('telRms').textContent = `RMS: ${data.rms.toFixed(3)}`; break;
      case 'transcript': transcriptBox.textContent = `${data.is_final ? 'You said' : 'Listening'} · ${data.text}`; document.getElementById('liveTranscript').textContent = data.text; break;
      case 'capture_limit':
        stopMic(); setCaptureHeld(true); updateState('holding'); transcriptBox.textContent = data.message; break;
      case 'turn_committed':
        setCaptureHeld(false); turnInProgress = true; updateState('transcribing'); break;
      case 'turn_started':
        setCaptureHeld(false);
        cancelVisualCapture();
        stopPlayback(); turnFinished = false; turnInProgress = true; responseText = '';
        appendConversation('You', data.transcript);
        responseNode = appendConversation('SAIL', ''); updateState('thinking'); break;
      case 'response_delta':
        responseText += data.text;
        if (!responseNode) responseNode = appendConversation('SAIL', '');
        responseNode.lastChild.textContent = responseText; break;
      case 'audio_chunk': queueAudio(data); break;
      case 'turn_finished':
        setCaptureHeld(false);
        turnInProgress = false;
        document.getElementById('assistantAnnounce').textContent = responseText;
        turnFinished = true;
        if (!pendingAudio && !playingSources.size) finishPlayback();
        if (data.latency_ms) setConnection(`Connected · local turn ${(data.latency_ms / 1000).toFixed(1)}s`, true);
        break;
      case 'interruption': cancelVisualCapture(); setCaptureHeld(false); turnInProgress = false; stopPlayback(); updateState(data.state || 'interrupted'); break;
      case 'control': applyControl(data.control); break;
      case 'tool_started': updateToolActivity(data.tool, true); break;
      case 'tool_result': updateToolActivity(data.result?.tool, false); renderToolResult(data.result); break;
      case 'canvas': renderArtifact(data.artifact); break;
      case 'error': showError(data.message); break;
    }
  };
}
function applyControl(control) {
  isMuted = control.is_muted;
  muteBtn.textContent = isMuted ? 'Unmute' : 'Mute'; muteBtn.setAttribute('aria-pressed', String(isMuted)); muteBtn.className = isMuted ? 'quiet-button muted' : 'quiet-button';
  const micIndicator = document.getElementById('micLiveIndicator');
  micIndicator.hidden = !isRecording; micIndicator.querySelector('span').textContent = isMuted ? 'Mic muted' : 'Mic on';
  document.getElementById('sessionMic').textContent = !isRecording ? 'Off' : isMuted ? 'Muted' : 'On';
  const dockMute = document.getElementById('muteDockBtn'); dockMute.hidden = !isRecording;
  dockMute.setAttribute('aria-label', isMuted ? 'Unmute microphone' : 'Mute microphone'); dockMute.setAttribute('aria-pressed', String(isMuted));
  dockMute.classList.toggle('muted', isMuted);
  document.getElementById('endTurnBtn').hidden = !isRecording && !captureHeld;
  const settings = control.turn_detection || { mode:'automatic', pause_ms:3000 };
  maxRecordingMs = control.max_recording_ms || 180000;
  document.getElementById('turnModeSelect').value = settings.mode;
  document.getElementById('turnPauseInput').value = settings.pause_ms / 1000;
  document.getElementById('turnPauseInput').disabled = settings.mode === 'manual';
  document.getElementById('turnDetectionNote').textContent = settings.mode === 'manual'
    ? 'Pauses stay inside this message. Press Send turn when you finish. Recording is held at the 3-minute limit.'
    : `Waits ${settings.pause_ms / 1000} seconds of silence. Send turn submits immediately. Recording is held at the 3-minute limit.`;
  const profile = { low_power: 'LowPower', balanced: 'Balanced', high_performance: 'HighPerformance' }[control.profile];
  tierIdx = Math.max(0, tiers.indexOf(profile)); tierBtn.textContent = tiers[tierIdx];
}
function formatCaptureTime(milliseconds) {
  const seconds = Math.floor(milliseconds / 1000); return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2,'0')}`;
}
function setCaptureHeld(held) {
  captureHeld = held; micBtn.disabled = held || ws?.readyState !== WebSocket.OPEN;
  document.getElementById('endTurnBtn').hidden = !isRecording && !held;
}
function setTurnDetection() {
  const input = document.getElementById('turnPauseInput');
  if (!input.checkValidity()) { input.reportValidity(); return; }
  sendAction('set_turn_detection', { settings:{ mode:document.getElementById('turnModeSelect').value, pause_ms:Math.round(Number(input.value)*1000) } });
}
function appendConversation(role, text) {
  const node = document.createElement('article'); node.className = `message message-${role.toLowerCase()}`;
  const label = document.createElement('span'); label.className = 'message-label'; label.textContent = role;
  const content = document.createElement('span'); content.textContent = text;
  node.append(label, content); document.getElementById('conversation').append(node); node.scrollIntoView({ block: 'nearest', behavior: 'smooth' }); return node;
}
async function renderArtifact(artifact) {
  if (!artifact || !['mermaid', 'json', 'image', 'svg'].includes(artifact.artifact_type)) return;
  const container = document.createElement('div'); container.className = 'artifact-card';
  document.getElementById('canvasPanel').append(container);
  document.getElementById('canvasEmptyState')?.remove();
  const heading = document.createElement('div'); heading.className = 'artifact-heading';
  heading.textContent = artifact.artifact_type === 'mermaid' ? 'Diagram' : artifact.artifact_type === 'json' ? 'Data' : 'Visual';
  container.append(heading);
  if (artifact.artifact_type === 'mermaid' && window.mermaid) {
    try {
      const rendered = await mermaid.render(`diagram-${++artifactSerial}`, artifact.content);
      // Mermaid strict mode escapes labels and disables interaction in model output.
      container.innerHTML = rendered.svg; return;
    } catch { /* Show source if a model emits an invalid diagram. */ }
  }
  if (artifact.artifact_type === 'json') {
    try {
      const parsed = JSON.parse(artifact.content);
      const fields = document.createElement('dl'); fields.className = 'json-fields';
      for (const [key, value] of Object.entries(parsed).slice(0, 12)) {
        const term = document.createElement('dt'); term.textContent = key.replaceAll('_', ' ');
        const description = document.createElement('dd');
        description.textContent = value === null ? 'None' : typeof value === 'object' ? `${Array.isArray(value) ? value.length : Object.keys(value).length} ${Array.isArray(value) ? 'items' : 'fields'}` : String(value);
        fields.append(term, description);
      }
      container.append(fields);
      const details = document.createElement('details'); const disclosure = document.createElement('summary'); disclosure.textContent = 'Show structured data';
      const pre = document.createElement('pre'); pre.textContent = JSON.stringify(parsed, null, 2); details.append(disclosure, pre); container.append(details); return;
    } catch { /* Show invalid output safely as text. */ }
  }
  if (artifact.artifact_type === 'image' && /^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(artifact.content)) {
    const image = document.createElement('img'); image.src = artifact.content; image.alt = 'Conversation visual'; image.style.maxWidth = '100%'; container.append(image); return;
  }
  if (artifact.artifact_type === 'svg' && artifact.content.trim().startsWith('<svg')) {
    container.innerHTML = artifact.content; return;
  }
}
function renderToolResult(result) {
  const container = document.createElement('article'); container.className = 'artifact-card';
  const output = result?.output ?? {}; const succeeded = Boolean(result?.success);
  if (result?.tool === 'os:status' && succeeded && output) updateSystemCard(output);
  const heading = document.createElement('div'); heading.className = 'artifact-heading';
  const title = document.createElement('span'); title.textContent = String(result?.tool || 'Local action').replaceAll(':', ' · ');
  const badge = document.createElement('span'); badge.className = succeeded ? 'result-success' : 'result-failure'; badge.textContent = succeeded ? '● Done' : '● Could not complete'; heading.append(title, badge);
  const summary = document.createElement('p'); summary.className = 'result-summary';
  if (Array.isArray(output?.processes)) summary.textContent = `${output.processes.length} ${output.processes.length === 1 ? 'process' : 'processes'} found`;
  else if (Array.isArray(output?.entries)) summary.textContent = `${output.entries.length} ${output.entries.length === 1 ? 'item' : 'items'} in ${String(output.path || 'the selected folder')}`;
  else if (output?.os) summary.textContent = `${String(output.os)} · ${String(output.logical_cpus || '—')} CPUs · ${String(output.memory_percent || '—')} memory used`;
  else if (output?.error) summary.textContent = String(output.error);
  else if (Number.isFinite(output?.width) && Number.isFinite(output?.height)) summary.textContent = `Image analyzed · ${output.width} × ${output.height} pixels`;
  else if (typeof output?.summary === 'string') summary.textContent = output.summary;
  else if (typeof output === 'string') summary.textContent = output;
  else summary.textContent = succeeded ? 'The local action completed.' : 'The local action needs attention.';
  const details = document.createElement('details'); const disclosure = document.createElement('summary'); disclosure.textContent = 'Show details';
  const pre = document.createElement('pre'); pre.textContent = JSON.stringify(result, null, 2); details.append(disclosure, pre);
  container.append(heading, summary, details); document.getElementById('canvasEmptyState')?.remove(); document.getElementById('canvasPanel').append(container);
}
function updateSystemCard(output) {
  document.getElementById('systemCpu').textContent = Number.isFinite(output.logical_cpus) ? `${output.logical_cpus} logical CPUs` : 'Unavailable';
  document.getElementById('systemMemory').textContent = output.memory_percent ? `${String(output.memory_percent)} used` : (output.memory_used_gb || 'Unavailable');
  document.getElementById('systemLoad').textContent = Array.isArray(output.load_average) ? output.load_average.slice(0, 3).map(value => Number(value).toFixed(2)).join(' · ') : 'Unavailable';
  document.getElementById('systemNote').textContent = [output.os, output.uptime_hours ? `Uptime ${output.uptime_hours} h` : ''].filter(Boolean).join(' · ') || 'Live status from the local device.';
}
async function ensureAudio() {
  if (!audioCtx || audioCtx.state === 'closed') { audioCtx = new (window.AudioContext || window.webkitAudioContext)(); workletLoaded=false; }
  await audioCtx.resume(); return audioCtx;
}
function stopPlayback() {
  playbackGeneration++; nextAudioTime = 0; pendingAudio = 0; playbackQueue = Promise.resolve(); turnFinished = false;
  for (const source of playingSources) { source.onended = null; try { source.stop(); } catch {} source.disconnect(); }
  for (const timer of playbackTimers) clearTimeout(timer); playbackTimers.clear();
  playingSources.clear(); currentAmplitude = 0; spectrum8 = Array(8).fill(0);
}
function queueAudio(data) {
  const generation = playbackGeneration; pendingAudio++;
  playbackQueue = playbackQueue.then(async () => {
    if (generation !== playbackGeneration) return;
    const context = await ensureAudio();
    const bytes = Uint8Array.from(atob(data.audio_base64), c => c.charCodeAt(0));
    const buffer = await context.decodeAudioData(bytes.buffer);
    if (generation !== playbackGeneration) return;
    const source = context.createBufferSource(); source.buffer = buffer; source.connect(context.destination);
    const when = Math.max(context.currentTime + 0.015, nextAudioTime); nextAudioTime = when + buffer.duration;
    playingSources.add(source); pendingAudio--;
    // Drive the Orb from audio that is actually playing, instead of packets received early.
    const timer = setTimeout(() => {
      playbackTimers.delete(timer);
      if (generation !== playbackGeneration) return;
      updateState('speaking'); currentAmplitude = data.telemetry.rms_level; wavePhase = data.telemetry.wave_phase;
      const s = data.telemetry.spectrum; spectrum8 = [s.sub_bass,s.bass,s.low_mid,s.mid,s.high_mid,s.presence,s.brilliance,s.air];
    }, Math.max(0, (when - context.currentTime) * 1000));
    playbackTimers.add(timer);
    source.onended = () => { clearTimeout(timer); playbackTimers.delete(timer); source.disconnect(); playingSources.delete(source); if (turnFinished && !pendingAudio && !playingSources.size) finishPlayback(); };
    source.start(when);
  }).catch(error => { if (generation === playbackGeneration) { pendingAudio = Math.max(0, pendingAudio - 1); showError(`Audio playback: ${error.message}`); } });
}
function finishPlayback() { updateState(isRecording ? 'listening' : 'idle'); currentAmplitude = 0; spectrum8 = Array(8).fill(0); sendAction('playback_finished'); }
function updateState(state) {
  const changed = orbState !== state;
  orbState = state;
  const labels = { holding:'Ready to send', idle:'Ready', listening:'Listening', transcribing:'Understanding', thinking:'Thinking', speaking:'Speaking', working:'Working', seeing:'Seeing', watching:'Watching', interrupted:'Interrupted', disconnected:'Offline', approval_required:'Approval needed', error:'Attention' };
  statusBadge.textContent = labels[state] || 'Ready'; document.getElementById('appShell').dataset.state = state;
  const palette = statePalette(paletteState(state));
  ['--ambient-one','--ambient-two','--ambient-three'].forEach((property,index) => document.documentElement.style.setProperty(property, [palette.primary,palette.secondary,palette.tertiary][index]));
  document.getElementById('sessionState').textContent = labels[state] || 'Ready';
  if (changed && reduceMotion.matches && !document.hidden) renderVisuals();
  syncActivityPill();
  document.getElementById('interruptBtn').hidden = !['thinking','speaking','working','transcribing'].includes(state);
}
function setConnection(message, connected) {
  const pill = document.getElementById('connection'); pill.className = `connection-pill ${connected ? 'is-connected' : 'is-disconnected'}`;
  pill.querySelector('span').textContent = message;
  document.getElementById('sessionConnection').textContent = connected ? 'Connected · local' : 'Reconnecting';
}
async function toggleMic() { if (isRecording) await submitVoiceTurn(true); else await startMic(); }
async function submitVoiceTurn(stopAfter = false) {
  if (submittingVoiceTurn) return;
  submittingVoiceTurn = true; document.getElementById('endTurnBtn').disabled = true;
  try {
    const capture = processor;
    if (isRecording && capture) {
      await new Promise((resolve,reject) => {
        const request_id = ++captureFlushSerial;
        const timer = setTimeout(() => { pendingCaptureFlush = null; reject(new Error('Microphone did not finish sending its last frame. Try Send turn again.')); },1000);
        pendingCaptureFlush = {request_id,resolve:() => {clearTimeout(timer);resolve();},reject:() => {clearTimeout(timer);reject(new Error('Microphone stopped before the turn was sent.'));}};
        capture.port.postMessage({action:'flush',request_id});
      });
      if (processor !== capture || !isRecording) return;
    }
    if (stopAfter) stopMic();
    sendAction('end_turn');
  } catch(error) { showError(error.message); }
  finally { submittingVoiceTurn = false; document.getElementById('endTurnBtn').disabled = false; }
}
async function startMic() {
  if (captureHeld) { showError('Press Send turn to submit the recording before starting again.'); return; }
  if (!window.isSecureContext || !navigator.mediaDevices?.getUserMedia) {
    showError('Microphone requires a secure connection. On this computer use http://localhost:8088. From another device use HTTPS with a trusted certificate (see scripts/voice/tls.sh).'); return;
  }
  if (!ws || ws.readyState !== WebSocket.OPEN) { showError('Wait for the voice server connection.'); return; }
  micBtn.disabled = true;
  try {
    errorBox.textContent = ''; await ensureAudio();
    micStream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
    if (!ws || ws.readyState !== WebSocket.OPEN) throw new Error('Voice connection closed while requesting microphone access');
    if (!workletLoaded) { await audioCtx.audioWorklet.addModule('/capture.js'); workletLoaded=true; }
    micStream.getTracks().forEach(track => { track.onended=stopMic; });
    micSource = audioCtx.createMediaStreamSource(micStream);
    processor = new AudioWorkletNode(audioCtx, 'sail-capture');
    processor.port.onmessage = ({ data }) => {
      if (data?.type === 'flushed') {
        if (pendingCaptureFlush?.request_id === data.request_id) {const pending = pendingCaptureFlush;pendingCaptureFlush = null;pending.resolve();}
        return;
      }
      if (!(data instanceof ArrayBuffer)) return;
      if (isRecording && !isMuted && ws?.readyState === WebSocket.OPEN) {
        if (ws.bufferedAmount > 64000) { showError('Audio connection is too slow; microphone stopped.'); stopMic(); return; }
        ws.send(data);
      }
    };
    micSource.connect(processor); processor.connect(audioCtx.destination);
    isRecording = true; micBtn.setAttribute('aria-label', 'Stop microphone'); micBtn.title = 'Stop microphone'; micBtn.classList.add('active'); document.getElementById('micStatus').textContent = 'On'; document.getElementById('sessionMic').textContent = isMuted ? 'Muted' : 'On'; document.getElementById('micLiveIndicator').hidden = false; document.getElementById('muteDockBtn').hidden = false; document.getElementById('endTurnBtn').hidden = false; updateState('listening');
  } catch (error) { stopMic(); showError(error.name === 'NotAllowedError' ? 'Microphone permission denied. Allow microphone access in browser settings.' : `Microphone: ${error.message}`); }
  finally { micBtn.disabled = false; }
}
function stopMic() {
  if (pendingCaptureFlush) { const pending = pendingCaptureFlush; pendingCaptureFlush = null; pending.reject(); }
  isRecording = false; micStream?.getTracks().forEach(track => track.stop());
  if (processor) { processor.port.onmessage = null; processor.disconnect(); processor = null; }
  micSource?.disconnect(); micSource = null; micStream = null;
  micBtn.setAttribute('aria-label', 'Start microphone'); micBtn.title = 'Start microphone'; micBtn.classList.remove('active'); document.getElementById('micStatus').textContent = 'Off'; document.getElementById('sessionMic').textContent = 'Off'; document.getElementById('micLiveIndicator').hidden = true; document.getElementById('muteDockBtn').hidden = true; document.getElementById('endTurnBtn').hidden = true;
  if (!playingSources.size && !turnInProgress) updateState('idle');
}
function toggleMute() { sendAction('toggle_mute'); }
function cycleTier() { sendAction('set_tier', { profile: tiers[(tierIdx + 1) % tiers.length] }); }
function interrupt() { stopPlayback(); sendAction('interrupt'); updateState('interrupted'); }
async function runVisionFrame() {
  const file = document.getElementById('imageInput').files[0];
  if (!file) { showError('Choose an image to analyze with vision:frame.'); return; }
  try {
    const image = await createImageBitmap(file);
    const scale = Math.min(1, 640 / image.width, 480 / image.height);
    const canvas = document.createElement('canvas'); canvas.width = Math.max(1, Math.round(image.width * scale)); canvas.height = Math.max(1, Math.round(image.height * scale));
    const context = canvas.getContext('2d'); context.drawImage(image, 0, 0, canvas.width, canvas.height); image.close();
    const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
    sendAction('kernel_tool', { tool: 'vision:frame', params: { width: canvas.width, height: canvas.height, format: 'rgba', data: Array.from(pixels) } });
  } catch (error) { showError(error.message); }
}
function runKernelTool(tool, params = {}) { if (sendAction('kernel_tool', { tool, params })) updateToolActivity(tool, true); }
function resetSession() { interrupt(); sendAction('reset'); responseNode = null; document.getElementById('conversation').replaceChildren(); document.getElementById('assistantAnnounce').textContent = ''; document.getElementById('canvasClearBtn').click(); }
async function checkModels() {
  const summary = document.getElementById('modelStatus'); summary.textContent = 'Checking local model status…'; summary.className = 'model-summary';
  const modelsCard = document.getElementById('modelsCard'); modelsCard.setAttribute('data-state', 'active');
  document.getElementById('modelStatusSettings').textContent = 'Querying local model workers…';
  try {
    const response = await fetch('/api/health'); if (!response.ok) throw new Error(response.statusText);
    const data = await response.json(); const speech = Object.values(data.speech || {}); const readySpeech = speech.filter(model => model.ready).length;
    const llmReady = Boolean(data.llm?.ready); const ready = llmReady && readySpeech === speech.length;
    summary.textContent = ready ? 'Local services ready' : `${readySpeech}/${speech.length} speech workers ready${llmReady ? ' · LLM ready' : ' · LLM unavailable'}`;
    summary.classList.toggle('ready', ready); summary.classList.toggle('unavailable', !ready);
    document.getElementById('llmStatus').textContent = llmReady ? `Ready${data.llm.model ? ` · ${data.llm.model}` : ''}` : 'Unavailable';
    document.getElementById('speechStatus').textContent = `${readySpeech}/${speech.length} ready`;
    const modalities = data.llm?.modalities;
    document.getElementById('modelCapabilities').textContent = modalities && typeof modalities === 'object'
      ? Object.entries(modalities).filter(([, enabled]) => enabled).map(([name]) => name).join(', ') || 'Text'
      : 'Local inference';
    document.getElementById('modelStatusSettings').textContent = ready ? 'Local language and speech services are ready.' : 'Some local services are unavailable. See the runner logs for details.';
  } catch (error) { summary.textContent = 'Local model status could not be reached.'; summary.classList.add('unavailable'); document.getElementById('modelStatusSettings').textContent = 'Local model status could not be reached.'; showError(error.message); }
  finally { modelsCard.removeAttribute('data-state'); }
}
document.querySelectorAll('.tool-action[data-tool]').forEach(button => button.addEventListener('click', () => {
  const tool = button.dataset.tool;
  runKernelTool(tool, tool === 'fs:query' ? { path:'.' } : {});
}));
document.getElementById('chooseVisionBtn').addEventListener('click', () => {
  visionSelectionPending = true; document.getElementById('imageInput').click();
});
document.getElementById('imageInput').addEventListener('change', () => {
  if (visionSelectionPending && document.getElementById('imageInput').files?.length) {
    visionSelectionPending = false; runVisionFrame();
  }
});
document.getElementById('textForm').addEventListener('submit', async event => {
  event.preventDefault(); errorBox.textContent = ''; errorBox.hidden = true;
  const input = document.getElementById('textInput'), file = document.getElementById('imageInput').files[0];
  try {
    await ensureAudio(); let vision;
    if (file) {
      if (file.size > 4000000 || !['image/png', 'image/jpeg'].includes(file.type)) throw new Error('Choose a PNG/JPEG up to 4 MB.');
      const data = await new Promise((resolve, reject) => { const reader = new FileReader(); reader.onload = () => resolve(reader.result); reader.onerror = reject; reader.readAsDataURL(file); });
      vision = { format: file.type, data_base64: data.split(',')[1] };
      renderArtifact({ artifact_type: 'image', content: data });
    }
    stopPlayback(); if (sendAction('text', { text: input.value, vision })) { input.value = ''; document.getElementById('imageInput').value = ''; }
  } catch (error) { showError(error.message); }
});
window.addEventListener('pagehide', () => { stopMic(); stopPlayback(); ws?.close(); audioCtx?.close(); });
if (window.mermaid) mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: 'dark' });
// Render loop for 3D Glowing Orb & 8-Band Equalizer
let time = 0;
let animationFrame = 0;
const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
function renderVisuals() {
  animationFrame = 0;
  if (document.hidden) return;
  if (!orbFrozen && !reduceMotion.matches) time += 0.035;

  // 1. Draw Voice Orb
  const w = orbCanvas.width;
  const h = orbCanvas.height;
  const cx = w / 2;
  const cy = h / 2;
  ctx.clearRect(0, 0, w, h);

  // Keep the original highlight/body/rim contrast; ambient tokens are too flat for sphere shading.
  const colors = {
    idle: { in: '#ffbd9d', mid: '#ec6e99', out: '#5750bd', glow: 'rgba(240, 138, 137, 0.38)' },
    listening: { in: '#a7eee1', mid: '#54bcb2', out: '#405eaf', glow: 'rgba(136, 200, 196, 0.46)' },
    thinking: { in: '#e4c4ff', mid: '#a99ae3', out: '#674bb5', glow: 'rgba(169, 154, 227, 0.46)' },
    speaking: { in: '#ffe0b0', mid: '#f08a89', out: '#a9489f', glow: 'rgba(240, 138, 137, 0.55)' },
    interrupted: { in: '#f7d591', mid: '#e5be79', out: '#a96c61', glow: 'rgba(229, 190, 121, 0.45)' },
    working: { in: '#e3c5ff', mid: '#9b80dc', out: '#564b96', glow: 'rgba(169, 154, 227, 0.42)' },
    seeing: { in: '#cff1ff', mid: '#83b9e5', out: '#6654ac', glow: 'rgba(131, 185, 229, 0.42)' },
    watching: { in: '#cff1ff', mid: '#83b9e5', out: '#6654ac', glow: 'rgba(131, 185, 229, 0.42)' },
    approval_required: { in: '#fff0c4', mid: '#e5be79', out: '#9b6653', glow: 'rgba(229, 190, 121, 0.42)' }
  }[orbState] || { in: '#ffbd9d', mid: '#ec6e99', out: '#5750bd', glow: 'rgba(240, 138, 137, 0.38)' };

  const amp = !reduceMotion.matches && (orbState === 'speaking' || orbState === 'listening') ? Math.max(currentAmplitude, ...spectrum8) : 0.02;
  const baseRadius = 79 + Math.min(amp, 1) * 23;
  const visualTime = reduceMotion.matches ? 0 : time;
  const visualPhase = reduceMotion.matches ? 0 : wavePhase;

  // Ambient outer glow
  const g = ctx.createRadialGradient(cx, cy, baseRadius * 0.2, cx, cy, baseRadius * 1.55);
  g.addColorStop(0, `${colors.mid}66`);
  g.addColorStop(0.5, `${colors.out}30`);
  g.addColorStop(1, 'rgba(0,0,0,0)');
  ctx.beginPath();
  ctx.arc(cx, cy, baseRadius * 1.4, 0, Math.PI * 2);
  ctx.fillStyle = g;
  ctx.fill();

  // Wavy oscillating orb surface
  ctx.beginPath();
  for (let i = 0; i <= 64; i++) {
    const angle = (i / 64) * Math.PI * 2;
    const wave = Math.sin(angle * 3 + visualTime * 0.55 + visualPhase) * (2 + amp * 12) +
                 Math.cos(angle * 2 - visualTime * 0.38) * (2 + amp * 7);
    const r = baseRadius + wave;
    const x = cx + Math.cos(angle) * r;
    const y = cy + Math.sin(angle) * r;
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  }
  ctx.closePath();
  const cg = ctx.createRadialGradient(cx - baseRadius * 0.28, cy - baseRadius * 0.32, 1, cx, cy, baseRadius * 1.08);
  cg.addColorStop(0, '#fffaf1');
  cg.addColorStop(0.18, colors.in);
  cg.addColorStop(0.58, colors.mid);
  cg.addColorStop(1, colors.out);
  ctx.fillStyle = cg;
  ctx.shadowColor = colors.glow;
  ctx.shadowBlur = 34 + amp * 26;
  ctx.fill();
  ctx.shadowBlur = 0;
  if (!reduceMotion.matches) animationFrame = requestAnimationFrame(renderVisuals);
}


connectWs();
renderVisuals();
document.addEventListener('visibilitychange', () => {
  if (document.hidden) { if (animationFrame) cancelAnimationFrame(animationFrame); animationFrame = 0; }
  else if (!animationFrame) renderVisuals();
});
const mockableStates = new Set(['idle','listening','transcribing','thinking','speaking','working','seeing','watching','interrupted','disconnected','approval_required']);
window.addEventListener('sail:orb-telemetry', event => {
  const data = event.detail || {};
  if (!mockableStates.has(data.state)) return;
  orbFrozen = true;
  const clamp = value => Number.isFinite(value) ? Math.max(0, Math.min(1, value)) : 0;
  updateState(data.state); currentAmplitude = clamp(data.rms); wavePhase = Number.isFinite(data.phase) ? data.phase : 0;
  if (Array.isArray(data.spectrum) && data.spectrum.length === 8) spectrum8 = data.spectrum.map(clamp);
});
// Browser-driven screenshot tools can dispatch fixed telemetry without opening
// the microphone or presenting a fake connected state.
window.sailUi = Object.freeze({
  setOrbTelemetry: data => window.dispatchEvent(new CustomEvent('sail:orb-telemetry', { detail:data })),
  resumeOrbAnimation: () => { orbFrozen = false; },
});

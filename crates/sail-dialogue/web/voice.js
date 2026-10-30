'use strict';
let ws, audioCtx, micStream, processor, micSource;
let workletLoaded=false;
const playbackTimers=new Set();
let isRecording = false, isMuted = false, orbState = 'idle';
let currentAmplitude = 0, wavePhase = 0, spectrum8 = Array(8).fill(0);
const tiers = ['Balanced', 'HighPerformance', 'LowPower']; let tierIdx = 0;
const orbCanvas = document.getElementById('orbCanvas'), ctx = orbCanvas.getContext('2d');
const eqCanvas = document.getElementById('equalizerCanvas'), eqCtx = eqCanvas.getContext('2d');
const statusBadge = document.getElementById('statusBadge'), transcriptBox = document.getElementById('transcript');
const micBtn = document.getElementById('micBtn'), muteBtn = document.getElementById('muteBtn'), tierBtn = document.getElementById('tierBtn');
const errorBox = document.getElementById('error'), connectionBox = document.getElementById('connection');
let playbackGeneration = 0, nextAudioTime = 0, playbackQueue = Promise.resolve(), pendingAudio = 0;
let responseNode, responseText = '', turnFinished = false, artifactSerial = 0;
const playingSources = new Set();
function showError(error) { errorBox.textContent = String(error); }
function sendAction(action, fields = {}) {
  if (!ws || ws.readyState !== WebSocket.OPEN) { showError('Connection unavailable. Wait for reconnection.'); return false; }
  ws.send(JSON.stringify({ action, ...fields })); return true;
}
function connectWs() {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  ws = new WebSocket(`${proto}//${location.host}/ws/voice`);
  ws.onopen = () => { connectionBox.textContent = 'Connected · models run locally'; micBtn.disabled = false; };
  ws.onerror = () => showError('Voice connection failed. Check the local server and HTTPS certificate.');
  ws.onclose = () => {
    stopMic(); stopPlayback(); connectionBox.textContent = 'Disconnected · reconnecting…'; micBtn.disabled = true;
    setTimeout(connectWs, 2000);
  };
  ws.onmessage = (event) => {
    let data;
    try { data = JSON.parse(event.data); } catch { showError('Invalid server message'); return; }
    switch (data.type) {
      case 'session':
        document.getElementById('conversation').replaceChildren(); responseNode = null;
        applyControl(data.control); break;
      case 'telemetry':
        if (!playingSources.size) {
          currentAmplitude = data.rms; wavePhase = data.phase; spectrum8 = data.spectrum;
          updateState(data.state);
        }
        document.getElementById('telVad').textContent = `VAD inference: ${(data.vad_us / 1000).toFixed(2)}ms · round trip: ${(data.vad_roundtrip_us / 1000).toFixed(2)}ms`;
        document.getElementById('telConf').textContent = `Confidence: ${(data.confidence * 100).toFixed(0)}%`;
        document.getElementById('telRms').textContent = `RMS: ${data.rms.toFixed(3)}`; break;
      case 'transcript': transcriptBox.textContent = `${data.is_final ? 'You' : 'Hearing'}: ${data.text}`; break;
      case 'turn_started':
        stopPlayback(); turnFinished = false; responseText = '';
        appendConversation('You', data.transcript);
        responseNode = appendConversation('SAIL', ''); updateState('thinking'); break;
      case 'response_delta':
        responseText += data.text;
        if (!responseNode) responseNode = appendConversation('SAIL', '');
        responseNode.textContent = `SAIL: ${responseText}`; break;
      case 'audio_chunk': queueAudio(data); break;
      case 'turn_finished':
        turnFinished = true;
        if (!pendingAudio && !playingSources.size) finishPlayback();
        if (data.latency_ms) connectionBox.textContent = `Connected · turn ${data.turn_id} · ${(data.latency_ms / 1000).toFixed(2)}s including synthesis`;
        break;
      case 'interruption': stopPlayback(); updateState(data.state || 'interrupted'); break;
      case 'control': applyControl(data.control); break;
      case 'tool_result': renderArtifact({ artifact_type: 'json', content: JSON.stringify(data.result, null, 2) }); break;
      case 'canvas': renderArtifact(data.artifact); break;
      case 'error': showError(data.message); break;
    }
  };
}
function applyControl(control) {
  isMuted = control.is_muted;
  muteBtn.textContent = `Mute: ${isMuted ? 'ON' : 'OFF'}`; muteBtn.className = isMuted ? 'muted' : '';
  const profile = { low_power: 'LowPower', balanced: 'Balanced', high_performance: 'HighPerformance' }[control.profile];
  tierIdx = Math.max(0, tiers.indexOf(profile)); tierBtn.textContent = `Tier: ${tiers[tierIdx]}`;
}
function appendConversation(role, text) {
  const node = document.createElement('p'); node.style.margin = '12px 0'; node.textContent = `${role}: ${text}`;
  document.getElementById('conversation').append(node); return node;
}
async function renderArtifact(artifact) {
  const container = document.createElement('div'); container.style.cssText = 'padding:16px;margin:12px 0;border:1px solid #334155;border-radius:10px;background:#111827';
  document.getElementById('canvasPanel').append(container);
  if (artifact.artifact_type === 'mermaid' && window.mermaid) {
    try {
      const rendered = await mermaid.render(`diagram-${++artifactSerial}`, artifact.content);
      // Mermaid strict mode escapes labels and disables interaction in model output.
      container.innerHTML = rendered.svg; return;
    } catch { /* Show source if a model emits an invalid diagram. */ }
  }
  if (artifact.artifact_type === 'image' && /^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(artifact.content)) {
    const image = document.createElement('img'); image.src = artifact.content; image.alt = 'Conversation visual'; image.style.maxWidth = '100%'; container.append(image); return;
  }
  const pre = document.createElement('pre'); pre.style.cssText = 'white-space:pre-wrap;overflow-wrap:anywhere'; pre.textContent = artifact.content; container.append(pre);
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
    const when = Math.max(context.currentTime + 0.025, nextAudioTime); nextAudioTime = when + buffer.duration;
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
function updateState(state) { orbState = state; statusBadge.className = `status-badge status-${state}`; statusBadge.textContent = state.toUpperCase(); }
async function toggleMic() { if (isRecording) stopMic(); else await startMic(); }
async function startMic() {
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
      if (isRecording && !isMuted && ws?.readyState === WebSocket.OPEN) {
        if (ws.bufferedAmount > 64000) { showError('Audio connection is too slow; microphone stopped.'); stopMic(); return; }
        ws.send(data);
      }
    };
    micSource.connect(processor); processor.connect(audioCtx.destination);
    isRecording = true; micBtn.textContent = 'Stop Microphone'; micBtn.classList.add('active'); updateState('listening');
  } catch (error) { stopMic(); showError(error.name === 'NotAllowedError' ? 'Microphone permission denied. Allow microphone access in browser settings.' : `Microphone: ${error.message}`); }
  finally { micBtn.disabled = false; }
}
function stopMic() {
  isRecording = false; micStream?.getTracks().forEach(track => track.stop());
  if (processor) { processor.port.onmessage = null; processor.disconnect(); processor = null; }
  micSource?.disconnect(); micSource = null; micStream = null;
  micBtn.textContent = 'Start Microphone'; micBtn.classList.remove('active');
  if (!playingSources.size) updateState('idle');
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
function runKernelTool(tool) { sendAction('kernel_tool', { tool, params: {} }); }
function resetSession() { interrupt(); sendAction('reset'); responseNode = null; document.getElementById('conversation').replaceChildren(); document.getElementById('canvasPanel').replaceChildren(); }
async function checkModels() {
  connectionBox.textContent = 'Loading local speech models…';
  try {
    const response = await fetch('/api/health'); if (!response.ok) throw new Error(response.statusText);
    const data = await response.json(); renderArtifact({ artifact_type: 'json', content: JSON.stringify(data, null, 2) });
    connectionBox.textContent = data.llm.ready && Object.values(data.speech).every(model => model.ready) ? 'Local speech models ready' : 'Some local models are unavailable; see details below';
  } catch (error) { showError(error.message); }
}
document.getElementById('textForm').addEventListener('submit', async event => {
  event.preventDefault(); errorBox.textContent = '';
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
function renderVisuals() {
  requestAnimationFrame(renderVisuals);
  time += 0.035;

  // 1. Draw Voice Orb
  const w = orbCanvas.width;
  const h = orbCanvas.height;
  const cx = w / 2;
  const cy = h / 2;
  ctx.clearRect(0, 0, w, h);

  const colors = {
    idle: { in: '#38bdf8', out: '#0369a1', glow: 'rgba(56, 189, 248, 0.25)' },
    listening: { in: '#06b6d4', out: '#0891b2', glow: 'rgba(6, 182, 212, 0.45)' },
    thinking: { in: '#c084fc', out: '#7e22ce', glow: 'rgba(192, 132, 252, 0.45)' },
    speaking: { in: '#34d399', out: '#059669', glow: 'rgba(52, 211, 153, 0.55)' },
    interrupted: { in: '#f87171', out: '#dc2626', glow: 'rgba(248, 113, 113, 0.65)' }
  }[orbState] || { in: '#38bdf8', out: '#0369a1', glow: 'rgba(56, 189, 248, 0.25)' };

  const amp = orbState === 'speaking' || orbState === 'listening' ? currentAmplitude : 0.08;
  const baseRadius = 75 + amp * 35;

  // Ambient outer glow
  const g = ctx.createRadialGradient(cx, cy, baseRadius * 0.4, cx, cy, baseRadius * 1.5);
  g.addColorStop(0, colors.in);
  g.addColorStop(0.6, colors.out);
  g.addColorStop(1, 'rgba(0,0,0,0)');
  ctx.beginPath();
  ctx.arc(cx, cy, baseRadius * 1.4, 0, Math.PI * 2);
  ctx.fillStyle = g;
  ctx.fill();

  // Wavy oscillating orb surface
  ctx.beginPath();
  for (let i = 0; i <= 64; i++) {
    const angle = (i / 64) * Math.PI * 2;
    const wave = Math.sin(angle * 5 + time * 3 + wavePhase) * (6 + amp * 20) +
                 Math.cos(angle * 3 - time * 2) * (3 + amp * 12);
    const r = baseRadius + wave;
    const x = cx + Math.cos(angle) * r;
    const y = cy + Math.sin(angle) * r;
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  }
  ctx.closePath();
  const cg = ctx.createRadialGradient(cx - 15, cy - 15, 10, cx, cy, baseRadius);
  cg.addColorStop(0, '#ffffff');
  cg.addColorStop(0.3, colors.in);
  cg.addColorStop(1, colors.out);
  ctx.fillStyle = cg;
  ctx.shadowColor = colors.glow;
  ctx.shadowBlur = 25 + amp * 35;
  ctx.fill();
  ctx.shadowBlur = 0;

  // 2. Draw 8-Band Equalizer
  eqCtx.clearRect(0, 0, eqCanvas.width, eqCanvas.height);
  const barWidth = 34;
  const barGap = 8;
  const startX = 6;
  for (let b = 0; b < 8; b++) {
    const val = spectrum8[b] || 0.02;
    const barHeight = Math.max(4, val * 42);
    const x = startX + b * (barWidth + barGap);
    const y = eqCanvas.height - barHeight;

    const bg = eqCtx.createLinearGradient(0, y, 0, eqCanvas.height);
    bg.addColorStop(0, colors.in);
    bg.addColorStop(1, colors.out);
    eqCtx.fillStyle = bg;
    eqCtx.fillRect(x, y, barWidth, barHeight);
  }
}


connectWs();
renderVisuals();

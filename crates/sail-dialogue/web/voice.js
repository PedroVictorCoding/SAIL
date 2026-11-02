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
const lastToolCalls = new Map();
const pendingApprovalActions = new Map();
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
let turnGenStart = 0, turnFirstTokenTime = 0, turnTokenCount = 0;
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
  if (name.startsWith('proc:') || name.startsWith('fs:') || name.startsWith('github:')) return document.getElementById('toolsCard');
  if (name.startsWith('forge:')) return document.getElementById('forgeCard');
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
  const labels = {
    'os:status':'Checking system',
    'proc:list':'Checking processes',
    'fs:query':'Looking in workspace',
    'fs:read':'Reading workspace file',
    'fs:write':'Writing workspace file',
    'vision:frame':'Analyzing image',
    'forge:enqueue':'Dispatching Forge task',
    'forge:status':'Checking Forge job',
    'forge:list':'Listing Forge tasks',
    'github:issues':'Checking GitHub repo',
    'web:read':'Reading web page',
  };
  document.getElementById('activityText').textContent = activeTools.size ? labels[[...activeTools.keys()].at(-1)] || 'Using a local tool' : stateLabels[orbState] || '';
}
function sendAction(action, fields = {}) {
  if (!ws || ws.readyState !== WebSocket.OPEN) { showError('Connection unavailable. Wait for reconnection.'); return false; }
  ws.send(JSON.stringify({ action, ...fields })); return true;
}
function connectWs() {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  ws = new WebSocket(`${proto}//${location.host}/ws/voice`);
  ws.onopen = () => {
    setConnection('Connected · local session', true);
    micBtn.disabled = false;
    if (orbState === 'disconnected') updateState('idle');
    sendAction('forge:list');
  };
  ws.onerror = () => showError('Voice connection failed. Check the local server and HTTPS certificate.');
  ws.onclose = () => {
    pendingApprovalActions.clear(); lastToolCalls.clear();
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
        turnGenStart = performance.now();
        turnFirstTokenTime = 0;
        turnTokenCount = 0;
        appendConversation('You', data.transcript);
        responseNode = appendConversation('SAIL', '');
        updateState('thinking');
        document.getElementById('sessionTokensPerSec').textContent = 'Calculating…';
        document.getElementById('sessionTtft').textContent = 'Waiting…';
        document.getElementById('sessionGenStats').textContent = '0 tokens';
        break;
      case 'response_delta':
        responseText += data.text;
        if (!responseNode) responseNode = appendConversation('SAIL', '');
        responseNode.lastChild.textContent = responseText;
        if (!turnTokenCount) updateState('generating');
        if (data.token_count !== undefined) {
          turnTokenCount = data.token_count;
        } else {
          turnTokenCount += Math.max(1, Math.round(data.text.length / 3.8));
        }
        updateGenMetrics(data);
        break;
      case 'response_finished':
        updateGenMetrics(data);
        updateState('speaking');
        break;
      case 'audio_chunk': queueAudio(data); break;
      case 'turn_finished':
        setCaptureHeld(false);
        turnInProgress = false;
        document.getElementById('assistantAnnounce').textContent = responseText;
        turnFinished = true;
        if (!pendingAudio && !playingSources.size) finishPlayback();
        if (data.latency_ms) setConnection(`Connected · local turn ${(data.latency_ms / 1000).toFixed(1)}s`, true);
        updateGenMetrics(data);
        updateState('idle');
        break;
      case 'interruption': cancelVisualCapture(); setCaptureHeld(false); turnInProgress = false; stopPlayback(); updateState(data.state || 'interrupted'); break;
      case 'control': applyControl(data.control); break;
      case 'tool_event': {
        const event = data.event;
        if (event.approval_session) {
          pendingApprovalActions.set(event.approval_session, { automatic:true });
          renderApprovalCard({session_id:event.approval_session,tool:event.tool,rationale:'Review the exact operation with your owner credentials before continuing.'});
          updateState('approval_required');
          transcriptBox.textContent = 'Approval needed · Review and approve on Canvas or Settings.';
        }
        updateToolActivity(event.tool, !['completed','failed','cancelled'].includes(event.state));
        if (event.state === 'executing') updateState('working');
        if (['completed','failed','cancelled'].includes(event.state)) updateState(turnInProgress ? 'thinking' : 'idle');
        break;
      }
      case 'tool_started': updateToolActivity(data.tool, true); break;
      case 'tool_result': updateToolActivity(data.result?.tool, false); renderToolResult(data.result); logToolActivity(data.result); break;
      case 'canvas': renderArtifact(data.artifact); break;
      case 'approval_required':
        renderApprovalCard(data);
        updateState('approval_required');
        break;
      case 'approval_resolved':
        handleApprovalResolved(data);
        break;
      case 'forge_event':
      case 'forge_enqueued':
      case 'forge_status_response':
        handleForgeEvent(data);
        break;
      case 'forge_jobs':
        renderForgeJobList(data.jobs);
        break;
      case 'forge_cancelled':
        handleForgeCancelled(data);
        break;
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
function renderToolResult(result) {
  const approval = result?.output?.approval_session;
  if (approval) {
    const action = result?.output?.approval_action || lastToolCalls.get(normalizeToolName(result.tool));
    if (action) { pendingApprovalActions.set(approval, action); if (pendingApprovalActions.size > 128) pendingApprovalActions.delete(pendingApprovalActions.keys().next().value); }
    renderApprovalCard({
      session_id: approval,
      tool: result.tool,
      rationale: result?.output?.error || 'Operation requires explicit user authorization before execution.'
    });
    updateState('approval_required');
    return;
  }
  const container = document.createElement('article'); container.className = 'artifact-card';
  const output = result?.output ?? {}; const succeeded = Boolean(result?.success);
  if (result?.tool === 'os:status' && succeeded && output) updateSystemCard(output);

  const heading = document.createElement('div'); heading.className = 'artifact-heading';
  const title = document.createElement('span'); title.textContent = String(result?.tool || 'Local action').replaceAll(':', ' · ');

  const rightWrap = document.createElement('div');
  rightWrap.style.display = 'flex';
  rightWrap.style.alignItems = 'center';
  rightWrap.style.gap = '8px';
  const badge = document.createElement('span');
  badge.className = succeeded ? 'result-success' : 'result-failure';
  badge.textContent = succeeded ? '● Done' : '● Could not complete';
  const expandBtn = document.createElement('button');
  expandBtn.type = 'button';
  expandBtn.className = 'card-expand-btn';
  expandBtn.title = 'Expand details into pop-up';
  expandBtn.setAttribute('aria-label', 'Expand details into pop-up');
  expandBtn.textContent = '⤢';
  rightWrap.append(badge, expandBtn);
  heading.append(title, rightWrap);

  const summary = document.createElement('p'); summary.className = 'result-summary';
  if (Array.isArray(output?.processes)) summary.textContent = `${output.processes.length} ${output.processes.length === 1 ? 'process' : 'processes'} found`;
  else if (Array.isArray(output?.entries)) summary.textContent = `${output.entries.length} ${output.entries.length === 1 ? 'item' : 'items'} in ${String(output.path || 'the selected folder')}`;
  else if (output?.issue && output?.title) summary.textContent = `GitHub Issue #${output.number || output.issue.number}: ${output.title} (${output.state || 'OPEN'})`;
  else if (Array.isArray(output?.issues)) summary.textContent = `${output.count} GitHub issues retrieved (sorted by priority)`;
  else if (result?.tool === 'web:read' && output?.url) summary.textContent = `Fetched ${output.bytes || output.content?.length || 0} bytes from ${output.url}`;
  else if (result?.tool === 'forge:enqueue' && output?.job_id) summary.textContent = `Forge job enqueued: ${output.job_id} (Issue #${output.issue_number || '—'})`;
  else if (result?.tool === 'forge:status' && output?.job_id) summary.textContent = `Forge job ${output.job_id}: ${output.state || 'active'} (${output.diff_stat || output.current_step || '—'})`;
  else if (output?.job_id) summary.textContent = `Forge job ${output.job_id}: ${output.state || 'active'}`;
  else if (output?.bytes_written) summary.textContent = `Wrote ${output.bytes_written} bytes to ${output.path}`;
  else if (output?.content) summary.textContent = `Read ${output.bytes || output.content.length} bytes from ${output.path}`;
  else if (output?.os) summary.textContent = `${String(output.os)} · ${String(output.logical_cpus || '—')} CPUs · ${String(output.memory_percent || '—')} memory used`;
  else if (output?.error) summary.textContent = String(output.error);
  else if (Number.isFinite(output?.width) && Number.isFinite(output?.height)) summary.textContent = `Image analyzed · ${output.width} × ${output.height} pixels`;
  else if (typeof output?.summary === 'string') summary.textContent = output.summary;
  else if (typeof output === 'string') summary.textContent = output;
  else summary.textContent = succeeded ? 'The local action completed.' : 'The local action needs attention.';

  const hint = document.createElement('div');
  hint.className = 'card-expand-hint';
  hint.innerHTML = '<span aria-hidden="true">⤢</span> View full details in pop-up';

  container.append(heading, summary, hint);

  const openModal = () => {
    const modalBody = document.createElement('div');
    const modalSummary = document.createElement('p');
    modalSummary.className = 'result-summary';
    modalSummary.style.fontSize = '14px';
    modalSummary.style.marginBottom = '14px';
    modalSummary.textContent = summary.textContent;
    modalBody.append(modalSummary);

    if (output?.issue && output?.title) {
      const issueDetails = document.createElement('div');
      issueDetails.style.marginBottom = '14px';
      const heading = document.createElement('h3');
      heading.style.margin = '0 0 8px';
      heading.textContent = `#${output.number || output.issue.number} ${output.title}`;
      const meta = document.createElement('p');
      meta.className = 'sheet-note';
      meta.textContent = `State: ${output.state || 'OPEN'} · Author: ${output.issue.author?.login || 'unknown'} · Created: ${output.issue.createdAt ? new Date(output.issue.createdAt).toLocaleString() : '—'}`;
      const body = document.createElement('pre');
      body.style.whiteSpace = 'pre-wrap';
      body.style.wordBreak = 'break-word';
      body.style.background = 'rgba(255,255,255,0.03)';
      body.style.padding = '10px';
      body.style.borderRadius = '8px';
      body.textContent = output.body || output.issue.body || 'No description provided.';
      const solveBtn = document.createElement('button');
      solveBtn.className = 'quiet-button mini-btn';
      solveBtn.style.marginTop = '8px';
      solveBtn.textContent = 'Solve with Forge';
      solveBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        sendAction('forge:enqueue', { issue_number: output.number || output.issue.number, task: output.title });
        document.getElementById('cardModal')?.close();
      });
      issueDetails.append(heading, meta, body, solveBtn);
      modalBody.append(issueDetails);
    }

    if (result?.tool === 'web:read' && output?.content) {
      const webDetails = document.createElement('div');
      webDetails.style.marginBottom = '14px';
      const webUrl = document.createElement('p');
      webUrl.className = 'sheet-note';
      webUrl.textContent = `Source: ${output.url}`;
      const pre = document.createElement('pre');
      pre.style.whiteSpace = 'pre-wrap';
      pre.style.wordBreak = 'break-word';
      pre.style.maxHeight = '400px';
      pre.style.overflowY = 'auto';
      pre.style.background = 'rgba(255,255,255,0.03)';
      pre.style.padding = '10px';
      pre.style.borderRadius = '8px';
      pre.textContent = output.content;
      webDetails.append(webUrl, pre);
      modalBody.append(webDetails);
    }

    if (Array.isArray(output?.issues)) {
      const issueList = document.createElement('div');
      issueList.className = 'forge-issue-list';
      issueList.style.marginBottom = '14px';
      for (const issue of output.issues) {
        const item = document.createElement('div');
        item.className = 'forge-issue-item';
        const priority=['P0','P1','P2','P3','P4'].includes(issue.priority) ? issue.priority : 'P?';
        const pill=document.createElement('span');pill.className=`forge-priority-pill ${priority==='P?' ? 'punknown' : priority.toLowerCase()}`;pill.textContent=priority;
        const label=document.createElement('span');label.textContent=`#${issue.number} ${issue.title}`;item.append(pill,label);
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'quiet-button mini-btn';
        btn.textContent = 'Solve with Forge';
        btn.addEventListener('click', (e) => {
          e.stopPropagation();
          sendAction('forge:enqueue', { issue_number: issue.number, task: issue.title });
          document.getElementById('cardModal')?.close();
        });
        item.append(btn);
        issueList.append(item);
      }
      modalBody.append(issueList);
    }

    const pre = document.createElement('pre');
    pre.textContent = JSON.stringify(result, null, 2);
    modalBody.append(pre);
    if (output?.payload_ref) {
      const view=document.createElement('button'); view.className='quiet-button'; view.textContent='Open full output';
      view.addEventListener('click',()=>{document.getElementById('cardModal')?.close();sendAction('tool_payload',{id:output.payload_ref});});
      modalBody.append(view);
    }

    const copyBtn = document.createElement('button');
    copyBtn.type = 'button';
    copyBtn.className = 'quiet-button';
    copyBtn.textContent = 'Copy JSON';
    copyBtn.addEventListener('click', () => {
      navigator.clipboard.writeText(JSON.stringify(result, null, 2));
      copyBtn.textContent = 'Copied!';
      setTimeout(() => { copyBtn.textContent = 'Copy JSON'; }, 2000);
    });

    openCardModal({
      icon: '⚙',
      title: String(result?.tool || 'Local action'),
      badge: succeeded ? '● Done' : '● Could not complete',
      badgeClass: succeeded ? 'result-success' : 'result-failure',
      bodyNode: modalBody,
      footerNode: copyBtn
    });
  };

  container.addEventListener('click', openModal);
  expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });

  document.getElementById('canvasEmptyState')?.remove();
  document.getElementById('canvasPanel').append(container);
}

function updateSystemCard(output) {
  const cpuText = Number.isFinite(output.logical_cpus)
    ? `${output.logical_cpus} CPUs${output.cpu_percent ? ` · ${output.cpu_percent}` : ''}`
    : 'Unavailable';
  document.getElementById('systemCpu').textContent = cpuText;
  document.getElementById('systemMemory').textContent = output.memory_percent ? `${String(output.memory_percent)} used` : (output.memory_used_gb || 'Unavailable');

  let loadStr = 'Unavailable';
  if (Array.isArray(output.load_averages) && output.load_averages.length > 0) {
    loadStr = output.load_averages.map(v => Number(v).toFixed(2)).join(' · ');
  } else if (Array.isArray(output.load_average)) {
    loadStr = output.load_average.slice(0, 3).map(value => Number(value).toFixed(2)).join(' · ');
  } else if (typeof output.load_average === 'string') {
    loadStr = output.load_average;
  }
  document.getElementById('systemLoad').textContent = loadStr;
  document.getElementById('systemNote').textContent = [output.os, output.uptime_hours ? `Uptime ${output.uptime_hours} h` : ''].filter(Boolean).join(' · ') || 'Live status from the local device.';

  if (window.AdaptiveFeedEngine) {
    const rawCpu = typeof output.cpu_usage_raw === 'number'
      ? output.cpu_usage_raw
      : (Array.isArray(output.load_averages) && output.logical_cpus ? (output.load_averages[0] / output.logical_cpus) * 100 : 15);
    window.AdaptiveFeedEngine.updateCpuUsage(rawCpu);
  }
}

function logToolActivity(result) {
  const list = document.getElementById('toolsActivityList');
  if (!list || !result) return;
  document.getElementById('toolsActivityEmpty')?.remove();
  const item = document.createElement('div');
  item.className = 'activity-item';
  const left = document.createElement('div');
  left.className = 'activity-item-left';
  const toolName = document.createElement('strong');
  toolName.textContent = result.tool || 'tool';
  const status = document.createElement('span');
  status.className = `activity-status-pill ${result.success ? 'success' : 'error'}`;
  status.textContent = result.success ? 'OK' : 'ERR';
  left.append(toolName, status);
  const time = document.createElement('span');
  time.className = 'activity-time';
  time.textContent = new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  item.append(left, time);
  list.prepend(item);
  while (list.children.length > 20) list.lastElementChild?.remove();
}

function renderApprovalCard(data) {
  const existing = document.getElementById(`approval-${data.session_id}`);
  if (existing) return;
  const container = document.createElement('article');
  container.className = 'artifact-card approval-card';
  container.id = `approval-${data.session_id}`;

  const heading = document.createElement('div');
  heading.className = 'artifact-heading';

  const title = document.createElement('span');
  title.innerHTML = `🛡️ <strong>Guardian Security Approval</strong>`;

  const rightWrap = document.createElement('div');
  rightWrap.style.display = 'flex';
  rightWrap.style.alignItems = 'center';
  rightWrap.style.gap = '8px';

  const badge = document.createElement('span');
  badge.className = 'approval-badge';
  badge.textContent = `${(data.risk_level || 'Medium').toUpperCase()} RISK`;

  const expandBtn = document.createElement('button');
  expandBtn.type = 'button';
  expandBtn.className = 'card-expand-btn';
  expandBtn.title = 'Expand approval details into pop-up';
  expandBtn.setAttribute('aria-label', 'Expand approval details into pop-up');
  expandBtn.textContent = '⤢';

  rightWrap.append(badge, expandBtn);
  heading.append(title, rightWrap);

  const summary = document.createElement('p');
  summary.className = 'approval-rationale';
  summary.textContent = data.rationale || `Operation requires explicit user authorization before execution.`;

  const details = document.createElement('div');
  details.className = 'approval-target';
  details.textContent = `Action: ${data.tool || 'Kernel tool'}`;

  const actions = document.createElement('div');
  actions.className = 'approval-actions';

  const approveBtn = document.createElement('button');
  approveBtn.type = 'button';
  approveBtn.className = 'approval-btn approve';
  approveBtn.innerHTML = '<span>✓</span> Approve & Execute';
  async function reviewOrApprove(button, scope) {
    button.disabled=true;
    try {
      if (!button.dataset.reviewed) {
        const pending=await ownerApprovalRequest('approvals');
        const request=pending.pending.find(item=>item.session_id===data.session_id);
        if(!request)throw new Error('This approval expired or was cancelled.');
        scope.textContent=JSON.stringify({actor:request.requester,capability:request.capability,correlation_id:request.correlation_id,expires_at:request.expires_at},null,2);
        button.dataset.reviewed='true';button.textContent='Approve and continue';
      } else {
        await ownerApprovalRequest('resolve',{session_id:data.session_id,approved:true});
        button.dataset.resolved='true';handleApprovalResolved({session_id:data.session_id,approved:true});
      }
    } catch(error){showError(error.message);}
    finally {button.disabled=Boolean(button.dataset.resolved);}
  }
  async function denyApproval(button) {
    button.disabled=true;
    try {await ownerApprovalRequest('revoke',{session_id:data.session_id});handleApprovalResolved({session_id:data.session_id,approved:false});}
    catch(error){showError(error.message);button.disabled=false;}
  }
  approveBtn.textContent='Review exact request';
  approveBtn.addEventListener('click',event=>{event.stopPropagation();return reviewOrApprove(approveBtn,details);});
  const alwaysBtn = document.createElement('button');
  alwaysBtn.type = 'button';
  alwaysBtn.className = 'approval-btn always';
  alwaysBtn.innerHTML = '<span>⚡</span> Always allow';
  alwaysBtn.title = 'Approve now and automatically allow matching actions in the future';
  alwaysBtn.addEventListener('click', async (event) => {
    event.stopPropagation();
    alwaysBtn.disabled = true;
    approveBtn.disabled = true;
    denyBtn.disabled = true;
    try {
      await ownerApprovalRequest('resolve', { session_id: data.session_id, approved: true, scope: 'always', remember: true });
      alwaysBtn.dataset.resolved = 'true';
      handleApprovalResolved({ session_id: data.session_id, approved: true });
      refreshLearnedRules();
    } catch (err) {
      showError(err.message);
      alwaysBtn.disabled = false;
      approveBtn.disabled = false;
      denyBtn.disabled = false;
    }
  });

  const denyBtn=document.createElement('button');denyBtn.type='button';denyBtn.className='approval-btn deny';denyBtn.textContent='Deny';
  denyBtn.addEventListener('click',event=>{event.stopPropagation();return denyApproval(denyBtn);});

  actions.append(approveBtn, alwaysBtn, denyBtn);

  const hint = document.createElement('div');
  hint.className = 'card-expand-hint';
  hint.innerHTML = '<span aria-hidden="true">⤢</span> View security audit parameters in pop-up';

  container.append(heading, summary, details, actions, hint);

  const openModal = () => {
    const modalBody = document.createElement('div');
    const modalRationale = document.createElement('p');
    modalRationale.className = 'approval-rationale';
    modalRationale.style.fontSize = '14px';
    modalRationale.style.marginBottom = '14px';
    modalRationale.textContent = data.rationale || 'Explicit approval required by Guardian broker.';
    modalBody.append(modalRationale);

    const pre = document.createElement('pre');
    pre.textContent = JSON.stringify({
      session_id: data.session_id,
      tool: data.tool,
      params: data.params,
      risk_level: data.risk_level || 'Medium'
    }, null, 2);
    modalBody.append(pre);

    const modalActions = document.createElement('div');
    modalActions.style.display = 'flex';
    modalActions.style.gap = '10px';

    const modalApprove = document.createElement('button');
    modalApprove.type = 'button';
    modalApprove.className = 'approval-btn approve';
    modalApprove.textContent='Review exact request';
    modalApprove.addEventListener('click',()=>reviewOrApprove(modalApprove,pre));
    const modalDeny=document.createElement('button');modalDeny.type='button';modalDeny.className='approval-btn deny';modalDeny.textContent='Deny';
    modalDeny.addEventListener('click',()=>denyApproval(modalDeny));

    modalActions.append(modalApprove, modalDeny);

    openCardModal({
      icon: '🛡️',
      title: 'Guardian Security Approval',
      badge: `${(data.risk_level || 'Medium').toUpperCase()} RISK`,
      badgeClass: 'approval-badge',
      bodyNode: modalBody,
      footerNode: modalActions
    });
  };

  container.addEventListener('click', openModal);
  expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });

  document.getElementById('canvasEmptyState')?.remove();
  document.getElementById('canvasPanel').prepend(container);
  container.scrollIntoView({ behavior:'smooth', block:'nearest' });
}

function handleApprovalResolved(data) {
  const card = document.getElementById(`approval-${data.session_id}`);
  if (!card) return;
  const actions = card.querySelector('.approval-actions');
  if (actions) {
    actions.replaceChildren();
    const status = document.createElement('span');
    if (data.approved) {
      status.className = 'approval-resolved approved';
      status.textContent = '✓ Approved · Execution may now continue';
    } else {
      status.className = 'approval-resolved denied';
      status.textContent = '✕ Denied by user';
    }
    actions.append(status);
  }
}

function updateForgePipeline(step) {
  const chips = document.querySelectorAll('.forge-chip');
  const stepMap = {
    'worktree': 0, 'created': 0, 'enqueued': 0,
    'planning': 1, 'solution': 1,
    'coding': 2,
    'testing': 3,
    'review': 4, 'reviewing': 4,
    'pr': 5, 'completed': 5, 'pr_draft_ready': 5
  };
  const activeIdx = stepMap[String(step).toLowerCase()] ?? -1;
  chips.forEach((chip, idx) => {
    chip.classList.toggle('active', idx === activeIdx);
    chip.classList.toggle('done', activeIdx >= 0 && idx < activeIdx);
  });
}

function formatDiffHighlight(diffText) {
  if (!diffText) return '';
  const lines = diffText.split('\n');
  return lines.map(line => {
    const escaped = line.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
    if (line.startsWith('+++') || line.startsWith('---') || line.startsWith('diff ')) {
      return `<span class="diff-line-file">${escaped}</span>`;
    } else if (line.startsWith('@@')) {
      return `<span class="diff-line-hunk">${escaped}</span>`;
    } else if (line.startsWith('+')) {
      return `<span class="diff-line-add">${escaped}</span>`;
    } else if (line.startsWith('-')) {
      return `<span class="diff-line-del">${escaped}</span>`;
    }
    return `<span>${escaped}</span>`;
  }).join('\n');
}

function handleForgeEvent(data) {
  const jobContainer = document.getElementById('forgeActiveJob');
  if (!jobContainer) return;
  document.getElementById('forgeEmptyState')?.remove();

  const eventType = data.event_type || data.type;
  const jobId = data.job_id || 'Job';
  const state = data.state || data.status?.state || (data.success ? 'completed' : 'active');

  updateForgePipeline(state);

  let jobCard = document.getElementById(`forge-job-${jobId}`);
  if (!jobCard) {
    jobCard = document.createElement('div');
    jobCard.id = `forge-job-${jobId}`;
    jobCard.className = 'forge-job-item';
    jobContainer.prepend(jobCard);
  }

  const badgeClass = state === 'completed' ? 'result-success' : state === 'blocked' ? 'result-failure' : 'forge-state-active';

  jobCard.replaceChildren();
  const heading=document.createElement('div');heading.className='forge-job-heading';
  const label=document.createElement('span');label.textContent=`${jobId}${data.issue_number ? ` · Issue #${data.issue_number}` : ''}`;
  const badge=document.createElement('span');badge.className=badgeClass;badge.textContent=`● ${state}`;heading.append(label,badge);jobCard.append(heading);
  const metadata=(label,value)=>{const line=document.createElement('div');line.className='forge-meta-line';const key=document.createElement('span');key.textContent=label;const text=document.createElement('strong');text.textContent=String(value);line.append(key,text);jobCard.append(line);};
  if(data.repo)metadata('Repo:',data.repo);
  if(data.model_id||data.model)metadata('Model:',data.model_id||data.model);
  if(data.iterations)metadata('Iterations:',data.iterations);
  if(data.failure_reason)metadata('Reason:',data.failure_reason);
  if(data.task||data.summary||data.message){const text=document.createElement('div');text.className='forge-task-desc';text.textContent=String(data.task||data.summary||data.message);jobCard.append(text);}
  if(data.branch)metadata('Branch:',data.branch);
  if(data.passed!==undefined)metadata('Review verdict:',`${data.passed ? '✓ Passed' : 'Failed'} (${data.finding_count||0} findings)`);
  if(data.pr_draft_ready)metadata('✓','Pull Request draft ready for review');

  // Live Forge Code Stream & Diff Viewer
  const liveStream = document.getElementById('forgeLiveStream');
  const liveTitle = document.getElementById('forgeLiveTitle');
  const liveFile = document.getElementById('forgeLiveFile');
  const liveDiff = document.getElementById('forgeLiveDiff');
  const liveLogs = document.getElementById('forgeLiveLogs');
  const expandDiffBtn = document.getElementById('forgeExpandDiffBtn');

  if (liveStream) {
    const isLivePhase = ['planning', 'coding', 'testing'].includes(state);
    if (data.diff || data.target_file || data.logs || isLivePhase) {
      liveStream.hidden = false;
      if (liveTitle) {
        const stateLabels = {
          worktree: '🌲 Provisioning isolated worktree...',
          planning: '🧠 Synthesizing solution plan...',
          coding: '⚡ Coding & applying unified patch...',
          testing: '🧪 Running sandbox cargo tests...',
          review: '🛡️ Automated Guardian code review...',
          pr: '🚀 Pull request drafted',
          completed: '✓ Solution verified & completed'
        };
        liveTitle.textContent = stateLabels[state] || `Forge: ${state}`;
      }
      if (data.target_file && liveFile) {
        liveFile.textContent = data.target_file;
        liveFile.hidden = false;
      }
      if (data.diff && liveDiff) {
        liveDiff.innerHTML = `<code>${formatDiffHighlight(data.diff)}</code>`;
        liveDiff.scrollTop = liveDiff.scrollHeight;
      } else if (!data.diff && ['planning', 'coding'].includes(state) && liveDiff) {
        liveDiff.innerHTML = `<code>// Generating code patch for ${data.target_file || 'workspace'}...</code>`;
      }
      if (Array.isArray(data.logs) && data.logs.length && liveLogs) {
        liveLogs.replaceChildren();
        data.logs.forEach((log, idx) => {
          const item = document.createElement('div');
          item.className = 'forge-live-log-item';
          if (idx === data.logs.length - 1) item.classList.add('active');
          item.textContent = log;
          liveLogs.appendChild(item);
        });
        liveLogs.scrollTop = liveLogs.scrollHeight;
      }

      if (expandDiffBtn && typeof openCardModal === 'function') {
        expandDiffBtn.onclick = (e) => {
          e.stopPropagation();
          const currentDiff = data.diff || liveDiff.textContent;
          openCardModal({
            title: `Forge Live Diff · ${data.target_file || jobId}`,
            badge: `${state.toUpperCase()}`,
            badgeClass: state === 'completed' ? 'result-success' : 'forge-state-active',
            content: currentDiff,
            contentType: 'code'
          });
        };
      }
    }
  }
}

function renderForgeJobList(jobs) {
  const jobContainer = document.getElementById('forgeActiveJob');
  if (!jobContainer) return;
  if (!Array.isArray(jobs) || jobs.length === 0) {
    jobContainer.innerHTML = '<p class="forge-empty" id="forgeEmptyState">No active Forge jobs. Ask SAIL or enqueue below.</p>';
    return;
  }
  jobContainer.replaceChildren();
  for (const job of jobs) {
    handleForgeEvent({
      job_id: job.job_id,
      repo: job.repo,
      issue_number: job.issue_number,
      state: typeof job.state === 'string' ? job.state : JSON.stringify(job.state),
      branch: job.branch,
      summary: job.diff_stat ? `${job.diff_stat.files_changed} files changed, +${job.diff_stat.insertions}/-${job.diff_stat.deletions}` : null,
    });
  }
}

function handleForgeCancelled(data) {
  const jobCard = document.getElementById(`forge-job-${data.job_id}`);
  if (jobCard) {
    const badge = jobCard.querySelector('.forge-job-heading span:last-child');
    if (badge) {
      badge.className = 'result-failure';
      badge.textContent = '● Cancelled';
    }
  }
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
  const detailedLabels = {
    holding:'Ready to send', idle:'Ready', listening:'Listening…', transcribing:'Processing audio…',
    thinking:'Processing / Thinking…', generating:'Generating tokens…', speaking:'Speaking response…',
    working:'Executing tool…', seeing:'Analyzing image…', watching:'Analyzing video…',
    interrupted:'Interrupted', disconnected:'Offline', approval_required:'Approval needed', error:'Attention needed'
  };
  statusBadge.textContent = labels[state] || 'Ready'; document.getElementById('appShell').dataset.state = state;
  const palette = statePalette(paletteState(state));
  ['--ambient-one','--ambient-two','--ambient-three'].forEach((property,index) => document.documentElement.style.setProperty(property, [palette.primary,palette.secondary,palette.tertiary][index]));
  document.getElementById('sessionState').textContent = labels[state] || 'Ready';
  const liveState = document.getElementById('sessionLiveState');
  if (liveState) liveState.textContent = detailedLabels[state] || labels[state] || state || 'Ready';
  if (changed && reduceMotion.matches && !document.hidden) renderVisuals();
  syncActivityPill();
  document.getElementById('interruptBtn').hidden = !['thinking','speaking','working','transcribing','generating'].includes(state);
}
function updateGenMetrics(data) {
  if (data?.token_count !== undefined) {
    turnTokenCount = data.token_count;
  }
  const ttftEl = document.getElementById('sessionTtft');
  if (ttftEl) {
    if (data?.ttft_ms !== undefined && data.ttft_ms !== null) {
      ttftEl.textContent = `${data.ttft_ms} ms`;
    } else if (!turnFirstTokenTime && turnGenStart) {
      turnFirstTokenTime = performance.now();
      ttftEl.textContent = `${Math.round(turnFirstTokenTime - turnGenStart)} ms`;
    }
  }

  const speedEl = document.getElementById('sessionTokensPerSec');
  if (speedEl) {
    if (data?.tokens_per_sec !== undefined && data.tokens_per_sec > 0) {
      speedEl.textContent = `${Number(data.tokens_per_sec).toFixed(1)} tok/s`;
    } else if (turnFirstTokenTime && turnTokenCount > 0) {
      const elapsedSec = (performance.now() - turnFirstTokenTime) / 1000;
      if (elapsedSec > 0.05) {
        speedEl.textContent = `${(turnTokenCount / elapsedSec).toFixed(1)} tok/s`;
      }
    }
  }

  const genStatsEl = document.getElementById('sessionGenStats');
  if (genStatsEl) {
    if (data?.generation_ms) {
      genStatsEl.textContent = `${turnTokenCount} tok · ${(data.generation_ms / 1000).toFixed(1)}s`;
    } else if (turnGenStart) {
      const elapsedSec = ((performance.now() - turnGenStart) / 1000).toFixed(1);
      genStatsEl.textContent = `${turnTokenCount} tok · ${elapsedSec}s`;
    }
  }
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
    runKernelTool('vision:frame', { width: canvas.width, height: canvas.height, format: 'rgba', data: Array.from(pixels) });
  } catch (error) { showError(error.message); }
}
function runKernelTool(tool, params = {}, approval_token) {
  if (sendAction('kernel_tool', { tool, params, approval_token })) {
    lastToolCalls.set(normalizeToolName(tool), {tool,params}); updateToolActivity(tool, true);
  }
}
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
let time = 0;
let animationFrame = 0;
const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
function renderVisuals() {
  animationFrame = 0;
  if (document.hidden) return;
  if (!orbFrozen && !reduceMotion.matches) time += 0.035;

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

  const g = ctx.createRadialGradient(cx, cy, baseRadius * 0.2, cx, cy, baseRadius * 1.55);
  g.addColorStop(0, `${colors.mid}66`);
  g.addColorStop(0.5, `${colors.out}30`);
  g.addColorStop(1, 'rgba(0,0,0,0)');
  ctx.beginPath();
  ctx.arc(cx, cy, baseRadius * 1.4, 0, Math.PI * 2);
  ctx.fillStyle = g;
  ctx.fill();

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
window.sailUi = Object.freeze({
  setOrbTelemetry: data => window.dispatchEvent(new CustomEvent('sail:orb-telemetry', { detail:data })),
  resumeOrbAnimation: () => { orbFrozen = false; },
});

let cachedOwnerToken = null;

async function getOrFetchOwnerToken() {
  const tokenInput = document.getElementById('ownerApprovalToken');
  if (tokenInput && tokenInput.value && tokenInput.value.trim().length >= 32) {
    cachedOwnerToken = tokenInput.value.trim();
    try { localStorage.setItem('sail_owner_token', cachedOwnerToken); } catch (_) {}
    updateApprovalStatusBadge(true);
    return cachedOwnerToken;
  }
  try {
    const stored = localStorage.getItem('sail_owner_token');
    if (stored && stored.trim().length >= 32) {
      cachedOwnerToken = stored.trim();
      if (tokenInput && !tokenInput.value) tokenInput.value = cachedOwnerToken;
      updateApprovalStatusBadge(true);
      return cachedOwnerToken;
    }
  } catch (_) {}

  try {
    const res = await fetch('/api/guardian/token', { cache: 'no-store' });
    if (res.ok) {
      const data = await res.json();
      if (data && data.token) {
        cachedOwnerToken = data.token;
        try { localStorage.setItem('sail_owner_token', cachedOwnerToken); } catch (_) {}
        if (tokenInput) tokenInput.value = cachedOwnerToken;
        updateApprovalStatusBadge(true);
        return cachedOwnerToken;
      }
    }
  } catch (err) {
    console.debug('Could not auto-fetch owner approval token:', err);
  }
  updateApprovalStatusBadge(Boolean(tokenInput && tokenInput.value && tokenInput.value.trim().length >= 32));
  return tokenInput ? tokenInput.value : '';
}

function updateApprovalStatusBadge(configured) {
  const badge = document.getElementById('approvalStatusBadge');
  if (!badge) return;
  if (configured) {
    badge.style.background = 'rgba(34,197,94,0.15)';
    badge.style.color = '#4ade80';
    badge.innerHTML = '<span style="width:6px; height:6px; border-radius:50%; background:#4ade80;"></span> Auto-authorized';
  } else {
    badge.style.background = 'rgba(239,68,68,0.15)';
    badge.style.color = '#f87171';
    badge.innerHTML = '<span style="width:6px; height:6px; border-radius:50%; background:#f87171;"></span> Manual required';
  }
}

async function ownerApprovalRequest(path, body) {
  const token = await getOrFetchOwnerToken();
  const response = await fetch(`/api/guardian/${path}`, { method:body ? 'POST' : 'GET', cache:'no-store',
    headers:{ Authorization:`Bearer ${token}`, ...(body ? {'Content-Type':'application/json'} : {}) },
    ...(body ? { body:JSON.stringify(body) } : {}) });
  if (!response.ok) throw new Error(response.status === 401 ? 'Owner token not accepted.' : response.status === 503 ? 'Owner approval service is not configured.' : 'Approval expired, revoked, or unavailable.');
  return response.status === 204 ? null : response.json();
}
async function refreshOwnerApprovals() {
  const output = document.getElementById('ownerApprovals'); output.textContent = 'Checking pending approvals…';
  try {
    const data = await ownerApprovalRequest('approvals'); output.replaceChildren();
    if (!data.pending.length) { output.textContent = 'No pending approvals.'; return; }
    for (const request of data.pending) {
      const card = document.createElement('article'); card.className = 'artifact-card';
      const title = document.createElement('p'); title.textContent = `${request.requester} · ${request.risk_level} risk`;
      const exact = document.createElement('pre'); exact.textContent = JSON.stringify(request.capability, null, 2);
      const scope = document.createElement('p'); scope.className = 'sheet-note'; scope.textContent = `Expires ${new Date(request.expires_at).toLocaleTimeString()} · Session ${request.correlation_id || 'native trusted caller'}`;
      card.append(title,exact,scope);
      const action = pendingApprovalActions.get(request.session_id) || { automatic:true };
      if (action) {
        const approve = document.createElement('button'); approve.className = 'quiet-button'; approve.textContent = action.automatic ? 'Approve once' : 'Approve & retry';
        approve.addEventListener('click', async () => {
          approve.disabled = true;
          try { const result = await ownerApprovalRequest('resolve',{session_id:request.session_id,approved:true});
            pendingApprovalActions.delete(request.session_id); if (!action.automatic) runKernelTool(action.tool,action.params,result.token.token_id); await refreshOwnerApprovals();
          } catch (error) { showError(error.message); approve.disabled = false; }
        }); card.append(approve);

        const always = document.createElement('button'); always.className = 'quiet-button'; always.textContent = 'Always allow'; always.style.color = '#38bdf8';
        always.title = 'Approve now and automatically allow matching future actions';
        always.addEventListener('click', async () => {
          always.disabled = true;
          try {
            const result = await ownerApprovalRequest('resolve', { session_id: request.session_id, approved: true, scope: 'always', remember: true });
            pendingApprovalActions.delete(request.session_id);
            if (!action.automatic) runKernelTool(action.tool, action.params, result.token.token_id);
            await refreshOwnerApprovals();
            await refreshLearnedRules();
          } catch (error) { showError(error.message); always.disabled = false; }
        });
        card.append(always);
      } else {
        const note = document.createElement('p'); note.className='sheet-note'; note.textContent='This request belongs to another caller. Only its original session can execute it.'; card.append(note);
      }
      const reject = document.createElement('button'); reject.className='quiet-button'; reject.textContent='Deny / revoke';
      reject.addEventListener('click', async () => { try { await ownerApprovalRequest('revoke',{session_id:request.session_id}); pendingApprovalActions.delete(request.session_id); await refreshOwnerApprovals(); } catch(error) { showError(error.message); } });
      card.append(reject); output.append(card);
    }
  } catch(error) { output.textContent = error.message; }
}

const toggleTokenBtn = document.getElementById('toggleTokenVisibilityBtn');
if (toggleTokenBtn) {
  toggleTokenBtn.addEventListener('click', () => {
    const input = document.getElementById('ownerApprovalToken');
    if (!input) return;
    if (input.type === 'password') {
      input.type = 'text';
      toggleTokenBtn.textContent = 'Hide';
    } else {
      input.type = 'password';
      toggleTokenBtn.textContent = 'Show';
    }
  });
}

const copyTokenBtn = document.getElementById('copyTokenBtn');
if (copyTokenBtn) {
  copyTokenBtn.addEventListener('click', async () => {
    const token = await getOrFetchOwnerToken();
    if (token) {
      try {
        await navigator.clipboard.writeText(token);
        const prev = copyTokenBtn.textContent;
        copyTokenBtn.textContent = 'Copied!';
        setTimeout(() => { copyTokenBtn.textContent = prev; }, 1800);
      } catch (err) {
        showError('Could not copy token to clipboard');
      }
    }
  });
}

const tokenInput = document.getElementById('ownerApprovalToken');
if (tokenInput) {
  tokenInput.addEventListener('input', (e) => {
    const val = e.target.value.trim();
    if (val.length >= 32) {
      cachedOwnerToken = val;
      try { localStorage.setItem('sail_owner_token', val); } catch (_) {}
      updateApprovalStatusBadge(true);
    } else {
      updateApprovalStatusBadge(false);
    }
  });
}

document.getElementById('refreshApprovalsBtn')?.addEventListener('click',refreshOwnerApprovals);
window.addEventListener('pagehide', () => { pendingApprovalActions.clear(); lastToolCalls.clear(); });
getOrFetchOwnerToken();

async function fetchSecurityStatus() {
  try {
    const res = await fetch('/api/guardian/status', { cache: 'no-store' });
    if (!res.ok) return;
    const data = await res.json();
    const badge = document.getElementById('nonoStatusBadge');
    const note = document.getElementById('nonoDetailNote');
    const select = document.getElementById('autonomyPresetSelect');

    if (badge && data.status) {
      const isProtected = data.status === 'protected';
      const isLimited = data.status === 'limited_protection';
      badge.style.background = isProtected ? 'rgba(34,197,94,0.15)' : isLimited ? 'rgba(234,179,8,0.15)' : 'rgba(239,68,68,0.15)';
      badge.style.color = isProtected ? '#4ade80' : isLimited ? '#facc15' : '#f87171';
      const label = isProtected ? 'Protected' : isLimited ? 'Limited protection' : 'Unavailable';
      badge.innerHTML = `<span style="width:6px; height:6px; border-radius:50%; background:${badge.style.color};"></span> ${label}`;
    }

    if (note && data.nono_version) {
      const abi = data.detected_abi ? `Landlock V${data.detected_abi}` : 'Standard sandbox';
      const bwrap = data.bubblewrap_available ? ' + Bubblewrap' : '';
      note.textContent = `${abi}${bwrap} · nono ${data.nono_version}`;
    }

    if (select && data.autonomy_preset) {
      select.value = data.autonomy_preset.toLowerCase();
      updatePresetExplanation(select.value);
    }
  } catch (err) {
    console.debug('Failed to fetch security status:', err);
  }
}

function updatePresetExplanation(preset) {
  const note = document.getElementById('presetExplanationNote');
  if (!note) return;
  if (preset === 'careful') {
    note.textContent = 'High review: all filesystem modifications and external operations require explicit approval.';
  } else if (preset === 'autonomous') {
    note.textContent = 'Unattended: routine tasks within strict sandbox boundaries run without approval interruptions.';
  } else {
    note.textContent = 'Balanced: routine workspace actions run automatically; elevated actions require approval.';
  }
}

const presetSelect = document.getElementById('autonomyPresetSelect');
if (presetSelect) {
  presetSelect.addEventListener('change', async (e) => {
    updatePresetExplanation(e.target.value);
    try {
      await ownerApprovalRequest('preset', { preset: e.target.value });
    } catch (err) {
      showError(`Failed to update autonomy preset: ${err.message}`);
    }
  });
}

fetchSecurityStatus();

async function refreshLearnedRules() {
  const container = document.getElementById('guardianLearnedRules');
  if (!container) return;
  container.textContent = 'Loading rules…';
  try {
    const data = await ownerApprovalRequest('rules');
    container.replaceChildren();
    if (!data?.rules || !data.rules.length) {
      container.textContent = 'No learned approval rules yet.';
      return;
    }
    for (const rule of data.rules) {
      const row = document.createElement('div');
      row.className = 'learned-rule-item';
      const info = document.createElement('div');
      const patternKey = Object.keys(rule.pattern)[0] || 'Rule';
      const patternVal = rule.pattern[patternKey];
      const patternLabel = typeof patternVal === 'string' ? `${patternKey}: ${patternVal}` : patternKey;
      info.innerHTML = `<strong>${rule.requester}</strong> · <span class="rule-pattern">${patternLabel}</span><span class="rule-count">(${rule.auto_approved_count} auto-approvals)</span>`;
      const revokeBtn = document.createElement('button');
      revokeBtn.className = 'quiet-button';
      revokeBtn.style.color = '#f87171';
      revokeBtn.style.padding = '2px 8px';
      revokeBtn.style.fontSize = '11px';
      revokeBtn.textContent = 'Revoke';
      revokeBtn.addEventListener('click', async () => {
        revokeBtn.disabled = true;
        try {
          await ownerApprovalRequest('rules/revoke', { rule_id: rule.id });
          await refreshLearnedRules();
        } catch (err) {
          showError(err.message);
          revokeBtn.disabled = false;
        }
      });
      row.append(info, revokeBtn);
      container.append(row);
    }
  } catch (error) {
    container.textContent = 'Could not load learned rules.';
  }
}
document.getElementById('refreshLearnedRulesBtn')?.addEventListener('click', refreshLearnedRules);
refreshLearnedRules();

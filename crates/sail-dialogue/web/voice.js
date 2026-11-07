'use strict';
let liveContextEnabled = false, liveContextReplyPending = false;
let learningSession = null, learningEnabled = true, learningBusy = false;
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
// Shell light comes only from the ambient state contract (ambient-state.js, DESIGN.md §3).
// `wire` tags events handled inside a /ws/voice message with that socket and turn epoch.
let wsGeneration = 0, wire = null;
const ambient = event => window.SailAmbient?.dispatch({ ...wire, ...event });
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
  ambient({ type:'error', message:String(error), socket:undefined, epoch:undefined });
  const priorState = orbState; updateState('error'); clearTimeout(errorStateTimer);
  errorStateTimer = setTimeout(() => { if (orbState === 'error') updateState(priorState === 'error' ? 'idle' : priorState); }, 1700);
}
function normalizeToolName(name) { return String(name || '').replaceAll('_', ':'); }
function toolCard(tool) {
  const name = normalizeToolName(tool);
  if (name.startsWith('vision:') || name === 'visual:snapshot') return document.getElementById('canvasCard');
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
    'visual:snapshot':'Looking',
    'forge:enqueue':'Dispatching Forge task',
    'forge:status':'Checking Forge job',
    'forge:list':'Listing Forge tasks',
    'github:issues':'Checking GitHub repo',
    'web:read':'Reading web page',
    'web:speak':'Reading web page aloud',
  };
  document.getElementById('activityText').textContent = activeTools.size ? labels[[...activeTools.keys()].at(-1)] || 'Using a local tool' : stateLabels[orbState] || '';
}
function sendAction(action, fields = {}) {
  if (!ws || ws.readyState !== WebSocket.OPEN) { showError('Connection unavailable. Wait for reconnection.'); return false; }
  if (['text', 'set_live_context', 'toggle_mute', 'reset', 'interrupt'].includes(action)) {
    discardListeningCapture();
    liveContextReplyPending = liveContextEnabled && action === 'text';
  }
  ws.send(JSON.stringify({ action, ...fields })); return true;
}
function connectWs() {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  ws = new WebSocket(`${proto}//${location.host}/ws/voice`);
  const generation = ++wsGeneration;
  ws.onopen = () => {
    ambient({ type:'connect', socket:generation });
    setConnection('Connected · local session', true);
    micBtn.disabled = false; document.getElementById('liveContextBtn').disabled = false;
    if (orbState === 'disconnected') updateState('idle');
    sendAction('forge:list');
  };
  ws.onerror = () => showError('Voice connection failed. Check the local server and HTTPS certificate.');
  ws.onclose = () => {
    if (generation !== wsGeneration) return;
    ambient({ type:'disconnect', socket:generation });
    liveContextReplyPending = false; liveContextEnabled = false; renderLiveContext(); document.getElementById('liveContextBtn').disabled = true;
    learningSession = null; learningEnabled = true; renderLearning();
    pendingApprovalActions.clear(); lastToolCalls.clear();
    cancelVisualCapture(); captureHeld = false; turnInProgress = false; stopMic(); stopPlayback(); for (const tool of activeTools.keys()) updateToolActivity(tool, false); setConnection('Disconnected · reconnecting…', false); updateState('disconnected'); micBtn.disabled = true;
    setTimeout(connectWs, 2000);
  };
  ws.onmessage = (event) => {
    let data;
    if (generation !== wsGeneration) return;
    try { data = JSON.parse(event.data); } catch { showError('Invalid server message'); return; }
    wire = { socket:generation, epoch:Number.isFinite(data.epoch) ? data.epoch : undefined };
    try { handleVoiceMessage(data); } finally { wire = null; }
  };
}
function handleVoiceMessage(data) {
    switch (data.type) {
      case 'snapshot_request':
        requestVisualCapture(data); break;
      case 'session':
        learningSession = data.session_id; learningEnabled = data.learning !== false; renderLearning();
        ambient({ type:'snapshot', session:data.session_id, epoch:0, listenOnly:false });
        cancelVisualCapture();
        document.getElementById('conversation').replaceChildren(); responseNode = null; chatFollow.resume();
        applyControl(data.control); announceClientSpeech(); break;
      case 'live_context_state':
        discardListeningCapture(); liveContextReplyPending = false;
        liveContextEnabled = data.enabled === true; renderLiveContext(); ambient({ type:'listen_only', enabled:liveContextEnabled }); break;
      case 'live_context_observed':
        transcriptBox.textContent = `Context · ${data.text}`; break;
      case 'learning_state':
        learningEnabled = data.enabled !== false; renderLearning(); break;
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
        stopMic(); setCaptureHeld(true); updateState('holding'); transcriptBox.textContent = data.message; ambient({ type:'warning', message:data.message }); break;
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
        setMessageText(responseNode, responseText);
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
      case 'speech_text': queueClientSpeech(data); break;
      case 'client_speech': clientSpeechActive = { stt:data.stt === true, tts:data.tts === true }; renderSpeechPlacement(); break;
      case 'turn_finished':
        liveContextReplyPending = false;
        setCaptureHeld(false);
        turnInProgress = false;
        document.getElementById('assistantAnnounce').textContent = responseNode?.lastChild?.innerText || responseText;
        turnFinished = true;
        if (!pendingAudio && !playingSources.size) finishPlayback();
        if (data.latency_ms) setConnection(`Connected · local turn ${(data.latency_ms / 1000).toFixed(1)}s`, true);
        updateGenMetrics(data);
        updateState('idle');
        break;
      case 'interruption':
        if (data.state === 'interrupted') {
          for (const tool of activeTools.keys()) updateToolActivity(tool, false);
        }
        cancelVisualCapture(); setCaptureHeld(false); turnInProgress = false; stopPlayback(); updateState(data.state || 'interrupted'); break;
      case 'control': applyControl(data.control); break;
      case 'tool_event': {
        const event = data.event;
        if (event.approval_session) {
          pendingApprovalActions.set(event.approval_session, { automatic:true });
          ambient({ type:'approval', session:event.approval_session });
          renderApprovalCard({session_id:event.approval_session,tool:event.tool,rationale:'Review the exact operation with your owner credentials before continuing.'});
          updateState('approval_required');
          transcriptBox.textContent = 'Approval needed · Review and approve on Canvas or Settings.';
        }
        updateToolActivity(event.tool, !['completed','failed','cancelled'].includes(event.state));
        ambient({ type:'tool', tool:normalizeToolName(event.tool), state:event.state });
        if (event.state === 'executing') updateState('working');
        if (['completed','failed','cancelled'].includes(event.state)) updateState(turnInProgress ? 'thinking' : 'idle');
        break;
      }
      case 'tool_planned': updateToolActivity(data.tool, true); break;
      case 'ack_started':
        if (!responseNode) responseNode = appendConversation('SAIL', '');
        if (data.text) {
          responseText += data.text + ' ';
          setMessageText(responseNode, responseText);
        }
        updateState('speaking');
        break;
      case 'tool_started': updateToolActivity(data.tool, true); break;
      case 'tool_completed': updateToolActivity(data.tool, false); break;
      case 'tool_result': updateToolActivity(data.result?.tool, false); renderToolResult(data.result); logToolActivity(data.result); break;
      case 'canvas': renderArtifact(data.artifact); break;
      case 'plan': window.renderPlanCard?.(data.plan); break;
      case 'approval_required':
        ambient({ type:'approval', session:data.session_id });
        renderApprovalCard(data);
        updateState('approval_required');
        break;
      case 'approval_resolved':
        ambient({ type:'approval', session:data.session_id, pending:false });
        handleApprovalResolved(data);
        break;
      case 'forge_event':
      case 'forge_enqueued':
        handleForgeEvent(data);
        break;
      case 'forge_status_response':
        handleForgeEvent(data);
        handleForgeLogResponse(data);
        break;
      case 'forge_jobs':
        renderForgeJobList(data.jobs);
        break;
      case 'forge_cancelled':
        handleForgeCancelled(data);
        break;
      case 'forge_cleared':
        handleForgeCleared(data);
        break;
      case 'safety_review_completed':
        handleSafetyReviewCompleted(data);
        break;
      case 'error':
        liveContextReplyPending = false; showError(data.message); break;
    }
}
function applyControl(control) {
  if (control.is_muted !== isMuted) discardListeningCapture();
  isMuted = control.is_muted;
  ambient({ type:'capture', active:isRecording && !isMuted });
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
  SailClientSpeech.configure({ mode:settings.mode, pauseMs:settings.pause_ms, maxRecordingMs });
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
  const content = document.createElement('div'); content.className = 'message-body';
  node.append(label, content); setMessageText(node, text);
  if (role === 'You') chatFollow.resume();
  document.getElementById('conversation').append(node); return node;
}
// Keeps the conversation pinned to the newest text while replies stream in, until the
// owner scrolls away from the bottom; scrolling back to the bottom resumes following.
const chatFollow = (() => {
  const box = document.getElementById('conversation');
  let follow = true, queued = false, programScroll = false;
  const atBottom = () => box.scrollHeight - box.scrollTop - box.clientHeight <= 4;
  const stick = () => {
    queued = false;
    // Hidden (e.g. behind another tab in the Dialogue column): nothing to scroll yet.
    if (follow && box.clientHeight) {
      programScroll = true;
      box.scrollTop = box.scrollHeight;
    }
  };
  const schedule = () => { if (follow && !queued) { queued = true; requestAnimationFrame(stick); } };
  // Upward intent stops following at once, before a pending stick can pull the view back.
  const leave = () => { if (box.scrollHeight > box.clientHeight) follow = false; };
  box.addEventListener('wheel', (e) => { if (e.deltaY < 0) leave(); }, { passive: true });
  box.addEventListener('keydown', (e) => { if (['ArrowUp', 'PageUp', 'Home'].includes(e.key)) leave(); });
  let touchY = null;
  box.addEventListener('touchstart', (e) => { touchY = e.touches[0]?.clientY ?? null; }, { passive: true });
  box.addEventListener('touchmove', (e) => { const y = e.touches[0]?.clientY; if (touchY !== null && y > touchY) leave(); touchY = y; }, { passive: true });
  // Scrollbar drags and everything else: follow exactly when the view is at the bottom.
  box.addEventListener('scroll', () => {
    if (programScroll) { programScroll = false; return; }
    if (box.clientHeight) follow = atBottom();
  }, { passive: true });
  // Streamed tokens, Markdown/diagram renders and cards all grow the log.
  new MutationObserver(schedule).observe(box, { childList: true, subtree: true, characterData: true });
  new ResizeObserver(schedule).observe(box);
  return { resume() { follow = true; schedule(); } };
})();
// Assistant replies are Markdown; user turns (mostly speech transcripts) stay plain text.
function setMessageText(node, text) {
  const content = node.lastChild;
  if (node.classList.contains('message-sail') && window.SailMarkdown) {
    content.classList.add('markdown'); window.SailMarkdown.renderInto(content, text);
  } else content.textContent = text;
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
  const output = result?.output ?? {}; const succeeded = Boolean(result?.success);
  if (result?.tool === 'os:status') {
    if (succeeded && output) updateSystemCard(output);
    return;
  }
  const container = document.createElement('article'); container.className = 'artifact-card';

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
  else if ((result?.tool === 'web:speak' || result?.tool === 'web_speak') && output?.url) summary.textContent = `Streamed ${output.paragraphs?.length || 0} paragraphs from ${output.title || output.url} directly to voice`;
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
    const s = data.telemetry.spectrum;
    schedulePlayback(context, buffer, generation, { rms:data.telemetry.rms_level, phase:data.telemetry.wave_phase, spectrum:[s.sub_bass,s.bass,s.low_mid,s.mid,s.high_mid,s.presence,s.brilliance,s.air] });
    pendingAudio--;
  }).catch(error => { if (generation === playbackGeneration) { pendingAudio = Math.max(0, pendingAudio - 1); showError(`Audio playback: ${error.message}`); } });
}
// Play `buffer` after everything already scheduled. The Orb follows audio that is
// actually playing, not packets received early.
function schedulePlayback(context, buffer, generation, telemetry) {
  const source = context.createBufferSource(); source.buffer = buffer; source.connect(context.destination);
  const when = nextAudioTime > context.currentTime ? nextAudioTime : context.currentTime + 0.06; nextAudioTime = when + buffer.duration;
  playingSources.add(source);
  const timer = setTimeout(() => {
    playbackTimers.delete(timer);
    if (generation !== playbackGeneration) return;
    updateState('speaking'); currentAmplitude = telemetry.rms; wavePhase = telemetry.phase; spectrum8 = telemetry.spectrum;
  }, Math.max(0, (when - context.currentTime) * 1000));
  playbackTimers.add(timer);
  source.onended = () => { clearTimeout(timer); playbackTimers.delete(timer); source.disconnect(); playingSources.delete(source); if (turnFinished && !pendingAudio && !playingSources.size) finishPlayback(); };
  source.start(when);
}

// ---- Speech on this device (client-speech.js) ----
let clientSpeechActive = { stt:false, tts:false };
let clientFrames = Promise.resolve(), clientCapturing = false, clientCaptureSerial = 0, clientFrameGeneration = 0;
const selectedVoice = () => document.getElementById('voiceSelect')?.value || 'af_heart';
// Tell the server what this browser runs. TTS only for voices the browser engine has
// (kokoro-js speaks English); other voices keep host synthesis.
function announceClientSpeech() {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  const engine = SailClientSpeech.engine;
  sendAction('client_speech', { stt:SailClientSpeech.sttOnDevice(), tts:SailClientSpeech.ttsOnDevice(selectedVoice()), engine:{ device:engine.device, measured:engine.measured } });
}
// Loading bar beside "Mic on": real download progress, then an indeterminate bar while
// the device times itself; hidden once speech has settled on the device or the host.
function renderSpeechLoad() {
  const indicator = document.getElementById('speechLoadIndicator'); if (!indicator) return;
  const loading = SailClientSpeech.engine.loading;
  indicator.hidden = !loading; if (!loading) return;
  const bar = document.getElementById('speechLoadBar'), value = document.getElementById('speechLoadValue');
  const where = loading.device === 'webgpu' ? ' · WebGPU' : loading.device === 'wasm' ? ' · CPU' : '';
  if (loading.phase === 'measuring') {
    document.getElementById('speechLoadLabel').textContent = `Checking speed${where}`;
    bar.removeAttribute('value'); value.textContent = '';
    return;
  }
  document.getElementById('speechLoadLabel').textContent = `Loading voice models${where}`;
  if (loading.total > 0) {
    const percent = Math.min(100, Math.floor(loading.loaded * 100 / loading.total));
    bar.value = percent; value.textContent = `${percent}%`;
  } else { bar.removeAttribute('value'); value.textContent = ''; }
}
// Loading bar for the resident model (Qwen): how much of its weights is in memory, then an
// indeterminate bar while it initializes; hidden once it is ready. Polled quickly only
// while a model is starting.
async function refreshModelLoad() {
  let starting = false;
  try {
    const response = await fetch('/api/mind/status', { cache:'no-store' });
    if (response.ok) starting = renderModelLoad(await response.json());
  } catch (_) {}
  setTimeout(refreshModelLoad, starting ? 1000 : 5000);
}
function renderModelLoad(status) {
  const indicator = document.getElementById('modelLoadIndicator'); if (!indicator) return false;
  const model = (status.models || []).find(m => m.state === 'starting');
  indicator.hidden = !model; if (!model) return false;
  const bar = document.getElementById('modelLoadBar'), value = document.getElementById('modelLoadValue');
  const label = document.getElementById('modelLoadLabel');
  const load = model.load;
  // The last few percent of a GGUF may never be read; past 95% the wait is initialization.
  const percent = load && load.total_bytes > 0 ? Math.floor(load.loaded_bytes * 100 / load.total_bytes) : 100;
  if (percent < 95) {
    label.textContent = `Loading ${model.model}`;
    bar.value = percent; value.textContent = `${percent}%`;
  } else {
    // Weights are in memory (or this runtime is not the one loading): still initializing.
    label.textContent = `Starting ${model.model}`;
    bar.removeAttribute('value'); value.textContent = '';
  }
  return true;
}
refreshModelLoad();
function renderSpeechPlacement() {
  renderSpeechLoad();
  const status = document.getElementById('speechPlacementStatus'); if (!status) return;
  const engine = SailClientSpeech.engine;
  const parts = [engine.detail];
  if (engine.status === 'ready' && engine.tts && !clientSpeechActive.tts) parts.push('This voice is synthesized on the SAIL host.');
  status.textContent = parts.join(' ');
  document.getElementById('speechPlacementSelect').value = SailClientSpeech.preferred() ? 'device' : 'host';
}
async function loadClientSpeech() {
  await SailClientSpeech.load(renderSpeechPlacement);
  announceClientSpeech(); renderSpeechPlacement();
}
async function handleClientFrame(frame, generation = clientFrameGeneration) {
  if (generation !== clientFrameGeneration) return;
  if (!clientSpeechActive.stt || !isRecording || isMuted || listeningReplyBusy()) return;
  const serial = clientCaptureSerial;
  let result;
  try { result = await SailClientSpeech.frame(frame); }
  catch (error) { failClientSpeech(error); return; }
  if (serial !== clientCaptureSerial || !isRecording || isMuted || listeningReplyBusy()) return;
  let rms = 0; for (const v of frame) rms += (v / 32768) ** 2; rms = Math.sqrt(rms / frame.length);
  if (!playingSources.size) {
    currentAmplitude = rms; wavePhase = (wavePhase + 0.12) % (Math.PI * 2); spectrum8 = Array(8).fill(Math.min(1, rms * 4));
    if (!captureHeld && !turnInProgress && pendingAudio === 0) updateState(clientCapturing ? 'listening' : (isRecording ? 'listening' : 'idle'));
  }
  document.getElementById('sessionConfidence').textContent = `${(result.probability * 100).toFixed(0)}%`;
  document.getElementById('sessionRms').textContent = rms.toFixed(3);
  document.getElementById('sessionDuration').textContent = `${formatCaptureTime(result.capturedMs)} / ${formatCaptureTime(maxRecordingMs)}`;
  document.getElementById('telVad').textContent = `VAD inference on this device: ${result.inferenceMs.toFixed(2)}ms`;
  if (result.event === 'start') {
    // Barge-in, as the host VAD does: stop playback and let the server preempt background work.
    clientCapturing = true; clientCaptureSerial++; stopPlayback(); sendAction('speech_start', { capture_id:clientCaptureSerial });
  } else if (result.event === 'end') {
    // Not awaited: the VAD keeps running during transcription so barge-in still works.
    void commitClientCapture();
  } else if (result.event === 'limit') {
    stopMic(false); setCaptureHeld(true); updateState('holding');
    transcriptBox.textContent = 'Recording limit reached. Your audio is held; send the turn or start a new conversation to discard it.';
  }
}
// Transcribe the captured turn here and send the text. A newer speech onset supersedes it,
// as barge-in cancels a turn on the host.
async function commitClientCapture() {
  const samples = SailClientSpeech.takeCapture(); const serial = clientCaptureSerial; clientCapturing = false;
  if (!samples) { sendAction('speech_cancel'); return; }
  setCaptureHeld(false); updateState('transcribing');
  try {
    const { text } = await SailClientSpeech.transcribe(samples);
    if (serial !== clientCaptureSerial || listeningReplyBusy()) return;
    if (text) sendAction('voice_text', { text, capture_id:serial }); else { sendAction('speech_cancel'); updateState(isRecording ? 'listening' : 'idle'); }
  } catch (error) { if (serial === clientCaptureSerial) { sendAction('speech_cancel'); failClientSpeech(error); } }
}
// Synthesize a reply fragment here and queue it behind earlier audio, in order.
function queueClientSpeech(data) {
  const generation = playbackGeneration; pendingAudio++;
  playbackQueue = playbackQueue.then(async () => {
    if (generation !== playbackGeneration) return;
    const [context, speech] = await Promise.all([ensureAudio(), SailClientSpeech.speak(data.text, selectedVoice())]);
    if (generation !== playbackGeneration) return;
    const pause = Math.round(speech.sampling_rate * (data.pause_ms || 0) / 1000);
    const pcm = new Float32Array(speech.audio.length + pause); pcm.set(speech.audio);
    // 100 ms buffers, so the Orb follows the loudness of what is playing.
    const step = Math.round(speech.sampling_rate / 10);
    for (let offset = 0; offset < pcm.length; offset += step) {
      const part = pcm.subarray(offset, Math.min(pcm.length, offset + step));
      const buffer = context.createBuffer(1, part.length, speech.sampling_rate); buffer.copyToChannel(part, 0);
      let rms = 0; for (const v of part) rms += v * v; rms = Math.sqrt(rms / part.length);
      schedulePlayback(context, buffer, generation, { rms, phase:(offset / step) * 0.4 % (Math.PI * 2), spectrum:Array(8).fill(Math.min(1, rms * 4)) });
    }
    pendingAudio--;
  }).catch(error => { if (generation === playbackGeneration) { pendingAudio = Math.max(0, pendingAudio - 1); failClientSpeech(error); } });
}
// Hand speech back to the host workers and say so; never fail silently.
function failClientSpeech(error) {
  SailClientSpeech.stop(`On-device speech stopped (${error.message}); the SAIL host handles speech.`);
  clientCapturing = false; announceClientSpeech(); renderSpeechPlacement();
  showError('On-device speech stopped; the SAIL host handles speech now.');
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
  statusBadge.textContent = labels[state] || 'Ready';
  const shell = document.getElementById('appShell');
  // #appShell[data-state] is the fine voice phase (Orb, wave); <html data-sail-state> and
  // the light come from the ambient contract.
  if (shell) shell.dataset.state = state;
  ambient({ type:'phase', phase:state });
  document.getElementById('sessionState').textContent = detailedLabels[state] || labels[state] || 'Ready';
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
    if (clientSpeechActive.stt) {
      await clientFrames;
      if (stopAfter) stopMic(false);
      await commitClientCapture();
      return;
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
      if (clientSpeechActive.stt) {
        if (isRecording && !isMuted && !listeningReplyBusy()) { const frame = new Int16Array(data); const generation = clientFrameGeneration; clientFrames = clientFrames.then(() => handleClientFrame(frame, generation)); }
        return;
      }
      if (isRecording && !isMuted && !listeningReplyBusy() && ws?.readyState === WebSocket.OPEN) {
        if (ws.bufferedAmount > 64000) { showError('Audio connection is too slow; microphone stopped.'); stopMic(); return; }
        ws.send(data);
      }
    };
    micSource.connect(processor); processor.connect(audioCtx.destination);
    isRecording = true; micBtn.setAttribute('aria-label', 'Stop microphone'); micBtn.title = 'Stop microphone'; micBtn.classList.add('active'); document.getElementById('micStatus').textContent = 'On'; document.getElementById('sessionMic').textContent = isMuted ? 'Muted' : 'On'; document.getElementById('micLiveIndicator').hidden = false; document.getElementById('muteDockBtn').hidden = false; document.getElementById('endTurnBtn').hidden = false;
    ambient({ type:'mic_denied', denied:false }); ambient({ type:'capture', active:!isMuted }); updateState('listening');
  } catch (error) { stopMic(); if (error.name === 'NotAllowedError') ambient({ type:'mic_denied' }); showError(error.name === 'NotAllowedError' ? 'Microphone permission denied. Allow microphone access in browser settings.' : `Microphone: ${error.message}`); }
  finally { micBtn.disabled = false; }
}
function stopMic(discardClientCapture = true) {
  if (pendingCaptureFlush) { const pending = pendingCaptureFlush; pendingCaptureFlush = null; pending.reject(); }
  if (discardClientCapture) { discardListeningCapture(); sendAction('speech_cancel'); }
  isRecording = false; ambient({ type:'capture', active:false }); micStream?.getTracks().forEach(track => track.stop());
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


// Connect once every deferred script has run, so incoming messages always find their
// handlers (forge.js, approvals.js and the rest load after this file).
document.addEventListener('DOMContentLoaded', () => { connectWs(); loadClientSpeech(); }, { once: true });
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
// Consented capture stays visible whatever the shell colour is (DESIGN.md §3).
window.SailAmbient?.subscribe(view => {
  const rec = document.getElementById('recIndicator');
  if (rec) rec.hidden = !view.indicators.recording;
});
window.sailUi = Object.freeze({
  setOrbTelemetry: data => window.dispatchEvent(new CustomEvent('sail:orb-telemetry', { detail:data })),
  resumeOrbAnimation: () => { orbFrozen = false; },
});


function listeningReplyBusy() {
  return liveContextEnabled && (liveContextReplyPending || pendingAudio > 0 || playingSources.size > 0);
}
function discardListeningCapture() {
  if (typeof transcriptBox !== 'undefined' && transcriptBox.textContent.startsWith('Context ·')) transcriptBox.textContent = '';
  clientCaptureSerial++; clientFrameGeneration++; clientCapturing = false;
  if (typeof SailClientSpeech !== 'undefined') SailClientSpeech.resetCapture();
}
function renderLiveContext() {
  const button = document.getElementById('liveContextBtn');
  button.setAttribute('aria-pressed', String(liveContextEnabled));
  button.textContent = liveContextEnabled ? 'Disable and clear context' : 'Enable listening context';
  document.getElementById('liveContextStatus').textContent = liveContextEnabled
    ? 'Listening context enabled · start microphone to capture; type to ask'
    : 'Listening context off';
}
document.getElementById('liveContextBtn').addEventListener('click', async () => {
  const button = document.getElementById('liveContextBtn');
  button.disabled = true;
  const connection = ws;
  try {
    const owner_token = liveContextEnabled ? undefined : await getOrFetchOwnerToken();
    if (ws !== connection || ws.readyState !== WebSocket.OPEN) return;
    stopPlayback();
    sendAction('set_live_context', {enabled: !liveContextEnabled, owner_token});
  } catch (error) { showError(error.message); }
  finally { button.disabled = !ws || ws.readyState !== WebSocket.OPEN; }
});

function renderLearning() {
  const indicator = document.getElementById('learningIndicator');
  indicator.hidden = !learningSession || learningEnabled === false;
  indicator.textContent = learningEnabled === null ? 'Learning state unknown' : 'Learning enabled';
  const button = document.getElementById('learningBtn');
  button.disabled = !learningSession || learningBusy;
  button.setAttribute('aria-pressed', String(learningEnabled));
  button.textContent = learningEnabled !== false ? 'Disable learning' : 'Enable learning';
  document.getElementById('learningStatus').textContent = !learningSession ? 'Disconnected · learning off' : learningEnabled === null ? 'State unavailable · disable learning or reconnect' : learningEnabled ? 'Learning enabled for this conversation' : 'Learning off';
}
document.getElementById('learningBtn').addEventListener('click', async () => {
  if (!learningSession || learningBusy) return;
  const session = learningSession;
  learningBusy = true; renderLearning();
  try {
    const token = await getOrFetchOwnerToken();
    const response = await fetch('/api/memory/learning', {method:'POST', cache:'no-store', headers:{'Content-Type':'application/json', Authorization:`Bearer ${token}`}, body:JSON.stringify({session, enabled:learningEnabled === false})});
    if (!response.ok) throw new Error('Learning change failed. Check owner authorization and retry.');
    const result = await response.json();
    if (session === learningSession) learningEnabled = result.enabled === true;
  } catch (error) { if (session === learningSession) learningEnabled = null; showError(error.message); }
  finally { learningBusy = false; renderLearning(); }
});

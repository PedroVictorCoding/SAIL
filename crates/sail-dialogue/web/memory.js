'use strict';
(() => {
  const status = document.getElementById('memoryStatus');
  const entities = document.getElementById('memoryEntities');
  const detail = document.getElementById('memoryDetail');
  let generation = 0, selection = null, serial = 0, busy = false;
  const node = (tag, text, cls) => {const el=document.createElement(tag); if(text!==undefined) el.textContent=text; if(cls) el.className=cls; return el;};
  function button(label, action) {const b=node('button',label,'quiet-button'); b.type='button'; b.addEventListener('click',()=>Promise.resolve(action()).catch(fail)); return b;}
  function fail(error) {status.textContent=error.message || String(error);}
  async function request(path, body) {
    const token=await getOrFetchOwnerToken();
    const r=await fetch(`/api/memory/${path}`,{method:'POST',cache:'no-store',headers:{'Content-Type':'application/json',Authorization:`Bearer ${token}`},body:JSON.stringify(body)});
    if(!r.ok) {let message;try {message=(await r.json()).error;} catch(_) {} throw new Error(message || (r.status===401?'Owner authentication required.':'Memory is unavailable.'));}
    return r.json();
  }
  async function change(action) {
    if(busy) return;
    busy=true;
    const controls=[...document.querySelectorAll('#memoryPanelMain button')];controls.forEach(b=>b.disabled=true);
    try {await request('change',{generation,action}); await load(); if(selection) await showEntity(selection);}
    finally {busy=false;controls.forEach(b=>b.disabled=false);}
  }
  function choose(entity,pinned=false) {const b=button(`${pinned?'Pinned · ':''}${entity.name} · ${entity.kind} #${entity.id}`,()=>showEntity(entity.id));b.classList.add('memory-entity');return b;}
  async function load() {
    const ticket=++serial;status.textContent='Loading memory…';
    const data=await request('read',{action:'overview'});if(ticket!==serial)return;
    generation=data.generation;entities.replaceChildren();
    for(const [entity,pinned] of data.entities||[]) entities.append(choose(entity,pinned));
    if(!entities.childElementCount) entities.append(node('p','No knowledge has been consolidated yet.'));
    const history=document.getElementById('memoryHistory');history.replaceChildren();
    for(const op of data.history||[]) history.append(node('li',`${new Date(op.at).toLocaleString()} · ${op.op} · ${op.target_kind} #${op.target_id ?? '—'}${op.reason?' · '+op.reason:''}`));
    status.textContent=`${(data.entities||[]).length} entities shown (up to 500).`;
  }
  async function showEntity(id) {
    const ticket=++serial;const data=await request('read',{action:'entity',id});if(ticket!==serial)return;
    selection=id;generation=data.generation;detail.replaceChildren();
    const e=data.entity;
    detail.append(node('h3',`${e.name} #${e.id}`),node('p',`${e.kind} · ${e.scope} · ${e.sensitivity}`));
    if(e.aliases.length) detail.append(node('p',`Also known as: ${e.aliases.join(', ')}`));
    const actions=node('div',undefined,'memory-actions');
    actions.append(button('Pin',()=>change({action:'pin',entity:id,pinned:true})),button('Unpin',()=>change({action:'pin',entity:id,pinned:false})),button('Mark private',()=>change({action:'private',entity:id})),button('Forget entity',()=>{if(confirm(`Forget ${e.name} and its claims?`)){selection=null;detail.replaceChildren();return change({action:'forget_entity',entity:id});}}));
    if(e.merged_into) actions.append(button('Separate',()=>change({action:'separate',entity:id})));
    detail.append(actions);
    const merge=node('form');const label=node('label','Merge into entity ID');const target=node('input');target.type='number';target.min='1';target.required=true;label.append(target);const submit=node('button','Merge','quiet-button');submit.type='submit';merge.append(label,submit);
    merge.addEventListener('submit',event=>{event.preventDefault();if(confirm(`Merge ${e.name} into #${target.value}?`))change({action:'merge',from:id,into:Number(target.value)}).catch(fail);});detail.append(merge);
    for(const claim of data.claims||[]) {
      const row=node('section',undefined,'memory-claim');const value=claim.object.type==='value'?claim.object.value:`Entity #${claim.object.value}`;
      row.append(node('h4',`${claim.predicate}: ${value}`),node('p',`${claim.status} · observed ${new Date(claim.observed_at).toLocaleString()}`));
      const why=node('details');why.append(node('summary','Why remembered'));
      const evidence=(data.why||[]).find(w=>w.claim_id===claim.id);
      for(const link of evidence?.evidence||[]) why.append(node('p',`${link.stance} · ${link.evidence.source_kind} · ${link.evidence.source_ref || 'source receipt'}: ${link.evidence.receipt || 'No excerpt retained'}`));
      row.append(why);
      if(!['superseded','invalidated'].includes(claim.status)) {
        const form=node('form');const label=node('label','Correct this value');const input=node('input');input.value=value;input.required=true;input.maxLength=2000;label.append(input);const save=node('button','Save correction','quiet-button');save.type='submit';form.append(label,save);
        form.addEventListener('submit',event=>{event.preventDefault();change({action:'correct',claim:claim.id,value:input.value}).catch(fail);});row.append(form);
      }
      row.append(button('Forget claim',()=>{if(confirm('Forget this claim and its unused evidence?'))return change({action:'forget_claim',claim:claim.id});}));detail.append(row);
    }
    const graph=node('details');graph.open=true;graph.append(node('summary','Relationships'));
    const edges=data.graph||[];
    if(!edges.length) graph.append(node('p','No relationships recorded.'));
    else {
      const ns='http://www.w3.org/2000/svg';const svg=document.createElementNS(ns,'svg');svg.setAttribute('viewBox','0 0 560 320');svg.setAttribute('role','img');svg.setAttribute('aria-label',`Relationships of ${e.name}; full details below`);svg.classList.add('memory-graph');
      const label=(x,y,text)=>{const t=document.createElementNS(ns,'text');t.setAttribute('x',x);t.setAttribute('y',y);t.textContent=text;svg.append(t);};label(230,160,e.name.slice(0,22));
      edges.slice(0,8).forEach((edge,i)=>{const angle=i*Math.PI/4;const x=280+200*Math.cos(angle),y=160+125*Math.sin(angle);const line=document.createElementNS(ns,'line');for(const [k,v] of Object.entries({x1:280,y1:160,x2:x,y2:y}))line.setAttribute(k,v);svg.prepend(line);label(x-35,y,edge.other.name.slice(0,16));});graph.append(svg);
      for(const edge of edges) graph.append(button(`${edge.outgoing?'→':'←'} ${edge.relation}: ${edge.other.name} · ${edge.status}`,()=>showEntity(edge.other.id)));
    }
    detail.append(graph);
    for(const [name,records] of [['Procedures',data.procedures],['Project items',data.project_items]]) {const section=node('details');section.append(node('summary',name),node('pre',JSON.stringify(records||[],null,2)));detail.append(section);}
  }
  document.getElementById('memoryRefresh').addEventListener('click',()=>load().catch(fail));
  document.getElementById('memoryTab')?.addEventListener('click',()=>load().catch(fail));
  document.getElementById('memorySearch').addEventListener('submit',async event=>{
    event.preventDefault();const ticket=++serial;
    try {const data=await request('read',{action:'search',query:{version:1,text:document.getElementById('memoryQuery').value,limit:100,token_budget:0,expand_graph:true}});if(ticket!==serial)return;generation=data.generation;entities.replaceChildren();for(const hit of data.hits||[])entities.append(button(`${hit.entity_name}: ${hit.predicate} ${hit.value} · ${hit.status}`,()=>showEntity(hit.entity_id)));status.textContent=`${(data.hits||[]).length} matching claims.`;} catch(error){fail(error);}
  });
  document.getElementById('memoryExport').addEventListener('click',async()=>{try {const data=await request('read',{action:'export'});const url=URL.createObjectURL(new Blob([JSON.stringify(data,null,2)],{type:'application/json'}));const a=node('a');a.href=url;a.download='sail-knowledge.json';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}catch(error){fail(error);}});
  window.addEventListener('sail:page',event=>{if(event.detail==='memory')load().catch(fail);});

  // Live Transcript Mode (Listen without reply)
  const STORAGE_KEY = 'sail.liveTranscript.v1';
  const ltCard = document.getElementById('ltCard');
  const ltTitleInput = document.getElementById('ltTitleInput');
  const ltStatusPill = document.getElementById('ltStatusPill');
  const ltStatusText = document.getElementById('ltStatusText');
  const ltTabDot = document.getElementById('transcriptTabDot');
  const ltMicOrb = document.getElementById('ltMicOrb');
  const ltSubStatus = document.getElementById('ltSubStatus');
  const ltTimer = document.getElementById('ltTimer');
  const ltPauseBtn = document.getElementById('ltPauseBtn');
  const ltPauseIcon = document.getElementById('ltPauseIcon');
  const ltPauseLabel = document.getElementById('ltPauseLabel');
  const ltStopBtn = document.getElementById('ltStopBtn');
  const ltEnableBtn = document.getElementById('ltEnableBtn');
  const ltCopyBtn = document.getElementById('ltCopyBtn');
  const ltExportBtn = document.getElementById('ltExportBtn');
  const ltClearBtn = document.getElementById('ltClearBtn');
  const ltStream = document.getElementById('ltStream');
  const ltSegments = document.getElementById('ltSegments');
  const ltPartial = document.getElementById('ltPartial');
  const ltEmpty = document.getElementById('ltEmpty');
  const ltAnswer = document.getElementById('ltAnswer');
  const ltAnswerQuestion = document.getElementById('ltAnswerQuestion');
  const ltAnswerBody = document.getElementById('ltAnswerBody');
  const ltAnswerClose = document.getElementById('ltAnswerClose');
  const ltAskForm = document.getElementById('ltAskForm');
  const ltAskInput = document.getElementById('ltAskInput');

  let ltEntries = [];
  let ltBaseElapsedMs = 0;
  let ltRunStartedAt = null;
  let ltHasRecorded = false;
  let ltTimerId = null;
  let ltPendingStartMic = false;
  let ltPendingQuestion = null;
  let ltWaitingForAnswer = false;

  function formatDuration(totalSeconds) {
    const sec = Math.max(0, Math.floor(totalSeconds));
    const hrs = Math.floor(sec / 3600);
    const mins = Math.floor((sec % 3600) / 60);
    const rem = sec % 60;
    const mm = String(mins).padStart(2, '0');
    const ss = String(rem).padStart(2, '0');
    return hrs > 0 ? `${String(hrs).padStart(2, '0')}:${mm}:${ss}` : `${mm}:${ss}`;
  }

  function currentElapsedMs() {
    return ltBaseElapsedMs + (ltRunStartedAt !== null ? Math.max(0, Date.now() - ltRunStartedAt) : 0);
  }

  function currentElapsedSeconds() {
    return Math.floor(currentElapsedMs() / 1000);
  }

  function saveTranscriptState() {
    try {
      if (!ltEntries.length && currentElapsedMs() === 0 && !ltHasRecorded) {
        localStorage.removeItem(STORAGE_KEY);
        return;
      }
      localStorage.setItem(STORAGE_KEY, JSON.stringify({
        version: 1,
        title: (ltTitleInput?.value || 'Live lesson transcript').trim(),
        entries: ltEntries,
        baseElapsedMs: currentElapsedMs(),
        hasRecorded: ltHasRecorded,
      }));
    } catch (_) {}
  }

  function renderSegmentDom(entry) {
    if (!ltSegments) return;
    const article = node('article', undefined, 'lt-segment');
    const timeEl = node('time', entry.elapsed, 'lt-seg-time');
    if (entry.clock) timeEl.title = `Captured at ${entry.clock}`;
    const bodyEl = node('div', undefined, 'lt-seg-body');
    const textEl = node('p', entry.text, 'lt-seg-text');
    bodyEl.append(textEl);
    article.append(timeEl, bodyEl);
    ltSegments.append(article);
  }

  function restoreTranscriptState() {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (!raw) return;
      const parsed = JSON.parse(raw);
      if (!parsed || typeof parsed !== 'object') return;
      if (typeof parsed.title === 'string' && parsed.title.trim() && ltTitleInput) {
        ltTitleInput.value = parsed.title;
      }
      if (Number.isFinite(parsed.baseElapsedMs) && parsed.baseElapsedMs >= 0) {
        ltBaseElapsedMs = parsed.baseElapsedMs;
      }
      ltHasRecorded = Boolean(parsed.hasRecorded);
      if (Array.isArray(parsed.entries) && parsed.entries.length > 0) {
        ltEntries = parsed.entries
          .filter(e => e && typeof e.text === 'string' && e.text.trim())
          .map(e => ({
            elapsed: String(e.elapsed || '00:00'),
            clock: String(e.clock || ''),
            text: e.text.trim(),
          }));
        if (ltSegments) ltSegments.replaceChildren();
        for (const entry of ltEntries) renderSegmentDom(entry);
        if (ltEmpty && ltEntries.length > 0) ltEmpty.hidden = true;
      }
      if (ltTimer) ltTimer.textContent = formatDuration(currentElapsedSeconds());
    } catch (_) {}
  }

  function updateTimerLoop(active) {
    if (active) {
      if (ltRunStartedAt === null) ltRunStartedAt = Date.now();
      ltHasRecorded = true;
      if (!ltTimerId) {
        ltTimerId = setInterval(() => {
          if (ltTimer) ltTimer.textContent = formatDuration(currentElapsedSeconds());
        }, 250);
      }
    } else {
      if (ltRunStartedAt !== null) {
        ltBaseElapsedMs = currentElapsedMs();
        ltRunStartedAt = null;
        saveTranscriptState();
      }
      if (ltTimerId) {
        clearInterval(ltTimerId);
        ltTimerId = null;
      }
    }
    if (ltTimer) ltTimer.textContent = formatDuration(currentElapsedSeconds());
  }

  function syncLiveTranscript() {
    if (!ltCard) return;
    const enabled = typeof liveContextEnabled !== 'undefined' && Boolean(liveContextEnabled);
    const recording = typeof isRecording !== 'undefined' && Boolean(isRecording);
    const muted = typeof isMuted !== 'undefined' && Boolean(isMuted);
    const connected = typeof ws !== 'undefined' && Boolean(ws && ws.readyState === WebSocket.OPEN);
    const activelyCapturing = connected && enabled && recording && !muted;

    updateTimerLoop(activelyCapturing);

    const state = !connected ? 'off' : activelyCapturing ? 'recording' : enabled ? 'ready' : 'off';
    ltCard.dataset.state = state;
    if (ltStatusPill) ltStatusPill.dataset.state = state;
    if (ltTabDot) ltTabDot.hidden = !enabled;

    if (ltStatusText) {
      if (!connected) {
        ltStatusText.textContent = 'Disconnected';
      } else if (activelyCapturing) {
        ltStatusText.textContent = 'Recording';
      } else if (enabled) {
        ltStatusText.textContent = (recording && muted)
          ? 'Muted'
          : ltHasRecorded
            ? 'Paused'
            : 'Ready';
      } else {
        ltStatusText.textContent = (ltHasRecorded && ltEntries.length > 0) ? 'Stopped' : 'Off';
      }
    }

    if (ltMicOrb) {
      ltMicOrb.setAttribute('aria-pressed', String(activelyCapturing));
      ltMicOrb.disabled = !connected;
    }

    if (ltSubStatus) {
      if (!connected) {
        ltSubStatus.textContent = 'Disconnected · waiting for local voice service…';
      } else if (activelyCapturing) {
        ltSubStatus.textContent = 'Listening and transcribing live without replying…';
      } else if (enabled && recording && muted) {
        ltSubStatus.textContent = 'Microphone muted · unmute or press Resume to continue transcribing.';
      } else if (enabled) {
        ltSubStatus.textContent = ltHasRecorded
          ? 'Transcription paused · press Resume or start microphone to continue.'
          : 'Listen without reply enabled · press Record or start microphone to capture.';
      } else {
        ltSubStatus.textContent = 'Enable Listen without reply or press Record to transcribe a lesson live.';
      }
    }

    if (ltPauseIcon) ltPauseIcon.textContent = activelyCapturing ? '⏸' : '▶';
    if (ltPauseLabel) {
      ltPauseLabel.textContent = activelyCapturing
        ? 'Pause'
        : (enabled && ltHasRecorded ? 'Resume' : 'Record');
    }
    if (ltPauseBtn) {
      ltPauseBtn.disabled = !connected;
      ltPauseBtn.setAttribute('aria-pressed', String(activelyCapturing));
    }

    if (ltStopBtn) {
      ltStopBtn.disabled = !enabled && !recording;
    }

    if (ltEnableBtn) {
      ltEnableBtn.disabled = !connected;
      ltEnableBtn.setAttribute('aria-pressed', String(enabled));
      ltEnableBtn.textContent = enabled ? 'Disable Listen without reply' : 'Enable Listen without reply';
    }

    const hasEntries = ltEntries.length > 0;
    if (ltCopyBtn) ltCopyBtn.hidden = !hasEntries;
    if (ltExportBtn) ltExportBtn.hidden = !hasEntries;
    if (ltClearBtn) ltClearBtn.hidden = !hasEntries && currentElapsedSeconds() === 0;
  }

  function appendObservedSegment(rawText) {
    const text = String(rawText || '').trim();
    if (!text || !ltSegments) return;
    if (ltPartial) {
      ltPartial.textContent = '';
      ltPartial.hidden = true;
    }
    if (ltEmpty) ltEmpty.hidden = true;

    const now = new Date();
    const clock = now.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false });
    const elapsedSec = currentElapsedSeconds();
    const elapsed = (ltRunStartedAt !== null || elapsedSec > 0)
      ? formatDuration(elapsedSec)
      : clock;
    const entry = { elapsed, clock, text };
    ltEntries.push(entry);
    renderSegmentDom(entry);
    saveTranscriptState();

    syncLiveTranscript();
    if (ltStream) ltStream.scrollTop = ltStream.scrollHeight;
  }

  function updatePartialSegment(rawText) {
    if (!ltPartial) return;
    const text = String(rawText || '').trim();
    if (!text) {
      ltPartial.textContent = '';
      ltPartial.hidden = true;
      return;
    }
    if (ltEmpty) ltEmpty.hidden = true;
    ltPartial.textContent = text;
    ltPartial.hidden = false;
    if (ltStream) ltStream.scrollTop = ltStream.scrollHeight;
  }

  function clearTranscriptEntries() {
    ltEntries = [];
    ltBaseElapsedMs = 0;
    ltRunStartedAt = null;
    ltHasRecorded = false;
    ltPendingQuestion = null;
    ltWaitingForAnswer = false;
    if (ltTimer) ltTimer.textContent = '00:00';
    if (ltSegments) ltSegments.replaceChildren();
    if (ltPartial) {
      ltPartial.textContent = '';
      ltPartial.hidden = true;
    }
    if (ltAnswer) ltAnswer.hidden = true;
    if (ltEmpty) ltEmpty.hidden = false;
    try { localStorage.removeItem(STORAGE_KEY); } catch (_) {}
    syncLiveTranscript();
  }

  function resetLiveStateOnly() {
    ltPendingStartMic = false;
    ltPendingQuestion = null;
    ltWaitingForAnswer = false;
    if (ltPartial) {
      ltPartial.textContent = '';
      ltPartial.hidden = true;
    }
    syncLiveTranscript();
  }

  function toggleRecordOrPause() {
    const enabled = typeof liveContextEnabled !== 'undefined' && Boolean(liveContextEnabled);
    const recording = typeof isRecording !== 'undefined' && Boolean(isRecording);
    const muted = typeof isMuted !== 'undefined' && Boolean(isMuted);

    if (!enabled) {
      ltPendingStartMic = true;
      if (typeof window.setLiveContext === 'function') window.setLiveContext(true);
      return;
    }
    if (!recording) {
      if (typeof startMic === 'function') startMic();
    } else if (muted) {
      if (typeof toggleMute === 'function') toggleMute();
    } else {
      if (typeof stopMic === 'function') stopMic();
    }
    syncLiveTranscript();
  }

  ltTitleInput?.addEventListener('change', saveTranscriptState);
  ltMicOrb?.addEventListener('click', toggleRecordOrPause);
  ltPauseBtn?.addEventListener('click', toggleRecordOrPause);

  ltStopBtn?.addEventListener('click', () => {
    ltPendingStartMic = false;
    if (typeof isRecording !== 'undefined' && isRecording && typeof stopMic === 'function') {
      stopMic();
    }
    if (typeof liveContextEnabled !== 'undefined' && liveContextEnabled && typeof window.setLiveContext === 'function') {
      window.setLiveContext(false);
    }
    syncLiveTranscript();
  });

  ltEnableBtn?.addEventListener('click', () => {
    ltPendingStartMic = false;
    if (typeof window.setLiveContext === 'function') {
      const enabled = typeof liveContextEnabled !== 'undefined' && Boolean(liveContextEnabled);
      window.setLiveContext(!enabled);
    }
  });

  ltCopyBtn?.addEventListener('click', async () => {
    if (!ltEntries.length) return;
    const title = (ltTitleInput?.value || 'Live lesson transcript').trim();
    const lines = [title, '', ...ltEntries.map(e => `[${e.elapsed}] ${e.text}`)];
    try {
      await navigator.clipboard.writeText(lines.join('\n'));
      const prev = ltCopyBtn.textContent;
      ltCopyBtn.textContent = 'Copied';
      setTimeout(() => { ltCopyBtn.textContent = prev; }, 1400);
    } catch (err) {
      if (typeof showError === 'function') showError(err.message || 'Clipboard copy failed.');
    }
  });

  ltExportBtn?.addEventListener('click', () => {
    if (!ltEntries.length) return;
    const title = (ltTitleInput?.value || 'Live lesson transcript').trim();
    const lines = [title, `Exported: ${new Date().toLocaleString()}`, '', ...ltEntries.map(e => `[${e.elapsed}] ${e.text}`)];
    const url = URL.createObjectURL(new Blob([lines.join('\n') + '\n'], { type: 'text/plain;charset=utf-8' }));
    const a = node('a');
    a.href = url;
    const slug = title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'live-transcript';
    a.download = `${slug}.txt`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });

  ltClearBtn?.addEventListener('click', clearTranscriptEntries);

  ltAnswerClose?.addEventListener('click', () => {
    ltWaitingForAnswer = false;
    if (ltAnswer) ltAnswer.hidden = true;
  });

  function buildTranscriptQuestionPayload(question) {
    if (!ltEntries.length) return question;
    const title = (ltTitleInput?.value || 'Live lesson transcript').trim();
    const header = `Lesson transcript (${title}):\n`;
    const footer = `\n\nQuestion: ${question}`;
    const maxTranscriptChars = Math.max(1000, 15500 - header.length - footer.length);
    const selected = [];
    let used = 0;
    for (let i = ltEntries.length - 1; i >= 0; i--) {
      const line = `[${ltEntries[i].elapsed}] ${ltEntries[i].text}`;
      if (used + line.length + 1 > maxTranscriptChars) break;
      selected.push(line);
      used += line.length + 1;
    }
    selected.reverse();
    return `${header}${selected.join('\n')}${footer}`;
  }

  ltAskForm?.addEventListener('submit', async event => {
    event.preventDefault();
    const question = (ltAskInput?.value || '').trim();
    if (!question) return;
    const payload = buildTranscriptQuestionPayload(question);
    try {
      if (typeof ensureAudio === 'function') await ensureAudio();
      if (typeof stopPlayback === 'function') stopPlayback();
      ltPendingQuestion = question;
      ltWaitingForAnswer = true;
      if (ltAnswerQuestion) ltAnswerQuestion.textContent = question;
      if (ltAnswerBody) ltAnswerBody.textContent = 'Thinking…';
      if (ltAnswer) ltAnswer.hidden = false;
      if (typeof sendAction === 'function' && sendAction('text', { text: payload })) {
        ltAskInput.value = '';
      } else {
        ltPendingQuestion = null;
        ltWaitingForAnswer = false;
      }
    } catch (err) {
      ltPendingQuestion = null;
      ltWaitingForAnswer = false;
      if (typeof showError === 'function') showError(err.message || String(err));
    }
  });

  window.sailLiveTranscript = Object.freeze({
    sync: syncLiveTranscript,
    onStateChange(enabled) {
      if (enabled) {
        const currentPage = typeof window.sailCurrentPage === 'function'
          ? window.sailCurrentPage()
          : (location.hash || '').slice(1).split('?')[0].toLowerCase();
        if (currentPage === 'memory' && typeof switchCardTab === 'function') {
          switchCardTab('memoryCard', 'memoryPanelTranscript');
        }
        if (ltPendingStartMic) {
          ltPendingStartMic = false;
          if (typeof isRecording !== 'undefined' && !isRecording && typeof startMic === 'function') {
            startMic();
          }
        }
      } else {
        ltPendingStartMic = false;
        if (ltPartial) {
          ltPartial.textContent = '';
          ltPartial.hidden = true;
        }
      }
      syncLiveTranscript();
    },
    onObserved: appendObservedSegment,
    onPartial: updatePartialSegment,
    onReset: clearTranscriptEntries,
    onSessionReset: resetLiveStateOnly,
    consumePendingQuestion() {
      const q = ltPendingQuestion;
      ltPendingQuestion = null;
      return q;
    },
    onAnswerDelta(text) {
      if (!ltWaitingForAnswer || !ltAnswerBody) return;
      ltAnswerBody.textContent = text;
      if (ltAnswer) ltAnswer.hidden = false;
    },
    onAnswerComplete(text) {
      if (!ltWaitingForAnswer) return;
      ltWaitingForAnswer = false;
      if (ltAnswerBody && text) ltAnswerBody.textContent = text;
      if (ltAnswer) ltAnswer.hidden = false;
    },
  });

  restoreTranscriptState();
  syncLiveTranscript();

  // Tool Log (Memory > Tool Log tab, persisted in /srv/sail-data)
  const tlTab = document.getElementById('toolLogTab');
  const tlPanel = document.getElementById('memoryPanelToolLog');
  const tlStatus = document.getElementById('toolLogStatus');
  const tlList = document.getElementById('toolLogList');
  const tlEmpty = document.getElementById('toolLogEmpty');
  const tlRefresh = document.getElementById('toolLogRefresh');
  const tlExport = document.getElementById('toolLogExport');
  const tlClear = document.getElementById('toolLogClear');
  const tlSearchForm = document.getElementById('toolLogSearchForm');
  const tlSearchInput = document.getElementById('toolLogSearch');
  const tlFilterBtns = [...document.querySelectorAll('[data-tl-filter]')];
  const tlFooter = document.getElementById('toolLogFooter');
  const tlLoadMore = document.getElementById('toolLogLoadMore');

  const TL_PAGE_SIZE = 100;
  let tlEntries = [];
  let tlFilter = 'all'; // 'all' | 'ok' | 'err'
  let tlSerial = 0;
  let tlLoaded = false;

  function formatPrettyJson(raw) {
    if (!raw) return '';
    if (typeof raw !== 'string') {
      try { return JSON.stringify(raw, null, 2); } catch (_) { return String(raw); }
    }
    try {
      return JSON.stringify(JSON.parse(raw), null, 2);
    } catch (_) {
      return raw;
    }
  }

  function buildToolSummary(output) {
    if (!output) return '';
    if (typeof output === 'string') return output.slice(0, 240);
    if (typeof output.error === 'string' && output.error) return output.error.slice(0, 240);
    for (const key of ['message', 'summary', 'status', 'title', 'text']) {
      if (typeof output[key] === 'string' && output[key].trim()) return output[key].trim().slice(0, 240);
    }
    try { return JSON.stringify(output).slice(0, 240); } catch (_) { return ''; }
  }

  function renderToolLogEntry(entry) {
    const row = node('article', undefined, 'tl-entry');
    row.dataset.success = entry.success ? 'true' : 'false';

    const head = node('div', undefined, 'tl-entry-head');
    const left = node('div', undefined, 'tl-entry-left');
    left.append(node('strong', entry.tool || 'tool', 'tl-tool-name'));

    const statusPill = node('span', entry.success ? 'OK' : 'ERR', `tl-status-pill ${entry.success ? 'ok' : 'err'}`);
    left.append(statusPill);

    if (entry.channel) left.append(node('span', entry.channel, 'tl-channel'));
    if (typeof entry.duration_ms === 'number') left.append(node('span', `${entry.duration_ms} ms`, 'tl-duration'));

    const createdMs = typeof entry.created_at_ms === 'number' ? entry.created_at_ms : Date.now();
    const dt = new Date(createdMs);
    const timeEl = node('time', dt.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' }), 'tl-time');
    timeEl.dateTime = dt.toISOString();
    timeEl.title = dt.toLocaleString();

    head.append(left, timeEl);
    row.append(head);

    if (entry.summary) row.append(node('p', entry.summary, 'tl-summary'));

    if (entry.params_json || entry.output_json) {
      const details = node('details', undefined, 'tl-details');
      details.append(node('summary', 'Parameters & Output'));
      if (entry.params_json) {
        details.append(node('div', 'Parameters', 'tl-detail-label'));
        details.append(node('pre', formatPrettyJson(entry.params_json), 'tl-pre'));
      }
      if (entry.output_json) {
        details.append(node('div', 'Output', 'tl-detail-label'));
        details.append(node('pre', formatPrettyJson(entry.output_json), 'tl-pre'));
      }
      row.append(details);
    }
    return row;
  }

  function renderToolLogList() {
    if (!tlList) return;
    tlList.replaceChildren();
    if (!tlEntries.length) {
      if (tlEmpty) {
        tlEmpty.hidden = false;
        tlList.append(tlEmpty);
      } else {
        tlList.append(node('p', 'No tool executions recorded yet.', 'tl-empty'));
      }
      return;
    }
    for (const entry of tlEntries) {
      tlList.append(renderToolLogEntry(entry));
    }
  }

  function matchesCurrentFilter(entry) {
    if (tlFilter === 'ok' && !entry.success) return false;
    if (tlFilter === 'err' && entry.success) return false;
    const q = (tlSearchInput?.value || '').trim().toLowerCase();
    if (!q) return true;
    const hay = [entry.tool, entry.channel, entry.summary, entry.params_json, entry.output_json]
      .filter(Boolean)
      .join(' ')
      .toLowerCase();
    return hay.includes(q);
  }

  async function loadToolLogs(append = false) {
    if (!tlList) return;
    const ticket = ++tlSerial;
    if (tlStatus) tlStatus.textContent = append ? 'Loading older tool logs…' : 'Loading tool logs…';
    const beforeId = append && tlEntries.length ? tlEntries[tlEntries.length - 1].id : null;
    const query = (tlSearchInput?.value || '').trim() || null;
    const success = tlFilter === 'ok' ? true : (tlFilter === 'err' ? false : null);
    try {
      const data = await request('read', {
        action: 'tool_logs',
        query,
        success,
        limit: TL_PAGE_SIZE,
        before_id: beforeId,
      });
      if (ticket !== tlSerial) return;
      const incoming = Array.isArray(data.entries) ? data.entries : [];
      tlEntries = append ? tlEntries.concat(incoming) : incoming;
      tlLoaded = true;
      renderToolLogList();
      if (tlFooter) tlFooter.hidden = incoming.length < TL_PAGE_SIZE;
      if (tlStatus) {
        tlStatus.textContent = tlEntries.length
          ? `${tlEntries.length} tool execution${tlEntries.length === 1 ? '' : 's'} shown (persisted in /srv/sail-data).`
          : 'No matching tool executions in /srv/sail-data.';
      }
    } catch (err) {
      if (ticket !== tlSerial) return;
      if (tlStatus) tlStatus.textContent = err.message || String(err);
    }
  }

  tlTab?.addEventListener('click', () => { loadToolLogs(false); });
  tlRefresh?.addEventListener('click', () => { loadToolLogs(false); });
  tlLoadMore?.addEventListener('click', () => { loadToolLogs(true); });

  tlSearchForm?.addEventListener('submit', (event) => {
    event.preventDefault();
    loadToolLogs(false);
  });

  for (const btn of tlFilterBtns) {
    btn.addEventListener('click', () => {
      tlFilter = btn.dataset.tlFilter || 'all';
      for (const other of tlFilterBtns) {
        const active = other === btn;
        other.classList.toggle('active', active);
        other.setAttribute('aria-pressed', active ? 'true' : 'false');
      }
      loadToolLogs(false);
    });
  }

  tlExport?.addEventListener('click', () => {
    const url = URL.createObjectURL(new Blob([JSON.stringify(tlEntries, null, 2)], { type: 'application/json' }));
    const a = node('a');
    a.href = url;
    a.download = 'sail-tool-log.json';
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });

  tlClear?.addEventListener('click', async () => {
    if (!confirm('Clear all persisted tool logs?')) return;
    try {
      const token = await getOrFetchOwnerToken();
      const r = await fetch('/api/memory/tool-logs/clear', {
        method: 'POST',
        cache: 'no-store',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
        body: '{}',
      });
      if (!r.ok) throw new Error('Could not clear tool logs.');
      tlEntries = [];
      renderToolLogList();
      if (tlFooter) tlFooter.hidden = true;
      if (tlStatus) tlStatus.textContent = 'Tool log cleared.';
    } catch (err) {
      if (tlStatus) tlStatus.textContent = err.message || String(err);
    }
  });

  window.addEventListener('sail:page', (event) => {
    if (event.detail === 'memory' && tlPanel && !tlPanel.hidden) {
      loadToolLogs(false);
    }
  });

  window.sailToolLog = Object.freeze({
    refresh: () => loadToolLogs(false),
    appendLive(result, channel) {
      if (!result || !tlList) return;
      const entry = {
        id: Date.now(),
        created_at_ms: Date.now(),
        tool: result.tool || 'tool',
        channel: channel || 'dialogue',
        session: '',
        success: Boolean(result.success),
        duration_ms: typeof result.duration_ms === 'number' ? result.duration_ms : null,
        summary: buildToolSummary(result.output),
        params_json: result.params ? JSON.stringify(result.params) : null,
        output_json: result.output !== undefined ? JSON.stringify(result.output) : null,
      };
      if (!matchesCurrentFilter(entry)) return;
      if (!tlLoaded) return;
      tlEntries.unshift(entry);
      if (tlEntries.length > 500) tlEntries.length = 500;
      tlEmpty?.remove();
      tlList.prepend(renderToolLogEntry(entry));
      if (tlStatus) {
        tlStatus.textContent = `${tlEntries.length} tool execution${tlEntries.length === 1 ? '' : 's'} shown (persisted in /srv/sail-data).`;
      }
    },
  });
})();

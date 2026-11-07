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
  const ltAskForm = document.getElementById('ltAskForm');
  const ltAskInput = document.getElementById('ltAskInput');

  let ltEntries = [];
  let ltElapsedSec = 0;
  let ltTimerId = null;
  let ltPendingStartMic = false;

  function formatDuration(totalSeconds) {
    const sec = Math.max(0, Math.floor(totalSeconds));
    const hrs = Math.floor(sec / 3600);
    const mins = Math.floor((sec % 3600) / 60);
    const rem = sec % 60;
    const mm = String(mins).padStart(2, '0');
    const ss = String(rem).padStart(2, '0');
    return hrs > 0 ? `${String(hrs).padStart(2, '0')}:${mm}:${ss}` : `${mm}:${ss}`;
  }

  function updateTimerLoop(active) {
    if (active && !ltTimerId) {
      ltTimerId = setInterval(() => {
        ltElapsedSec += 1;
        if (ltTimer) ltTimer.textContent = formatDuration(ltElapsedSec);
      }, 1000);
    } else if (!active && ltTimerId) {
      clearInterval(ltTimerId);
      ltTimerId = null;
    }
  }

  function syncLiveTranscript() {
    if (!ltCard) return;
    const enabled = typeof liveContextEnabled !== 'undefined' && Boolean(liveContextEnabled);
    const recording = typeof isRecording !== 'undefined' && Boolean(isRecording);
    const muted = typeof isMuted !== 'undefined' && Boolean(isMuted);
    const connected = typeof ws !== 'undefined' && Boolean(ws && ws.readyState === WebSocket.OPEN);
    const activelyCapturing = enabled && recording && !muted;

    const state = activelyCapturing ? 'recording' : enabled ? 'ready' : 'off';
    ltCard.dataset.state = state;
    if (ltStatusPill) ltStatusPill.dataset.state = state;
    if (ltTabDot) ltTabDot.hidden = !enabled;

    if (ltStatusText) {
      ltStatusText.textContent = activelyCapturing
        ? 'Recording'
        : enabled
          ? (recording && muted ? 'Muted' : ltEntries.length > 0 ? 'Paused' : 'Ready')
          : 'Off';
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
        ltSubStatus.textContent = ltEntries.length > 0
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
        : (enabled && ltEntries.length > 0 ? 'Resume' : 'Record');
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
    if (ltClearBtn) ltClearBtn.hidden = !hasEntries && ltElapsedSec === 0;

    updateTimerLoop(activelyCapturing);
  }

  function appendObservedSegment(rawText) {
    const text = String(rawText || '').trim();
    if (!text || !ltSegments) return;
    if (ltPartial) {
      ltPartial.textContent = '';
      ltPartial.hidden = true;
    }
    if (ltEmpty) ltEmpty.hidden = true;

    const elapsed = formatDuration(ltElapsedSec);
    const clock = new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    ltEntries.push({ elapsed, clock, text });

    const article = node('article', undefined, 'lt-segment');
    const timeEl = node('time', elapsed, 'lt-seg-time');
    timeEl.title = `Captured at ${clock}`;
    const bodyEl = node('div', undefined, 'lt-seg-body');
    const textEl = node('p', text, 'lt-seg-text');
    bodyEl.append(textEl);
    article.append(timeEl, bodyEl);
    ltSegments.append(article);

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
    ltElapsedSec = 0;
    if (ltTimer) ltTimer.textContent = '00:00';
    if (ltSegments) ltSegments.replaceChildren();
    if (ltPartial) {
      ltPartial.textContent = '';
      ltPartial.hidden = true;
    }
    if (ltEmpty) ltEmpty.hidden = false;
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

  ltAskForm?.addEventListener('submit', async event => {
    event.preventDefault();
    const text = (ltAskInput?.value || '').trim();
    if (!text) return;
    try {
      if (typeof ensureAudio === 'function') await ensureAudio();
      if (typeof stopPlayback === 'function') stopPlayback();
      if (typeof sendAction === 'function' && sendAction('text', { text })) {
        ltAskInput.value = '';
      }
    } catch (err) {
      if (typeof showError === 'function') showError(err.message || String(err));
    }
  });

  window.sailLiveTranscript = Object.freeze({
    sync: syncLiveTranscript,
    onStateChange(enabled) {
      if (enabled) {
        const hash = (location.hash || '').replace(/^#\/?/, '').toLowerCase();
        if (hash !== 'learn' && hash !== 'courses' && typeof switchCardTab === 'function') {
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
  });

  syncLiveTranscript();
})();

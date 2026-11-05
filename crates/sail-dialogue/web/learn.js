'use strict';
// Arena Learn page (#247). The server owns session truth; this page renders views from
// /api/learn and sends answers. Model text is inserted as text nodes only.

// ---- Pure helpers (unit-tested in tests/learn_ui_test.cjs) ----

/** Splits a Server-Sent Events buffer into complete events and the unfinished rest. */
function learnParseSse(buffer) {
  const events = [];
  const frames = buffer.replace(/\r\n/g, '\n').split('\n\n');
  const rest = frames.pop();
  for (const frame of frames) {
    let event = 'message'; const data = [];
    for (const line of frame.split('\n')) {
      if (line.startsWith(':')) continue;
      const i = line.indexOf(':'); const field = i < 0 ? line : line.slice(0, i);
      const value = i < 0 ? '' : line.slice(i + 1).replace(/^ /, '');
      if (field === 'event') event = value; else if (field === 'data') data.push(value);
    }
    if (data.length) events.push({event, data: data.join('\n')});
  }
  return {events, rest};
}

/** Plan steps in teaching order with a text state, so state never relies on color. */
function learnStepStates(view) {
  const plan = view && view.plan; if (!plan) return [];
  const progress = view.progress || {}; const visited = new Set(progress.visited || []);
  const titles = new Map((plan.nodes || []).map(n => [n.id, n.title]));
  return (plan.order || []).map(id => {
    const state = id === progress.current ? 'current' : visited.has(id) ? 'visited' : 'upcoming';
    return {id, title: titles.get(id) || id, state, label: {current: 'Now', visited: 'Covered', upcoming: 'Next'}[state]};
  });
}

/** What the status line says. */
function learnStatus(view) {
  if (!view) return {kind: 'idle', text: ''};
  if (view.paused) return {kind: 'paused', text: 'Paused. Resume when you are ready.'};
  if (view.running) return {kind: 'running', text: view.mode === 'assess' ? 'Choosing the next question…' : 'The tutor is preparing the next step…'};
  if (view.last_failure) return {kind: 'failed', retry: true, text: `The tutor could not continue: ${view.last_failure}`};
  if (view.active_question) return {kind: 'waiting', text: 'Your turn.'};
  if (view.needs_turn) return {kind: 'running', text: 'Starting…'};
  return {kind: 'idle', text: ''};
}

/** Start a turn the model owes (after a crash or a race), at most once per view revision. */
function learnShouldAutoTurn(view, lastRequestedSeq) {
  return !!view && view.needs_turn && !view.running && !view.paused && !view.active_question && view.last_seq !== lastRequestedSeq;
}

/** Builds the answer body from the dock's fields, or returns an error message. */
function learnBuildAnswer(kind, fields) {
  const answer = {};
  const thinking = (fields.thinking || '').trim(); if (thinking) answer.thinking = thinking;
  const confidence = Number(fields.confidence); if (confidence >= 1 && confidence <= 5) answer.confidence = confidence;
  if (kind === 'free_recall') {
    const text = (fields.text || '').trim(); if (!text) return {error: 'Write your answer first.'};
    answer.text = text;
  } else {
    const choices = fields.choices || []; if (!choices.length) return {error: 'Choose an option first.'};
    if (kind === 'single' && choices.length !== 1) return {error: 'Choose one option.'};
    answer.choices = choices;
  }
  return {answer};
}

/** Context meter from the view's token estimate (#254). No cost estimate: local models. */
function learnContextMeter(view) {
  const fraction = Math.max(0, Math.min(1, Number(view && view.context_fraction) || 0));
  const percent = Math.round(fraction * 100);
  return {percent, label: `Context ${percent}% used`,
    warn: percent >= 80 ? 'Context is nearly full; older turns are summarised.' : ''};
}

/** Practice outcome wording. Never a score or a mastery claim. */
function learnPracticeText(outcome) {
  return {solved: 'Solved.', solved_with_hints: 'Solved with hints.', not_solved: 'Not solved yet.'}[outcome] || '';
}

/** The verdict class for styling: a free-recall judgment wins over the id grade. */
function learnVerdict(question) {
  return (question.judgment && question.judgment.verdict) || question.grade || '';
}

function learnGradeText(question) {
  const judgment = question.judgment;
  if (judgment) {
    if (judgment.verdict === 'unclear') {
      return judgment.follow_up ? 'Not clear enough to judge yet. Answer the follow-up question.'
        : 'Still not clear enough to judge. The tutor will explain it another way.';
    }
    return {correct: 'Correct.', partial: 'Partly there.', incorrect: 'Not quite. The tutor will go over it.',
      could_not_judge: 'SAIL could not judge this answer. The tutor will respond to your reasoning.'}[judgment.verdict] || '';
  }
  if (!question.grade) return '';
  return {correct: 'Correct.', incorrect: 'Not quite. The tutor will go over it.', needs_judgment: 'Sent to the tutor.'}[question.grade] || '';
}

/** Feedback lines under a judged free-recall answer: the missing relation, then the reasoning note. */
function learnJudgmentLines(question) {
  const judgment = question.judgment;
  if (!judgment || judgment.verdict === 'could_not_judge') return [];
  const lines = [];
  if (judgment.missing && judgment.verdict !== 'correct') lines.push(`Missing: ${judgment.missing}`);
  if (judgment.feedback) lines.push(judgment.feedback);
  return lines;
}

// ---- Page ----
if (typeof document !== 'undefined' && document.getElementById('learnPanelMain')) (() => {
  const $ = id => document.getElementById(id);
  const node = (tag, text, cls) => {const el = document.createElement(tag); if (text !== undefined) el.textContent = text; if (cls) el.className = cls; return el;};
  const status = $('learnStatus');
  let sessionId = null, view = null, stream = null, retryMs = 1000, lastAutoTurn = null, dockFor = null, diagramRevision = null;
  const diagramSvg = new Map(), diagramReported = new Set();
  let dictation = null;

  async function api(method, path, body) {
    const token = await getOrFetchOwnerToken();
    const r = await fetch(`/api/learn${path}`, {method, cache: 'no-store', headers: {'Content-Type': 'application/json', Authorization: `Bearer ${token}`}, body: body === undefined ? undefined : JSON.stringify(body)});
    let data = null; try {data = await r.json();} catch (_) {}
    if (!r.ok) {const e = new Error((data && data.error) || (r.status === 401 ? 'Owner authentication required.' : 'Learning is unavailable.')); e.status = r.status; throw e;}
    return data;
  }
  function say(text, kind = 'idle') {status.textContent = text; status.dataset.kind = kind;}
  function fail(error) {say(error.message || String(error), 'failed');}

  async function loadSessions() {
    const list = $('learnSessions'); list.replaceChildren();
    try {
      const data = await api('GET', '/sessions');
      const sessions = data.sessions || [];
      if (sessions.length) list.append(node('h4', 'Continue learning'));
      for (const s of sessions.slice(0, 8)) {
        const b = node('button', s.goal, 'quiet-button learn-resume'); b.type = 'button';
        b.setAttribute('aria-label', `Continue: ${s.goal}`);
        b.addEventListener('click', () => open(s.id).catch(fail)); list.append(b);
      }
    } catch (error) {list.append(node('p', error.message));}
  }

  function showStart() {
    closeStream(); sessionId = null; view = null; dockFor = null;
    $('learnStart').hidden = false; $('learnSession').hidden = true; $('learnNew').hidden = true;
    loadSessions();
  }

  async function open(id) {
    sessionId = id; dockFor = null; diagramRevision = null; lastAutoTurn = null;
    $('learnStart').hidden = true; $('learnSession').hidden = false; $('learnNew').hidden = false;
    render(await api('GET', `/sessions/${encodeURIComponent(id)}`));
    connect();
  }

  function closeStream() {if (stream) {stream.abort(); stream = null;}}

  async function connect() {
    closeStream(); const id = sessionId; if (!id) return;
    const controller = new AbortController(); stream = controller;
    try {
      const token = await getOrFetchOwnerToken();
      const r = await fetch(`/api/learn/sessions/${encodeURIComponent(id)}/stream`, {cache: 'no-store', headers: {Authorization: `Bearer ${token}`}, signal: controller.signal});
      if (!r.ok || !r.body) throw new Error('stream unavailable');
      retryMs = 1000;
      const reader = r.body.getReader(); const decoder = new TextDecoder(); let buffer = '';
      for (;;) {
        const {value, done} = await reader.read(); if (done) break;
        const parsed = learnParseSse(buffer + decoder.decode(value, {stream: true})); buffer = parsed.rest;
        for (const e of parsed.events) {
          if (e.event === 'view' && id === sessionId) render(JSON.parse(e.data));
          if (e.event === 'gone' && id === sessionId) {showStart(); return;}
        }
      }
    } catch (error) {if (controller.signal.aborted) return;}
    if (stream !== controller || id !== sessionId) return;
    say('Reconnecting…', 'running');
    setTimeout(() => {if (id === sessionId && stream === controller) connect();}, retryMs);
    retryMs = Math.min(retryMs * 2, 10000);
  }

  function render(next) {
    view = next;
    $('learnGoalTitle').textContent = view.goal;
    $('learnMode').textContent = view.mode === 'assess' ? 'Finding your starting point'
      : view.mode === 'practice' ? 'Practice' : 'Learning';
    const meter = learnContextMeter(view);
    $('learnContextLabel').textContent = meter.label + (meter.warn ? ` · ${meter.warn}` : '');
    $('learnContextBar').style.width = `${meter.percent}%`;
    $('learnContextBar').parentElement.setAttribute('aria-valuenow', String(meter.percent));
    $('learnPause').textContent = view.paused ? 'Resume' : 'Pause';
    $('learnPause').setAttribute('aria-pressed', view.paused ? 'true' : 'false');
    $('learnStop').hidden = !view.running;
    renderMap(); renderTranscript(); renderDock(); renderPractice(); renderReview();
    const s = learnStatus(view); say(s.text, s.kind);
    $('learnRetry').hidden = !s.retry;
    const busy = view.running || !!view.active_question || view.paused;
    $('learnMessageInput').disabled = busy; $('learnMessageSend').disabled = busy;
    if (learnShouldAutoTurn(view, lastAutoTurn)) {lastAutoTurn = view.last_seq; api('POST', `/sessions/${encodeURIComponent(view.session)}/turn`).catch(() => {});}
  }

  function renderMap() {
    const steps = learnStepStates(view); const list = $('learnSteps'); list.replaceChildren();
    $('learnMapEmpty').hidden = steps.length > 0; $('learnMapDetails').hidden = !steps.length;
    for (const step of steps) {
      const li = node('li', undefined, `learn-step ${step.state}`);
      if (step.state === 'current') li.setAttribute('aria-current', 'step');
      li.append(node('span', step.label, 'learn-step-state'), node('span', step.title));
      list.append(li);
    }
    const details = $('learnMapDetails');
    if (details.open && view.plan && window.mermaid && diagramRevision !== view.plan.revision) {
      diagramRevision = view.plan.revision; const target = $('learnDiagram');
      // Mermaid runs with securityLevel 'strict' (voice.js); titles are sanitized server-side too.
      mermaid.render(`learn-map-${view.plan.revision}-${Date.now()}`, view.plan.mermaid)
        .then(r => {target.innerHTML = r.svg;})
        .catch(() => {target.replaceChildren(node('p', 'The diagram could not be drawn; the list above has every step.'));});
    }
  }

  function renderTranscript() {
    const list = $('learnTranscript'); const atEnd = list.scrollHeight - list.scrollTop - list.clientHeight < 40;
    list.replaceChildren();
    const titles = new Map(((view.plan && view.plan.nodes) || []).map(n => [n.id, n.title]));
    for (const entry of view.transcript) {
      const li = node('li', undefined, `learn-entry ${entry.type}`);
      if (entry.type === 'learner') li.append(node('span', 'You', 'learn-who'), node('p', entry.text));
      else if (entry.type === 'assistant') li.append(node('span', 'Tutor', 'learn-who'), node('p', entry.text));
      else if (entry.type === 'explain') li.append(node('span', titles.get(entry.node_id) || 'Explanation', 'learn-who'), node('p', entry.text));
      else if (entry.type === 'notice') li.append(node('p', entry.text, 'learn-notice'));
      else if (entry.type === 'problem') li.append(node('span', 'Practice', 'learn-who'), node('p', entry.statement));
      else if (entry.type === 'diagram') {
        li.append(node('span', 'Diagram', 'learn-who'), node('p', entry.caption || '', 'learn-caption'));
        const target = node('div', undefined, 'learn-diagram');
        li.append(target);
        if (entry.error) {
          target.append(node('p', `The diagram could not be drawn: ${entry.error}`, 'learn-notice'));
        } else if (window.mermaid) {
          mermaid.initialize({startOnLoad: false, securityLevel: 'strict', theme: 'dark'});
          const svg = diagramSvg.get(entry.call_id);
          if (svg) target.innerHTML = svg;
          else {
            target.append(node('p', 'Drawing…', 'learn-hint'));
            mermaid.render(`learn-diagram-${entry.call_id}-${Date.now()}`, entry.mermaid)
              .then(r => {diagramSvg.set(entry.call_id, r.svg); target.innerHTML = r.svg;})
              .catch(error => {
                target.replaceChildren(node('p', 'The diagram could not be drawn; use the explanation and caption.', 'learn-notice'));
                if (!diagramReported.has(entry.call_id)) {
                  diagramReported.add(entry.call_id);
                  api('POST', `/sessions/${encodeURIComponent(sessionId)}/diagram-error`, {call_id: entry.call_id, message: String(error && error.message || error)}).catch(() => {});
                }
              });
          }
        } else {
          target.append(node('p', `Diagram source: ${entry.mermaid}`, 'learn-answer'));
        }
      }
      else if (entry.type === 'question') {
        li.append(node('span', entry.follow_up_of ? 'Follow-up' : 'Question', 'learn-who'), node('p', entry.prompt));
        if (entry.answer) {
          const chosen = (entry.answer.choices || []).map(id => (entry.options.find(o => o.id === id) || {}).text || id);
          li.append(node('p', `Your answer: ${entry.answer.text || chosen.join(', ')}`, 'learn-answer'));
          const correct = (entry.correct || []).map(id => (entry.options.find(o => o.id === id) || {}).text || id);
          if (entry.grade === 'incorrect' && correct.length) li.append(node('p', `Expected: ${correct.join(', ')}`, 'learn-answer'));
          li.append(node('p', learnGradeText(entry), `learn-grade ${learnVerdict(entry)}`));
          for (const line of learnJudgmentLines(entry)) li.append(node('p', line, 'learn-answer'));
        } else li.append(node('p', 'Waiting for your answer below.', 'learn-answer'));
      }
      list.append(li);
    }
    if (atEnd) list.scrollTop = list.scrollHeight;
  }

  const draftKey = id => `sail.learn.draft.${sessionId}.${id}`;
  function saveDraft(question, form) {
    const fields = readDock(question, form);
    try {localStorage.setItem(draftKey(question.id), JSON.stringify(fields));} catch (_) {}
  }
  function readDock(question, form) {
    return {
      choices: [...form.querySelectorAll('input[name="learnChoice"]:checked')].map(i => i.value),
      text: form.querySelector('#learnAnswerText') ? form.querySelector('#learnAnswerText').value : '',
      confidence: form.querySelector('#learnConfidence').value,
      thinking: form.querySelector('#learnThinking').value,
    };
  }

  function renderDock() {
    const dock = $('learnDock'); const id = view.active_question;
    const question = id && view.transcript.find(e => e.type === 'question' && e.id === id);
    if (!question || view.paused) {dock.hidden = true; dock.replaceChildren(); dockFor = null; return;}
    dock.hidden = false; if (dockFor === id) return; dockFor = id;
    let draft = {}; try {draft = JSON.parse(localStorage.getItem(draftKey(id)) || '{}');} catch (_) {}
    const fieldset = node('fieldset'); fieldset.append(node('legend', question.prompt));
    if (question.kind === 'free_recall') {
      const label = node('label', 'Your answer'); const area = node('textarea'); area.id = 'learnAnswerText'; area.rows = 3; area.maxLength = 8000; area.value = draft.text || '';
      label.htmlFor = 'learnAnswerText'; fieldset.append(label, area);
      fieldset.append(dictationButton(area, 'answer'));
    } else {
      for (const option of question.options) {
        const label = node('label', undefined, 'learn-option'); const input = node('input');
        input.type = question.kind === 'single' ? 'radio' : 'checkbox'; input.name = 'learnChoice'; input.value = option.id;
        input.checked = (draft.choices || []).includes(option.id);
        label.append(input, node('span', option.text)); fieldset.append(label);
      }
    }
    const extras = node('details'); extras.append(node('summary', 'Add your reasoning or confidence'));
    const tl = node('label', 'How you thought about it (optional)'); const thinking = node('textarea'); thinking.id = 'learnThinking'; thinking.rows = 2; thinking.maxLength = 8000; thinking.value = draft.thinking || '';
    tl.htmlFor = 'learnThinking'; extras.append(tl, thinking, dictationButton(thinking, 'reasoning'));
    const cl = node('label', 'How sure are you? (optional)'); const conf = node('select'); conf.id = 'learnConfidence'; cl.htmlFor = 'learnConfidence';
    [['', 'Prefer not to say'], ['1', 'Guessing'], ['2', 'Unsure'], ['3', 'Fairly sure'], ['4', 'Sure'], ['5', 'Certain']].forEach(([v, t]) => {const o = node('option', t); o.value = v; conf.append(o);});
    conf.value = draft.confidence || '';
    extras.append(cl, conf); if (draft.thinking || draft.confidence) extras.open = true;
    const submit = node('button', 'Submit answer', 'quiet-button learn-primary'); submit.type = 'submit';
    const error = node('p', '', 'learn-dock-error'); error.setAttribute('role', 'alert');
    dock.replaceChildren(fieldset, extras, error, submit);
    dock.oninput = () => saveDraft(question, dock);
    dock.onsubmit = async event => {
      event.preventDefault();
      const built = learnBuildAnswer(question.kind, readDock(question, dock));
      if (built.error) {error.textContent = built.error; return;}
      submit.disabled = true; error.textContent = '';
      try {
        await api('POST', `/sessions/${encodeURIComponent(sessionId)}/answer`, {question_id: id, answer: built.answer});
        try {localStorage.removeItem(draftKey(id));} catch (_) {}
      } catch (e) {
        error.textContent = e.status === 409 ? 'The tutor is still finishing the last step. Try again in a moment.' : e.message;
        submit.disabled = false;
      }
    };
    (dock.querySelector('input,textarea') || submit).focus({preventScroll: true});
  }

  function dictationButton(target, what) {
    const b = node('button', `Dictate ${what}`, 'quiet-button learn-dictate'); b.type = 'button';
    b.setAttribute('aria-pressed', 'false');
    b.addEventListener('click', () => toggleDictation(target, b));
    return b;
  }

  async function toggleDictation(target, button) {
    if (dictation) { dictation.stop(); return; }
    if (!window.SailClientSpeech || !navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
      say('Dictation is unavailable in this browser.', 'failed'); return;
    }
    let stream;
    try { stream = await navigator.mediaDevices.getUserMedia({audio: true}); }
    catch (error) { say('Microphone permission was not granted; nothing was recorded.', 'failed'); return; }
    try {
      await SailClientSpeech.load();
      if (!SailClientSpeech.ready() || !SailClientSpeech.sttOnDevice()) {
        stream.getTracks().forEach(track => track.stop());
        say('On-device dictation is not available; type your answer instead.', 'failed'); return;
      }
    } catch (error) {
      stream.getTracks().forEach(track => track.stop());
      say('Dictation could not start; type your answer instead.', 'failed'); return;
    }
    const recorder = new MediaRecorder(stream); const chunks = [];
    recorder.ondataavailable = event => {if (event.data.size) chunks.push(event.data);};
    recorder.onstop = async () => {
      stream.getTracks().forEach(track => track.stop()); dictation = null;
      button.textContent = `Dictate ${what}`; button.setAttribute('aria-pressed', 'false');
      try {
        const audio = await new AudioContext().decodeAudioData(await new Blob(chunks).arrayBuffer());
        const samples = SailClientSpeech.resample(audio.getChannelData(0), audio.sampleRate);
        const {text} = await SailClientSpeech.transcribe(samples);
        const addition = (text || '').trim();
        if (addition) target.value = target.value ? `${target.value.trim()} ${addition}` : addition;
        else say('Nothing was heard; try again.', 'idle');
      } catch (error) { say('The recording could not be transcribed; type your answer instead.', 'failed'); }
    };
    dictation = {stop: () => recorder.state !== 'inactive' && recorder.stop()};
    recorder.start();
    button.textContent = 'Stop dictation'; button.setAttribute('aria-pressed', 'true');
    say('Recording… press Stop dictation to insert the transcript.', 'running');
  }

  function renderPractice() {
    const box = $('learnPractice'); const practice = view.practice;
    if (!practice || view.paused) {box.hidden = true; box.replaceChildren(); return;}
    const active = (view.practice.problems || []).find(p => p.id === practice.active_problem);
    if (!active) {
      const done = (practice.problems || []).slice(-1)[0];
      if (!done || !done.solution) {box.hidden = true; box.replaceChildren(); return;}
      box.hidden = false; box.replaceChildren(node('h4', `Solution · ${learnPracticeText(done.outcome)}`), node('p', done.solution));
      return;
    }
    box.hidden = false; box.replaceChildren(node('h4', 'Practice problem'), node('p', active.statement));
    if (active.hints.length) {
      const list = node('ul', undefined, 'learn-hints');
      for (const hint of active.hints) list.append(node('li', hint));
      box.append(list);
    }
    if (active.hints.length < active.hint_count) {
      const hint = node('button', 'Ask for a hint', 'quiet-button'); hint.type = 'button';
      hint.addEventListener('click', () => api('POST', `/sessions/${encodeURIComponent(sessionId)}/hint`, {problem_id: active.id, index: active.hints.length}).catch(fail));
      box.append(hint);
    }
    const label = node('label', 'Your attempt'); const area = node('textarea'); area.rows = 3; area.maxLength = 8000; area.id = 'learnPracticeAnswer';
    label.htmlFor = 'learnPracticeAnswer';
    const submit = node('button', 'Send attempt', 'quiet-button learn-primary'); submit.type = 'button';
    submit.addEventListener('click', async () => {
      const text = area.value.trim(); if (!text) {say('Write an attempt first.', 'idle'); return;}
      submit.disabled = true;
      try {await api('POST', `/sessions/${encodeURIComponent(sessionId)}/attempt`, {problem_id: active.id, text}); area.value = '';}
      catch (error) {fail(error); submit.disabled = false;}
    });
    box.append(label, area, dictationButton(area, 'attempt'), submit);
    box.append(node('p', 'Hints are recorded for this problem. Outcome is practice evidence only.', 'learn-hint'));
  }

  function renderReview() {
    const box = $('learnReview'); const practice = view.practice;
    if (!practice) {box.hidden = true; box.replaceChildren(); return;}
    if (view.review) {
      box.hidden = false; box.replaceChildren(node('p', `Practice reminder scheduled in ${view.review.days} day${view.review.days === 1 ? '' : 's'}.`));
      const cancel = node('button', 'Cancel reminder', 'quiet-button'); cancel.type = 'button';
      cancel.addEventListener('click', () => api('DELETE', `/sessions/${encodeURIComponent(sessionId)}/review`).catch(fail));
      box.append(cancel); return;
    }
    if (!practice.last_outcome) {box.hidden = true; box.replaceChildren(); return;}
    box.hidden = false; box.replaceChildren(node('p', 'Practise this again later? Off unless you choose a day.'));
    const select = node('select'); select.id = 'learnReviewDays';
    for (const days of [1, 3, 7]) {const o = node('option', `${days} day${days === 1 ? '' : 's'}`); o.value = String(days); select.append(o);}
    const go = node('button', 'Remind me to practice', 'quiet-button'); go.type = 'button';
    go.addEventListener('click', () => {
      const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
      api('POST', `/sessions/${encodeURIComponent(sessionId)}/review`, {days: Number(select.value), timezone, utc_offset_minutes: -new Date().getTimezoneOffset()}).catch(fail);
    });
    box.append(select, go);
  }

  $('learnStartForm').addEventListener('submit', async event => {
    event.preventDefault(); const goal = $('learnGoal').value.trim(); if (!goal) return;
    const button = event.submitter; if (button) button.disabled = true;
    const mode = $('learnStartMode').value === 'practice' ? 'practice' : 'learn';
    try {const created = await api('POST', '/sessions', {goal, mode}); $('learnGoal').value = ''; await open(created.session);}
    catch (error) {fail(error);} finally {if (button) button.disabled = false;}
  });
  $('learnMessageForm').addEventListener('submit', async event => {
    event.preventDefault(); const input = $('learnMessageInput'); const text = input.value.trim(); if (!text || !sessionId) return;
    try {await api('POST', `/sessions/${encodeURIComponent(sessionId)}/message`, {key: crypto.randomUUID(), text}); input.value = '';}
    catch (error) {fail(error);}
  });
  $('learnPause').addEventListener('click', () => sessionId && api('POST', `/sessions/${encodeURIComponent(sessionId)}/pause`, {paused: !(view && view.paused)}).then(render).catch(fail));
  $('learnStop').addEventListener('click', () => sessionId && api('POST', `/sessions/${encodeURIComponent(sessionId)}/abort`).catch(fail));
  $('learnRetry').addEventListener('click', () => sessionId && api('POST', `/sessions/${encodeURIComponent(sessionId)}/turn`).catch(fail));
  $('learnNew').addEventListener('click', showStart);
  $('learnMapDetails').addEventListener('toggle', () => {diagramRevision = null; if (view) renderMap();});
  window.addEventListener('sail:page', event => {
    if (event.detail === 'arena') {if (sessionId) open(sessionId).catch(fail); else loadSessions();}
    else closeStream();
  });
})();

if (typeof module !== 'undefined') module.exports = {learnParseSse, learnStepStates, learnStatus, learnShouldAutoTurn, learnBuildAnswer, learnGradeText, learnJudgmentLines, learnVerdict, learnContextMeter, learnPracticeText};

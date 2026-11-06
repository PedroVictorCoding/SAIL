'use strict';
// Forge UI: pipeline chips, job cards, diffs, the activity-log pop-up and the job list.
// Classic script sharing the page's global scope with voice.js, which loads first.

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

// Durable task states that say more than the Forge stage: work waiting, resuming after a
// restart, or stopped until the user decides. Running and succeeded jobs keep the stage label.
const FORGE_TASK_LABELS = {
  queued: 'queued',
  interrupted: 'resuming after restart',
  needs_intervention: 'needs your attention',
  failed: 'failed',
  cancelled: 'cancelled',
};

function handleForgeEvent(data) {
  const jobContainer = document.getElementById('forgeActiveJob');
  if (!jobContainer) return;
  document.getElementById('forgeEmptyState')?.remove();

  const eventType = data.event_type || data.type;
  const jobId = data.job_id || 'Job';
  const state = data.state || data.status?.state || (data.success ? 'completed' : 'active');

  updateForgePipeline(state);
  // Background work: counted by the ambient contract, never drives the shell light.
  if (data.job_id) window.SailAmbient?.dispatch({ type:'background', id:`forge:${data.job_id}`,
    active:!['completed','succeeded','failed','cancelled','blocked'].includes(state) });

  let jobCard = document.getElementById(`forge-job-${jobId}`);
  if (!jobCard) {
    jobCard = document.createElement('div');
    jobCard.id = `forge-job-${jobId}`;
    jobCard.className = 'forge-job-item';
    jobContainer.prepend(jobCard);
  }

  const task = data.continuity;
  const taskLabel = task && FORGE_TASK_LABELS[task.state];
  const stopped = task && (task.needs_attention || task.state === 'failed' || task.state === 'cancelled');
  const badgeClass = stopped ? 'result-failure' : state === 'completed' ? 'result-success' : state === 'blocked' ? 'result-failure' : 'forge-state-active';

  jobCard.replaceChildren();
  const heading=document.createElement('div');heading.className='forge-job-heading';
  const label=document.createElement('span');label.textContent=`${jobId}${data.issue_number ? ` · Issue #${data.issue_number}` : ''}`;
  const badge=document.createElement('span');badge.className=badgeClass;badge.textContent=`● ${taskLabel || state}`;heading.append(label,badge);jobCard.append(heading);
  const metadata=(label,value)=>{const line=document.createElement('div');line.className='forge-meta-line';const key=document.createElement('span');key.textContent=label;const text=document.createElement('strong');text.textContent=String(value);line.append(key,text);jobCard.append(line);};
  if(data.repo)metadata('Repo:',data.repo);
  if(data.model_id||data.model)metadata('Model:',data.model_id||data.model);
  if(data.iterations)metadata('Iterations:',data.iterations);
  if(task?.needs_attention&&task.reason)metadata('Needs you:',task.reason);
  else if(taskLabel&&task.reason&&task.reason!==data.failure_reason)metadata('Status:',task.reason);
  if(data.failure_reason&&!(task?.needs_attention&&task.reason===data.failure_reason))metadata('Reason:',data.failure_reason);
  if(data.task||data.summary||data.message){const text=document.createElement('div');text.className='forge-task-desc';text.textContent=String(data.task||data.summary||data.message);jobCard.append(text);}
  if(data.branch)metadata('Branch:',data.branch);
  if(data.passed!==undefined)metadata('Review verdict:',`${data.passed ? '✓ Passed' : 'Failed'} (${data.finding_count||0} findings)`);
  if(data.pr_draft_ready)metadata('✓','Pull Request draft ready for review');

  // Live action strip on the active job card in Agent tab
  if (data.model_action || data.turn) {
    const strip = document.createElement('div');
    strip.className = 'forge-active-action-strip';
    if (data.turn) {
      const t = document.createElement('span');
      t.className = 'forge-turn-badge';
      t.textContent = `Turn ${data.turn}`;
      strip.appendChild(t);
    }
    if (data.model_action) {
      const a = document.createElement('span');
      a.className = 'forge-action-desc';
      a.textContent = data.model_action;
      strip.appendChild(a);
    }
    const viewBtn = document.createElement('button');
    viewBtn.type = 'button';
    viewBtn.className = 'forge-view-stream-btn';
    viewBtn.textContent = 'View Live';
    viewBtn.onclick = (e) => {
      e.stopPropagation();
      const streamTab = document.querySelector('.card-tab[data-tab-panel="forgePanelStream"]');
      if (streamTab) streamTab.click();
    };
    strip.appendChild(viewBtn);
    jobCard.append(strip);
  }
  if (data.live_output) {
    const snippet = document.createElement('div');
    snippet.className = 'forge-active-live-snippet';
    snippet.textContent = data.live_output.length > 240 ? '...' + data.live_output.slice(-240) : data.live_output;
    jobCard.append(snippet);
  }

  // Live Forge Code Stream, Model Outputs & Diff Viewer
  const liveStream = document.getElementById('forgeLiveStream');
  const liveTitle = document.getElementById('forgeLiveTitle');
  const liveFile = document.getElementById('forgeLiveFile');
  const liveDiff = document.getElementById('forgeLiveDiff');
  const liveLogs = document.getElementById('forgeLiveLogs');
  const turnBadge = document.getElementById('forgeTurnBadge');
  const actionLabel = document.getElementById('forgeActionLabel');
  const modelStreamBox = document.getElementById('forgeModelStreamBox');
  const liveOutputText = document.getElementById('forgeLiveOutputText');
  const expandDiffBtn = document.getElementById('forgeExpandDiffBtn');

  if (liveStream) {
    const isLivePhase = ['planning', 'coding', 'testing'].includes(state);
    if (data.diff || data.target_file || data.logs || data.live_output || data.model_action || isLivePhase) {
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
      if (turnBadge) {
        if (data.turn) {
          turnBadge.textContent = `Turn ${data.turn}`;
          turnBadge.hidden = false;
        } else {
          turnBadge.hidden = true;
        }
      }
      if (actionLabel) {
        if (data.model_action) {
          actionLabel.textContent = data.model_action;
          actionLabel.hidden = false;
        } else {
          actionLabel.hidden = true;
        }
      }
      if (modelStreamBox && liveOutputText) {
        if (data.live_output) {
          modelStreamBox.hidden = false;
          liveOutputText.textContent = data.live_output;
          liveOutputText.scrollTop = liveOutputText.scrollHeight;
        } else if (state === 'coding') {
          modelStreamBox.hidden = false;
          liveOutputText.textContent = 'Generating reasoning & code diff...';
        }
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
        // Latest live data is shown immediately; the full recorded log replaces it when
        // the server answers the status request.
        const live = { live_output: data.live_output, diff: data.diff || liveDiff?.textContent || '' };
        expandDiffBtn.onclick = (e) => {
          e.stopPropagation();
          requestForgeLog(jobId, state, live);
        };
      }
    }
  }
}

// Full Forge activity log pop-up: every model reply, tool result, test run and pipeline
// stage recorded for the job, followed by the final diff.
let pendingForgeLog = null;
const FORGE_LOG_OPEN_KINDS = new Set(['model', 'outcome', 'gates', 'review', 'waiting']);

function requestForgeLog(jobId, state, live) {
  pendingForgeLog = { jobId, state, live };
  renderForgeLogModal(jobId, state, { loading: true, live });
  if (!sendAction('forge:status', { job_id: jobId, include_activity: true })) {
    renderForgeLogModal(jobId, state, { live, note: 'Not connected; showing the latest live output only.' });
  }
}

function handleForgeLogResponse(data) {
  // Only responses to an include_activity request carry an array.
  if (!pendingForgeLog || !Array.isArray(data.activity)) return;
  const status = data.status || {};
  if (![data.job_id, status.job_id].includes(pendingForgeLog.jobId)) return;
  const dialog = document.getElementById('cardModal');
  if (dialog && !dialog.open) { pendingForgeLog = null; return; }
  renderForgeLogModal(status.job_id || pendingForgeLog.jobId, status.state || pendingForgeLog.state, {
    status, activity: data.activity, live: pendingForgeLog.live
  });
}

function forgeLogTime(iso) {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function forgeLogPlainText(jobId, status, activity, diff) {
  const lines = [`Forge job ${jobId} · ${status.state || ''}`];
  if (status.failure_reason) lines.push(`Reason: ${status.failure_reason}`);
  for (const e of activity) {
    lines.push('', `=== [${forgeLogTime(e.at)}] Turn ${e.turn} · ${e.kind} · ${e.title} ===`);
    if (e.body) lines.push(e.body);
  }
  if (diff) lines.push('', '=== Final diff ===', diff);
  return lines.join('\n');
}

function renderForgeLogModal(jobId, state, { loading = false, status = {}, activity = null, live = {}, note = '' }) {
  const body = document.createElement('div');
  body.className = 'forge-log';

  const summary = document.createElement('div');
  summary.className = 'forge-log-summary';
  const facts = [
    ['State', status.state || state],
    ['Result', status.verification_status],
    ['Reason', status.failure_reason],
    ['Files', (status.modified_files || []).join(', ')],
    ['Model endpoint', status.model_endpoint]
  ].filter(([, v]) => v);
  for (const [k, v] of facts) {
    const row = document.createElement('div');
    const key = document.createElement('span'); key.textContent = k;
    const val = document.createElement('strong'); val.textContent = String(v);
    row.append(key, val); summary.append(row);
  }
  body.append(summary);

  const notice = (text) => { const p = document.createElement('p'); p.className = 'forge-log-note'; p.textContent = text; body.append(p); };
  if (note) notice(note);

  if (loading) {
    notice('Loading the full activity log…');
  } else if (Array.isArray(activity) && activity.length) {
    const list = document.createElement('ol');
    list.className = 'forge-log-list';
    for (const e of activity) {
      const item = document.createElement('li');
      item.className = `forge-log-entry forge-log-${e.kind}`;
      const details = document.createElement('details');
      details.open = FORGE_LOG_OPEN_KINDS.has(e.kind) || (e.kind === 'feedback' && (e.body || '').length < 1500);
      const head = document.createElement('summary');
      const time = document.createElement('span'); time.className = 'forge-log-time'; time.textContent = forgeLogTime(e.at);
      const kind = document.createElement('span'); kind.className = 'forge-log-kind'; kind.textContent = `T${e.turn} · ${e.kind}`;
      const title = document.createElement('span'); title.className = 'forge-log-title'; title.textContent = e.title;
      head.append(time, kind, title);
      details.append(head);
      if (e.body) { const pre = document.createElement('pre'); pre.textContent = e.body; details.append(pre); }
      item.append(details);
      list.append(item);
    }
    body.append(list);
  } else {
    notice('No detailed activity was recorded for this job (jobs started before activity logging only keep their stage timeline).');
    const events = status.events || [];
    if (events.length) {
      const pre = document.createElement('pre');
      pre.textContent = events.map(ev => `[${forgeLogTime(ev.timestamp)}] ${ev.stage}: ${ev.detail}`).join('\n');
      body.append(pre);
    }
    const lastOutput = status.live_output || live.live_output;
    if (lastOutput) {
      const h = document.createElement('h4'); h.textContent = 'Latest model output';
      const pre = document.createElement('pre'); pre.textContent = lastOutput;
      body.append(h, pre);
    }
  }

  const diff = status.patch || live.diff || '';
  if (diff && !diff.startsWith('// ')) {
    const h = document.createElement('h4'); h.textContent = 'Final diff';
    const pre = document.createElement('pre'); pre.className = 'forge-log-diff';
    // One block span per line; the joining newlines would render as blank lines in <pre>.
    pre.innerHTML = formatDiffHighlight(diff).split('\n').join('');
    body.append(h, pre);
  }

  const footer = document.createElement('div');
  footer.className = 'forge-log-footer';
  const refresh = document.createElement('button');
  refresh.type = 'button'; refresh.className = 'quiet-button'; refresh.textContent = 'Refresh';
  refresh.addEventListener('click', () => requestForgeLog(jobId, state, live));
  const copy = document.createElement('button');
  copy.type = 'button'; copy.className = 'quiet-button'; copy.textContent = 'Copy full log';
  copy.disabled = loading;
  copy.addEventListener('click', () => {
    navigator.clipboard?.writeText(forgeLogPlainText(jobId, status, activity || [], diff));
    copy.textContent = 'Copied!';
    setTimeout(() => { copy.textContent = 'Copy full log'; }, 2000);
  });
  footer.append(refresh, copy);

  const finalState = status.state || state;
  openCardModal({
    icon: '⚒',
    title: `Forge activity · ${jobId}`,
    badge: String(finalState || '').toUpperCase(),
    badgeClass: finalState === 'completed' ? 'result-success' : finalState === 'blocked' ? 'result-failure' : 'forge-state-active',
    bodyNode: body,
    footerNode: footer
  });
  const list = body.querySelector('.forge-log-list');
  if (list && !loading) list.lastElementChild?.scrollIntoView({ block: 'nearest' });
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
      failure_reason: job.failure_reason,
      continuity: job.continuity,
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

function handleForgeCleared(data) {
  if (Array.isArray(data.cleared_jobs)) {
    for (const id of data.cleared_jobs) {
      document.getElementById(`forge-job-${id}`)?.remove();
    }
  }
  const jobContainer = document.getElementById('forgeActiveJob');
  if (jobContainer && jobContainer.children.length === 0) {
    jobContainer.innerHTML = '<p class="forge-empty" id="forgeEmptyState">No active Forge jobs. Ask SAIL or enqueue below.</p>';
  }
}

// Auto-ping Forge jobs periodically
setInterval(() => {
  if (!document.hidden && ws && ws.readyState === WebSocket.OPEN) {
    sendAction('forge:list');
  }
}, 5000);

'use strict';
// Owner approvals and security: approval cards, owner token, autonomy preset, learned rules and the daily safety review.
// Classic script sharing the page's global scope with voice.js, which loads first.

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

// Auto-ping approvals periodically
setInterval(() => {
  refreshOwnerApprovals();
}, 6000);

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

// Auto-ping learned rules periodically
setInterval(() => {
  refreshLearnedRules();
}, 15000);

// ============================================================================
// Daily Safety Review of Learned Approval Rules & Web Notifications
// ============================================================================
async function fetchSafetyReview() {
  try {
    const data = await ownerApprovalRequest('safety/review');
    renderSafetyReview(data);
  } catch (error) {
    console.warn('Could not fetch safety review:', error);
  }
}

async function triggerSafetyReviewManual() {
  const runBtn = document.getElementById('safetyReviewRunNowBtn');
  const runSettingsBtn = document.getElementById('runSafetyReviewBtn');
  if (runBtn) runBtn.disabled = true;
  if (runSettingsBtn) runSettingsBtn.disabled = true;
  try {
    const data = await ownerApprovalRequest('safety/review/run');
    renderSafetyReview(data);
  } catch (error) {
    showError(`Failed to run safety review: ${error.message}`);
  } finally {
    if (runBtn) runBtn.disabled = false;
    if (runSettingsBtn) runSettingsBtn.disabled = false;
  }
}

async function resolveSafetyFinding(findingId, action) {
  try {
    await ownerApprovalRequest('safety/findings/resolve', { finding_id: findingId, action });
    await fetchSafetyReview();
    await refreshLearnedRules();
  } catch (error) {
    showError(`Failed to apply action: ${error.message}`);
  }
}

function handleSafetyReviewCompleted(data) {
  fetchSafetyReview();
  if (data.high_or_critical_count > 0) {
    const dot = document.getElementById('safetyBadgeDot');
    if (dot) dot.hidden = false;
  }
}

function renderSafetyReview(data) {
  const findings = data?.findings || [];
  const lastRun = data?.last_run || data?.summary;

  const dot = document.getElementById('safetyBadgeDot');
  const unreadHighOrCrit = findings.filter(f => f.status === 'unread' && (f.severity === 'high' || f.severity === 'critical'));
  if (dot) dot.hidden = unreadHighOrCrit.length === 0;

  const statusBadge = document.getElementById('safetyReviewStatusBadge');
  if (statusBadge) {
    if (unreadHighOrCrit.length > 0) {
      statusBadge.textContent = `${unreadHighOrCrit.length} Critical/High Risk Finding(s)`;
      statusBadge.className = 'safety-status-badge has-findings';
    } else if (findings.filter(f => f.status === 'unread').length > 0) {
      statusBadge.textContent = 'Review Findings Present';
      statusBadge.className = 'safety-status-badge';
    } else {
      statusBadge.textContent = 'Status: Clean';
      statusBadge.className = 'safety-status-badge';
    }
  }

  const lastRunEl = document.getElementById('safetyReviewLastRun');
  if (lastRunEl && lastRun) {
    const d = new Date(lastRun.completed_at);
    lastRunEl.textContent = `Last run: ${d.toLocaleTimeString([], { hour:'2-digit', minute:'2-digit' })} (${lastRun.rules_evaluated} rules evaluated)`;
  }

  const summaryText = document.getElementById('safetyReviewSummaryText');
  if (summaryText && lastRun) {
    summaryText.textContent = `Last evaluated ${lastRun.rules_evaluated} rules: ${lastRun.findings_count} findings (${lastRun.high_or_critical_count} high/critical).`;
  }

  // Render into Activity tab list and Settings list
  const containers = [
    document.getElementById('safetyFindingsList'),
    document.getElementById('safetyReviewFindingsContainer')
  ];

  containers.forEach(container => {
    if (!container) return;
    const activeFindings = findings.filter(f => f.status !== 'dismissed' && typeof f.status !== 'object');
    if (activeFindings.length === 0) {
      container.innerHTML = '<p class="canvas-empty">No safety findings detected. All learned rules conform to security invariants.</p>';
      return;
    }
    container.replaceChildren();

    for (const f of activeFindings) {
      const card = document.createElement('div');
      card.className = `safety-finding-card severity-${f.severity}`;

      const top = document.createElement('div');
      top.className = 'safety-finding-top';

      const title = document.createElement('h5');
      title.className = 'safety-finding-title';
      title.textContent = f.title;

      const pill = document.createElement('span');
      pill.className = `safety-severity-pill ${f.severity}`;
      pill.textContent = f.severity;

      top.append(title, pill);

      const reason = document.createElement('p');
      reason.className = 'safety-finding-reason';
      reason.textContent = f.reason;

      const evidence = document.createElement('div');
      evidence.className = 'safety-finding-evidence';
      evidence.textContent = f.evidence.join(' · ');

      const actions = document.createElement('div');
      actions.className = 'safety-finding-actions';

      if (f.recommended_action === 'revoke' || (typeof f.recommended_action === 'object' && f.recommended_action.narrow_scope)) {
        const revokeBtn = document.createElement('button');
        revokeBtn.className = 'safety-btn revoke-btn';
        revokeBtn.textContent = 'Revoke Rule';
        revokeBtn.addEventListener('click', () => resolveSafetyFinding(f.finding_id, 'revoke'));
        actions.append(revokeBtn);
      }

      if (typeof f.recommended_action === 'object' && f.recommended_action.narrow_scope) {
        const narrowBtn = document.createElement('button');
        narrowBtn.className = 'safety-btn narrow-btn';
        narrowBtn.textContent = 'Narrow Scope';
        narrowBtn.addEventListener('click', () => resolveSafetyFinding(f.finding_id, 'narrow'));
        actions.append(narrowBtn);
      }

      const keepBtn = document.createElement('button');
      keepBtn.className = 'safety-btn keep-btn';
      keepBtn.textContent = 'Keep';
      keepBtn.addEventListener('click', () => resolveSafetyFinding(f.finding_id, 'keep'));
      actions.append(keepBtn);

      const dismissBtn = document.createElement('button');
      dismissBtn.className = 'safety-btn';
      dismissBtn.textContent = 'Dismiss';
      dismissBtn.addEventListener('click', () => resolveSafetyFinding(f.finding_id, 'dismiss'));
      actions.append(dismissBtn);

      card.append(top, reason, evidence, actions);
      container.append(card);
    }
  });
}

document.getElementById('safetyReviewRunNowBtn')?.addEventListener('click', triggerSafetyReviewManual);
document.getElementById('runSafetyReviewBtn')?.addEventListener('click', triggerSafetyReviewManual);
document.getElementById('refreshSafetyReviewBtn')?.addEventListener('click', fetchSafetyReview);

fetchSafetyReview();
setInterval(() => {
  fetchSafetyReview();
}, 20000);

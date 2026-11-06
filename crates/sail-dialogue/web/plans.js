'use strict';
// Agent task plans: one card per plan on Canvas, with its steps, states and results, and the
// owner's approve, edit and cancel controls. Classic script sharing the page's global scope;
// it uses the owner token helpers from approvals.js. Plan text comes from the model and from
// tool output, so it is only ever set as text, never as HTML.

const PLAN_TERMINAL = new Set(['succeeded', 'failed', 'cancelled']);
const PLAN_LABELS = { awaiting_approval:'Waiting for your approval', running:'Running', succeeded:'Done', failed:'Failed', cancelled:'Cancelled' };
const STEP_ICONS = { pending:'○', running:'◐', done:'✓', failed:'✕' };
const planCards = new Map();

async function planRequest(path, body) {
  const token = await getOrFetchOwnerToken();
  const response = await fetch(`/api/plans${path}`, { method:body ? 'POST' : 'GET', cache:'no-store',
    headers:{ Authorization:`Bearer ${token}`, ...(body ? {'Content-Type':'application/json'} : {}) },
    ...(body ? { body:JSON.stringify(body) } : {}) });
  const data = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(response.status === 401 ? 'Owner token not accepted.' : data.error || 'The plan service is unavailable.');
  return data;
}

function planElement(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function planButton(label, kind, action) {
  const button = planElement('button', `approval-btn ${kind}`, label);
  button.type = 'button';
  button.addEventListener('click', async event => {
    event.stopPropagation();
    button.disabled = true;
    try { await action(); } catch (error) { showError(error.message); } finally { button.disabled = false; }
  });
  return button;
}

/** The plan is effectively finished, or needs the owner before it can go on. */
function planSettled(view) {
  return PLAN_TERMINAL.has(view.plan.state) || view.task_state === 'needs_intervention';
}

function renderPlanCard(view) {
  if (!view?.plan?.id) return;
  const { plan } = view;
  let card = document.getElementById(`plan-${plan.id}`);
  if (!card) {
    card = planElement('article', 'artifact-card plan-card');
    card.id = `plan-${plan.id}`;
    document.getElementById('canvasEmptyState')?.remove();
    document.getElementById('canvasPanel')?.prepend(card);
  }
  planCards.set(plan.id, view);

  const heading = planElement('div', 'artifact-heading');
  const attention = view.task_state === 'needs_intervention';
  heading.append(planElement('span', null, `Plan · revision ${plan.revision}`),
    planElement('span', `plan-state ${attention ? 'needs_attention' : plan.state}`, attention ? 'Needs your attention' : PLAN_LABELS[plan.state] || plan.state));

  const steps = planElement('ol', 'plan-steps');
  for (const step of plan.steps) {
    const item = planElement('li', `plan-step ${step.state}`);
    item.append(planElement('span', 'plan-step-icon', STEP_ICONS[step.state] || '○'));
    const body = planElement('div', 'plan-step-body');
    body.append(planElement('span', 'plan-step-intent', step.intent));
    const meta = planElement('span', 'plan-step-meta', step.tool);
    if (step.effectful) meta.append(planElement('span', 'plan-effect', 'changes something'));
    body.append(meta);
    const error = step.receipt?.error || (step.receipt && !step.receipt.check_passed && step.receipt.success ? 'The result did not pass its check' : null);
    if (error) body.append(planElement('span', 'plan-step-error', error));
    item.append(body);
    steps.append(item);
  }

  const reason = view.task_reason || plan.reason;
  const history = planElement('details', 'plan-history');
  history.append(planElement('summary', null, 'History'));
  for (const line of plan.history) history.append(planElement('p', 'sheet-note', line));

  const actions = planElement('div', 'approval-actions');
  if (plan.state === 'awaiting_approval') {
    actions.append(planButton('Approve plan', 'approve', async () => renderPlanCard((await planRequest(`/${plan.id}/approve`, {})).plan)),
      planButton('Edit', 'always', async () => editPlan(view)));
  }
  if (attention) {
    actions.append(planButton('It did not happen: retry', 'approve', async () => renderPlanCard((await planRequest(`/${plan.id}/resolve`, { retry:true })).plan)),
      planButton('Cancel plan', 'deny', async () => renderPlanCard((await planRequest(`/${plan.id}/resolve`, { retry:false })).plan)));
  }
  if (!planSettled(view)) {
    actions.append(planButton('Cancel', 'deny', async () => renderPlanCard((await planRequest(`/${plan.id}/cancel`, {})).plan)));
  }
  const waiting = planElement('p', 'sheet-note plan-guardian');
  waiting.hidden = true;

  card.replaceChildren(heading, planElement('p', 'approval-rationale', plan.goal), steps,
    ...(reason ? [planElement('p', 'plan-reason', reason)] : []), waiting, actions, history);
  if (plan.state === 'running') showGuardianWaits(plan, waiting);
}

/** A running step may be waiting for a Guardian approval bound to this plan: offer it here. */
async function showGuardianWaits(plan, slot) {
  let pending;
  try { pending = (await ownerApprovalRequest('approvals')).pending || []; } catch (_) { return; }
  const mine = pending.filter(request => request.correlation_id === plan.correlation_id);
  if (!mine.length) return;
  slot.hidden = false;
  slot.replaceChildren(planElement('span', null, 'A step is waiting for Guardian approval. '));
  for (const request of mine) {
    const exact = planElement('details', 'plan-exact');
    exact.append(planElement('summary', null, 'Exact request'), planElement('pre', 'approval-target', JSON.stringify(request.capability, null, 2)));
    slot.append(
      planButton('Approve this step', 'approve', async () => { await ownerApprovalRequest('resolve', { session_id:request.session_id, approved:true }); slot.hidden = true; }),
      planButton('Deny', 'deny', async () => { await ownerApprovalRequest('revoke', { session_id:request.session_id }); slot.hidden = true; }),
      exact);
  }
}

/** Edit the steps that have not run, as JSON; the server validates the edit like a draft. */
function editPlan(view) {
  const remaining = view.plan.steps.filter(step => step.state !== 'done')
    .map(({ id, intent, tool, input, depends_on, check }) => ({ id, intent, tool, input, depends_on, check }));
  const body = planElement('div');
  body.append(planElement('p', 'sheet-note', 'Change, remove or reorder the steps that have not run. Each step needs an id, an intent, a registered tool and its input. The plan is checked again before it is saved.'));
  const editor = planElement('textarea', 'plan-editor');
  editor.value = JSON.stringify({ steps:remaining }, null, 2);
  editor.spellcheck = false;
  body.append(editor);
  const footer = planElement('div', 'approval-actions');
  footer.append(planButton('Save changes', 'approve', async () => {
    let draft;
    try { draft = JSON.parse(editor.value); } catch (_) { throw new Error('The plan is not valid JSON.'); }
    renderPlanCard((await planRequest(`/${view.plan.id}/edit`, draft)).plan);
    document.getElementById('cardModal')?.close();
  }));
  openCardModal({ icon:'▤', title:'Edit plan', badge:`revision ${view.plan.revision}`, bodyNode:body, footerNode:footer });
}

async function refreshPlans() {
  const open = [...planCards.values()].filter(view => !planSettled(view));
  for (const view of open) {
    try { renderPlanCard((await planRequest(`/${view.plan.id}`)).plan); } catch (_) {}
  }
}

/** Plans outlive the page: show the ones still in progress or needing the owner. */
async function loadPlans() {
  try {
    const { plans } = await planRequest('');
    for (const view of [...plans].reverse()) if (!planSettled(view) || view.task_state === 'needs_intervention') renderPlanCard(view);
  } catch (error) {
    console.debug('Could not load plans:', error);
  }
}

loadPlans();
setInterval(refreshPlans, 4000);

'use strict';
let artifactSerial = 0;

function openCardModal({ icon = '▧', title = 'Details', badge = '', badgeClass = '', bodyNode, footerNode = null }) {
  const dialog = document.getElementById('cardModal');
  if (!dialog) return;
  document.getElementById('cardModalIcon').textContent = icon;
  document.getElementById('cardModalTitle').textContent = title;
  const badgeEl = document.getElementById('cardModalBadge');
  badgeEl.textContent = badge;
  badgeEl.className = `card-modal-badge ${badgeClass}`;

  const bodyEl = document.getElementById('cardModalBody');
  bodyEl.replaceChildren();
  if (bodyNode) bodyEl.append(bodyNode);

  const footerEl = document.getElementById('cardModalFooter');
  footerEl.replaceChildren();
  if (footerNode) footerEl.append(footerNode);
  const closeBtn = document.createElement('button');
  closeBtn.type = 'button';
  closeBtn.className = 'quiet-button';
  closeBtn.textContent = 'Close';
  closeBtn.addEventListener('click', () => dialog.close());
  footerEl.append(closeBtn);

  dialog.showModal();
}

const SailToolRenderer = {
  renderers: new Map(),

  register(name, rendererFn) {
    this.renderers.set(name, rendererFn);
  },

  renderToolMessage(msg) {
    if (!msg) return null;
    const conv = document.getElementById('conversation');
    if (!conv) return null;

    const id = `toolMsg-${msg.invocation_id || msg.message_id || 'unknown'}`;
    let card = document.getElementById(id);
    const isNew = !card;
    if (isNew) {
      card = document.createElement('article');
      card.id = id;
      card.className = 'message message-tool';
      card.setAttribute('data-invocation-id', msg.invocation_id || '');
      card.setAttribute('data-tool', msg.tool_name || '');
      conv.append(card);
    }

    card.setAttribute('data-status', msg.status || 'requested');
    this.populateCard(card, msg);

    if (isNew && typeof chatFollow !== 'undefined' && chatFollow?.resume) {
      chatFollow.resume();
    }
    return card;
  },

  populateCard(card, msg) {
    card.replaceChildren();

    const header = document.createElement('div');
    header.className = 'tool-msg-header';

    const title = document.createElement('span');
    title.className = 'tool-msg-title';
    const toolIcon = this.getToolIcon(msg.tool_name);
    title.innerHTML = `${toolIcon} ${String(msg.tool_name || 'Tool').replaceAll(':', ' · ')}`;

    const rightWrap = document.createElement('div');
    rightWrap.style.display = 'flex';
    rightWrap.style.alignItems = 'center';
    rightWrap.style.gap = '8px';

    const statusPill = document.createElement('span');
    const status = msg.status || 'requested';
    statusPill.className = `tool-status-pill status-${status}`;
    statusPill.textContent = status === 'executing' ? '● Executing…' :
                             status === 'completed' ? '● Done' :
                             status === 'failed' ? '● Failed' :
                             status === 'cancelled' ? '● Cancelled' : '● Requested';

    const expandBtn = document.createElement('button');
    expandBtn.type = 'button';
    expandBtn.className = 'card-expand-btn';
    expandBtn.title = 'Expand details into pop-up';
    expandBtn.setAttribute('aria-label', 'Expand details into pop-up');
    expandBtn.textContent = '⤢';

    rightWrap.append(statusPill, expandBtn);
    header.append(title, rightWrap);
    card.append(header);

    const body = document.createElement('div');
    body.className = 'tool-msg-body';

    if (status === 'requested' || status === 'executing') {
      const pendingEl = document.createElement('div');
      pendingEl.className = 'tool-pending-state';
      const text = document.createElement('p');
      text.className = 'result-summary';
      text.textContent = status === 'executing' ? `Executing ${msg.tool_name}…` : `Requested ${msg.tool_name}`;
      pendingEl.append(text);
      body.append(pendingEl);
    } else if (msg.payload) {
      this.renderPayload(body, msg.payload, msg, expandBtn, card);
    }

    card.append(body);

    if (Array.isArray(msg.artifact_refs) && msg.artifact_refs.length > 0) {
      const refsWrap = document.createElement('div');
      refsWrap.className = 'tool-artifact-refs';
      for (const ref of msg.artifact_refs) {
        const link = document.createElement('span');
        link.className = 'tool-artifact-tag';
        link.textContent = `📎 ${ref.artifact_id} (${ref.kind})`;
        refsWrap.append(link);
      }
      card.append(refsWrap);
    }
  },

  getToolIcon(toolName = '') {
    if (toolName.startsWith('vision') || toolName.startsWith('camera') || toolName.includes('snapshot')) return '📷';
    if (toolName.startsWith('chart') || toolName.includes('diagram') || toolName.includes('visual')) return '📊';
    if (toolName.startsWith('fs:') || toolName.includes('file')) return '📁';
    if (toolName.startsWith('web:')) return '🌐';
    if (toolName.startsWith('github:') || toolName.startsWith('forge:')) return '🔨';
    return '⚙';
  },

  renderPayload(container, payload, msg, expandBtn, card) {
    const custom = this.renderers.get(msg.tool_name);
    if (custom) {
      custom(container, payload, msg, expandBtn, card);
      return;
    }

    if (payload.CameraCapture) {
      this.renderCameraCapture(container, payload.CameraCapture, msg, expandBtn, card);
    } else if (payload.Visualization) {
      this.renderVisualization(container, payload.Visualization, msg, expandBtn, card);
    } else if (payload.Table) {
      this.renderTable(container, payload.Table, msg, expandBtn, card);
    } else if (payload.Document) {
      this.renderDocument(container, payload.Document, msg, expandBtn, card);
    } else if (payload.InteractiveControls) {
      this.renderInteractiveControls(container, payload.InteractiveControls, msg, expandBtn, card);
    } else if (payload.ExecutionResult) {
      this.renderExecutionResult(container, payload.ExecutionResult, msg, expandBtn, card);
    } else {
      const summary = document.createElement('p');
      summary.className = 'result-summary';
      summary.textContent = typeof payload === 'string' ? payload : JSON.stringify(payload);
      container.append(summary);
    }
  },

  renderCameraCapture(container, data, msg, expandBtn, card) {
    const dataUrl = `data:${data.format || 'image/jpeg'};base64,${data.data_base64}`;
    const img = document.createElement('img');
    img.className = 'tool-snapshot-thumb';
    img.src = dataUrl;
    img.alt = 'Captured visual';
    container.append(img);

    const meta = document.createElement('div');
    meta.className = 'tool-meta-bar';
    const dims = data.width && data.height ? ` · ${data.width}×${data.height}` : '';
    meta.textContent = `${data.source || 'camera'}${dims} · ${data.timestamp ? new Date(data.timestamp).toLocaleTimeString() : new Date().toLocaleTimeString()}`;
    container.append(meta);

    const openModal = () => {
      const full = document.createElement('img');
      full.src = dataUrl;
      full.style.maxWidth = '100%';
      full.style.maxHeight = '70vh';
      full.style.objectFit = 'contain';
      openCardModal({
        icon: '📷',
        title: 'Camera Snapshot',
        badge: data.source || 'Local',
        badgeClass: 'result-success',
        bodyNode: full
      });
    };
    card.addEventListener('click', openModal);
    expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });
  },

  renderVisualization(container, data, msg, expandBtn, card) {
    if (data.format === 'mermaid' && window.mermaid) {
      const preview = document.createElement('div');
      preview.className = 'artifact-mermaid-preview';
      container.append(preview);
      mermaid.render(`tool-diag-${++artifactSerial}`, data.spec).then(res => {
        preview.innerHTML = res.svg;
        const openModal = () => {
          const full = document.createElement('div');
          full.className = 'modal-mermaid-container';
          full.innerHTML = res.svg;
          openCardModal({
            icon: '📊',
            title: data.title || 'Diagram',
            badge: 'Mermaid',
            badgeClass: 'result-success',
            bodyNode: full
          });
        };
        card.addEventListener('click', openModal);
        expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });
      }).catch(() => {
        preview.textContent = data.spec;
      });
    } else if (data.format === 'svg') {
      const img = document.createElement('img');
      img.alt = data.title || 'Generated visual';
      img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(data.spec)}`;
      img.className = 'tool-snapshot-thumb';
      container.append(img);
      const openModal = () => {
        const full = document.createElement('img');
        full.src = img.src;
        openCardModal({ icon: '📊', title: data.title || 'Visual', bodyNode: full });
      };
      card.addEventListener('click', openModal);
      expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });
    }
  },

  renderTable(container, data, msg, expandBtn, card) {
    const wrap = document.createElement('div');
    wrap.className = 'tool-table-wrap';
    const table = document.createElement('table');
    table.className = 'tool-table';

    if (Array.isArray(data.columns)) {
      const thead = document.createElement('thead');
      const tr = document.createElement('tr');
      for (const col of data.columns) {
        const th = document.createElement('th');
        th.textContent = col;
        tr.append(th);
      }
      thead.append(tr);
      table.append(thead);
    }

    if (Array.isArray(data.rows)) {
      const tbody = document.createElement('tbody');
      for (const row of data.rows.slice(0, 5)) {
        const tr = document.createElement('tr');
        for (const cell of (Array.isArray(row) ? row : Object.values(row))) {
          const td = document.createElement('td');
          td.textContent = String(cell ?? '—');
          tr.append(td);
        }
        tbody.append(tr);
      }
      table.append(tbody);
    }
    wrap.append(table);
    container.append(wrap);

    const openModal = () => {
      const fullTable = table.cloneNode(true);
      openCardModal({
        icon: '📋',
        title: 'Structured Table',
        badge: `${data.rows?.length || 0} rows`,
        bodyNode: fullTable
      });
    };
    card.addEventListener('click', openModal);
    expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });
  },

  renderDocument(container, data, msg, expandBtn, card) {
    const docWrap = document.createElement('div');
    docWrap.className = 'tool-doc-preview';
    const h = document.createElement('h4');
    h.textContent = data.title || 'Document';
    const pre = document.createElement('pre');
    pre.className = 'tool-code-preview';
    pre.textContent = data.content.slice(0, 300) + (data.content.length > 300 ? '…' : '');
    docWrap.append(h, pre);
    container.append(docWrap);

    const openModal = () => {
      const fullPre = document.createElement('pre');
      fullPre.style.whiteSpace = 'pre-wrap';
      fullPre.textContent = data.content;
      openCardModal({
        icon: '📄',
        title: data.title || 'Document',
        badge: data.format || 'Text',
        bodyNode: fullPre
      });
    };
    card.addEventListener('click', openModal);
    expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });
  },

  renderInteractiveControls(container, data, msg, expandBtn, card) {
    if (data.prompt) {
      const p = document.createElement('p');
      p.className = 'result-summary';
      p.textContent = data.prompt;
      container.append(p);
    }
    const bar = document.createElement('div');
    bar.className = 'tool-actions-bar';
    for (const ctrl of data.controls || []) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'quiet-button mini-btn';
      btn.textContent = ctrl.label;
      if (ctrl.disabled) btn.disabled = true;
      btn.addEventListener('click', (e) => {
        e.stopPropagation();
        if (typeof sendAction === 'function') sendAction(ctrl.action);
      });
      bar.append(btn);
    }
    container.append(bar);
  },

  renderExecutionResult(container, data, msg, expandBtn, card) {
    const p = document.createElement('p');
    p.className = 'result-summary';
    p.textContent = data.summary || (data.success ? 'The operation completed successfully.' : (data.error || 'Operation failed.'));
    container.append(p);

    const openModal = () => {
      const pre = document.createElement('pre');
      pre.textContent = JSON.stringify(data.output || data, null, 2);
      openCardModal({
        icon: '⚙',
        title: msg.tool_name,
        badge: data.success ? '● Success' : '● Error',
        badgeClass: data.success ? 'result-success' : 'result-failure',
        bodyNode: pre
      });
    };
    card.addEventListener('click', openModal);
    expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });
  }
};
if (typeof window !== 'undefined') window.SailToolRenderer = SailToolRenderer;

async function renderArtifact(artifact) {
  if (!artifact || typeof artifact.content !== 'string' || !['mermaid', 'json', 'image', 'svg'].includes(artifact.artifact_type)) return;
  const container = document.createElement('div'); container.className = 'artifact-card message-tool';
  const conv = document.getElementById('conversation');
  const canvasPanel = document.getElementById('canvasPanel');
  if (conv) {
    conv.append(container);
    if (typeof chatFollow !== 'undefined' && chatFollow?.resume) chatFollow.resume();
  }
  if (canvasPanel) {
    if (!conv) {
      canvasPanel.append(container);
    } else if (typeof container.cloneNode === 'function') {
      try {
        canvasPanel.append(container.cloneNode(true));
      } catch {}
    }
  }
  document.getElementById('canvasEmptyState')?.remove();

  const heading = document.createElement('div'); heading.className = 'artifact-heading';
  const title = document.createElement('span');
  title.textContent = artifact.artifact_type === 'mermaid' ? 'Diagram' : artifact.artifact_type === 'json' ? 'Structured Data' : 'Visual';

  const rightWrap = document.createElement('div');
  rightWrap.style.display = 'flex';
  rightWrap.style.alignItems = 'center';
  rightWrap.style.gap = '8px';
  const badge = document.createElement('span');
  badge.className = 'approval-badge';
  badge.textContent = artifact.artifact_type.toUpperCase();
  const expandBtn = document.createElement('button');
  expandBtn.type = 'button';
  expandBtn.className = 'card-expand-btn';
  expandBtn.title = 'Expand into pop-up';
  expandBtn.setAttribute('aria-label', 'Expand into pop-up');
  expandBtn.textContent = '⤢';
  rightWrap.append(badge, expandBtn);
  heading.append(title, rightWrap);
  container.append(heading);

  if (artifact.artifact_type === 'mermaid' && window.mermaid) {
    try {
      const rendered = await mermaid.render(`diagram-${++artifactSerial}`, artifact.content);
      const preview = document.createElement('div');
      preview.className = 'artifact-mermaid-preview';
      preview.innerHTML = rendered.svg;
      container.append(preview);
      const hint = document.createElement('div');
      hint.className = 'card-expand-hint';
      hint.innerHTML = '<span aria-hidden="true">⤢</span> Click to inspect diagram in pop-up';
      container.append(hint);

      const openModal = () => {
        const fullView = document.createElement('div');
        fullView.className = 'modal-mermaid-container';
        fullView.innerHTML = rendered.svg;
        openCardModal({
          icon: '▧',
          title: 'Diagram',
          badge: 'Mermaid',
          badgeClass: 'result-success',
          bodyNode: fullView
        });
      };
      container.addEventListener('click', openModal);
      expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });
      return;
    } catch { /* Show source if a model emits an invalid diagram. */ }
  }

  if (artifact.artifact_type === 'json') {
    try {
      const parsed = JSON.parse(artifact.content);
      const fields = document.createElement('dl'); fields.className = 'json-fields';
      for (const [key, value] of Object.entries(parsed).slice(0, 4)) {
        const term = document.createElement('dt'); term.textContent = key.replaceAll('_', ' ');
        const description = document.createElement('dd');
        description.textContent = value === null ? 'None' : typeof value === 'object' ? `${Array.isArray(value) ? value.length : Object.keys(value).length} items` : String(value);
        fields.append(term, description);
      }
      container.append(fields);
      const hint = document.createElement('div');
      hint.className = 'card-expand-hint';
      hint.innerHTML = '<span aria-hidden="true">⤢</span> View full data in pop-up';
      container.append(hint);

      const openModal = () => {
        const modalContent = document.createElement('div');
        const allFields = document.createElement('dl'); allFields.className = 'json-fields';
        for (const [key, value] of Object.entries(parsed)) {
          const term = document.createElement('dt'); term.textContent = key.replaceAll('_', ' ');
          const description = document.createElement('dd');
          description.textContent = value === null ? 'None' : typeof value === 'object' ? `${Array.isArray(value) ? value.length : Object.keys(value).length} items` : String(value);
          allFields.append(term, description);
        }
        const pre = document.createElement('pre');
        pre.textContent = JSON.stringify(parsed, null, 2);
        modalContent.append(allFields, pre);

        const copyBtn = document.createElement('button');
        copyBtn.type = 'button';
        copyBtn.className = 'quiet-button';
        copyBtn.textContent = 'Copy JSON';
        copyBtn.addEventListener('click', () => {
          navigator.clipboard.writeText(JSON.stringify(parsed, null, 2));
          copyBtn.textContent = 'Copied!';
          setTimeout(() => { copyBtn.textContent = 'Copy JSON'; }, 2000);
        });

        openCardModal({
          icon: '▧',
          title: 'Structured Data',
          badge: `${Object.keys(parsed).length} keys`,
          badgeClass: '',
          bodyNode: modalContent,
          footerNode: copyBtn
        });
      };
      container.addEventListener('click', openModal);
      expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });
      return;
    } catch { /* Show invalid output safely as text. */ }
  }

  if (artifact.artifact_type === 'image' && /^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(artifact.content)) {
    const image = document.createElement('img'); image.src = artifact.content; image.alt = 'Conversation visual';
    container.append(image);
    const hint = document.createElement('div');
    hint.className = 'card-expand-hint';
    hint.innerHTML = '<span aria-hidden="true">⤢</span> View full image in pop-up';
    container.append(hint);

    const openModal = () => {
      const fullImg = document.createElement('img'); fullImg.src = artifact.content; fullImg.alt = 'Conversation visual';
      openCardModal({
        icon: '🖼',
        title: 'Image Visual',
        badge: 'Local',
        badgeClass: 'result-success',
        bodyNode: fullImg
      });
    };
    container.addEventListener('click', openModal);
    expandBtn.addEventListener('click', (e) => { e.stopPropagation(); openModal(); });
    return;
  }

  if (artifact.artifact_type === 'svg' && artifact.content.trim().startsWith('<svg')) {
    // or event handlers in the owner approval page. Never insert it as page markup.
    const image = document.createElement('img'); image.alt = 'Generated visual';
    image.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(artifact.content)}`;
    container.append(image);
    const openModal=()=>{const full=document.createElement('img');full.alt='Generated visual';full.src=image.src;openCardModal({title:'Generated visual',bodyNode:full});};
    container.addEventListener('click',openModal);expandBtn.addEventListener('click',event=>{event.stopPropagation();openModal();});return;
  }
}

'use strict';
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

async function renderArtifact(artifact) {
  if (!artifact || typeof artifact.content !== 'string' || !['mermaid', 'json', 'image', 'svg'].includes(artifact.artifact_type)) return;
  const container = document.createElement('div'); container.className = 'artifact-card';
  document.getElementById('canvasPanel').append(container);
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

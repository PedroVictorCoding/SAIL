'use strict';

// Matches sail_common::SubsystemId; Agents and Forge are separate workspaces.
const subsystemPages = [
  {
    "id": "core",
    "name": "Core",
    "icon": "home",
    "subtitle": "System coordination",
    "description": "The local runtime connects SAIL’s subsystems and coordinates their work.",
    "card": "systemCard",
    "panel": "systemPanelMetrics",
    "settings": null,
    "details": [
      "Device health and resource use",
      "Event-driven coordination",
      "Local service lifecycle"
    ]
  },
  {
    "id": "dialogue",
    "name": "Dialogue",
    "icon": "dialogue",
    "subtitle": "Voice and conversation",
    "description": "Talk with SAIL and manage speech, interruption, and turn taking.",
    "card": "sessionCard",
    "panel": "sessionPanelMain",
    "settings": "voice",
    "details": [
      "Voice session and audio diagnostics",
      "Streaming conversation",
      "Microphone and turn-taking preferences"
    ]
  },
  {
    "id": "arena",
    "name": "Arena",
    "icon": "arena",
    "subtitle": "Knowledge and memory",
    "description": "Connect memories, research collections, and semantic relationships.",
    "card": "memoryCard",
    "panel": "memoryPanelMain",
    "settings": null,
    "details": [
      "Episodic memory",
      "Associative knowledge graph",
      "Research collections"
    ]
  },
  {
    "id": "canvas",
    "name": "Canvas",
    "icon": "canvas",
    "subtitle": "Visual workspace",
    "description": "Explore visual answers, diagrams, and the camera and desktop previews you choose to share.",
    "card": "canvasCard",
    "panel": "canvasPanelMain",
    "settings": null,
    "details": [
      "Visual answers and diagrams",
      "Structured conversation results",
      "Camera and desktop previews"
    ]
  },
  {
    "id": "flow",
    "name": "Flow",
    "icon": "flow",
    "subtitle": "Workflows and automation",
    "description": "Coordinate workflows through triggers, conditions, and scheduled execution.",
    "card": null,
    "panel": null,
    "settings": null,
    "details": [
      "Workflow triggers",
      "Automation nodes and conditions",
      "Execution scheduling"
    ]
  },
  {
    "id": "discovery",
    "name": "Discovery",
    "icon": "discovery",
    "subtitle": "Research and exploration",
    "description": "Explore learning resources and knowledge gaps through SAIL’s curiosity engine.",
    "card": null,
    "panel": null,
    "settings": null,
    "details": [
      "Knowledge-gap identification",
      "Learning resource discovery",
      "Research interests"
    ]
  },
  {
    "id": "agents",
    "name": "Agents",
    "icon": "agents",
    "subtitle": "Task coordination",
    "description": "Break down goals, coordinate workers, and verify the steps of a task.",
    "card": "toolsCard",
    "panel": "toolsPanelMain",
    "settings": null,
    "details": [
      "Goal decomposition",
      "Tool proposals and authorization",
      "Step verification",
      "Plan cards on Canvas, with approval, edit and cancel"
    ]
  },
  {
    "id": "mind",
    "name": "Mind",
    "icon": "mind",
    "subtitle": "Models and inference",
    "description": "Inspect local language and speech models alongside compute allocation.",
    "card": "modelsCard",
    "panel": "modelsPanelMain",
    "settings": "models",
    "details": [
      "Local model availability",
      "Speech and language services",
      "Compute routing and resource budgets"
    ]
  },
  {
    "id": "bridge",
    "name": "Bridge",
    "icon": "bridge",
    "subtitle": "Devices and local tools",
    "description": "Connect SAIL to host tools, files, applications, and peripherals through authorized actions.",
    "card": "toolsCard",
    "panel": "toolsPanelMain",
    "settings": null,
    "details": [
      "Host and device integration",
      "Authorized filesystem and system tools",
      "Application connectors"
    ]
  },
  {
    "id": "guardian",
    "name": "Guardian",
    "icon": "guardian",
    "subtitle": "Security and approvals",
    "description": "Review security preferences and approvals for the actions SAIL may perform.",
    "card": "toolsCard",
    "panel": "toolsPanelSafetyReview",
    "settings": "guardian",
    "details": [
      "Capability grants and access boundaries",
      "Owner approvals and autonomy preferences",
      "Safety review findings"
    ]
  },
  {
    "id": "forge",
    "name": "Forge",
    "icon": "agents",
    "subtitle": "Autonomous coding",
    "description": "Queue coding tasks, follow their progress, and inspect patches and review outcomes.",
    "card": "forgeCard",
    "panel": null,
    "settings": "forge",
    "details": [
      "Coding queue and progress",
      "Isolated workspaces and verification",
      "Patch and review results"
    ]
  }
];

function initializeSubsystemPages() {
  const workspace = document.getElementById('workspace');
  const home = document.getElementById('homePage');
  const page = document.getElementById('subsystemPage');
  const title = document.getElementById('subsystemTitle');
  const content = document.getElementById('subsystemContent');
  const cardHomes = new Map();
  let currentPage = 'home';
  let requestedPanel = null;
  for (const id of new Set(subsystemPages.map(item => item.card).filter(Boolean))) {
    const card = document.getElementById(id);
    const marker = document.createComment(`${id} Home position`);
    card.before(marker);
    cardHomes.set(card, marker);
  }
  function pageLink(item, className) {
    const link = document.createElement('a');
    link.className = className; link.href = `#${item.id}`; link.dataset.page = item.id;
    const icon = document.createElement('span'); icon.className = 'nav-icon'; icon.setAttribute('aria-hidden', 'true');
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    const use = document.createElementNS(svg.namespaceURI, 'use');
    use.setAttribute('href', `/icons.svg#${item.icon}`); svg.append(use); icon.append(svg);
    const label = document.createElement('span'); label.textContent = item.name;
    link.append(icon, label); return link;
  }
  for (const item of subsystemPages) {
    document.getElementById('subsystemNav').append(pageLink(item, 'quiet-button nav-item'));
  }
  function renderRoute(focus = true) {
    const id = location.hash.slice(1) || 'home';
    if (id === 'spaces') return; // Spaces overlays the current workspace.
    const item = subsystemPages.find(entry => entry.id === id);
    const route = item?.id || 'home';
    if (!item && id !== 'home') history.replaceState(null, '', '#home');
    closeSheet();
    // Move original nodes so socket updates, form state, and listeners stay live.
    for (const [card, marker] of cardHomes) marker.after(card);
    content.replaceChildren(); home.hidden = !!item; page.hidden = !item;
    workspace.setAttribute('aria-label', `${item?.name || 'Home'} workspace`);
    document.title = `SAIL — ${item?.name || 'Home'}`;
    document.querySelectorAll('[data-page]').forEach(link => {
      const selected = link.dataset.page === route; link.classList.toggle('selected', selected);
      if (selected) link.setAttribute('aria-current', 'page'); else link.removeAttribute('aria-current');
    });
    if (item) {
      title.textContent = item.name;
      document.getElementById('subsystemDescription').textContent = item.description;
      document.getElementById('subsystemEyebrow').textContent = item.subtitle;
      const list = document.getElementById('subsystemCapabilities'); list.replaceChildren();
      for (const text of item.details) { const li = document.createElement('li'); li.textContent = text; list.append(li); }
      const notice = document.getElementById('subsystemNotice'); notice.hidden = !!item.card;
      notice.textContent = item.card ? '' : `${item.name} controls and results are not connected to this interface yet.`;
      const settings = document.getElementById('subsystemSettings'); settings.hidden = !item.settings;
      settings.onclick = () => { openSheet(contextSheet); contextSheet.querySelector(`[data-settings-tab="${item.settings}"]`)?.click(); };
      if (item.card) { content.append(document.getElementById(item.card)); if (item.panel) switchCardTab(item.card, requestedPanel || item.panel); }

    }
    currentPage = route; requestedPanel = null; workspace.scrollTop = 0;
    window.dispatchEvent(new CustomEvent('sail:page', {detail:route}));
    if (focus) (item ? title : document.getElementById('welcomeGreeting')).focus({ preventScroll:true });
  }
  window.sailNavigate = (id, panel = null) => {
    requestedPanel = panel;
    if (location.hash === `#${id}`) renderRoute(); else location.hash = id;
  };
  window.sailCurrentPage = () => currentPage;
  document.getElementById('homeNav').addEventListener('click', event => {
    if (currentPage === 'home' && location.hash !== '#spaces') { event.preventDefault(); renderRoute(); }
  });
  window.addEventListener('hashchange', () => renderRoute()); renderRoute(false);
}

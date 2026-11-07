'use strict';

// Mostly matches sail_common::SubsystemId; Agents and Forge are separate workspaces, and
// Memory is Arena's store shown on its own page. `tabs` limits a shared card to the
// tabs that belong to the page.
const SETTINGS_ROUTES = {dialogue: 'voice', core: 'developer', system: 'developer', mind: 'models', guardian: 'guardian'};

const subsystemPages = [
  {
    "id": "factory",
    "name": "Factory",
    "icon": "factory",
    "subtitle": "Live tools map",
    "description": "Your workspace: every system on one live map, with Dialogue beside it.",
    "card": null,
    "panel": null,
    "settings": null,
    "details": [
      "Subsystems and their connections on one map",
      "Select a station to inspect its status and tasks",
      "Pan and zoom the map, or switch to the list view"
    ]
  },
  {
    "id": "arena",
    "name": "Arena",
    "icon": "arena",
    "subtitle": "Learn step by step",
    "description": "Learn something step by step with an assessment, a lesson map and guided teaching.",
    "card": "memoryCard",
    "panel": "learnPanelMain",
    "tabs": ["learnPanelMain"],
    "settings": null,
    "details": [
      "Arena Learn: assessment, a lesson map and step-by-step teaching",
      "Research collections"
    ]
  },
  {
    "id": "memory",
    "name": "Memory",
    "icon": "mesh",
    "subtitle": "What SAIL remembers",
    "description": "Search, review and export what SAIL remembers from your conversations.",
    "card": "memoryCard",
    "panel": "memoryPanelMain",
    "tabs": ["memoryPanelMain"],
    "settings": null,
    "details": [
      "Episodic memory",
      "Associative knowledge graph",
      "Change history and export"
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
    "id": "away",
    "name": "While You Were Away",
    "icon": "discovery",
    "subtitle": "Your return briefing",
    "description": "Review the prepared music, learning, events and continuity for your return.",
    "card": null,
    "panel": null,
    "settings": null,
    "details": [
      "Staged music and learning",
      "Upcoming events and relevant news",
      "Project and host continuity"
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
    "id": "forge",
    "name": "Forge",
    "icon": "forge",
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
  },
  {
    "id": "pulse",
    "name": "Pulse",
    "icon": "pulse",
    "subtitle": "Reminders and timing",
    "description": "Create local reminders, snooze or cancel them, and choose quiet hours and notification preferences.",
    "card": "pulseCard",
    "panel": null,
    "settings": null,
    "details": [
      "One-time local reminders that survive restarts",
      "Snooze, cancel and one-time delivery",
      "Quiet hours and opt-in spoken reminders"
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
    if (!card) continue;
    const marker = document.createComment(`${id} Home position`);
    card.before(marker);
    cardHomes.set(card, marker);
  }
  // Hide the tabs of a shared card that belong to another page; with one tab left, hide the row.
  function showCardTabs(card, allowed) {
    const tabs = [...card.querySelectorAll('.card-tab')];
    for (const tab of tabs) tab.hidden = !!allowed && !allowed.includes(tab.dataset.tabPanel);
    for (const list of card.querySelectorAll('.card-tabs')) list.hidden = !!allowed && tabs.filter(tab => !tab.hidden).length < 2;
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
    document.getElementById('subsystemNav')?.append(pageLink(item, 'quiet-button nav-item'));
  }
  // Routes: #home (also #factory) is the Tools Map workspace; a subsystem id (or #tracker)
  // replaces the map in the workspace's centre (toolsmap.js).
  function renderRoute(focus = true) {
    // Fragments may carry a deep-link query (`#arena?practice=…`); the route is the id.
    let id = location.hash.slice(1).split('?')[0] || 'home';
    const settingsCategory = SETTINGS_ROUTES[id];
    if (settingsCategory) {
      history.replaceState(null, '', '#home');
      id = 'home';
    }
    if (id === 'discovery') {
      history.replaceState(null, '', '#away');
      id = 'away';
    }
    if (id === 'classic') {
      history.replaceState(null, '', '#home');
      id = 'home';
    }
    if (id === 'spaces') return; // Spaces overlays the current workspace.
    const found = subsystemPages.find(entry => entry.id === id);
    const item = found?.id === 'factory' ? null : found;
    const route = item?.id || (id === 'tracker' || id === 'entertainment' ? id : 'home');
    if (!found && route === 'home' && id !== 'home') history.replaceState(null, '', '#home');
    closeSheet();
    // Move original nodes so socket updates, form state, and listeners stay live.
    for (const [card, marker] of cardHomes) { marker.after(card); showCardTabs(card, null); }
    content.replaceChildren();
    if (home) home.hidden = true;
    page.hidden = !item;
    if (workspace) workspace.setAttribute('aria-label', `${item?.name || (route === 'entertainment' ? 'Entertainment' : 'Home')} workspace`);
    document.title = `SAIL — ${item?.name || {tracker: 'Tracker', entertainment: 'Entertainment'}[route] || 'Workspace'}`;
    document.querySelectorAll('[data-page]').forEach(link => {
      const page = link.dataset.page === 'factory' ? 'home' : link.dataset.page;
      const selected = page === route; link.classList.toggle('selected', selected);
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
      if (item.card) {
        const card = document.getElementById(item.card); content.append(card); showCardTabs(card, item.tabs);
        const panel = item.tabs && !item.tabs.includes(requestedPanel) ? item.panel : requestedPanel || item.panel;
        if (panel) switchCardTab(item.card, panel);
      }

    }
    currentPage = route; requestedPanel = null;
    if (workspace) workspace.scrollTop = 0;
    window.dispatchEvent(new CustomEvent('sail:page', {detail:route}));
    if (settingsCategory) window.sailOpenSettings(settingsCategory);
    // The map restores focus to whatever opened a closed popup.
    if (focus && item) title.focus({ preventScroll:true });
  }
  window.sailNavigate = (id, panel = null) => {
    if (SETTINGS_ROUTES[id]) {
      window.sailOpenSettings(SETTINGS_ROUTES[id]);
      return;
    }
    requestedPanel = panel;
    if (location.hash === `#${id}`) renderRoute(); else location.hash = id;
  };
  window.sailCurrentPage = () => currentPage;
  document.getElementById('homeNav')?.addEventListener('click', event => {
    if (currentPage === 'home' && location.hash !== '#spaces') { event.preventDefault(); renderRoute(); }
  });
  window.addEventListener('hashchange', () => renderRoute()); renderRoute(false);
}

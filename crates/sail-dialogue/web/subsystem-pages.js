'use strict';

// Mostly matches sail_common::SubsystemId. SAIL Agents groups Flow, Forge and Hermes
// skills/plugins as sections of one workspace (old #flow and #forge links still work), and
// Memory is Arena's store shown on its own page. `tabs` limits a shared card to the
// tabs that belong to the page.
const SETTINGS_ROUTES = {dialogue: 'voice', core: 'developer', system: 'developer', mind: 'models', guardian: 'guardian'};
// Pages that became sections of SAIL Agents: old links and map stations land on the section.
const SECTION_ROUTES = {flow: ['agents', 'workflows'], forge: ['agents', 'coding'], hermes: ['agents', 'skills'], skills: ['agents', 'skills'], market: ['agents', 'market'], schedule: ['agents', 'schedule']};
const SECTION_KEY = 'sail.agents.section';

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
    "description": "Learn something step by step with an assessment, a lesson map and guided teaching, or take short interactive courses.",
    "card": "memoryCard",
    "panel": "learnPanelMain",
    "tabs": ["learnPanelMain", "coursesPanelMain", "learnFactoryPanel"],
    "settings": null,
    "details": [
      "Arena Learn: assessment, a lesson map and step-by-step teaching",
      "Courses: short interactive lessons with spaced review",
      "Course Factory: turn your books and notes into checked courses",
      "Research collections"
    ]
  },
  {
    "id": "memory",
    "name": "Memory",
    "icon": "mesh",
    "subtitle": "What SAIL remembers",
    "description": "Search, review and export what SAIL remembers from your conversations, or transcribe a live lesson without replies.",
    "card": "memoryCard",
    "panel": "memoryPanelMain",
    "tabs": ["memoryPanelMain", "memoryPanelTranscript", "memoryPanelToolLog"],
    "settings": null,
    "details": [
      "Episodic memory",
      "Associative knowledge graph",
      "Live lesson and conversation transcript (Listen without reply)",
      "Persistent log of all tools used by SAIL",
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
    "name": "SAIL Agents",
    "icon": "agents",
    "subtitle": "Workflows, coding and skills",
    "description": "Everything SAIL does on its own, in one place: Flow workflows, Forge coding jobs, and the Hermes skills and plugins the model can use.",
    "card": null,
    "panel": null,
    "settings": null,
    "sections": [
      {"id": "workflows", "label": "Workflows", "card": "flowCard"},
      {"id": "coding", "label": "Coding", "card": "forgeCard", "settings": "forge"},
      {"id": "skills", "label": "Skills & plugins", "card": "hermesCard"},
      {"id": "market", "label": "Market", "card": "marketCard"},
      {"id": "schedule", "label": "Schedule", "card": "scheduleCard"}
    ],
    "details": [
      "Flow: versioned local workflows with saved execution results",
      "Forge: queued coding tasks in isolated workspaces, with tests, review and draft PRs",
      "Hermes: Agent Skills the model can read, and sandboxed plugins enabled by owner approval",
      "Market: browse and install Hermes skills and plugins from the public catalogs",
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
  },
  {
    "id": "terminal",
    "name": "Terminal",
    "icon": "terminal",
    "subtitle": "Interactive developer shell",
    "description": "Run shell commands in your host workspace with live ANSI terminal output.",
    "card": "terminalCard",
    "panel": "terminalPanel",
    "settings": null,
    "details": [
      "Interactive PTY host shell over WebSocket",
      "Tab completion, history, ANSI colors, and Ctrl+C",
      "Quick developer commands: git, cargo, and directory inspection"
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
  const sectionBar = document.getElementById('subsystemSections');
  let currentPage = 'home';
  let currentSection = null;
  let requestedPanel = null;
  const pageCards = subsystemPages.flatMap(item => [item.card, ...(item.sections || []).map(section => section.card)]);
  for (const id of new Set(pageCards.filter(Boolean))) {
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
    if (SECTION_ROUTES[id]) {
      const [target, section] = SECTION_ROUTES[id];
      history.replaceState(null, '', `#${target}?section=${section}`);
      id = target;
    }
    if (id === 'classic') {
      history.replaceState(null, '', '#home');
      id = 'home';
    }
    if (id === 'spaces') return; // Spaces overlays the current workspace.
    const found = subsystemPages.find(entry => entry.id === id);
    const item = found?.id === 'factory' ? null : found;
    const route = item?.id || (id === 'tracker' || id === 'moodle' || id === 'capital' || id === 'entertainment' ? id : 'home');
    if (!found && route === 'home' && id !== 'home') history.replaceState(null, '', '#home');
    closeSheet();
    // Move original nodes so socket updates, form state, and listeners stay live.
    for (const [card, marker] of cardHomes) { marker.after(card); showCardTabs(card, null); }
    content.replaceChildren();
    if (sectionBar) { sectionBar.replaceChildren(); sectionBar.hidden = true; }
    content.removeAttribute('role'); content.removeAttribute('aria-labelledby');
    currentSection = null;
    if (home) home.hidden = true;
    page.hidden = !item;
    if (page) page.dataset.page = route;
    if (workspace) workspace.setAttribute('aria-label', `${item?.name || (route === 'entertainment' ? 'Entertainment' : 'Home')} workspace`);
    document.title = `SAIL — ${item?.name || {tracker: 'Tracker', moodle: 'School', capital: 'Capital', entertainment: 'Entertainment'}[route] || 'Workspace'}`;
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
      const notice = document.getElementById('subsystemNotice'); notice.hidden = !!(item.card || item.sections);
      notice.textContent = item.card ? '' : `${item.name} controls and results are not connected to this interface yet.`;
      setSettings(item.settings);
      if (item.sections) showSections(item, requestedPanel);
      else if (item.card) {
        const card = document.getElementById(item.card); content.append(card); showCardTabs(card, item.tabs);
        const defaultPanel = (item.id === 'memory' && typeof liveContextEnabled !== 'undefined' && liveContextEnabled) ? 'memoryPanelTranscript' : item.panel;
        const panel = item.tabs && !item.tabs.includes(requestedPanel) ? defaultPanel : requestedPanel || defaultPanel;
        if (panel) switchCardTab(item.card, panel);
      }

    }
    currentPage = route; requestedPanel = null;
    if (workspace) workspace.scrollTop = 0;
    window.dispatchEvent(new CustomEvent('sail:page', {detail:route}));
    if (currentSection) window.dispatchEvent(new CustomEvent('sail:section', {detail:currentSection}));
    if (settingsCategory) window.sailOpenSettings(settingsCategory);
    // The map restores focus to whatever opened a closed popup.
    if (focus && item) title.focus({ preventScroll:true });
  }
  function setSettings(category) {
    const settings = document.getElementById('subsystemSettings'); settings.hidden = !category;
    settings.onclick = () => { openSheet(contextSheet); contextSheet.querySelector(`[data-settings-tab="${category}"]`)?.click(); };
  }
  // A page with sections shows one card at a time behind an ARIA tab strip. The section is
  // part of the address (`#agents?section=coding`) and remembered for the next visit.
  function requestedSection(item, requested) {
    const ids = item.sections.map(section => section.id);
    const fromHash = new URLSearchParams(location.hash.split('?')[1] || '').get('section');
    let remembered = null;
    try { remembered = localStorage.getItem(SECTION_KEY); } catch {}
    return [requested, fromHash, remembered].find(id => ids.includes(id)) || ids[0];
  }
  function showSections(item, requested) {
    const chosen = requestedSection(item, requested);
    sectionBar.replaceChildren();
    sectionBar.hidden = false;
    sectionBar.setAttribute('aria-label', `${item.name} sections`);
    for (const section of item.sections) {
      const tab = document.createElement('button');
      tab.type = 'button'; tab.className = 'page-section-tab'; tab.id = `section-${section.id}`;
      tab.setAttribute('role', 'tab'); tab.dataset.section = section.id;
      tab.setAttribute('aria-controls', 'subsystemContent');
      tab.textContent = section.label;
      tab.onclick = () => selectSection(item, section.id, true);
      sectionBar.append(tab);
    }
    selectSection(item, chosen, false);
  }
  function selectSection(item, id, announce) {
    const section = item.sections.find(entry => entry.id === id) || item.sections[0];
    for (const [card, marker] of cardHomes) if (content.contains(card)) { marker.after(card); showCardTabs(card, null); }
    content.replaceChildren();
    for (const tab of sectionBar.querySelectorAll('[role="tab"]')) {
      const selected = tab.dataset.section === section.id;
      tab.setAttribute('aria-selected', selected ? 'true' : 'false');
      tab.tabIndex = selected ? 0 : -1;
      if (selected) content.setAttribute('aria-labelledby', tab.id);
    }
    content.setAttribute('role', 'tabpanel');
    const card = document.getElementById(section.card);
    if (card) { content.append(card); showCardTabs(card, section.tabs); if (section.panel) switchCardTab(section.card, section.panel); }
    setSettings(section.settings || item.settings);
    currentSection = section.id;
    try { localStorage.setItem(SECTION_KEY, section.id); } catch {}
    const address = `#${item.id}?section=${section.id}`;
    if (location.hash !== address) history.replaceState(null, '', address);
    if (announce) {
      window.dispatchEvent(new CustomEvent('sail:section', {detail:section.id}));
      sectionBar.querySelector(`[data-section="${section.id}"]`)?.focus();
    }
  }
  sectionBar?.addEventListener('keydown', event => {
    const tabs = [...sectionBar.querySelectorAll('[role="tab"]')];
    const index = tabs.indexOf(document.activeElement);
    if (index < 0) return;
    const next = {ArrowRight: index + 1, ArrowLeft: index - 1, Home: 0, End: tabs.length - 1}[event.key];
    if (next === undefined) return;
    event.preventDefault();
    tabs[(next + tabs.length) % tabs.length].click();
  });
  window.sailCurrentSection = () => currentSection;
  window.sailNavigate = (id, panel = null) => {
    if (SECTION_ROUTES[id]) { [id, panel] = [SECTION_ROUTES[id][0], panel || SECTION_ROUTES[id][1]]; }
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

// SAIL Capital — core.
"use strict";

let currentAccount = null;

let currentTz = "Europe/Lisbon";

let selectedFile = null;

let accountList = [];

function setState(state, text) {
  const pill = document.getElementById("cpState");
  const stateText = document.getElementById("cpStateText");
  if (pill && stateText) {
    pill.setAttribute("data-state", state);
    stateText.textContent = text;
  }
}

function showBanner(message, isError = false) {
  const banner = document.getElementById("cpBanner");
  if (!banner) return;
  if (!message) {
    banner.hidden = true;
    banner.textContent = "";
  } else {
    banner.hidden = false;
    banner.textContent = message;
    banner.style.backgroundColor = isError ? "rgba(225, 112, 85, 0.15)" : "rgba(100, 160, 240, 0.15)";
    banner.style.color = isError ? "#ffb8a8" : "#c0dbff";
  }
}

// Amounts arrive as exact decimal strings. They are shown exactly (with thousands
// grouping) and summed exactly with BigInt, never through floats.
function groupDecimal(text) {
  const str = String(text);
  const negative = str.startsWith("-");
  const [whole, frac] = (negative ? str.slice(1) : str).split(".");
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, " ");
  return `${negative ? "-" : ""}${grouped}${frac !== undefined ? "." + frac : ""}`;
}

function formatMoney(money) {
  if (!money || money.amount === undefined || money.amount === null) return "—";
  return `${groupDecimal(money.amount)} ${money.currency || ""}`.trim();
}

/** "5 h ago" up to two days, then "3 days ago". */
function formatAge(hours) {
  if (hours < 48) return `${hours} h ago`;
  const days = Math.floor(hours / 24);
  return `${days} days ago`;
}

function sumDecimals(amounts) {
  let scale = 0;
  for (const a of amounts) {
    const frac = String(a).split(".")[1] || "";
    scale = Math.max(scale, frac.length);
  }
  let total = 0n;
  for (const a of amounts) {
    const str = String(a);
    const negative = str.startsWith("-");
    const [whole, frac = ""] = (negative ? str.slice(1) : str).split(".");
    const units = BigInt((whole || "0") + frac.padEnd(scale, "0"));
    total += negative ? -units : units;
  }
  const negative = total < 0n;
  const digits = (negative ? -total : total).toString().padStart(scale + 1, "0");
  const whole = digits.slice(0, digits.length - scale);
  const frac = scale ? "." + digits.slice(digits.length - scale) : "";
  return `${negative ? "-" : ""}${whole}${frac}`;
}

function formatSum(moneys) {
  if (moneys.length === 0) return null;
  if (!moneys.every(m => m.currency === moneys[0].currency)) return null;
  return formatMoney({ amount: sumDecimals(moneys.map(m => m.amount)), currency: moneys[0].currency });
}

let hasTodayAccount = false;

function updateAccountHeaderTiles(account) {
  const accountVal = document.getElementById("cpAccountVal");
  const investedVal = document.getElementById("cpInvestedVal");
  const freeFundsVal = document.getElementById("cpFreeFundsVal");
  const openResultVal = document.getElementById("cpOpenResultVal");

  if (!account) {
    if (accountVal) accountVal.textContent = "—";
    if (investedVal) investedVal.textContent = "—";
    if (freeFundsVal) freeFundsVal.textContent = "—";
    if (openResultVal) {
      openResultVal.textContent = "—";
      openResultVal.classList.remove("positive", "negative");
    }
    return;
  }

  if (accountVal) accountVal.textContent = formatMoney(account.account_value);
  if (investedVal) investedVal.textContent = account.invested ? formatMoney(account.invested) : "—";
  if (freeFundsVal) freeFundsVal.textContent = formatMoney(account.free_funds);
  if (openResultVal) {
    if (account.open_result) {
      openResultVal.textContent = formatMoney(account.open_result);
      const isNeg = String(account.open_result.amount).startsWith("-");
      openResultVal.classList.toggle("negative", isNeg);
      openResultVal.classList.toggle("positive", !isNeg);
    } else {
      openResultVal.textContent = "—";
      openResultVal.classList.remove("positive", "negative");
    }
  }
}

function formatDate(isoStr) {
  if (!isoStr) return "—";
  try {
    const d = new Date(isoStr);
    return isNaN(d.getTime()) ? isoStr : d.toLocaleDateString(undefined, {
      year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit"
    });
  } catch {
    return isoStr;
  }
}

/** A calendar date only, read in UTC: received dates are stored as the start of a UTC day. */
function formatDay(isoStr) {
  if (!isoStr) return "—";
  const d = new Date(isoStr);
  return isNaN(d.getTime())
    ? isoStr
    : d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" });
}

// Every /api/capital route needs the owner token, obtained the same way the School and
// Tracker pages do it.
let ownerTokenValue = null;

async function ownerToken(force) {
  if (!force && ownerTokenValue) return ownerTokenValue;
  if (!force) {
    try {
      const stored = localStorage.getItem("sail_owner_token");
      if (stored && stored.trim().length >= 32) return (ownerTokenValue = stored.trim());
    } catch (_) {}
  }
  const r = await fetch("/api/guardian/token", { cache: "no-store" });
  if (!r.ok) throw new Error("Owner token unavailable. Open SAIL settings and enter the owner token.");
  const data = await r.json();
  ownerTokenValue = data.token;
  try {
    localStorage.setItem("sail_owner_token", ownerTokenValue);
  } catch (_) {}
  return ownerTokenValue;
}

async function fetchJson(url, options = {}) {
  for (let attempt = 0; attempt < 2; attempt++) {
    const token = await ownerToken(attempt > 0);
    const res = await fetch(url, {
      ...options,
      cache: "no-store",
      headers: { ...(options.headers || {}), Authorization: `Bearer ${token}` },
    });
    if (res.status === 401 && attempt === 0) {
      ownerTokenValue = null;
      try {
        localStorage.removeItem("sail_owner_token");
      } catch (_) {}
      continue;
    }
    const data = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
    return data;
  }
  throw new Error("Owner authentication failed.");
}

async function loadAccounts() {
  try {
    const accounts = await fetchJson("/api/capital/accounts");
    accountList = accounts || [];
    const select = document.getElementById("cpAccountSelect");
    if (!select) return;

    if (!accounts || accounts.length === 0) {
      select.hidden = true;
      currentAccount = null;
      return;
    }

    select.innerHTML = "";
    for (const acc of accounts) {
      const opt = document.createElement("option");
      opt.value = acc.account;
      opt.textContent = `${acc.account} (${acc.currency || "multi"}) · ${acc.entries || 0} rows`;
      select.appendChild(opt);
    }

    if (accounts.length > 1) {
      select.hidden = false;
    } else {
      select.hidden = true;
    }

    if (!currentAccount || !accounts.some(a => a.account === currentAccount)) {
      currentAccount = accounts[0].account;
    }
    select.value = currentAccount;
  } catch (err) {
    showBanner(`Failed to load accounts: ${err.message}`, true);
  }
}

function setupUploadDialog() {
  const uploadBtn = document.getElementById("cpUploadBtn");
  const fileInput = document.getElementById("cpFileInput");
  const dialog = document.getElementById("cpUploadDialog");
  const cancelBtn = document.getElementById("cpDialogCancel");
  const submitBtn = document.getElementById("cpDialogSubmit");
  const fileName = document.getElementById("cpFileName");
  const fileSize = document.getElementById("cpFileSize");
  const tzSelect = document.getElementById("cpTzSelect");

  if (!uploadBtn || !fileInput || !dialog) return;

  uploadBtn.addEventListener("click", () => {
    fileInput.click();
  });

  fileInput.addEventListener("change", () => {
    if (fileInput.files && fileInput.files[0]) {
      selectedFile = fileInput.files[0];
      fileName.textContent = selectedFile.name;
      const sizeKb = (selectedFile.size / 1024).toFixed(1);
      fileSize.textContent = `${sizeKb} KB`;
      dialog.showModal();
    }
  });

  cancelBtn.addEventListener("click", () => {
    dialog.close();
    selectedFile = null;
    fileInput.value = "";
  });

  submitBtn.addEventListener("click", async () => {
    if (!selectedFile) return;
    const tz = tzSelect.value || currentTz;
    const currencySelect = document.getElementById("cpCurrencySelect");
    const currency = currencySelect ? currencySelect.value : "";

    dialog.close();
    setState("loading", "Uploading…");
    showBanner(`Uploading ${selectedFile.name}…`);

    try {
      const buffer = await selectedFile.arrayBuffer();
      // The account the file names wins on the server; the selected account only covers
      // exports that don't name one (e.g. a bare positions sheet).
      const accountParam = currentAccount ? `&account=${encodeURIComponent(currentAccount)}` : "";
      const url = `/api/capital/import/xtb?tz=${encodeURIComponent(tz)}&currency=${encodeURIComponent(currency)}${accountParam}`;
      const batch = await fetchJson(url, {
        method: "POST",
        headers: {
          "Content-Type": "application/octet-stream",
          "X-File-Name": selectedFile.name.replace(/[^\x20-\x7e]/g, "_"),
        },
        body: buffer,
      });
      currentAccount = batch.account;

      // Refresh first, then report the result so the refresh doesn't hide it.
      await loadAccounts();
      await loadAll();
      showBanner(
        batch.already_imported
          ? "This exact file was already imported; nothing changed."
          : `Imported: ${batch.added || 0} new rows, ${batch.duplicates || 0} already known, ${batch.conflicts || 0} conflicts.`
      );
    } catch (err) {
      setState("error", "Error");
      showBanner(`Import failed: ${err.message}`, true);
    } finally {
      selectedFile = null;
      fileInput.value = "";
    }
  });
}

function escapeHtml(str) {
  if (str === null || str === undefined) return "";
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

// Collapsible sections. Each section keeps its header visible; the chevron (or the title)
// hides the rest. Which sections are collapsed is remembered per browser.
const COLLAPSED_KEY = "sail_capital_collapsed";

function readCollapsed() {
  try {
    const value = JSON.parse(localStorage.getItem(COLLAPSED_KEY) || "[]");
    return new Set(Array.isArray(value) ? value : []);
  } catch (_) {
    return new Set();
  }
}

function writeCollapsed(set) {
  try {
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...set]));
  } catch (_) {}
}

function setupCollapsibles() {
  const collapsed = readCollapsed();
  const sections = document.querySelectorAll("#cpMain section[id]");
  for (const section of sections) {
    // A section marked cp-fixed is always open (Today, the scorecard).
    if (section.classList.contains("cp-metrics") || section.classList.contains("cp-fixed")) continue;
    const head = section.querySelector(":scope > .cp-panel-head, :scope > .cp-recon-header");
    const title = head && head.querySelector("h2");
    if (!title) continue;
    section.classList.add("cp-collapsible");

    const button = document.createElement("button");
    button.type = "button";
    button.className = "cp-collapse";
    button.setAttribute("aria-controls", section.id);
    button.innerHTML = `<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    title.prepend(button);

    const apply = (isCollapsed) => {
      section.classList.toggle("cp-collapsed", isCollapsed);
      button.setAttribute("aria-expanded", String(!isCollapsed));
      button.setAttribute(
        "aria-label",
        `${isCollapsed ? "Expand" : "Collapse"} ${title.textContent.trim()}`,
      );
    };
    apply(collapsed.has(section.id));

    title.addEventListener("click", () => {
      const now = !section.classList.contains("cp-collapsed");
      apply(now);
      if (now) collapsed.add(section.id);
      else collapsed.delete(section.id);
      writeCollapsed(collapsed);
    });
  }
}

const TABS = ["today", "portfolio", "income", "outlook", "record", "settings"];
// Links and bookmarks from before round 17.
const TAB_ALIASES = { holdings: "portfolio", research: "portfolio", signals: "outlook", forecasts: "record", data: "settings" };

function normalizeTab(tabId) {
  const id = String(tabId || "").toLowerCase();
  return TAB_ALIASES[id] || id;
}

const TAB_STORAGE_KEY = "sail_capital_active_tab";

function getActiveTab() {
  const hash = normalizeTab(window.location.hash.replace(/^#/, ""));
  if (TABS.includes(hash)) return hash;
  try {
    const saved = normalizeTab(localStorage.getItem(TAB_STORAGE_KEY));
    if (TABS.includes(saved)) return saved;
  } catch (_) {}
  return "today";
}

function switchTab(tabId, updateUrl = true) {
  tabId = normalizeTab(tabId);
  if (!TABS.includes(tabId)) tabId = "today";
  for (const t of TABS) {
    const btn = document.getElementById(`cpTab-${t}`);
    const pane = document.getElementById(`cpPanel-${t}`);
    const isActive = t === tabId;
    if (btn) {
      btn.setAttribute("aria-selected", String(isActive));
      btn.tabIndex = isActive ? 0 : -1;
      btn.classList.toggle("cp-tab-active", isActive);
    }
    if (pane) {
      pane.hidden = !isActive;
      pane.classList.toggle("cp-pane-active", isActive);
    }
  }
  try {
    localStorage.setItem(TAB_STORAGE_KEY, tabId);
  } catch (_) {}
  if (updateUrl && window.location.hash !== `#${tabId}`) {
    history.replaceState(null, "", `#${tabId}`);
  }
  const crumb = document.getElementById("cpCrumb");
  const btn = document.getElementById(`cpTab-${tabId}`);
  if (crumb && btn) crumb.textContent = (btn.querySelector(".cp-tab-wide, span") || btn).textContent.trim();
  if (typeof loadTab === "function" && accountList.length > 0) loadTab(tabId);
}

function setupTabs() {
  const tabButtons = TABS.map(t => document.getElementById(`cpTab-${t}`)).filter(Boolean);

  tabButtons.forEach((btn, idx) => {
    btn.addEventListener("click", () => {
      const tabId = btn.id.replace("cpTab-", "");
      switchTab(tabId, true);
    });

    btn.addEventListener("keydown", (e) => {
      let targetIdx = null;
      if (e.key === "ArrowRight" || e.key === "ArrowDown") {
        targetIdx = (idx + 1) % tabButtons.length;
      } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
        targetIdx = (idx - 1 + tabButtons.length) % tabButtons.length;
      } else if (e.key === "Home") {
        targetIdx = 0;
      } else if (e.key === "End") {
        targetIdx = tabButtons.length - 1;
      }
      if (targetIdx !== null) {
        e.preventDefault();
        const targetBtn = tabButtons[targetIdx];
        const tabId = targetBtn.id.replace("cpTab-", "");
        switchTab(tabId, true);
        targetBtn.focus();
      }
    });
  });

  window.addEventListener("hashchange", () => {
    const hash = normalizeTab(window.location.hash.replace(/^#/, ""));
    if (TABS.includes(hash)) {
      switchTab(hash, false);
    }
  });

  const initial = getActiveTab();
  switchTab(initial, false);
}

function ordinalSuffix(n) {
  const rounded = Math.round(n);
  const j = rounded % 10;
  const k = rounded % 100;
  if (j === 1 && k !== 11) return `${rounded}st`;
  if (j === 2 && k !== 12) return `${rounded}nd`;
  if (j === 3 && k !== 13) return `${rounded}rd`;
  return `${rounded}th`;
}

// --- Dashboard material: sparklines, change pills, avatars, the market board ----------

let sparkSeq = 0;

// An SVG sparkline. `area` fills under the line; colour follows the direction unless given.
function sparklineSvg(values, { width = 300, height = 90, area = true, color = null, dot = true } = {}) {
  const pts = (values || []).filter((v) => Number.isFinite(v));
  if (pts.length < 2) return "";
  const min = Math.min(...pts);
  const max = Math.max(...pts);
  const span = max - min || 1;
  const pad = 4;
  const x = (i) => (i / (pts.length - 1)) * width;
  const y = (v) => pad + (1 - (v - min) / span) * (height - pad * 2);
  const up = pts[pts.length - 1] >= pts[0];
  const stroke = color || (up ? "var(--cp-up)" : "var(--cp-down)");
  const line = pts.map((v, i) => `${i ? "L" : "M"}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const id = `cpSpark${++sparkSeq}`;
  const fill = area
    ? `<defs><linearGradient id="${id}" x1="0" y1="0" x2="0" y2="1">
         <stop offset="0" stop-color="${stroke}" stop-opacity=".32"/>
         <stop offset="1" stop-color="${stroke}" stop-opacity="0"/>
       </linearGradient></defs>
       <path d="${line} L${width},${height} L0,${height} Z" fill="url(#${id})" stroke="none"/>`
    : "";
  const last = dot
    ? `<circle cx="${x(pts.length - 1).toFixed(1)}" cy="${y(pts[pts.length - 1]).toFixed(1)}" r="3.2" fill="${stroke}" stroke="#0b0c10" stroke-width="1.5"/>`
    : "";
  return `<svg class="cp-sparkline" viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" aria-hidden="true">
      ${fill}<path d="${line}" fill="none" stroke="${stroke}" stroke-width="1.8" stroke-linejoin="round" stroke-linecap="round" vector-effect="non-scaling-stroke"/>${last}
    </svg>`;
}

// A green/red pill for a fractional change (0.012 → "+1.20%"); a dash when unknown.
function changePill(fraction, digits = 2) {
  if (fraction === null || fraction === undefined || !Number.isFinite(fraction)) {
    return `<span class="cp-chg-none">—</span>`;
  }
  const pctValue = fraction * 100;
  const dir = Math.abs(pctValue) < 0.005 ? "flat" : pctValue > 0 ? "up" : "down";
  const arrow = dir === "up" ? "↗" : dir === "down" ? "↘" : "→";
  const sign = pctValue > 0 ? "+" : pctValue < 0 ? "−" : "";
  return `<span class="cp-chg" data-dir="${dir}"><span aria-hidden="true">${arrow}</span>${sign}${Math.abs(pctValue).toFixed(digits)}%</span>`;
}

function changeDir(fraction) {
  if (!Number.isFinite(fraction) || Math.abs(fraction) < 0.00005) return "flat";
  return fraction > 0 ? "up" : "down";
}

function instrumentAvatar(symbol, cls) {
  const letters = String(symbol || "?").replace(/[^A-Za-z0-9]/g, "").slice(0, 3).toUpperCase();
  return `<span class="cp-avatar" data-class="${escapeHtml(cls || "stock")}" aria-hidden="true">${escapeHtml(letters || "?")}</span>`;
}

function formatPrice(value, currency) {
  if (!Number.isFinite(value)) return "—";
  const digits = Math.abs(value) < 10 ? 4 : 2;
  const text = value.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: digits });
  return currency ? `${text} ${currency}` : text;
}

// The market board, fetched once per load and shared by the dashboard and the ticker.
let marketsPromise = null;

function loadMarkets(force = false) {
  if (!force && marketsPromise) return marketsPromise;
  const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
  marketsPromise = fetchJson(`/api/capital/markets${query}`).catch((err) => {
    marketsPromise = null;
    throw err;
  });
  return marketsPromise;
}

function renderTicker(board) {
  const strip = document.getElementById("cpTicker");
  const track = document.getElementById("cpTickerTrack");
  if (!strip || !track) return;
  const tiles = (board?.tiles || []).slice(0, 24);
  if (tiles.length === 0) {
    strip.hidden = true;
    return;
  }
  const items = tiles.map((t) => `<span class="cp-tick">
      ${instrumentAvatar(t.symbol, t.class).replace("cp-avatar", "cp-avatar cp-avatar-xs")}
      <span class="cp-tick-sym">${escapeHtml(t.symbol)}</span>
      <span class="cp-tick-px">${formatPrice(t.close)}</span>
      ${changePill(t.change_1d)}
    </span>`).join("");
  // Two copies make the loop seamless; the second is hidden from assistive tech.
  track.innerHTML = `<div class="cp-tick-set">${items}</div><div class="cp-tick-set" aria-hidden="true">${items}</div>`;
  strip.hidden = false;
}

function updateHello() {
  const hello = document.getElementById("cpHello");
  if (!hello) return;
  const hour = new Date().getHours();
  hello.textContent = hour < 5 ? "Good night" : hour < 12 ? "Good morning" : hour < 18 ? "Good afternoon" : "Good evening";
}

// Names shared by Outlook, Portfolio and Track record.
const SETUP_NAMES = {
  trend_pullback: "Trend pullback",
  squeeze_breakout: "Squeeze breakout",
  donchian55: "55-day breakout",
  donchian_55: "55-day breakout",
  bollinger_reversion: "Bollinger reversion",
  inside_bar_break: "Inside-bar break",
  three_day_reversal: "Three-day reversal",
};

function formatSetupName(setup) {
  if (!setup) return "—";
  const key = String(setup).replace(/([a-z])([A-Z0-9])/g, "$1_$2").toLowerCase();
  return SETUP_NAMES[key] || key.replace(/_/g, " ");
}

function signalSideLabel(side) {
  return side === "long" ? "Buy (long)" : "Sell (short)";
}

// Marks tabs to reload the next time they open (e.g. after new prices arrive).
function invalidateTabs(...tabIds) {
  for (const id of tabIds) loadedTabs.delete(id);
}

// Each tab registers what it loads. The header (portfolio and today's tiles) loads at
// start; a tab's own data loads the first time it is opened, and again on Refresh.
const TAB_LOADERS = {};
const TAB_SETUPS = [];
const loadedTabs = new Set();

function registerTab(tabId, { setup, load } = {}) {
  if (setup) TAB_SETUPS.push(setup);
  if (load) TAB_LOADERS[tabId] = load;
}

async function loadTab(tabId, force = false) {
  const load = TAB_LOADERS[tabId];
  if (!load || (!force && loadedTabs.has(tabId))) return;
  loadedTabs.add(tabId);
  try {
    await load();
  } catch (err) {
    loadedTabs.delete(tabId);
    showBanner(`Couldn't load ${tabId}: ${err.message}`, true);
  }
}

async function loadAll() {
  loadedTabs.clear();
  marketsPromise = null;
  loadMarkets().then(renderTicker).catch(() => renderTicker(null));
  await Promise.all([loadPortfolio(), loadToday()]);
  await loadTab(getActiveTab(), true);
}

document.addEventListener("DOMContentLoaded", () => {
  updateHello();
  setupTabs();
  setupCollapsibles();
  setupUploadDialog();
  for (const setup of TAB_SETUPS) setup();

  document.getElementById("cpRefreshBtn")?.addEventListener("click", () => {
    showBanner("");
    loadAll();
  });

  document.getElementById("cpAccountSelect")?.addEventListener("change", (e) => {
    currentAccount = e.target.value;
    loadAll();
  });

  loadAccounts().then(loadAll);
});

// SAIL Capital Web Client
// Interacts with /api/capital endpoints according to the final contract (Round 7).

let currentAccount = null;
let currentTz = "Europe/Lisbon";
let selectedFile = null;
let accountList = [];
let currentHorizon = 20;
let currentModel = "no_change";
let currentOutlookSymbol = "";
let currentOutlookHorizon = 21;
let currentOutlookTrim = 0;
let currentOutlookSessions = [];
let modelsPollingTimer = null;

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
      openResultVal.className = "cp-tile-val";
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
      openResultVal.className = `cp-tile-val ${isNeg ? "negative" : "positive"}`;
    } else {
      openResultVal.textContent = "—";
      openResultVal.className = "cp-tile-val";
    }
  }

  // Legacy metric hook updates
  const cashHook = document.getElementById("cpCashVal");
  if (cashHook) cashHook.textContent = formatMoney(account.free_funds);
  const holdingsHook = document.getElementById("cpHoldingsVal");
  if (holdingsHook && account.invested) holdingsHook.textContent = formatMoney(account.invested);
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

async function loadPortfolio() {
  setState("loading", "Loading…");

  if (accountList.length === 0) {
    // Nothing imported yet: an empty state, not an error.
    renderEmptyPortfolio();
    setState("idle", "No data yet");
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const p = await fetchJson(`/api/capital/portfolio${query}`);

    renderPortfolio(p);
    setState("idle", "Ready");
  } catch (err) {
    setState("error", "Error");
    showBanner(`Portfolio error: ${err.message}`, true);
    renderEmptyPortfolio();
  }
}

function renderEmptyPortfolio() {
  const freshness = document.getElementById("cpFreshness");
  if (freshness) freshness.textContent = "XTB data as of — · imported —";

  const reconSection = document.getElementById("cpReconciliation");
  if (reconSection) reconSection.hidden = true;

  const grantsContainer = document.getElementById("cpGrants");
  if (grantsContainer) grantsContainer.hidden = true;

  const insightsBody = document.getElementById("cpInsightsBody");
  if (insightsBody) insightsBody.innerHTML = `<p class="cp-empty">Import a report to see insights</p>`;
  const insightsFreshness = document.getElementById("cpInsightsFreshness");
  if (insightsFreshness) insightsFreshness.textContent = "Deterministic calculation from ledger";

  document.getElementById("cpCashVal").textContent = "—";
  document.getElementById("cpHoldingsVal").textContent = "—";
  document.getElementById("cpHoldingsSub").textContent = "0 positions";
  document.getElementById("cpDividendsVal").textContent = "—";
  document.getElementById("cpFeesVal").textContent = "—";

  document.getElementById("cpPositionsBody").innerHTML = `<tr><td colspan="5" class="cp-empty">No portfolio data yet. Upload an XTB export to begin.</td></tr>`;
  document.getElementById("cpCfdBody").innerHTML = `<tr><td colspan="5" class="cp-empty">No open CFD contracts.</td></tr>`;
  document.getElementById("cpDividendsBody").innerHTML = `<tr><td colspan="5" class="cp-empty">No dividend events recorded.</td></tr>`;
  updateOutlookHoldings([]);
}

function renderPortfolio(p) {
  // Freshness bar
  const freshness = document.getElementById("cpFreshness");
  if (freshness) {
    const snapAsOf = p.snapshot_as_of ? formatDate(p.snapshot_as_of) : "not stated";
    const acct = accountList.find(a => a.account === p.account);
    const imported = acct && acct.last_import_at ? formatDate(acct.last_import_at) : "—";
    freshness.textContent = `XTB data as of: ${snapAsOf} · imported ${imported}`;
  }

  // Reconciliation
  const reconSection = document.getElementById("cpReconciliation");
  const reconBadge = document.getElementById("cpReconBadge");
  const reconSummary = document.getElementById("cpReconSummary");
  const variancesDiv = document.getElementById("cpVariances");
  const variancesList = document.getElementById("cpVariancesList");

  if (reconSection && p.reconciliation) {
    reconSection.hidden = false;
    const status = p.reconciliation.status || "unverified";
    reconBadge.setAttribute("data-status", status);
    reconBadge.textContent = status.toUpperCase();

    const compared = p.reconciliation.compared || 0;
    const varCount = (p.reconciliation.variances || []).length;

    if (status === "matched") {
      reconSummary.textContent = `All ${compared} verified broker figures match the deterministic ledger projection exactly.`;
      variancesDiv.hidden = true;
    } else if (status === "variance") {
      reconSummary.textContent = `${varCount} variance(s) found across ${compared} compared figures between the ledger and broker snapshot.`;
      variancesDiv.hidden = false;
      variancesList.innerHTML = "";
      for (const v of p.reconciliation.variances) {
        const li = document.createElement("li");
        li.className = "cp-var-item";

        // Check if broker has more units than ledger
        const missingQty = getMissingQuantity(v);
        let actionBtnHtml = "";
        if (missingQty !== null) {
          const sym = resolveVarianceSymbol(v.subject, p);
          actionBtnHtml = `
            <div class="cp-var-actions">
              <button type="button" class="cp-button cp-button-sm cp-record-grant-btn"
                data-symbol="${escapeHtml(sym)}"
                data-qty="${escapeHtml(missingQty)}">
                Record received shares
              </button>
            </div>
          `;
        }

        li.innerHTML = `
          <div class="cp-var-subject">${escapeHtml(v.subject)}</div>
          <div class="cp-var-figures">
            <span>Ledger: <strong>${escapeHtml(v.ledger || "—")}</strong></span>
            <span>Broker: <strong>${escapeHtml(v.broker || "—")}</strong></span>
          </div>
          <div class="cp-var-note">${escapeHtml(v.note || "")}</div>
          ${actionBtnHtml}
        `;
        variancesList.appendChild(li);
      }

      variancesList.querySelectorAll(".cp-record-grant-btn").forEach(btn => {
        btn.addEventListener("click", () => {
          const sym = btn.getAttribute("data-symbol");
          const qty = btn.getAttribute("data-qty");
          openGrantDialog(sym, qty, p.snapshot_as_of);
        });
      });
    } else {
      reconSummary.textContent = `No snapshot available to verify ledger against broker figures.`;
      variancesDiv.hidden = true;
    }
  }

  // Metrics: Cash
  const cashVal = document.getElementById("cpCashVal");
  if (cashVal) {
    if (!p.cash || p.cash.length === 0) {
      cashVal.textContent = "—";
    } else {
      cashVal.textContent = p.cash.map(formatMoney).join(" · ");
    }
  }

  // Metrics: Holdings
  const holdingsVal = document.getElementById("cpHoldingsVal");
  const holdingsSub = document.getElementById("cpHoldingsSub");
  if (holdingsVal) {
    const posCount = (p.positions || []).length;
    holdingsSub.textContent = `${posCount} security position${posCount === 1 ? "" : "s"}`;
    // Sum market values if all have same currency
    const mvs = (p.positions || []).map(pos => pos.broker_market_value).filter(Boolean);
    const allValued = mvs.length === posCount;
    if (allValued && formatSum(mvs)) {
      holdingsVal.textContent = formatSum(mvs);
    } else if (mvs.length > 0) {
      holdingsVal.textContent = `${mvs.length} valued`;
    } else {
      holdingsVal.textContent = `${posCount} open`;
    }
  }

  // Metrics: Dividends
  const divVal = document.getElementById("cpDividendsVal");
  if (divVal) {
    const divs = p.dividends || [];
    const nets = divs.map(d => d.net).filter(Boolean);
    if (nets.length === divs.length && formatSum(nets)) {
      divVal.textContent = formatSum(nets);
    } else if (divs.length > 0) {
      divVal.textContent = `${divs.length} payouts`;
    } else {
      divVal.textContent = "—";
    }
  }

  // Metrics: Fees
  const feesVal = document.getElementById("cpFeesVal");
  if (feesVal) {
    const flows = p.flows || [];
    const feeFlows = flows.filter(f => f.label.startsWith("fee:"));
    const feeSum = formatSum(feeFlows.map(f => f.total));
    if (feeSum) {
      feesVal.textContent = feeSum;
    } else if (feeFlows.length > 0) {
      feesVal.textContent = `${feeFlows.length} fee types`;
    } else {
      feesVal.textContent = "—";
    }
  }

  // Owned Positions Table
  renderPositionsTable("cpPositionsBody", p.positions || [], false);

  // CFD Positions Table
  renderPositionsTable("cpCfdBody", p.cfd_positions || [], true);

  // Dividends Table
  renderDividendsTable(p.dividends || []);

  // Update Outlook holding picker (Round 12)
  const heldSymbols = Array.from(new Set((p.positions || []).map(pos => pos.symbol).filter(Boolean))).sort();
  updateOutlookHoldings(heldSymbols);
}

function renderPositionsTable(tbodyId, positions, isCfd) {
  const tbody = document.getElementById(tbodyId);
  if (!tbody) return;

  if (positions.length === 0) {
    tbody.innerHTML = `<tr><td colspan="5" class="cp-empty">${isCfd ? "No CFD positions derived from the ledger. Anything the broker lists appears under Reconciliation." : "No owned positions derived from the ledger. Anything the broker lists appears under Reconciliation."}</td></tr>`;
    return;
  }

  tbody.innerHTML = "";
  for (let i = 0; i < positions.length; i++) {
    const pos = positions[i];
    const inst = pos.instrument || {};
    const tr = document.createElement("tr");

    const lotCount = (pos.lots || []).length;
    const lotsBtn = lotCount > 0
      ? `<button type="button" class="cp-lots-toggle" data-idx="${i}" data-table="${tbodyId}">${lotCount} lot${lotCount === 1 ? "" : "s"}</button>`
      : "0";

    tr.innerHTML = `
      <td>
        <strong>${escapeHtml(inst.symbol || "UNKNOWN")}</strong>
        ${inst.name ? `<br><small style="color:var(--sail-text-subtle)">${escapeHtml(inst.name)}</small>` : ""}
        ${inst.isin ? `<br><small style="color:var(--sail-text-muted)">${escapeHtml(inst.isin)}</small>` : ""}
      </td>
      <td class="num">${escapeHtml(pos.quantity || "0")}</td>
      <td class="num">${pos.cost_basis ? formatMoney(pos.cost_basis) : "—"}</td>
      <td class="num">${pos.broker_market_value ? formatMoney(pos.broker_market_value) : "—"}</td>
      <td>${lotsBtn}</td>
    `;
    tbody.appendChild(tr);

    // Expandable lot row
    if (lotCount > 0) {
      const lotTr = document.createElement("tr");
      lotTr.id = `${tbodyId}-lots-${i}`;
      lotTr.hidden = true;
      const lotsHtml = pos.lots.map(l => {
        const isGranted = l.origin === "granted";
        const originBadge = isGranted
          ? `<span class="cp-lot-badge cp-lot-badge-granted">granted</span>`
          : "";
        return `<li>${formatDate(l.opened ? l.opened.at : null)} · <strong>${escapeHtml(l.quantity)}</strong> units @ ${l.unit_cost ? formatMoney(l.unit_cost) : "not stated"} ${originBadge}</li>`;
      }).join("");
      lotTr.innerHTML = `
        <td colspan="5" class="cp-lots-detail">
          <ul class="cp-lots-list">${lotsHtml}</ul>
        </td>
      `;
      tbody.appendChild(lotTr);
    }
  }

  // Attach toggle listeners
  tbody.querySelectorAll(".cp-lots-toggle").forEach(btn => {
    btn.addEventListener("click", () => {
      const idx = btn.getAttribute("data-idx");
      const tbl = btn.getAttribute("data-table");
      const lotRow = document.getElementById(`${tbl}-lots-${idx}`);
      if (lotRow) {
        lotRow.hidden = !lotRow.hidden;
      }
    });
  });
}

function renderDividendsTable(dividends) {
  const tbody = document.getElementById("cpDividendsBody");
  if (!tbody) return;

  if (dividends.length === 0) {
    tbody.innerHTML = `<tr><td colspan="5" class="cp-empty">No dividend events recorded.</td></tr>`;
    return;
  }

  tbody.innerHTML = "";
  for (const d of dividends) {
    const tr = document.createElement("tr");
    const inst = d.instrument ? d.instrument.symbol : "—";
    tr.innerHTML = `
      <td class="cp-nowrap">${formatDate(d.paid ? d.paid.at : null)}</td>
      <td><strong>${escapeHtml(inst)}</strong></td>
      <td class="num">${formatMoney(d.gross)}</td>
      <td class="num" style="color:#f3b05a">${d.withheld ? formatMoney(d.withheld) : "—"}</td>
      <td class="num" style="color:#5ccda4"><strong>${d.net ? formatMoney(d.net) : "not stated"}</strong></td>
    `;
    tbody.appendChild(tr);
  }
}

async function loadSyncStatus() {
  const list = document.getElementById("cpSyncList");
  if (!list) return;

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const batches = await fetchJson(`/api/capital/sync/status${query}`);

    if (!batches || batches.length === 0) {
      list.innerHTML = `<p class="cp-empty">No imports recorded yet. Upload an official XTB export above.</p>`;
      return;
    }

    list.innerHTML = "";
    for (const b of batches) {
      const card = document.createElement("div");
      card.className = "cp-batch-card";

      const shaShort = b.file_sha256 ? b.file_sha256.substring(0, 10) : "";
      const issues = b.issues || [];
      const issueCount = issues.length;

      let issuesHtml = "";
      if (issueCount > 0) {
        const items = issues.map(iss =>
          `<li class="cp-issue-item" data-severity="${escapeHtml(iss.severity)}">
            [${escapeHtml(iss.severity.toUpperCase())}] ${escapeHtml(iss.code)}: ${escapeHtml(iss.message)}
          </li>`
        ).join("");
        issuesHtml = `
          <details class="cp-issues-details">
            <summary>${issueCount} issue(s) recorded ${b.issues_dropped ? `(${b.issues_dropped} dropped)` : ""}</summary>
            <ul class="cp-issues-list">${items}</ul>
          </details>
        `;
      }

      card.innerHTML = `
        <div class="cp-batch-header">
          <span>${escapeHtml(b.file_name || "Statement export")}</span>
          <span style="color:var(--sail-text-subtle);font-family:monospace;font-size:11px">${shaShort}…</span>
        </div>
        <div class="cp-batch-meta">
          <span>Imported ${formatDate(b.imported_at)}</span>
          <span>Added: <strong>${b.added || 0}</strong></span>
          <span>Duplicates: <strong>${b.duplicates || 0}</strong></span>
          <span>Conflicts: <strong>${b.conflicts || 0}</strong></span>
        </div>
        ${issuesHtml}
      `;
      list.appendChild(card);
    }
  } catch (err) {
    list.innerHTML = `<p class="cp-empty" style="color:#e17055">Failed to load import status: ${escapeHtml(err.message)}</p>`;
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

async function loadSecStatus() {
  try {
    const status = await fetchJson("/api/capital/sec/status");
    const notConfiguredNote = document.getElementById("cpSecNotConfiguredNote");
    if (notConfiguredNote) {
      notConfiguredNote.hidden = Boolean(status.configured);
    }
    const metaSpan = document.getElementById("cpSecStatusMeta");
    if (metaSpan) {
      if (!status.configured) {
        metaSpan.textContent = "SEC access not configured";
      } else if (status.last_refresh_at && status.last_error) {
        metaSpan.textContent = `Last check ${formatDate(status.last_refresh_at)} failed: ${status.last_error}`;
      } else if (status.last_refresh_at) {
        metaSpan.textContent = `Last check ${formatDate(status.last_refresh_at)} · ${status.new_evidence || 0} new filings`;
      } else {
        metaSpan.textContent = "Ready to check SEC EDGAR";
      }
    }
    const badge = document.getElementById("cpSecBadge");
    if (badge) {
      badge.textContent = status.configured ? "EDGAR Active" : "Unconfigured";
    }
  } catch (err) {
    console.error("Failed to load SEC status:", err);
  }
}

async function loadFilings() {
  const list = document.getElementById("cpFilingsList");
  const unmatchedWrap = document.getElementById("cpUnmatchedWrap");
  const unmatchedList = document.getElementById("cpUnmatchedList");
  if (!list) return;

  if (accountList.length === 0) {
    // Nothing imported yet: an empty state, not an error.
    list.innerHTML = `<p class="cp-empty">Import an XTB report to see filings for your holdings.</p>`;
    if (unmatchedWrap) unmatchedWrap.hidden = true;
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const data = await fetchJson(`/api/capital/filings${query}`);
    const items = data.items || [];
    const matches = data.matches || [];

    if (items.length === 0) {
      list.innerHTML = `<p class="cp-empty">No filings recorded for owned holdings.</p>`;
    } else {
      list.innerHTML = "";
      for (const item of items) {
        const card = document.createElement("div");
        card.className = "cp-filing-card";

        const isProposal = item.action === "research_proposal";
        const badgeClass = isProposal ? "cp-badge-proposal" : "cp-badge-watch";
        const badgeLabel = isProposal ? "Research Proposal" : "Watch";

        // Find match for symbol/ticker
        const match = matches.find(m => m.instrument_key === (item.exposure && item.exposure.instrument_key));
        const symbol = match ? match.symbol : (item.exposure && item.exposure.instrument_key) || "";
        const ticker = (match && match.match && match.match.ticker)
          ? match.match.ticker
          : symbol.replace(/\.US$/i, "");

        // Exposure info
        const exposure = item.exposure || {};
        const exposureParts = [];
        if (exposure.quantity) {
          exposureParts.push(`<span>Holding: <strong>${escapeHtml(exposure.quantity)}</strong> units</span>`);
        }
        if (exposure.broker_market_value) {
          exposureParts.push(`<span>Value: <strong>${formatMoney(exposure.broker_market_value)}</strong></span>`);
        }
        if (exposure.share_of_owned_value !== null && exposure.share_of_owned_value !== undefined) {
          const pct = (parseFloat(exposure.share_of_owned_value) * 100).toFixed(2);
          exposureParts.push(`<span>Share: <strong>${pct}%</strong> of owned value</span>`);
        }

        // Link validation: must start with https://www.sec.gov/
        const url = (item.filing && item.filing.url && item.filing.url.startsWith("https://www.sec.gov/"))
          ? item.filing.url
          : null;
        const linkHtml = url
          ? `<a class="cp-filing-link" href="${escapeHtml(url)}" rel="noopener noreferrer" target="_blank">View on sec.gov ↗</a>`
          : "";

        card.innerHTML = `
          <div class="cp-filing-top">
            <div class="cp-filing-title-group">
              <span class="${badgeClass}">${escapeHtml(badgeLabel)}</span>
              <span class="cp-filing-form">${escapeHtml((item.filing && item.filing.form) || "")}</span>
              <span class="cp-filing-issuer">${escapeHtml(item.issuer_name || "")} (${escapeHtml(ticker)})</span>
            </div>
            ${linkHtml}
          </div>
          <div class="cp-filing-reason">${escapeHtml(item.reason || "")}</div>
          ${exposureParts.length > 0 ? `<div class="cp-filing-exposure">${exposureParts.join("")}</div>` : ""}
          <div class="cp-filing-bottom">
            <span class="cp-filing-time">Published ${formatDate(item.published_at)} · first seen by SAIL ${formatDate(item.first_observed_at)}</span>
          </div>
        `;
        list.appendChild(card);
      }
    }

    // Unmatched / non-US holdings
    const uncovered = matches.filter(m => m.match && m.match.status !== "matched");
    if (unmatchedWrap && unmatchedList) {
      if (uncovered.length > 0) {
        unmatchedWrap.hidden = false;
        unmatchedList.innerHTML = uncovered.map(m => {
          const reason = m.match.status === "not_us_listed"
            ? "Non-US listing (no .US suffix); SEC does not apply"
            : (m.match.reason || "Unresolved");
          return `
            <li class="cp-unmatched-item">
              <span class="cp-unmatched-symbol">${escapeHtml(m.symbol)}</span>
              <span class="cp-unmatched-reason">${escapeHtml(reason)}</span>
            </li>
          `;
        }).join("");
      } else {
        unmatchedWrap.hidden = true;
      }
    }
  } catch (err) {
    list.innerHTML = `<p class="cp-empty">Filings unavailable: ${escapeHtml(err.message)}</p>`;
  }
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
    if (section.classList.contains("cp-metrics") || section.classList.contains("cp-suggestions-section")) continue;
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

// -----------------------------------------------------------------------------
// Received Shares Grants & Variance Helpers (Round 9)
// -----------------------------------------------------------------------------

function getMissingQuantity(v) {
  if (!v || !v.subject) return null;
  if (!v.subject.startsWith("quantity ") && !v.subject.startsWith("fifo ")) {
    return null;
  }
  // Received shares are owned units; a CFD difference is never one.
  if (v.subject.endsWith("#cfd")) {
    return null;
  }
  const brokerStr = v.broker || "0";
  const ledgerStr = v.ledger || "0";
  const brokerVal = parseFloat(brokerStr);
  const ledgerVal = parseFloat(ledgerStr);
  if (isNaN(brokerVal) || brokerVal <= ledgerVal) {
    return null;
  }
  return sumDecimals([brokerStr, "-" + ledgerStr]);
}

function resolveVarianceSymbol(subject, portfolio) {
  const raw = subject.replace(/^(quantity|fifo)\s+/, "").replace(/#cfd$/, "").trim();
  if (raw.startsWith("sym:")) {
    return raw.slice(4).trim().toUpperCase();
  }
  if (raw.startsWith("isin:")) {
    const isin = raw.slice(5).trim();
    if (portfolio && portfolio.positions) {
      const match = portfolio.positions.find(p => p.instrument && p.instrument.isin === isin);
      if (match && match.instrument && match.instrument.symbol) {
        return match.instrument.symbol;
      }
    }
    if (portfolio && portfolio.dividends) {
      const match = portfolio.dividends.find(d => d.instrument && d.instrument.isin === isin);
      if (match && match.instrument && match.instrument.symbol) {
        return match.instrument.symbol;
      }
    }
    if (portfolio && portfolio.realized) {
      const match = portfolio.realized.find(r => r.instrument && r.instrument.isin === isin);
      if (match && match.instrument && match.instrument.symbol) {
        return match.instrument.symbol;
      }
    }
    return isin;
  }
  return raw.toUpperCase();
}

function openGrantDialog(symbol, quantity, snapshotAsOf) {
  const dialog = document.getElementById("cpGrantDialog");
  const symInput = document.getElementById("cpGrantSymbol");
  const qtyInput = document.getElementById("cpGrantQuantity");
  const dateInput = document.getElementById("cpGrantDate");
  const costInput = document.getElementById("cpGrantCostAmount");
  const currSelect = document.getElementById("cpGrantCostCurrency");
  const noteInput = document.getElementById("cpGrantNote");
  const kindSelect = document.getElementById("cpGrantKind");

  if (!dialog) return;

  if (symInput) symInput.value = symbol || "";
  if (qtyInput) qtyInput.value = quantity || "1";
  if (kindSelect) kindSelect.value = "stock";
  if (costInput) costInput.value = "";
  if (noteInput) noteInput.value = "";

  const defaultDate = snapshotAsOf
    ? snapshotAsOf.slice(0, 10)
    : new Date().toISOString().slice(0, 10);
  if (dateInput) dateInput.value = defaultDate;

  if (currSelect) {
    const acct = accountList.find(a => a.account === currentAccount);
    if (acct && acct.currency && acct.currency !== "multi") {
      currSelect.value = acct.currency;
    } else {
      currSelect.value = "EUR";
    }
  }

  dialog.showModal();
}

function setupGrantDialog() {
  const dialog = document.getElementById("cpGrantDialog");
  const cancelBtn = document.getElementById("cpGrantCancel");
  const submitBtn = document.getElementById("cpGrantSubmit");

  if (!dialog || !cancelBtn || !submitBtn) return;

  cancelBtn.addEventListener("click", () => {
    dialog.close();
  });

  submitBtn.addEventListener("click", async () => {
    const symInput = document.getElementById("cpGrantSymbol");
    const qtyInput = document.getElementById("cpGrantQuantity");
    const dateInput = document.getElementById("cpGrantDate");
    const costInput = document.getElementById("cpGrantCostAmount");
    const currSelect = document.getElementById("cpGrantCostCurrency");
    const noteInput = document.getElementById("cpGrantNote");
    const kindSelect = document.getElementById("cpGrantKind");

    const symbol = (symInput ? symInput.value : "").trim().toUpperCase();
    const quantity = (qtyInput ? qtyInput.value : "").trim();
    const receivedAt = (dateInput ? dateInput.value : "").trim();
    const costAmount = (costInput ? costInput.value : "").trim();
    const currency = currSelect ? currSelect.value : "EUR";
    const note = (noteInput ? noteInput.value : "").trim();
    const kind = (kindSelect ? kindSelect.value : "stock").toLowerCase();

    if (!symbol) {
      showBanner("Please enter a security symbol.", true);
      return;
    }
    const qtyNum = parseFloat(quantity);
    if (isNaN(qtyNum) || qtyNum <= 0) {
      showBanner("Quantity must be a positive number.", true);
      return;
    }
    if (!receivedAt) {
      showBanner("Please enter the date shares were received.", true);
      return;
    }

    let cost = null;
    if (costAmount && costAmount !== "0") {
      const costVal = parseFloat(costAmount);
      if (isNaN(costVal) || costVal < 0) {
        showBanner("Cost basis must be non-negative.", true);
        return;
      }
      cost = { amount: costAmount, currency: currency };
    }

    const payload = {
      symbol,
      kind,
      quantity,
      received_at: receivedAt,
      cost,
      note: note || undefined,
    };

    submitBtn.disabled = true;
    submitBtn.textContent = "Recording…";

    try {
      const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
      const grant = await fetchJson(`/api/capital/grants${query}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });

      dialog.close();
      showBanner(`Recorded received shares: ${grant.quantity} units of ${grant.symbol}.`);
      await loadAccounts();
      await loadPortfolio();
      await loadInsights();
      await loadGrants();
    } catch (err) {
      showBanner(`Failed to record grant: ${err.message}`, true);
    } finally {
      submitBtn.disabled = false;
      submitBtn.textContent = "Record Shares";
    }
  });
}

async function loadGrants() {
  const grantsContainer = document.getElementById("cpGrants");
  const grantsList = document.getElementById("cpGrantsList");
  if (!grantsContainer || !grantsList) return;

  if (accountList.length === 0) {
    grantsContainer.hidden = true;
    grantsList.innerHTML = "";
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const grants = await fetchJson(`/api/capital/grants${query}`);
    if (!grants || grants.length === 0) {
      grantsContainer.hidden = true;
      grantsList.innerHTML = "";
      return;
    }

    grantsContainer.hidden = false;
    grantsList.innerHTML = "";
    for (const g of grants) {
      const li = document.createElement("li");
      li.className = "cp-grant-item";
      const isRetracted = Boolean(g.retracted_at);
      const statusHtml = isRetracted
        ? `<span class="cp-grant-retracted">Retracted ${formatDate(g.retracted_at)}</span>`
        : `<button type="button" class="cp-button cp-button-sm cp-retract-grant-btn" data-grant-id="${escapeHtml(g.id)}" data-symbol="${escapeHtml(g.symbol)}">Retract</button>`;

      const costText = g.cost && g.cost.amount && g.cost.amount !== "0"
        ? formatMoney(g.cost)
        : "Free (zero cost)";

      li.innerHTML = `
        <div class="cp-grant-info">
          <span class="cp-grant-symbol">${escapeHtml(g.symbol)}</span>
          <span class="cp-badge">${escapeHtml((g.kind || "stock").toUpperCase())}</span>
          <span class="cp-grant-qty"><strong>${escapeHtml(g.quantity)}</strong> units</span>
          <span class="cp-grant-date">Received ${formatDay(g.received_at)}</span>
          <span class="cp-grant-cost">Basis: ${costText}</span>
          ${g.note ? `<span class="cp-grant-note">${escapeHtml(g.note)}</span>` : ""}
        </div>
        <div class="cp-grant-status">${statusHtml}</div>
      `;
      grantsList.appendChild(li);
    }

    grantsList.querySelectorAll(".cp-retract-grant-btn").forEach(btn => {
      btn.addEventListener("click", async () => {
        const id = btn.getAttribute("data-grant-id");
        const sym = btn.getAttribute("data-symbol");
        btn.disabled = true;
        btn.textContent = "Retracting…";
        try {
          const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
          await fetchJson(`/api/capital/grants/${encodeURIComponent(id)}/retract${query}`, {
            method: "POST",
          });
          showBanner(`Retracted received shares for ${sym}.`);
          await loadPortfolio();
          await loadInsights();
          await loadGrants();
        } catch (err) {
          showBanner(`Failed to retract grant: ${err.message}`, true);
          btn.disabled = false;
          btn.textContent = "Retract";
        }
      });
    });
  } catch (err) {
    console.error("Failed to load grants:", err);
  }
}

// -----------------------------------------------------------------------------
// Portfolio Insights (Round 9)
// -----------------------------------------------------------------------------

async function loadInsights() {
  const body = document.getElementById("cpInsightsBody");
  const freshnessSpan = document.getElementById("cpInsightsFreshness");
  if (!body) return;

  if (accountList.length === 0) {
    body.innerHTML = `<p class="cp-empty">Import a report to see insights</p>`;
    if (freshnessSpan) freshnessSpan.textContent = "Deterministic calculation from ledger";
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const ins = await fetchJson(`/api/capital/insights${query}`);
    if (!hasTodayAccount && ins.account) {
      updateAccountHeaderTiles(ins.account);
    }
    renderInsights(ins);
  } catch (err) {
    // 404 means no portfolio yet — show empty state, not an error
    if (String(err.message).includes("404") || String(err.message).toLowerCase().includes("not found")) {
      body.innerHTML = `<p class="cp-empty">Import a report to see insights</p>`;
      if (freshnessSpan) freshnessSpan.textContent = "Deterministic calculation from ledger";
    } else {
      body.innerHTML = `<p class="cp-empty" style="color:var(--sail-text-subtle)">Insights unavailable: ${escapeHtml(err.message)}</p>`;
    }
  }
}

function renderInsights(ins) {
  const body = document.getElementById("cpInsightsBody");
  const freshnessSpan = document.getElementById("cpInsightsFreshness");
  if (!body) return;

  // 1. Freshness
  const f = ins.freshness || {};
  let freshnessHtml = "";
  if (f.snapshot_as_of) {
    const age = f.snapshot_age_hours !== null && f.snapshot_age_hours !== undefined
      ? ` (${formatAge(f.snapshot_age_hours)})`
      : "";
    const label = `Data as of ${formatDate(f.snapshot_as_of)}${age}`;
    if (freshnessSpan) {
      freshnessSpan.textContent = label;
      freshnessSpan.className = f.stale ? "cp-sub-notice cp-freshness-stale" : "cp-sub-notice";
    }
    freshnessHtml = `
      <div class="cp-insights-freshness-line">
        <span class="cp-freshness ${f.stale ? "cp-freshness-stale" : ""}">Data as of ${formatDate(f.snapshot_as_of)}${age}</span>
        ${f.stale ? `<span style="font-size:11px;color:#f3b05a">Snapshot older than 72h</span>` : ""}
      </div>
    `;
  } else {
    if (freshnessSpan) freshnessSpan.textContent = "No broker snapshot";
  }

  // 2. Return since the first deposit
  const signClass = (money) =>
    money && String(money.amount).startsWith("-") ? "negative" : "positive";
  let returnHtml = "";
  if (ins.total_return) {
    const tr = ins.total_return;
    const ratio = tr.gain_ratio !== null && tr.gain_ratio !== undefined
      ? ` (${(parseFloat(tr.gain_ratio) * 100).toFixed(2)}%)`
      : "";
    returnHtml += `
      <div class="cp-insights-block">
        <h3 class="cp-insights-subtitle">Since the first deposit</h3>
        <p class="cp-insights-line">
          Deposited ${formatMoney(tr.deposits)}, withdrew ${formatMoney(tr.withdrawals)}:
          <strong>net ${formatMoney(tr.net_deposits)}</strong> put in.
          The account is worth ${formatMoney(tr.current_value)} now, so it is
          <strong class="${signClass(tr.gain)}">${formatMoney(tr.gain)}${escapeHtml(ratio)}</strong>
          above what was put in. This includes dividends, interest, fees, closed trades and CFD results.
        </p>
      </div>
    `;
  }

  // 3. Concentration
  let concentrationHtml = "";
  if (ins.concentration) {
    const c = ins.concentration;
    const effStr = c.effective_positions ? `${c.effective_positions}` : "—";
    const largestPct = (parseFloat(c.largest_share) * 100).toFixed(1);
    const topNPct = (parseFloat(c.top_n_share) * 100).toFixed(1);

    concentrationHtml = `
      <div class="cp-insights-block">
        <h3 class="cp-insights-subtitle">Portfolio Concentration</h3>
        <div class="cp-concentration-card">
          <p class="cp-concentration-summary">
            Portfolio concentration is <strong>as concentrated as ${escapeHtml(effStr)} equal holdings</strong> across ${c.positions} valued position${c.positions === 1 ? "" : "s"}.
          </p>
          <div class="cp-concentration-grid">
            <div class="cp-conc-item">
              <span class="cp-conc-label">Largest Position</span>
              <span class="cp-conc-val">${escapeHtml(c.largest_symbol)} (${largestPct}%)</span>
            </div>
            <div class="cp-conc-item">
              <span class="cp-conc-label">Top 5 Holdings</span>
              <span class="cp-conc-val">${topNPct}% of portfolio</span>
            </div>
            <div class="cp-conc-item">
              <span class="cp-conc-label">Effective Positions</span>
              <span class="cp-conc-val">${escapeHtml(effStr)} equal holdings</span>
            </div>
            <div class="cp-conc-item">
              <span class="cp-conc-label">HHI Index</span>
              <span class="cp-conc-val">${escapeHtml(c.hhi)}</span>
            </div>
          </div>
        </div>
      </div>
    `;
  }

  // 4. Allocation by Position & By Kind
  let allocationHtml = "";
  if (ins.allocation && ins.allocation.length > 0) {
    const rows = ins.allocation.map(slice => {
      const shareVal = parseFloat(slice.share) || 0;
      const pct = (shareVal * 100).toFixed(1);
      const widthPct = Math.min(100, Math.max(0, shareVal * 100));
      return `
        <div class="cp-alloc-row">
          <div class="cp-alloc-info">
            <span class="cp-alloc-symbol">${escapeHtml(slice.symbol)}</span>
            ${slice.name ? `<span class="cp-alloc-name">${escapeHtml(slice.name)}</span>` : ""}
            <span class="cp-alloc-val">${formatMoney(slice.value)}</span>
            <span class="cp-alloc-share">${pct}%</span>
          </div>
          <div class="cp-bar-track">
            <div class="cp-bar-fill" style="width: ${widthPct}%"></div>
          </div>
        </div>
      `;
    }).join("");

    let kindCards = "";
    if (ins.by_kind && ins.by_kind.length > 0) {
      kindCards = ins.by_kind.map(k => {
        const shareVal = parseFloat(k.share) || 0;
        const pct = (shareVal * 100).toFixed(1);
        const kindLabel = k.kind.charAt(0).toUpperCase() + k.kind.slice(1);
        return `
          <div class="cp-kind-card">
            <div class="cp-kind-top">
              <strong>${escapeHtml(kindLabel)}</strong>
              <span class="cp-badge">${k.positions} pos</span>
            </div>
            <div class="cp-kind-meta">
              <span class="cp-kind-val">${formatMoney(k.value)}</span>
              <span class="cp-kind-share">${pct}%</span>
            </div>
            <div class="cp-bar-track">
              <div class="cp-bar-fill" style="width: ${Math.min(100, Math.max(0, shareVal * 100))}%"></div>
            </div>
          </div>
        `;
      }).join("");
    }

    allocationHtml = `
      <div class="cp-insights-block">
        <div class="cp-insights-subtitle">
          <span>Allocation by Position</span>
          <span style="font-size:11px;color:var(--sail-text-subtle);font-weight:normal">${ins.allocation.length} position${ins.allocation.length === 1 ? "" : "s"}</span>
        </div>
        <div class="cp-alloc-bars">${rows}</div>
        ${kindCards ? `
          <div class="cp-insights-subtitle" style="margin-top:12px">
            <span>By Instrument Kind</span>
          </div>
          <div class="cp-by-kind-grid">${kindCards}</div>
        ` : ""}
      </div>
    `;
  }

  // 5. Income & Distributions
  let incomeHtml = "";
  if (ins.income) {
    const inc = ins.income;
    const trailing12m = inc.trailing_12m_net && inc.trailing_12m_net.length > 0
      ? inc.trailing_12m_net.map(formatMoney).join(" · ")
      : "—";
    const yieldStr = inc.trailing_yield !== null && inc.trailing_yield !== undefined
      ? `${(parseFloat(inc.trailing_yield) * 100).toFixed(2)}%`
      : "—";

    const months = (inc.by_month || []).slice(-24);
    let monthRows = "";
    if (months.length > 0) {
      let maxNet = 0;
      for (const m of months) {
        const netVal = parseFloat(m.net ? m.net.amount : "0");
        if (netVal > maxNet) maxNet = netVal;
      }
      monthRows = months.map(m => {
        const netVal = parseFloat(m.net ? m.net.amount : "0");
        const barPct = maxNet > 0 ? Math.min(100, Math.max(0, (netVal / maxNet) * 100)) : 0;
        return `
          <div class="cp-income-month-row">
            <span class="cp-month-label">${escapeHtml(m.month)}</span>
            <div class="cp-month-bar-wrap">
              <div class="cp-month-bar-fill" style="width: ${barPct}%"></div>
            </div>
            <span class="cp-month-net">${formatMoney(m.net)}</span>
          </div>
        `;
      }).join("");
    } else {
      monthRows = `<p class="cp-empty" style="padding:10px">No income recorded in the last 24 months.</p>`;
    }

    incomeHtml = `
      <div class="cp-insights-block">
        <h3 class="cp-insights-subtitle">Income & Distributions</h3>
        <div class="cp-income-meta">
          <div class="cp-income-stat">Trailing 12M Net: <strong>${trailing12m}</strong></div>
          <div class="cp-income-stat">Trailing Yield: <strong>${yieldStr}</strong></div>
        </div>
        <div class="cp-income-months">${monthRows}</div>
      </div>
    `;
  }

  // 6. Warnings and Unvalued Notes
  let notesHtml = "";
  const notes = [];
  if (ins.unvalued && ins.unvalued.length > 0) {
    notes.push(`Positions without broker market value (excluded from allocation): <strong>${ins.unvalued.map(escapeHtml).join(", ")}</strong>`);
  }
  if (ins.warnings && ins.warnings.length > 0) {
    for (const w of ins.warnings) {
      notes.push(escapeHtml(w));
    }
  }
  if (notes.length > 0) {
    const items = notes.map(n => `<div class="cp-insight-note">${n}</div>`).join("");
    notesHtml = `
      <div class="cp-insight-notes">
        ${items}
      </div>
    `;
  }

  body.innerHTML = `
    ${freshnessHtml}
    ${returnHtml}
    ${concentrationHtml}
    ${allocationHtml}
    ${incomeHtml}
    ${notesHtml}
  `;
}

// -----------------------------------------------------------------------------
// Forecasts & Baselines (Round 10)
// -----------------------------------------------------------------------------

async function loadPriceStatus() {
  try {
    const status = await fetchJson("/api/capital/prices/status");
    const optInNote = document.getElementById("cpPriceOptInNote");
    if (optInNote) {
      optInNote.hidden = Boolean(status.configured);
    }
    const dataOptInNote = document.getElementById("cpDataPriceOptInNote");
    if (dataOptInNote) {
      dataOptInNote.hidden = Boolean(status.configured);
    }

    const statusLine = document.getElementById("cpPriceStatusLine");
    const dataStatusLine = document.getElementById("cpDataPriceStatusLine");
    let statusText = "";
    if (!status.configured) {
      statusText = "Prices not configured · 0 instruments priced";
    } else {
      const sourceName = status.source || "provider";
      const instCount = status.instruments || 0;
      const obsCount = status.observations || 0;
      statusText = `Source: ${sourceName} · ${instCount} instruments priced (${obsCount} bars) · Last refresh: ${formatDate(status.last_refresh_at)}`;
      if (status.last_error) {
        statusText += ` · Last error: ${status.last_error}`;
      }
    }
    if (statusLine) statusLine.textContent = statusText;
    if (dataStatusLine) dataStatusLine.textContent = statusText;

    const badge = document.getElementById("cpPriceBadge");
    if (badge) {
      badge.textContent = status.configured ? ({ yfinance: "Yahoo", stooq: "Stooq" }[status.source] || status.source || "Active") : "Unconfigured";
    }

    const unmappedNote = document.getElementById("cpPriceUnmappedNote");
    const dataUnmappedNote = document.getElementById("cpDataPriceUnmappedNote");
    if (status.unmapped && status.unmapped.length > 0) {
      const unmappedHtml = `<p>Unmapped instruments (CFD or unrecognised suffix): <strong>${status.unmapped.map(escapeHtml).join(", ")}</strong>. Track prices by uploading a price file.</p>`;
      if (unmappedNote) {
        unmappedNote.hidden = false;
        unmappedNote.innerHTML = unmappedHtml;
      }
      if (dataUnmappedNote) {
        dataUnmappedNote.hidden = false;
        dataUnmappedNote.innerHTML = unmappedHtml;
      }
    } else {
      if (unmappedNote) {
        unmappedNote.hidden = true;
        unmappedNote.innerHTML = "";
      }
      if (dataUnmappedNote) {
        dataUnmappedNote.hidden = true;
        dataUnmappedNote.innerHTML = "";
      }
    }
  } catch (err) {
    console.error("Failed to load price status:", err);
  }
}

function setupPriceControls() {
  const horizonBtns = document.querySelectorAll(".cp-horizon-btn");
  horizonBtns.forEach(btn => {
    btn.addEventListener("click", () => {
      horizonBtns.forEach(b => {
        b.classList.remove("cp-active");
        b.removeAttribute("aria-checked");
      });
      btn.classList.add("cp-active");
      btn.setAttribute("aria-checked", "true");
      currentHorizon = parseInt(btn.getAttribute("data-horizon"), 10) || 20;
      loadForecasts();
    });
  });

  const modelSelect = document.getElementById("cpForecastModelSelect");
  if (modelSelect) {
    modelSelect.addEventListener("change", (e) => {
      currentModel = e.target.value;
      loadForecasts();
    });
  }

  const refreshBtn = document.getElementById("cpPriceRefreshBtn");
  if (refreshBtn) {
    refreshBtn.addEventListener("click", async () => {
      refreshBtn.disabled = true;
      refreshBtn.textContent = "Fetching prices...";
      try {
        const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
        const res = await fetchJson(`/api/capital/prices/refresh${query}`, { method: "POST" });
        await loadPriceStatus();
        await loadForecasts();
        await loadEvaluation();
        if (res.last_error) {
          showBanner(`Price fetch stopped: ${res.last_error}`, true);
        } else {
          showBanner(`Prices refreshed: ${res.instruments || 0} instruments priced (${res.observations || 0} bars).`);
        }
      } catch (err) {
        showBanner(`Price refresh failed: ${err.message}`, true);
      } finally {
        refreshBtn.disabled = false;
        refreshBtn.textContent = "Fetch prices";
      }
    });
  }
}

function setupPriceFileDialog() {
  const dialog = document.getElementById("cpPriceFileDialog");
  const openBtn = document.getElementById("cpPriceUploadBtn");
  const cancelBtn = document.getElementById("cpPriceFileCancel");
  const submitBtn = document.getElementById("cpPriceFileSubmit");
  const symInput = document.getElementById("cpPriceFileSymbol");
  const curSelect = document.getElementById("cpPriceFileCurrency");
  const adjSelect = document.getElementById("cpPriceFileAdjustment");
  const fileInput = document.getElementById("cpPriceFileInput");
  if (!dialog || !openBtn) return;

  openBtn.addEventListener("click", () => {
    if (symInput) symInput.value = "";
    if (fileInput) fileInput.value = "";
    dialog.showModal();
  });

  if (cancelBtn) {
    cancelBtn.addEventListener("click", () => {
      dialog.close();
    });
  }

  if (submitBtn) {
    submitBtn.addEventListener("click", async () => {
      const symbol = (symInput.value || "").trim().toUpperCase();
      const file = fileInput.files && fileInput.files[0];
      if (!symbol) {
        showBanner("Please enter a broker symbol (e.g. GALP.PT).", true);
        return;
      }
      if (!file) {
        showBanner("Please choose a CSV file to import.", true);
        return;
      }

      dialog.close();
      setState("loading", "Importing prices…");
      showBanner(`Uploading price file for ${symbol}…`);

      try {
        const buffer = await file.arrayBuffer();
        const params = new URLSearchParams({
          symbol,
          currency: curSelect.value,
          adjustment: adjSelect.value,
        });
        if (currentAccount) params.set("account", currentAccount);
        const res = await fetchJson(`/api/capital/prices/import?${params.toString()}`, {
          method: "POST",
          headers: {
            "Content-Type": "application/octet-stream",
            "X-File-Name": file.name,
          },
          body: buffer,
        });

        showBanner(`Imported ${res.added || 0} daily closes for ${res.symbol || symbol} (${res.sessions_seen || 0} seen, ${res.revisions || 0} revisions).`);
        setState("idle", "Ready");
        await loadPriceStatus();
        await loadForecasts();
        await loadEvaluation();
      } catch (err) {
        setState("error", "Error");
        showBanner(`Price import failed: ${err.message}`, true);
      } finally {
        symInput.value = "";
        fileInput.value = "";
      }
    });
  }
}

async function loadForecasts() {
  const list = document.getElementById("cpForecastsList");
  const withoutPricesWrap = document.getElementById("cpWithoutPricesWrap");
  if (!list) return;

  if (accountList.length === 0) {
    list.innerHTML = `<p class="cp-empty">Fetch or upload prices to see forecasts</p>`;
    if (withoutPricesWrap) withoutPricesWrap.hidden = true;
    return;
  }

  try {
    const params = new URLSearchParams({
      horizon: String(currentHorizon),
      model: currentModel,
    });
    if (currentAccount) params.set("account", currentAccount);
    const data = await fetchJson(`/api/capital/forecasts?${params.toString()}`);
    renderForecasts(data);
  } catch (err) {
    if (String(err.message).includes("404") || String(err.message).toLowerCase().includes("not found")) {
      list.innerHTML = `<p class="cp-empty">Fetch or upload prices to see forecasts</p>`;
    } else {
      list.innerHTML = `<p class="cp-empty" style="color:var(--sail-text-subtle)">Forecasts unavailable: ${escapeHtml(err.message)}</p>`;
    }
    if (withoutPricesWrap) withoutPricesWrap.hidden = true;
  }
}

function renderForecasts(data) {
  const list = document.getElementById("cpForecastsList");
  const withoutPricesWrap = document.getElementById("cpWithoutPricesWrap");
  if (!list) return;

  if (!data || !data.forecasts || data.forecasts.length === 0) {
    list.innerHTML = `<p class="cp-empty">Fetch or upload prices to see forecasts</p>`;
    if (withoutPricesWrap) withoutPricesWrap.hidden = true;
    return;
  }

  list.innerHTML = "";
  for (const f of data.forecasts) {
    const card = document.createElement("div");
    card.className = "cp-forecast-card";

    // Symbol & Close header
    const sym = escapeHtml(f.symbol);
    const dateStr = formatDay(f.as_of_session);
    const closeStr = f.reference_close ? `${groupDecimal(f.reference_close)}` : "—";

    let calibratedHtml = "";
    if (f.probability_positive !== null && f.probability_positive !== undefined) {
      const probPct = (f.probability_positive * 100).toFixed(0);
      calibratedHtml = `<span class="cp-badge-calibrated" title="Verified calibrated in evaluation">P(&gt;0): ${probPct}% (calibrated)</span>`;
    }

    const headHtml = `
      <div class="cp-forecast-card-head">
        <div class="cp-forecast-card-sym">
          <strong>${sym}</strong>
          <span class="cp-forecast-card-close">Last close: ${closeStr} (${dateStr})</span>
        </div>
        <div class="cp-forecast-badges">
          ${calibratedHtml}
        </div>
      </div>
    `;

    let bodyHtml = "";
    if (f.status && f.status.status === "insufficient_evidence") {
      const reason = f.status.reason || "Not enough observations for baseline calculation";
      bodyHtml = `
        <div class="cp-forecast-insufficient">
          <span class="cp-badge-insufficient">insufficient evidence</span>
          <span class="cp-forecast-reason">${escapeHtml(reason)}</span>
        </div>
      `;
    } else if (f.quantiles && f.quantiles.length >= 2) {
      // Find 5%, median (50%), and 95% quantiles
      const q05 = f.quantiles.find(q => Math.abs(q.p - 0.05) < 0.01) || f.quantiles[0];
      const q95 = f.quantiles.find(q => Math.abs(q.p - 0.95) < 0.01) || f.quantiles[f.quantiles.length - 1];
      const q50 = f.quantiles.find(q => Math.abs(q.p - 0.50) < 0.01) || {
        log_return: f.median_log_return !== null && f.median_log_return !== undefined ? f.median_log_return : 0,
      };

      const r05 = q05.log_return;
      const r50 = q50.log_return;
      const r95 = q95.log_return;

      // Percent changes: exp(q) - 1
      const pct05Val = (Math.exp(r05) - 1) * 100;
      const pct50Val = (Math.exp(r50) - 1) * 100;
      const pct95Val = (Math.exp(r95) - 1) * 100;

      const fmtPct = (val) => `${val >= 0 ? "+" : ""}${val.toFixed(1)}%`;
      const pct05Str = fmtPct(pct05Val);
      const pct50Str = fmtPct(pct50Val);
      const pct95Str = fmtPct(pct95Val);

      // Price levels: close * exp(q)
      const refCloseNum = parseFloat(f.reference_close);
      const p05Val = !isNaN(refCloseNum) ? refCloseNum * Math.exp(r05) : null;
      const p50Val = f.median_terminal_price !== null && f.median_terminal_price !== undefined
        ? f.median_terminal_price
        : (!isNaN(refCloseNum) ? refCloseNum * Math.exp(r50) : null);
      const p95Val = !isNaN(refCloseNum) ? refCloseNum * Math.exp(r95) : null;

      const fmtPrice = (val) => val !== null && !isNaN(val) ? val.toFixed(2) : "—";
      const p05Str = fmtPrice(p05Val);
      const p50Str = fmtPrice(p50Val);
      const p95Str = fmtPrice(p95Val);

      // Visual range bar calculations
      const span = r95 - r05;
      let medianPos = 50;
      let zeroPos = null;
      if (span > 0.000001) {
        medianPos = Math.min(98, Math.max(2, ((r50 - r05) / span) * 100));
        if (r05 <= 0 && r95 >= 0) {
          zeroPos = ((0 - r05) / span) * 100;
        }
      }

      const zeroTickHtml = zeroPos !== null
        ? `<div class="cp-forecast-bar-zero" style="left: ${zeroPos.toFixed(1)}%" title="Reference price (0% change)"></div>`
        : "";
      const medianTickHtml = `<div class="cp-forecast-bar-median" style="left: ${medianPos.toFixed(1)}%" title="Median: ${pct50Str} (${p50Str})"></div>`;

      bodyHtml = `
        <div class="cp-forecast-range-wrap">
          <div class="cp-forecast-bar-track">
            <div class="cp-forecast-bar-fill"></div>
            ${zeroTickHtml}
            ${medianTickHtml}
          </div>
          <div class="cp-forecast-range-labels">
            <div class="cp-forecast-label-col">
              <span class="cp-forecast-val-pct ${pct05Val < 0 ? "neg" : "pos"}">5%: ${pct05Str}</span>
              <span class="cp-forecast-val-price">${p05Str}</span>
            </div>
            <div class="cp-forecast-label-col center">
              <span class="cp-forecast-val-pct ${pct50Val < 0 ? "neg" : "pos"}">Median: ${pct50Str}</span>
              <span class="cp-forecast-val-price">${p50Str}</span>
            </div>
            <div class="cp-forecast-label-col right">
              <span class="cp-forecast-val-pct ${pct95Val < 0 ? "neg" : "pos"}">95%: ${pct95Str}</span>
              <span class="cp-forecast-val-price">${p95Str}</span>
            </div>
          </div>
        </div>
      `;
    }

    card.innerHTML = `${headHtml}${bodyHtml}`;
    list.appendChild(card);
  }

  if (withoutPricesWrap) {
    if (data.without_prices && data.without_prices.length > 0) {
      withoutPricesWrap.hidden = false;
      withoutPricesWrap.innerHTML = `Holdings without price history: <strong>${data.without_prices.map(escapeHtml).join(", ")}</strong> (import prices using <em>Upload price file</em>).`;
    } else {
      withoutPricesWrap.hidden = true;
      withoutPricesWrap.innerHTML = "";
    }
  }
}

function modelDisplayName(m) {
  switch (m) {
    case "no_change": return "No Change (baseline)";
    case "trailing_drift": return "Trailing Drift";
    case "ewma_volatility": return "EWMA Volatility";
    case "empirical_horizon": return "Empirical Horizon";
    default: return m;
  }
}

async function loadEvaluation() {
  const wrap = document.getElementById("cpScorecardWrap");
  const meta = document.getElementById("cpScorecardMeta");
  const notesWrap = document.getElementById("cpScorecardNotes");
  if (!wrap) return;

  if (accountList.length === 0) {
    wrap.innerHTML = `<p class="cp-empty">No evaluation available yet</p>`;
    if (notesWrap) notesWrap.hidden = true;
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const report = await fetchJson(`/api/capital/forecasts/evaluation${query}`);
    renderEvaluation(report);
  } catch (err) {
    if (String(err.message).includes("404") || String(err.message).toLowerCase().includes("not found")) {
      wrap.innerHTML = `<p class="cp-empty">No evaluation available yet</p>`;
    } else {
      wrap.innerHTML = `<p class="cp-empty" style="color:var(--sail-text-subtle)">Evaluation unavailable: ${escapeHtml(err.message)}</p>`;
    }
    if (notesWrap) notesWrap.hidden = true;
  }
}

function renderEvaluation(report) {
  const wrap = document.getElementById("cpScorecardWrap");
  const meta = document.getElementById("cpScorecardMeta");
  const notesWrap = document.getElementById("cpScorecardNotes");
  if (!wrap) return;

  if (!report || !report.scores || report.scores.length === 0) {
    wrap.innerHTML = `<p class="cp-empty">Evaluation needs at least 252 daily returns per instrument to score baselines.</p>`;
    if (notesWrap) notesWrap.hidden = true;
    if (meta) meta.textContent = "Out-of-sample backtest";
    return;
  }

  if (meta) {
    const instCount = report.instruments ? report.instruments.length : 0;
    const originsStr = report.origins && report.origins[0] && report.origins[1]
      ? ` · ${report.origins[0]} to ${report.origins[1]}`
      : "";
    meta.textContent = `${instCount} instruments evaluated${originsStr}`;
  }

  const horizons = (report.config && report.config.horizons) || [5, 20, 60];
  const rankIcMap = new Map();
  if (report.rank_ic) {
    for (const r of report.rank_ic) {
      rankIcMap.set(`${r.model}:${r.horizon_sessions}`, r);
    }
  }

  let html = `<div class="cp-scorecard-groups">`;
  for (const h of horizons) {
    const horizonScores = report.scores.filter(s => s.horizon_sessions === h);
    if (horizonScores.length === 0) continue;

    const horizonLabel = h === 5 ? "5 sessions (1 week)" : (h === 20 ? "20 sessions (1 month)" : `${h} sessions (1 quarter)`);

    let rowsHtml = "";
    for (const s of horizonScores) {
      const name = modelDisplayName(s.model);
      const count = s.forecasts;

      // CRPS Skill vs NoChange
      let skillHtml = "—";
      if (s.model === "no_change") {
        skillHtml = `<span class="cp-skill-zero">0.0% (base)</span>`;
      } else {
        const skillPct = (s.crps_skill * 100).toFixed(1);
        if (s.crps_skill > 0.0005) {
          skillHtml = `<span class="cp-skill-pos">+${skillPct}%</span>`;
        } else if (s.crps_skill < -0.0005) {
          skillHtml = `<span class="cp-skill-neg">${skillPct}%</span>`;
        } else {
          skillHtml = `<span class="cp-skill-zero">${skillPct}%</span>`;
        }
      }

      // 90% coverage
      const cov90Pct = (s.interval_90 * 100).toFixed(1);

      // Direction hit rate
      const dirPct = (s.direction_hit_rate * 100).toFixed(1);

      // Calibrated tick
      const calHtml = s.calibrated
        ? `<span class="cp-scorecard-calibrated-yes" title="Calibrated (covers 85-95% & median 42-58%)">✓</span>`
        : `<span class="cp-scorecard-calibrated-no">—</span>`;

      // Rank IC
      const icKey = `${s.model}:${h}`;
      const ic = rankIcMap.get(icKey);
      let icStr = "—";
      if (ic && ic.origins > 0) {
        icStr = ic.mean >= 0 ? `+${ic.mean.toFixed(3)}` : ic.mean.toFixed(3);
      }

      rowsHtml += `
        <tr>
          <td><strong>${escapeHtml(name)}</strong></td>
          <td class="num">${count}</td>
          <td class="num">${skillHtml}</td>
          <td class="num">${cov90Pct}%</td>
          <td class="num">${dirPct}%</td>
          <td class="center">${calHtml}</td>
          <td class="num">${icStr}</td>
        </tr>
      `;
    }

    html += `
      <div class="cp-scorecard-group">
        <h4 class="cp-scorecard-group-title">Horizon: ${horizonLabel}</h4>
        <div class="cp-table-wrap">
          <table class="cp-scorecard-table">
            <thead>
              <tr>
                <th scope="col">Model</th>
                <th scope="col" class="num">Forecasts</th>
                <th scope="col" class="num">CRPS Skill</th>
                <th scope="col" class="num">90% Coverage</th>
                <th scope="col" class="num">Direction Hit</th>
                <th scope="col" class="center">Calibrated</th>
                <th scope="col" class="num">Rank IC</th>
              </tr>
            </thead>
            <tbody>
              ${rowsHtml}
            </tbody>
          </table>
        </div>
      </div>
    `;
  }
  html += `</div>`;
  wrap.innerHTML = html;

  // Notes and Skipped
  if (notesWrap) {
    const notes = [];
    if (report.skipped && report.skipped.length > 0) {
      const skippedStr = report.skipped.map(([sym, reason]) => `${sym} (${reason})`).join(", ");
      notes.push(`Skipped from evaluation: ${skippedStr}`);
    }
    if (report.notes && report.notes.length > 0) {
      for (const n of report.notes) {
        notes.push(n);
      }
    }
    if (notes.length > 0) {
      notesWrap.hidden = false;
      notesWrap.innerHTML = notes.map(n => `<div class="cp-scorecard-note">${escapeHtml(n)}</div>`).join("");
    } else {
      notesWrap.hidden = true;
      notesWrap.innerHTML = "";
    }
  }
}

// -----------------------------------------------------------------------------
// Tabs Setup (Round 11)
// -----------------------------------------------------------------------------
const TABS = ["today", "holdings", "income", "forecasts", "research", "data"];
const TAB_STORAGE_KEY = "sail_capital_active_tab";

function getActiveTab() {
  const hash = window.location.hash.replace(/^#/, "").toLowerCase();
  if (TABS.includes(hash)) return hash;
  try {
    const saved = localStorage.getItem(TAB_STORAGE_KEY);
    if (TABS.includes(saved)) return saved;
  } catch (_) {}
  return "today";
}

function switchTab(tabId, updateUrl = true) {
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
    const hash = window.location.hash.replace(/^#/, "").toLowerCase();
    if (TABS.includes(hash)) {
      switchTab(hash, false);
    }
  });

  const initial = getActiveTab();
  switchTab(initial, false);

  const reviewLimitsLink = document.getElementById("cpReviewLimitsLink");
  if (reviewLimitsLink) {
    reviewLimitsLink.addEventListener("click", () => {
      switchTab("data", true);
      const limitsSection = document.getElementById("cpLimitsSection");
      if (limitsSection) {
        limitsSection.scrollIntoView({ behavior: "smooth" });
      }
    });
  }
}

// -----------------------------------------------------------------------------
// Today View (Round 11)
// -----------------------------------------------------------------------------
async function loadToday() {
  const stanceBanner = document.getElementById("cpStanceBanner");
  const stanceSentence = document.getElementById("cpStanceSentence");
  const draftNotice = document.getElementById("cpPolicyDraftNotice");
  const suggestionsList = document.getElementById("cpSuggestionsList");
  const hiddenCount = document.getElementById("cpSuggestionsHidden");

  if (!suggestionsList) return;

  if (accountList.length === 0) {
    if (stanceSentence) stanceSentence.textContent = "No accounts recorded yet.";
    if (stanceBanner) stanceBanner.setAttribute("data-stance", "hold");
    if (draftNotice) draftNotice.hidden = true;
    suggestionsList.innerHTML = `<p class="cp-empty">Import an official XTB report above to see suggestions.</p>`;
    if (hiddenCount) hiddenCount.hidden = true;
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const today = await fetchJson(`/api/capital/today${query}`);

    // 1. Account header tiles
    if (today.account) {
      hasTodayAccount = true;
      updateAccountHeaderTiles(today.account);
    } else {
      hasTodayAccount = false;
    }

    // 2. Stance banner
    if (today.stance) {
      if (stanceSentence) stanceSentence.textContent = today.stance.sentence || "";
      if (stanceBanner) stanceBanner.setAttribute("data-stance", today.stance.label || "hold");
    }

    // 3. Policy draft notice
    if (draftNotice) {
      draftNotice.hidden = !(today.policy && today.policy.status === "draft");
    }

    // 4. Suggestions list
    renderSuggestions(today.suggestions || [], today.account);

    // 5. Hidden count
    if (hiddenCount) {
      const count = today.hidden || 0;
      hiddenCount.textContent = `${count} hidden`;
      hiddenCount.hidden = false;
    }
  } catch (err) {
    hasTodayAccount = false;
    // 404 indicates no XTB report imported yet
    if (String(err.message).includes("404") || String(err.message).toLowerCase().includes("not found")) {
      if (stanceSentence) stanceSentence.textContent = "No statements imported yet.";
      if (stanceBanner) stanceBanner.setAttribute("data-stance", "hold");
      if (draftNotice) draftNotice.hidden = true;
      suggestionsList.innerHTML = `<p class="cp-empty">Upload an official XTB export above to see suggestions.</p>`;
      if (hiddenCount) hiddenCount.hidden = true;
    } else {
      if (stanceSentence) stanceSentence.textContent = "Today suggestions unavailable.";
      suggestionsList.innerHTML = `<p class="cp-empty" style="color:var(--sail-text-subtle)">Could not load suggestions: ${escapeHtml(err.message)}</p>`;
    }
  }
}

function renderSuggestions(suggestions, account) {
  const container = document.getElementById("cpSuggestionsList");
  if (!container) return;

  if (suggestions.length === 0) {
    container.innerHTML = `<p class="cp-empty">No active suggestions right now.</p>`;
    return;
  }

  const accountCurrency = (account && account.account_value && account.account_value.currency) || "EUR";

  container.innerHTML = suggestions.map((s) => {
    const kindLabel = s.kind.charAt(0).toUpperCase() + s.kind.slice(1);
    const severity = (s.severity || "low").toLowerCase();
    const kind = (s.kind || "attention").toLowerCase();

    // Sized changes table
    let changesHtml = "";
    if (s.changes && s.changes.length > 0) {
      const rows = s.changes.map(c => {
        const fromPct = typeof c.from_pct === "number" ? c.from_pct.toFixed(1) : "—";
        const toPct = typeof c.to_pct === "number" ? c.to_pct.toFixed(1) : "—";
        const isPos = typeof c.amount === "number" && c.amount >= 0;
        const sign = isPos ? "+" : "-";
        const absAmt = typeof c.amount === "number" ? Math.abs(c.amount).toFixed(2) : "0.00";
        const formattedAmt = `${sign}${groupDecimal(absAmt)} ${accountCurrency}`;
        return `
          <tr>
            <td><strong>${escapeHtml(c.symbol)}</strong></td>
            <td class="num">${fromPct}%</td>
            <td class="num">${toPct}%</td>
            <td class="num ${isPos ? "positive" : "negative"}">${escapeHtml(formattedAmt)}</td>
          </tr>
        `;
      }).join("");

      changesHtml = `
        <div class="cp-changes-table-wrap">
          <table class="cp-changes-table">
            <thead>
              <tr>
                <th scope="col">Symbol</th>
                <th scope="col" class="num">From</th>
                <th scope="col" class="num">To</th>
                <th scope="col" class="num">Amount</th>
              </tr>
            </thead>
            <tbody>${rows}</tbody>
          </table>
        </div>
      `;
    }

    // Evidence chips
    let evidenceHtml = "";
    if (s.evidence && s.evidence.length > 0) {
      evidenceHtml = `
        <div class="cp-evidence-chips">
          ${s.evidence.map(e => `<span class="cp-evidence-chip">${escapeHtml(e)}</span>`).join("")}
        </div>
      `;
    }

    // Confidence
    let confidenceHtml = "";
    if (s.confidence) {
      confidenceHtml = `
        <span class="cp-confidence-tag">Confidence: <strong>${escapeHtml(s.confidence)}</strong></span>
      `;
    }

    // If nothing
    let ifNothingHtml = "";
    if (s.if_nothing) {
      ifNothingHtml = `
        <div class="cp-suggestion-if-nothing">
          <strong>If you do nothing:</strong> ${escapeHtml(s.if_nothing)}
        </div>
      `;
    }

    const targetTab = (s.tab || "today").toLowerCase();
    const tabName = targetTab.charAt(0).toUpperCase() + targetTab.slice(1);

    return `
      <article class="cp-suggestion-card cp-card-collapsed" data-id="${escapeHtml(s.id)}">
        <header class="cp-suggestion-head" role="button" tabindex="0" aria-expanded="false" aria-label="Toggle suggestion: ${escapeHtml(s.title)}">
          <span class="cp-severity-dot ${severity}" title="Severity: ${severity}"></span>
          <span class="cp-kind-chip ${kind}">${escapeHtml(kindLabel)}</span>
          <span class="cp-suggestion-title">${escapeHtml(s.title)}</span>
          <svg class="cp-suggestion-chevron" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
        </header>
        <div class="cp-suggestion-body">
          <p class="cp-suggestion-why">${escapeHtml(s.why)}</p>
          ${ifNothingHtml}
          ${changesHtml}
          <div class="cp-suggestion-meta-row">
            ${confidenceHtml}
            ${evidenceHtml}
          </div>
          <div class="cp-suggestion-actions">
            <button type="button" class="cp-button cp-btn-sm cp-action-dismiss" data-id="${escapeHtml(s.id)}">Dismiss</button>
            <button type="button" class="cp-button cp-btn-sm cp-action-done" data-id="${escapeHtml(s.id)}">Mark done</button>
            <button type="button" class="cp-button cp-btn-sm cp-action-open" data-tab="${escapeHtml(targetTab)}">Open ${escapeHtml(tabName)}</button>
          </div>
        </div>
      </article>
    `;
  }).join("");

  // Attach toggle listeners
  container.querySelectorAll(".cp-suggestion-card").forEach(card => {
    const head = card.querySelector(".cp-suggestion-head");
    head.addEventListener("click", () => {
      const isCollapsed = card.classList.toggle("cp-card-collapsed");
      head.setAttribute("aria-expanded", String(!isCollapsed));
    });
    head.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        const isCollapsed = card.classList.toggle("cp-card-collapsed");
        head.setAttribute("aria-expanded", String(!isCollapsed));
      }
    });
  });

  // Attach button actions
  container.querySelectorAll(".cp-action-dismiss").forEach(btn => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      handleSuggestionAction(btn.getAttribute("data-id"), "dismissed");
    });
  });

  container.querySelectorAll(".cp-action-done").forEach(btn => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      handleSuggestionAction(btn.getAttribute("data-id"), "done");
    });
  });

  container.querySelectorAll(".cp-action-open").forEach(btn => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      const tab = btn.getAttribute("data-tab");
      switchTab(tab, true);
    });
  });
}

async function handleSuggestionAction(id, action) {
  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    await fetchJson(`/api/capital/suggestions/${encodeURIComponent(id)}/action${query}`, {
      method: "POST",
      body: JSON.stringify({ action }),
    });
    await loadToday();
  } catch (err) {
    showBanner(`Action failed: ${err.message}`, true);
  }
}

// -----------------------------------------------------------------------------
// Signal Skill View (Round 11)
// -----------------------------------------------------------------------------
function formatSignalName(sig) {
  switch (sig) {
    case "momentum12_1":
    case "momentum_12_1": return "12-1 Momentum";
    case "short_term_reversal": return "Short-term reversal";
    case "low_volatility": return "Low volatility";
    case "trend": return "Trend (200d)";
    case "chart_pattern": return "Chart patterns";
    case "foundation_forecast": return "Foundation forecast";
    case "filing_sentiment": return "Filing sentiment";
    case "insider_buying": return "Insider buying";
    case "chart_trend": return "Chart trend (YOLO)";
    default: return sig ? sig.replace(/_/g, " ") : "—";
  }
}

async function loadSignals() {
  const tbody = document.getElementById("cpSignalsBody");
  const notesWrap = document.getElementById("cpSignalsNotes");
  if (!tbody) return;

  if (accountList.length === 0) {
    tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">Import a report to see signal skill.</td></tr>`;
    if (notesWrap) notesWrap.hidden = true;
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const report = await fetchJson(`/api/capital/signals${query}`);

    if (!report.skill || report.skill.length === 0) {
      tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">No signal report yet.</td></tr>`;
      if (notesWrap) notesWrap.hidden = true;
      return;
    }

    tbody.innerHTML = report.skill.map(s => {
      const isDimmed = s.weight === 0 || s.ic_used === 0;
      const icStr = s.ic_mean >= 0 ? `+${s.ic_mean.toFixed(3)}` : s.ic_mean.toFixed(3);
      const tStr = s.t_stat >= 0 ? `+${s.t_stat.toFixed(2)}` : s.t_stat.toFixed(2);
      const icUsedStr = s.ic_used >= 0 ? `+${s.ic_used.toFixed(3)}` : s.ic_used.toFixed(3);
      const weightStr = `${(s.weight * 100).toFixed(1)}%`;

      return `
        <tr class="${isDimmed ? "cp-signal-dimmed" : ""}">
          <td><strong>${escapeHtml(formatSignalName(s.signal))}</strong></td>
          <td>${s.horizon_sessions} sessions</td>
          <td class="num">${icStr}</td>
          <td class="num">${tStr}</td>
          <td class="num">${icUsedStr}</td>
          <td class="num">${weightStr}</td>
        </tr>
      `;
    }).join("");

    if (notesWrap) {
      if (report.notes && report.notes.length > 0) {
        notesWrap.hidden = false;
        notesWrap.innerHTML = report.notes.map(n => `<div class="cp-scorecard-note">${escapeHtml(n)}</div>`).join("");
      } else {
        notesWrap.hidden = true;
        notesWrap.innerHTML = "";
      }
    }
  } catch (_) {
    tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">No signal report yet.</td></tr>`;
    if (notesWrap) notesWrap.hidden = true;
  }
}

// -----------------------------------------------------------------------------
// Risk Limits Policy Editor (Round 11)
// -----------------------------------------------------------------------------
async function loadPolicy() {
  const badge = document.getElementById("cpPolicyBadge");
  const historyList = document.getElementById("cpPolicyHistoryList");
  if (!badge) return;

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const res = await fetchJson(`/api/capital/policy${query}`);
    const current = res.current;

    if (current && current.limits) {
      const l = current.limits;
      const setVal = (id, val) => {
        const el = document.getElementById(id);
        if (el) el.value = val !== undefined && val !== null ? String(val) : "";
      };
      setVal("cpLimitMaxPos", l.max_position_pct);
      setVal("cpLimitCashMin", l.cash_min_pct);
      setVal("cpLimitCashMax", l.cash_max_pct);
      setVal("cpLimitHighVolThresh", l.high_vol_threshold_pct);
      setVal("cpLimitMaxHighVol", l.max_high_vol_pct);
      setVal("cpLimitMaxTurnover", l.max_turnover_pct);
      setVal("cpLimitMinTrade", l.min_trade);
      setVal("cpLimitTradeCost", l.trade_cost_bps);
      setVal("cpLimitFxCost", l.fx_cost_bps);
      setVal("cpLimitRiskAversion", l.risk_aversion);

      const isConfirmed = current.status === "confirmed";
      badge.textContent = isConfirmed ? "Confirmed" : "Draft";
      badge.className = isConfirmed ? "cp-badge cp-badge-confirmed" : "cp-badge cp-badge-draft";
    }

    if (historyList) {
      const history = res.history || [];
      if (history.length === 0) {
        historyList.innerHTML = `<li class="cp-empty">Built-in draft (version 0). No revisions saved yet.</li>`;
      } else {
        historyList.innerHTML = history.slice().reverse().map(p => {
          const isConfirmed = p.status === "confirmed";
          const dateStr = p.saved_at ? formatDate(p.saved_at) : "Built-in draft";
          return `
            <li class="cp-history-item">
              <div class="cp-history-item-left">
                <span class="cp-history-version">v${p.version}</span>
                <span class="cp-badge ${isConfirmed ? "cp-badge-confirmed" : "cp-badge-draft"}">${isConfirmed ? "Confirmed" : "Draft"}</span>
              </div>
              <span class="cp-history-time">${escapeHtml(dateStr)}</span>
            </li>
          `;
        }).join("");
      }
    }
  } catch (err) {
    // Before the first import there is no account, so no policy to show yet.
    if (!currentAccount && /No portfolio/.test(String(err && err.message))) return;
    console.error("Failed to load risk policy:", err);
  }
}

async function savePolicy(confirm) {
  const errorBox = document.getElementById("cpPolicyError");
  if (errorBox) {
    errorBox.hidden = true;
    errorBox.textContent = "";
  }

  const limits = {
    max_position_pct: (document.getElementById("cpLimitMaxPos")?.value || "").trim(),
    cash_min_pct: (document.getElementById("cpLimitCashMin")?.value || "").trim(),
    cash_max_pct: (document.getElementById("cpLimitCashMax")?.value || "").trim(),
    high_vol_threshold_pct: (document.getElementById("cpLimitHighVolThresh")?.value || "").trim(),
    max_high_vol_pct: (document.getElementById("cpLimitMaxHighVol")?.value || "").trim(),
    max_turnover_pct: (document.getElementById("cpLimitMaxTurnover")?.value || "").trim(),
    min_trade: (document.getElementById("cpLimitMinTrade")?.value || "").trim(),
    trade_cost_bps: (document.getElementById("cpLimitTradeCost")?.value || "").trim(),
    fx_cost_bps: (document.getElementById("cpLimitFxCost")?.value || "").trim(),
    risk_aversion: (document.getElementById("cpLimitRiskAversion")?.value || "").trim(),
  };

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const res = await fetchJson(`/api/capital/policy${query}`, {
      method: "POST",
      body: JSON.stringify({ limits, confirm }),
    });

    showBanner(`Limits saved as version ${res.version} (${res.status}).`);
    await loadPolicy();
    await loadToday();
  } catch (err) {
    if (errorBox) {
      errorBox.hidden = false;
      errorBox.textContent = err.message || "Failed to save limits.";
    } else {
      showBanner(err.message, true);
    }
  }
}

function setupPolicyControls() {
  const draftBtn = document.getElementById("cpSaveDraftPolicyBtn");
  if (draftBtn) {
    draftBtn.addEventListener("click", () => savePolicy(false));
  }
  const confirmBtn = document.getElementById("cpConfirmPolicyBtn");
  if (confirmBtn) {
    confirmBtn.addEventListener("click", () => savePolicy(true));
  }
}

function setupDataTabControls() {
  const dataUploadBtn = document.getElementById("cpDataUploadBtn");
  if (dataUploadBtn) {
    dataUploadBtn.addEventListener("click", () => {
      const fileInput = document.getElementById("cpFileInput");
      if (fileInput) fileInput.click();
    });
  }

  const dataRefreshBtn = document.getElementById("cpDataPriceRefreshBtn");
  if (dataRefreshBtn) {
    dataRefreshBtn.addEventListener("click", () => {
      const mainPriceRefresh = document.getElementById("cpPriceRefreshBtn");
      if (mainPriceRefresh) mainPriceRefresh.click();
    });
  }

  const dataPriceUploadBtn = document.getElementById("cpDataPriceUploadBtn");
  if (dataPriceUploadBtn) {
    dataPriceUploadBtn.addEventListener("click", () => {
      const mainPriceUpload = document.getElementById("cpPriceUploadBtn");
      if (mainPriceUpload) mainPriceUpload.click();
    });
  }
}

/* -----------------------------------------------------------------------------
 * Round 12: Blended Outlook with Replay & Predictive Models
 * ----------------------------------------------------------------------------- */

const COMPONENT_LABELS = {
  gbm: "Geometric Brownian Motion (GBM)",
  ewma: "EWMA Volatility (No-Change)",
  historical: "Historical Simulation",
  chronos: "Chronos-Bolt (Pretrained Model)",
  gbdt: "Gradient Boosted Trees (GBDT)",
};

function ordinalSuffix(n) {
  const rounded = Math.round(n);
  const j = rounded % 10;
  const k = rounded % 100;
  if (j === 1 && k !== 11) return `${rounded}st`;
  if (j === 2 && k !== 12) return `${rounded}nd`;
  if (j === 3 && k !== 13) return `${rounded}rd`;
  return `${rounded}th`;
}

function getStatusReason(status) {
  if (!status || status === "Ok" || status === "ok") return null;
  if (typeof status === "string") return status;
  if (status.insufficient_evidence) return status.insufficient_evidence.reason || "Insufficient evidence";
  if (status.InsufficientEvidence) return status.InsufficientEvidence.reason || "Insufficient evidence";
  if (status.status === "insufficient_evidence") return status.reason || "Insufficient evidence";
  if (status.reason) return status.reason;
  return "Insufficient evidence";
}

function updateOutlookHoldings(symbols) {
  const select = document.getElementById("cpOutlookSymbolSelect");
  if (!select) return;

  const prevValue = select.value || currentOutlookSymbol;
  select.innerHTML = "";

  if (symbols.length === 0) {
    select.innerHTML = `<option value="" disabled selected>No holdings</option>`;
    currentOutlookSymbol = "";
    loadOutlook();
    return;
  }

  for (const s of symbols) {
    const opt = document.createElement("option");
    opt.value = s;
    opt.textContent = s;
    select.appendChild(opt);
  }

  if (symbols.includes(prevValue)) {
    select.value = prevValue;
    currentOutlookSymbol = prevValue;
  } else {
    select.value = symbols[0];
    currentOutlookSymbol = symbols[0];
  }

  loadOutlook();
}

async function loadOutlook() {
  const wrap = document.getElementById("cpOutlookChartWrap");
  const replayBox = document.getElementById("cpOutlookReplaySummary");
  const metaGrid = document.getElementById("cpOutlookMetaGrid");
  if (!wrap) return;

  if (!currentOutlookSymbol) {
    wrap.innerHTML = `<p class="cp-empty">Select a holding to see its blended outlook.</p>`;
    if (replayBox) replayBox.hidden = true;
    if (metaGrid) metaGrid.hidden = true;
    return;
  }

  try {
    let cutoff = null;
    if (currentOutlookTrim > 0 && currentOutlookSessions.length > 0) {
      const idx = currentOutlookSessions.length - 1 - currentOutlookTrim;
      cutoff = currentOutlookSessions[Math.max(0, idx)];
    }

    let url = `/api/capital/outlook?account=${encodeURIComponent(currentAccount || "")}&symbol=${encodeURIComponent(currentOutlookSymbol)}&horizon=${currentOutlookHorizon}`;
    if (cutoff) {
      url += `&cutoff=${encodeURIComponent(cutoff)}`;
    }

    const res = await fetchJson(url);
    if (res.sessions && Array.isArray(res.sessions)) {
      currentOutlookSessions = res.sessions;
    }

    const outlook = res.outlook;
    if (!outlook) {
      wrap.innerHTML = `<p class="cp-empty">No outlook data returned.</p>`;
      if (replayBox) replayBox.hidden = true;
      if (metaGrid) metaGrid.hidden = true;
      return;
    }

    // Check status
    const reason = getStatusReason(outlook.status);
    if (reason && (!outlook.fan || outlook.fan.length === 0)) {
      wrap.innerHTML = `
        <div class="cp-forecast-insufficient">
          <span class="cp-badge-insufficient">insufficient evidence</span>
          <span class="cp-forecast-reason">${escapeHtml(reason)}</span>
        </div>
      `;
      if (replayBox) replayBox.hidden = true;
      if (metaGrid) metaGrid.hidden = true;
      return;
    }

    // Render SVG fan chart
    renderOutlookChart(res);

    // Replay summary sentence
    if (replayBox) {
      if (currentOutlookTrim > 0 || (outlook.actual && outlook.actual.length > 0)) {
        replayBox.hidden = false;
        let sentence = "";
        if (outlook.actual_percentile !== null && outlook.actual_percentile !== undefined) {
          let p = Number(outlook.actual_percentile);
          if (p <= 1.0 && p >= 0.0) p *= 100;
          const ord = ordinalSuffix(p);
          let band = "outside the 90% band";
          if (p >= 16 && p <= 84) {
            band = "inside the 68% band";
          } else if (p >= 5 && p <= 95) {
            band = "inside the 90% band";
          }
          sentence = `The actual close ended at the ${ord} percentile, ${band}.`;
        } else if (outlook.actual && outlook.actual.length > 0) {
          const lastClose = outlook.actual[outlook.actual.length - 1][1];
          sentence = `Replay in progress: ${outlook.actual.length} of ${outlook.horizon} sessions recorded (${groupDecimal(Number(lastClose).toFixed(2))} vs ${groupDecimal(Number(outlook.origin_close).toFixed(2))} origin).`;
        } else {
          sentence = `Replay selected from ${cutoff || "cutoff"} (awaiting actual closes).`;
        }
        replayBox.innerHTML = `<p class="cp-replay-text">${escapeHtml(sentence)}</p>`;
      } else {
        replayBox.hidden = true;
      }
    }

    // Component weights and tilt
    if (metaGrid) metaGrid.hidden = false;
    renderComponentWeights(outlook.weights || []);
    renderTilt(outlook.tilt, outlook.tilt_signals);

  } catch (err) {
    wrap.innerHTML = `<p class="cp-empty">Outlook unavailable: ${escapeHtml(err.message)}</p>`;
    if (replayBox) replayBox.hidden = true;
    if (metaGrid) metaGrid.hidden = true;
  }
}

function renderOutlookChart(res) {
  const wrap = document.getElementById("cpOutlookChartWrap");
  if (!wrap) return;

  const outlook = res.outlook || {};
  const history = res.history || [];
  const fan = outlook.fan || [];
  const actual = outlook.actual || [];

  const originClose = Number(outlook.origin_close) || 0;
  const originDate = outlook.origin ? formatDay(outlook.origin) : "Origin";

  // Gather all price values to compute Y bounds
  const prices = [originClose];
  for (const h of history) {
    const c = Array.isArray(h) ? h[1] : (h.close ?? h[1]);
    if (c !== null && c !== undefined && !isNaN(c)) prices.push(Number(c));
  }
  for (const fp of fan) {
    if (fp.prices && Array.isArray(fp.prices)) {
      for (const p of fp.prices) {
        if (!isNaN(p)) prices.push(Number(p));
      }
    }
    if (fp.mean !== null && fp.mean !== undefined && !isNaN(fp.mean)) {
      prices.push(Number(fp.mean));
    }
  }
  for (const a of actual) {
    const c = Array.isArray(a) ? a[1] : (a.close ?? a[1]);
    if (c !== null && c !== undefined && !isNaN(c)) prices.push(Number(c));
  }

  let minPrice = Math.min(...prices);
  let maxPrice = Math.max(...prices);
  if (minPrice === maxPrice) {
    minPrice *= 0.95;
    maxPrice *= 1.05;
  }
  const span = maxPrice - minPrice;
  const pMin = minPrice - span * 0.08;
  const pMax = maxPrice + span * 0.08;

  // ViewBox coordinates
  const W = 820;
  const H = 360;
  const ml = 65;
  const mr = 135;
  const mt = 40;
  const mb = 36;
  const pw = W - ml - mr;
  const ph = H - mt - mb;
  const yTop = mt;
  const yBottom = mt + ph;

  function yPrice(p) {
    return yBottom - ((p - pMin) / (pMax - pMin)) * ph;
  }

  const nHist = Math.max(1, history.length);
  const nFan = Math.max(1, outlook.horizon || 21);
  const histRatio = Math.min(0.45, Math.max(0.3, nHist / (nHist + nFan)));
  const xLeft = ml;
  const xOrigin = ml + pw * histRatio;
  const xRight = ml + pw;

  function xHist(i) {
    if (nHist <= 1) return xOrigin;
    return xLeft + (i / (nHist - 1)) * (xOrigin - xLeft);
  }

  function xFan(step) {
    return xOrigin + (step / nFan) * (xRight - xOrigin);
  }

  const yOrigin = yPrice(originClose);
  const originPct = Math.max(0, Math.min(100, ((yOrigin - yTop) / ph) * 100));

  // History path
  let histPath = "";
  if (history.length > 0) {
    histPath = history.map((h, i) => {
      const c = Number(Array.isArray(h) ? h[1] : (h.close ?? h[1]));
      const x = xHist(i);
      const y = yPrice(c);
      return `${i === 0 ? "M" : "L"} ${x.toFixed(1)} ${y.toFixed(1)}`;
    }).join(" ");
    histPath += ` L ${xOrigin.toFixed(1)} ${yOrigin.toFixed(1)}`;
  }

  // Fan steps starting at origin
  const fanSteps = [
    { step: 0, prices: [originClose, originClose, originClose, originClose, originClose], mean: originClose },
    ...fan
  ];

  // 90% band
  const top90 = fanSteps.map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[4]).toFixed(1)}`);
  const bot90 = [...fanSteps].reverse().map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[0]).toFixed(1)}`);
  const band90Path = `M ${top90.join(" L ")} L ${bot90.join(" L ")} Z`;

  // 68% band
  const top68 = fanSteps.map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[3]).toFixed(1)}`);
  const bot68 = [...fanSteps].reverse().map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[1]).toFixed(1)}`);
  const band68Path = `M ${top68.join(" L ")} L ${bot68.join(" L ")} Z`;

  // Median and Mean paths
  const medPath = "M " + fanSteps.map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[2]).toFixed(1)}`).join(" L ");
  const meanPath = "M " + fanSteps.map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.mean).toFixed(1)}`).join(" L ");

  // Actual replay path
  let actualPath = "";
  let lastActualPoint = null;
  if (actual.length > 0) {
    const pts = [`${xOrigin.toFixed(1)} ${yOrigin.toFixed(1)}`];
    actual.forEach((a, j) => {
      const c = Number(Array.isArray(a) ? a[1] : (a.close ?? a[1]));
      const step = j + 1;
      const x = xFan(step);
      const y = yPrice(c);
      pts.push(`${x.toFixed(1)} ${y.toFixed(1)}`);
      if (j === actual.length - 1) {
        lastActualPoint = { x, y, c };
      }
    });
    actualPath = "M " + pts.join(" L ");
  }

  // Grid lines
  const gridSteps = 4;
  const gridLines = [];
  for (let g = 0; g <= gridSteps; g++) {
    const pVal = pMin + (g / gridSteps) * (pMax - pMin);
    const yG = yPrice(pVal);
    gridLines.push(`
      <line x1="${xLeft}" y1="${yG.toFixed(1)}" x2="${xRight}" y2="${yG.toFixed(1)}" stroke="rgba(255,255,255,0.06)" stroke-width="1" />
      <text x="${(xLeft - 8).toFixed(1)}" y="${(yG + 3).toFixed(1)}" text-anchor="end" class="cp-chart-axis-text">${groupDecimal(pVal.toFixed(2))}</text>
    `);
  }

  // End labels at horizon
  let endLabelsHtml = "";
  const lastFan = fan[fan.length - 1];
  let p95End = 0, p50End = 0, p5End = 0;
  if (lastFan) {
    p95End = lastFan.prices[4];
    p50End = lastFan.prices[2];
    p5End = lastFan.prices[0];

    const pct95 = ((p95End - originClose) / originClose) * 100;
    const pct50 = ((p50End - originClose) / originClose) * 100;
    const pct5 = ((p5End - originClose) / originClose) * 100;

    let y95 = yPrice(p95End);
    let y50 = yPrice(p50End);
    let y5 = yPrice(p5End);

    if (y50 - y95 < 15) y95 = y50 - 15;
    if (y5 - y50 < 15) y5 = y50 + 15;

    const sign95 = pct95 >= 0 ? "+" : "";
    const sign50 = pct50 >= 0 ? "+" : "";
    const sign5 = pct5 >= 0 ? "+" : "";

    endLabelsHtml = `
      <g class="cp-end-labels">
        <text x="${(xRight + 8).toFixed(1)}" y="${y95.toFixed(1)}" class="cp-chart-label cp-label-high">95%: ${groupDecimal(p95End.toFixed(2))} (${sign95}${pct95.toFixed(1)}%)</text>
        <text x="${(xRight + 8).toFixed(1)}" y="${y50.toFixed(1)}" class="cp-chart-label cp-label-med">50%: ${groupDecimal(p50End.toFixed(2))} (${sign50}${pct50.toFixed(1)}%)</text>
        <text x="${(xRight + 8).toFixed(1)}" y="${y5.toFixed(1)}" class="cp-chart-label cp-label-low">5%: ${groupDecimal(p5End.toFixed(2))} (${sign5}${pct5.toFixed(1)}%)</text>
      </g>
    `;
  }

  // Accessible summary text
  const sym = escapeHtml(outlook.symbol || currentOutlookSymbol);
  const horizonLabelMap = { 21: "1 month", 63: "3 months", 126: "6 months", 252: "1 year" };
  const hLabel = horizonLabelMap[outlook.horizon] || `${outlook.horizon} sessions`;
  const accessibleSummary = `Blended outlook fan chart for ${sym} across ${hLabel}. Origin price ${groupDecimal(originClose.toFixed(2))} on ${originDate}. Median expectation at horizon is ${groupDecimal(p50End.toFixed(2))}. 68% range is ${groupDecimal((lastFan ? lastFan.prices[1] : 0).toFixed(2))} to ${groupDecimal((lastFan ? lastFan.prices[3] : 0).toFixed(2))}; 90% range is ${groupDecimal(p5End.toFixed(2))} to ${groupDecimal(p95End.toFixed(2))}.`;

  const svgHtml = `
    <svg viewBox="0 0 ${W} ${H}" width="100%" height="auto" role="img" aria-labelledby="cpFanTitle cpFanDesc" xmlns="http://www.w3.org/2000/svg">
      <title id="cpFanTitle">Blended Outlook Fan Chart for ${sym}</title>
      <desc id="cpFanDesc">${escapeHtml(accessibleSummary)}</desc>

      <defs>
        <!-- 90% fan gradient: green above origin price, red below -->
        <linearGradient id="cpFanGrad90" x1="0" y1="${yTop.toFixed(1)}" x2="0" y2="${yBottom.toFixed(1)}" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color="#5ccda4" stop-opacity="0.22" />
          <stop offset="${originPct.toFixed(1)}%" stop-color="#5ccda4" stop-opacity="0.14" />
          <stop offset="${originPct.toFixed(1)}%" stop-color="#e17055" stop-opacity="0.14" />
          <stop offset="100%" stop-color="#e17055" stop-opacity="0.22" />
        </linearGradient>

        <!-- 68% fan gradient: green above origin price, red below -->
        <linearGradient id="cpFanGrad68" x1="0" y1="${yTop.toFixed(1)}" x2="0" y2="${yBottom.toFixed(1)}" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color="#5ccda4" stop-opacity="0.45" />
          <stop offset="${originPct.toFixed(1)}%" stop-color="#5ccda4" stop-opacity="0.28" />
          <stop offset="${originPct.toFixed(1)}%" stop-color="#e17055" stop-opacity="0.28" />
          <stop offset="100%" stop-color="#e17055" stop-opacity="0.45" />
        </linearGradient>
      </defs>

      <!-- Background grid -->
      <g class="cp-chart-grid">
        ${gridLines.join("")}
      </g>

      <!-- Horizontal Origin Price Line -->
      <line x1="${xLeft}" y1="${yOrigin.toFixed(1)}" x2="${xRight}" y2="${yOrigin.toFixed(1)}" stroke="rgba(255, 255, 255, 0.18)" stroke-dasharray="3 3" stroke-width="1" />
      <text x="${(xOrigin + 6).toFixed(1)}" y="${(yOrigin - 6).toFixed(1)}" class="cp-chart-axis-text" fill="var(--sail-text-subtle)">Origin: ${groupDecimal(originClose.toFixed(2))}</text>

      <!-- Fan Bands -->
      ${fan.length > 0 ? `
        <path d="${band90Path}" fill="url(#cpFanGrad90)" />
        <path d="${band68Path}" fill="url(#cpFanGrad68)" />
        <path d="${medPath}" fill="none" stroke="#ffd69f" stroke-width="2" stroke-dasharray="5 4" />
        <path d="${meanPath}" fill="none" stroke="#94a3b8" stroke-width="1.5" stroke-dasharray="2 3" />
      ` : ""}

      <!-- History Line -->
      ${histPath ? `<path d="${histPath}" fill="none" stroke="#7dd3fc" stroke-width="2" stroke-linejoin="round" />` : ""}

      <!-- Vertical Origin Dashed Line -->
      <line x1="${xOrigin.toFixed(1)}" y1="${yTop.toFixed(1)}" x2="${xOrigin.toFixed(1)}" y2="${yBottom.toFixed(1)}" stroke="rgba(255, 255, 255, 0.4)" stroke-width="1.5" stroke-dasharray="4 4" />
      <circle cx="${xOrigin.toFixed(1)}" cy="${yOrigin.toFixed(1)}" r="3.5" fill="#7dd3fc" />

      <!-- Actual Replay Line -->
      ${actualPath ? `
        <path d="${actualPath}" fill="none" stroke="#ffffff" stroke-width="2.5" stroke-linejoin="round" />
        ${lastActualPoint ? `<circle cx="${lastActualPoint.x.toFixed(1)}" cy="${lastActualPoint.y.toFixed(1)}" r="4.5" fill="#ffffff" stroke="#0ea5e9" stroke-width="2" />` : ""}
      ` : ""}

      <!-- End labels -->
      ${endLabelsHtml}

      <!-- Bottom X Axis Labels -->
      <text x="${xLeft.toFixed(1)}" y="${(H - 12).toFixed(1)}" text-anchor="start" class="cp-chart-axis-text">${history.length > 0 ? formatDay(history[0][0]) : ""}</text>
      <text x="${xOrigin.toFixed(1)}" y="${(H - 12).toFixed(1)}" text-anchor="middle" class="cp-chart-axis-text">Origin (${originDate})</text>
      <text x="${xRight.toFixed(1)}" y="${(H - 12).toFixed(1)}" text-anchor="end" class="cp-chart-axis-text">+${outlook.horizon}d</text>

      <!-- Legend -->
      <g class="cp-chart-legend" transform="translate(${xLeft}, 16)">
        <line x1="0" y1="0" x2="16" y2="0" stroke="#7dd3fc" stroke-width="2" />
        <text x="22" y="3" class="cp-chart-axis-text">Past Closes</text>

        <line x1="90" y1="0" x2="106" y2="0" stroke="#ffd69f" stroke-width="2" stroke-dasharray="4 3" />
        <text x="112" y="3" class="cp-chart-axis-text">Median (50%)</text>

        <line x1="190" y1="0" x2="206" y2="0" stroke="#94a3b8" stroke-width="1.5" stroke-dasharray="2 3" />
        <text x="212" y="3" class="cp-chart-axis-text">Mean</text>

        <rect x="256" y="-5" width="12" height="10" fill="#5ccda4" opacity="0.35" />
        <text x="274" y="3" class="cp-chart-axis-text">68% / 90% Fan</text>

        ${actual.length > 0 ? `
          <line x1="365" y1="0" x2="381" y2="0" stroke="#ffffff" stroke-width="2.5" />
          <circle cx="373" cy="0" r="3" fill="#ffffff" />
          <text x="388" y="3" class="cp-chart-axis-text">Actual Replay</text>
        ` : ""}
      </g>
    </svg>
    <div class="cp-chart-accessible-summary">${escapeHtml(accessibleSummary)}</div>
  `;

  wrap.innerHTML = svgHtml;
}

function renderComponentWeights(weights) {
  const list = document.getElementById("cpOutlookWeightsList");
  if (!list) return;

  if (!weights || weights.length === 0) {
    list.innerHTML = `<p class="cp-empty">No component weights available.</p>`;
    return;
  }

  list.innerHTML = weights.map(item => {
    const compId = Array.isArray(item) ? item[0] : (item.component || item[0]);
    const w = Number(Array.isArray(item) ? item[1] : (item.weight ?? item[1])) || 0;
    const pct = Math.max(0, Math.min(100, w * 100));
    const isZero = pct < 0.1;
    const label = COMPONENT_LABELS[compId] || compId;

    return `
      <div class="cp-weight-row ${isZero ? "cp-weight-dimmed" : ""}">
        <span class="cp-weight-name">${escapeHtml(label)}</span>
        <div class="cp-weight-bar-wrap">
          <div class="cp-weight-bar-fill" style="width: ${pct.toFixed(1)}%;"></div>
        </div>
        <span class="cp-weight-pct">${pct.toFixed(1)}%</span>
      </div>
    `;
  }).join("");
}

function renderTilt(tilt, tiltSignals) {
  const note = document.getElementById("cpOutlookTiltNote");
  if (!note) return;

  const absTilt = Math.abs(tilt || 0);
  if (absTilt > 1e-4) {
    const pct = (tilt * 100).toFixed(1);
    const sign = tilt > 0 ? "+" : "";
    const sigStr = (tiltSignals && tiltSignals.length > 0) ? ` (${tiltSignals.map(escapeHtml).join(", ")})` : "";
    note.innerHTML = `<span class="cp-tilt-line">Direction tilt: <strong>${sign}${pct}%</strong>${sigStr}</span>`;
  } else {
    note.innerHTML = `<span class="cp-tilt-none">No direction tilt: no signal passed validation</span>`;
  }
}

async function loadTrackRecord() {
  const grid = document.getElementById("cpTrackRecordGrid");
  const label = document.getElementById("cpTrackRecordHorizonLabel");
  const horizonMap = { 21: "1M", 63: "3M", 126: "6M", 252: "1Y" };
  if (label) {
    label.textContent = horizonMap[currentOutlookHorizon] || `${currentOutlookHorizon}d`;
  }
  if (!grid) return;

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}&` : "?";
    const tr = await fetchJson(`/api/capital/outlook/track-record${query}horizon=${currentOutlookHorizon}`);
    renderTrackRecord(tr, currentOutlookHorizon);
  } catch (err) {
    grid.innerHTML = `<p class="cp-empty">Track record unavailable: ${escapeHtml(err.message)}</p>`;
  }
}

function renderTrackRecord(tr, horizon) {
  const grid = document.getElementById("cpTrackRecordGrid");
  const label = document.getElementById("cpTrackRecordHorizonLabel");
  const horizonMap = { 21: "1M", 63: "3M", 126: "6M", 252: "1Y" };
  if (label) {
    label.textContent = horizonMap[horizon] || `${horizon}d`;
  }
  if (!grid) return;

  if (!tr || tr.replays === 0) {
    grid.innerHTML = `<p class="cp-empty">No track record available yet for this horizon.</p>`;
    return;
  }

  const in68 = `${((tr.inside_68 || 0) * 100).toFixed(1)}%`;
  const in90 = `${((tr.inside_90 || 0) * 100).toFixed(1)}%`;
  const meanP = `${(tr.mean_percentile || 0).toFixed(1)}`;
  const skill = `${((tr.skill_vs_no_change || 0) * 100).toFixed(1)}%`;
  const dir = `${((tr.direction_hit_rate || 0) * 100).toFixed(1)}%`;

  grid.innerHTML = `
    <div class="cp-track-card">
      <span class="cp-track-val">${in68}</span>
      <span class="cp-track-label">inside 68% band</span>
      <span class="cp-track-aim">aim 68%</span>
    </div>
    <div class="cp-track-card">
      <span class="cp-track-val">${in90}</span>
      <span class="cp-track-label">inside 90% band</span>
      <span class="cp-track-aim">aim 90%</span>
    </div>
    <div class="cp-track-card">
      <span class="cp-track-val">${meanP}</span>
      <span class="cp-track-label">average percentile</span>
      <span class="cp-track-aim">aim 50</span>
    </div>
    <div class="cp-track-card">
      <span class="cp-track-val">${skill}</span>
      <span class="cp-track-label">skill vs no change</span>
      <span class="cp-track-aim">vs EWMA</span>
    </div>
    <div class="cp-track-card">
      <span class="cp-track-val">${dir}</span>
      <span class="cp-track-label">direction right</span>
      <span class="cp-track-aim">${tr.replays} replays</span>
    </div>
  `;
}

function setupOutlookControls() {
  const symbolSelect = document.getElementById("cpOutlookSymbolSelect");
  if (symbolSelect) {
    symbolSelect.addEventListener("change", (e) => {
      currentOutlookSymbol = e.target.value;
      loadOutlook();
    });
  }

  const horizonBtns = document.querySelectorAll(".cp-outlook-horizon-pills .cp-horizon-btn");
  horizonBtns.forEach((btn) => {
    btn.addEventListener("click", () => {
      horizonBtns.forEach((b) => {
        b.classList.remove("cp-active");
        b.setAttribute("aria-checked", "false");
      });
      btn.classList.add("cp-active");
      btn.setAttribute("aria-checked", "true");
      currentOutlookHorizon = parseInt(btn.dataset.horizon, 10) || 21;
      loadOutlook();
      loadTrackRecord();
    });
  });

  const replayBtns = document.querySelectorAll(".cp-outlook-replay-pills .cp-horizon-btn");
  replayBtns.forEach((btn) => {
    btn.addEventListener("click", () => {
      replayBtns.forEach((b) => {
        b.classList.remove("cp-active");
        b.setAttribute("aria-checked", "false");
      });
      btn.classList.add("cp-active");
      btn.setAttribute("aria-checked", "true");
      currentOutlookTrim = parseInt(btn.dataset.trim, 10) || 0;
      loadOutlook();
    });
  });
}

function renderModelsStatus(data) {
  const tbody = document.getElementById("cpModelsBody");
  const runBtn = document.getElementById("cpRunModelsBtn");
  if (!tbody) return;

  if (runBtn) {
    if (data && data.running) {
      runBtn.disabled = true;
      runBtn.textContent = "Running models…";
    } else {
      runBtn.disabled = false;
      runBtn.textContent = "Run models";
    }
  }

  const models = (data && data.models) || [];
  if (models.length === 0) {
    tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">No models configured.</td></tr>`;
    return;
  }

  tbody.innerHTML = models.map(m => {
    const spec = m.spec || {};
    const name = escapeHtml(spec.name || "Unknown");
    const model = escapeHtml(spec.model || "");
    const summary = escapeHtml(spec.summary || "");
    const license = escapeHtml(spec.license || "not stated");
    const enabledBadge = m.enabled
      ? `<span class="cp-badge cp-badge-enabled">Enabled</span>`
      : `<span class="cp-badge cp-badge-off">Disabled</span>`;

    let storedStr = "None";
    if (m.stored && m.stored.values > 0) {
      const lat = m.stored.latest ? `, latest ${escapeHtml(m.stored.latest)}` : "";
      storedStr = `${m.stored.values} values (${m.stored.instruments} instruments${lat})`;
    }

    let runStr = "Never run";
    if (m.last_run) {
      const at = formatDate(m.last_run.at);
      const added = m.last_run.added ?? 0;
      const skipped = m.last_run.skipped ?? 0;
      const err = m.last_run.error ? `<br><span class="cp-loss">Error: ${escapeHtml(m.last_run.error)}</span>` : "";
      runStr = `${at}: +${added} added, ${skipped} skipped${err}`;
    }

    return `
      <tr>
        <td><strong>${name}</strong><br><small class="cp-text-muted">${model}</small></td>
        <td>${summary}</td>
        <td><small class="cp-text-muted">${license}</small></td>
        <td>${enabledBadge}</td>
        <td class="num">${storedStr}</td>
        <td>${runStr}</td>
      </tr>
    `;
  }).join("");
}

function startModelsPolling() {
  if (modelsPollingTimer) return;
  modelsPollingTimer = setInterval(async () => {
    try {
      const data = await fetchJson("/api/capital/signals/models");
      renderModelsStatus(data);
      if (!data.running) {
        stopModelsPolling();
        showBanner("Model runs finished.");
        loadOutlook();
      }
    } catch (_) {
      stopModelsPolling();
    }
  }, 5000);
}

function stopModelsPolling() {
  if (modelsPollingTimer) {
    clearInterval(modelsPollingTimer);
    modelsPollingTimer = null;
  }
}

async function loadModelsStatus() {
  try {
    const data = await fetchJson("/api/capital/signals/models");
    renderModelsStatus(data);
    if (data && data.running) {
      startModelsPolling();
    }
    return data;
  } catch (err) {
    const tbody = document.getElementById("cpModelsBody");
    if (tbody) {
      tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">Models status unavailable: ${escapeHtml(err.message)}</td></tr>`;
    }
    return null;
  }
}

function setupModelsControls() {
  const runBtn = document.getElementById("cpRunModelsBtn");
  if (!runBtn) return;
  runBtn.addEventListener("click", async () => {
    runBtn.disabled = true;
    runBtn.textContent = "Starting models…";
    try {
      const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
      const status = await fetchJson(`/api/capital/signals/refresh${query}`, { method: "POST" });
      showBanner("Model run started in background. Polling for progress…");
      renderModelsStatus(status);
      if (status && status.running) {
        startModelsPolling();
      }
    } catch (err) {
      showBanner(`Model run failed: ${err.message}`, true);
      runBtn.disabled = false;
      runBtn.textContent = "Run models";
    }
  });
}

async function loadAll() {
  await loadPortfolio();
  await loadToday();
  await loadPolicy();
  await loadSignals();
  await loadInsights();
  await loadGrants();
  await loadFilings();
  await loadSecStatus();
  await loadSyncStatus();
  await loadPriceStatus();
  await loadForecasts();
  await loadEvaluation();
  await loadOutlook();
  await loadTrackRecord();
  await loadModelsStatus();
}

document.addEventListener("DOMContentLoaded", () => {
  setupTabs();
  setupCollapsibles();
  setupUploadDialog();
  setupGrantDialog();
  setupPriceControls();
  setupPriceFileDialog();
  setupPolicyControls();
  setupDataTabControls();
  setupOutlookControls();
  setupModelsControls();

  const refreshBtn = document.getElementById("cpRefreshBtn");
  if (refreshBtn) {
    refreshBtn.addEventListener("click", () => {
      showBanner("");
      loadAll();
    });
  }

  const secRefreshBtn = document.getElementById("cpSecRefreshBtn");
  if (secRefreshBtn) {
    secRefreshBtn.addEventListener("click", async () => {
      secRefreshBtn.disabled = true;
      secRefreshBtn.textContent = "Checking SEC...";
      try {
        const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
        const res = await fetchJson(`/api/capital/sec/refresh${query}`, { method: "POST" });
        await loadFilings();
        await loadSecStatus();
        if (res.last_error) {
          showBanner(`SEC check stopped: ${res.last_error}`, true);
        } else {
          showBanner(`SEC check complete: ${res.issuers_checked || 0} issuers checked, ${res.new_evidence || 0} new filings stored.`);
        }
      } catch (err) {
        showBanner(`SEC refresh failed: ${err.message}`, true);
      } finally {
        secRefreshBtn.disabled = false;
        secRefreshBtn.textContent = "Check SEC now";
      }
    });
  }

  const accountSelect = document.getElementById("cpAccountSelect");
  if (accountSelect) {
    accountSelect.addEventListener("change", (e) => {
      currentAccount = e.target.value;
      loadAll();
    });
  }

  loadAccounts().then(() => {
    loadAll();
  });
});

// SAIL Capital Web Client
// Interacts with /api/capital endpoints according to the final contract (Round 7).

let currentAccount = null;
let currentTz = "Europe/Lisbon";
let selectedFile = null;
let accountList = [];
let currentHorizon = 20;
let currentModel = "no_change";

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
      await loadPortfolio();
      await loadInsights();
      await loadGrants();
      await loadFilings();
      await loadSecStatus();
      await loadSyncStatus();
      await loadPriceStatus();
      await loadForecasts();
      await loadEvaluation();
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
  const sections = document.querySelectorAll("#cpMain > section[id]");
  for (const section of sections) {
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

  // 2. Return on Deposits
  let returnHtml = "";
  if (ins.total_return) {
    const tr = ins.total_return;
    const isGainNegative = tr.gain && tr.gain.amount && String(tr.gain.amount).startsWith("-");
    const gainClass = isGainNegative ? "negative" : "positive";
    const gainRatioFormatted = tr.gain_ratio !== null && tr.gain_ratio !== undefined
      ? `${(parseFloat(tr.gain_ratio) * 100).toFixed(2)}%`
      : "—";
    returnHtml = `
      <div class="cp-insights-block">
        <h3 class="cp-insights-subtitle">Return on Deposits</h3>
        <div class="cp-insights-stats">
          <div class="cp-stat-card">
            <span class="cp-stat-label">Net Deposits</span>
            <span class="cp-stat-val">${formatMoney(tr.net_deposits)}</span>
          </div>
          <div class="cp-stat-card">
            <span class="cp-stat-label">Current Value</span>
            <span class="cp-stat-val">${formatMoney(tr.current_value)}</span>
          </div>
          <div class="cp-stat-card">
            <span class="cp-stat-label">Gain / Loss</span>
            <span class="cp-stat-val ${gainClass}">${formatMoney(tr.gain)}</span>
          </div>
          <div class="cp-stat-card">
            <span class="cp-stat-label">Gain Ratio</span>
            <span class="cp-stat-val ${gainClass}">${gainRatioFormatted}</span>
          </div>
        </div>
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

    const statusLine = document.getElementById("cpPriceStatusLine");
    if (statusLine) {
      if (!status.configured) {
        statusLine.textContent = "Prices not configured · 0 instruments priced";
      } else {
        const sourceName = status.source || "provider";
        const instCount = status.instruments || 0;
        const obsCount = status.observations || 0;
        let line = `Source: ${sourceName} · ${instCount} instruments priced (${obsCount} bars) · Last refresh: ${formatDate(status.last_refresh_at)}`;
        if (status.last_error) {
          line += ` · Last error: ${status.last_error}`;
        }
        statusLine.textContent = line;
      }
    }

    const badge = document.getElementById("cpPriceBadge");
    if (badge) {
      badge.textContent = status.configured ? ({ yfinance: "Yahoo", stooq: "Stooq" }[status.source] || status.source || "Active") : "Unconfigured";
    }

    const unmappedNote = document.getElementById("cpPriceUnmappedNote");
    if (unmappedNote) {
      if (status.unmapped && status.unmapped.length > 0) {
        unmappedNote.hidden = false;
        unmappedNote.innerHTML = `<p>Unmapped instruments (CFD or unrecognised suffix): <strong>${status.unmapped.map(escapeHtml).join(", ")}</strong>. Track prices by uploading a price file.</p>`;
      } else {
        unmappedNote.hidden = true;
        unmappedNote.innerHTML = "";
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

document.addEventListener("DOMContentLoaded", () => {
  setupCollapsibles();
  setupUploadDialog();
  setupGrantDialog();
  setupPriceControls();
  setupPriceFileDialog();

  const refreshBtn = document.getElementById("cpRefreshBtn");
  if (refreshBtn) {
    refreshBtn.addEventListener("click", () => {
      showBanner("");
      loadPortfolio();
      loadInsights();
      loadGrants();
      loadFilings();
      loadSecStatus();
      loadSyncStatus();
      loadPriceStatus();
      loadForecasts();
      loadEvaluation();
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
      loadPortfolio();
      loadInsights();
      loadGrants();
      loadFilings();
      loadSecStatus();
      loadSyncStatus();
      loadPriceStatus();
      loadForecasts();
      loadEvaluation();
    });
  }

  loadAccounts().then(() => {
    loadPortfolio();
    loadInsights();
    loadGrants();
    loadFilings();
    loadSecStatus();
    loadSyncStatus();
    loadPriceStatus();
    loadForecasts();
    loadEvaluation();
  });
});

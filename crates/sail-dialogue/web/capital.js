// SAIL Capital Web Client
// Interacts with /api/capital endpoints according to the final contract (Round 7).

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
        li.innerHTML = `
          <div class="cp-var-subject">${escapeHtml(v.subject)}</div>
          <div class="cp-var-figures">
            <span>Ledger: <strong>${escapeHtml(v.ledger || "—")}</strong></span>
            <span>Broker: <strong>${escapeHtml(v.broker || "—")}</strong></span>
          </div>
          <div class="cp-var-note">${escapeHtml(v.note || "")}</div>
        `;
        variancesList.appendChild(li);
      }
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
      const lotsHtml = pos.lots.map(l =>
        `<li>${formatDate(l.opened ? l.opened.at : null)} · <strong>${escapeHtml(l.quantity)}</strong> units @ ${l.unit_cost ? formatMoney(l.unit_cost) : "not stated"}</li>`
      ).join("");
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
      await loadFilings();
      await loadSecStatus();
      await loadSyncStatus();
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

document.addEventListener("DOMContentLoaded", () => {
  setupUploadDialog();

  const refreshBtn = document.getElementById("cpRefreshBtn");
  if (refreshBtn) {
    refreshBtn.addEventListener("click", () => {
      showBanner("");
      loadPortfolio();
      loadFilings();
      loadSecStatus();
      loadSyncStatus();
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
      loadFilings();
      loadSecStatus();
      loadSyncStatus();
    });
  }

  loadAccounts().then(() => {
    loadPortfolio();
    loadFilings();
    loadSecStatus();
    loadSyncStatus();
  });
});

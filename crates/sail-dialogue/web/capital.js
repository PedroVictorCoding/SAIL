// SAIL Capital Web Client
// Interacts with /api/capital endpoints according to the final contract (Round 7).

let currentAccount = null;
let currentTz = "Europe/Lisbon";
let selectedFile = null;

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

function formatMoney(money) {
  if (!money || money.amount === undefined || money.amount === null) return "—";
  const num = parseFloat(money.amount);
  const formatted = isNaN(num) ? money.amount : num.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 4 });
  return `${formatted} ${money.currency || ""}`.trim();
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

async function fetchJson(url, options = {}) {
  const res = await fetch(url, options);
  if (!res.ok) {
    let errText = res.statusText;
    try {
      const errObj = await res.json();
      if (errObj && errObj.error) errText = errObj.error;
    } catch {}
    throw new Error(errText);
  }
  return res.json();
}

async function loadAccounts() {
  try {
    const accounts = await fetchJson("/api/capital/accounts");
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
  showBanner("");

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
    const snapAsOf = p.snapshot_as_of ? formatDate(p.snapshot_as_of) : "unknown";
    const knownBy = p.as_of && p.as_of.known_by ? formatDate(p.as_of.known_by) : "recent";
    freshness.textContent = `XTB data as of ${snapAsOf} · imported ${knownBy}`;
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
      cashVal.textContent = "0.00";
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
    if (mvs.length > 0 && mvs.every(m => m.currency === mvs[0].currency)) {
      const sum = mvs.reduce((acc, m) => acc + (parseFloat(m.amount) || 0), 0);
      holdingsVal.textContent = `${sum.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} ${mvs[0].currency}`;
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
    if (nets.length > 0 && nets.every(m => m.currency === nets[0].currency)) {
      const sum = nets.reduce((acc, m) => acc + (parseFloat(m.amount) || 0), 0);
      divVal.textContent = `${sum.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} ${nets[0].currency}`;
    } else if (nets.length > 0) {
      divVal.textContent = `${nets.length} payouts`;
    } else {
      divVal.textContent = "0.00";
    }
  }

  // Metrics: Fees
  const feesVal = document.getElementById("cpFeesVal");
  if (feesVal) {
    const flows = p.flows || [];
    const feeFlows = flows.filter(f => f.label.startsWith("fee:"));
    if (feeFlows.length > 0 && feeFlows.every(f => f.total.currency === feeFlows[0].total.currency)) {
      const sum = feeFlows.reduce((acc, f) => acc + (parseFloat(f.total.amount) || 0), 0);
      feesVal.textContent = `${sum.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} ${feeFlows[0].total.currency}`;
    } else if (feeFlows.length > 0) {
      feesVal.textContent = `${feeFlows.length} fee types`;
    } else {
      feesVal.textContent = "0.00";
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
    tbody.innerHTML = `<tr><td colspan="5" class="cp-empty">${isCfd ? "No open CFD contracts." : "No owned positions recorded."}</td></tr>`;
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
        `<li>${formatDate(l.opened ? l.opened.at : null)} · <strong>${l.quantity}</strong> units @ ${l.unit_cost ? formatMoney(l.unit_cost) : "unspecified"}</li>`
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
      <td>${formatDate(d.paid ? d.paid.at : null)}</td>
      <td><strong>${escapeHtml(inst)}</strong></td>
      <td class="num">${formatMoney(d.gross)}</td>
      <td class="num" style="color:#f3b05a">${d.withheld ? formatMoney(d.withheld) : "—"}</td>
      <td class="num" style="color:#5ccda4"><strong>${d.net ? formatMoney(d.net) : formatMoney(d.gross)}</strong></td>
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

    dialog.close();
    setState("loading", "Uploading…");
    showBanner(`Uploading ${selectedFile.name}…`);

    try {
      const buffer = await selectedFile.arrayBuffer();
      const accountParam = currentAccount ? `account=${encodeURIComponent(currentAccount)}&` : "";
      const url = `/api/capital/import/xtb?${accountParam}tz=${encodeURIComponent(tz)}`;

      const res = await fetch(url, {
        method: "POST",
        headers: {
          "Content-Type": "application/octet-stream",
          "X-File-Name": selectedFile.name,
        },
        body: buffer,
      });

      if (!res.ok) {
        let errMsg = res.statusText;
        try {
          const json = await res.json();
          if (json && json.error) errMsg = json.error;
        } catch {}
        throw new Error(errMsg);
      }

      const batch = await res.json();
      showBanner(`Report imported successfully: ${batch.added || 0} rows added, ${batch.duplicates || 0} duplicates.`);

      // Refresh everything
      await loadAccounts();
      await loadPortfolio();
      await loadSyncStatus();
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

document.addEventListener("DOMContentLoaded", () => {
  setupUploadDialog();

  const refreshBtn = document.getElementById("cpRefreshBtn");
  if (refreshBtn) {
    refreshBtn.addEventListener("click", () => {
      loadPortfolio();
      loadSyncStatus();
    });
  }

  const accountSelect = document.getElementById("cpAccountSelect");
  if (accountSelect) {
    accountSelect.addEventListener("change", (e) => {
      currentAccount = e.target.value;
      loadPortfolio();
      loadSyncStatus();
    });
  }

  loadAccounts().then(() => {
    loadPortfolio();
    loadSyncStatus();
  });
});

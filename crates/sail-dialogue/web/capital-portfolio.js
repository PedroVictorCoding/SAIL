// SAIL Capital — portfolio.
"use strict";

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

  // Owned Positions Table
  renderPositionsTable("cpPositionsBody", p.positions || [], false);

  // CFD Positions Table
  renderPositionsTable("cpCfdBody", p.cfd_positions || [], true);

  // Dividends Table
  renderDividendsTable(p.dividends || []);

  // The Outlook tab's holding picker.
  const heldSymbols = Array.from(new Set((p.positions || []).map(pos => (pos.instrument && pos.instrument.symbol) || pos.symbol).filter(Boolean))).sort();
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

let latestHoldingGuards = null;

async function loadHoldingGuards() {
  const tbody = document.getElementById("cpGuardsBody");
  if (!tbody) return;

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const guards = await fetchJson(`/api/capital/signals/trade/guards${query}`);
    latestHoldingGuards = guards;

    if (!Array.isArray(guards) || guards.length === 0) {
      tbody.innerHTML = `<tr><td colspan="8" class="cp-empty">No held positions to guard.</td></tr>`;
      return;
    }

    tbody.innerHTML = guards.map(g => {
      const dist = Number(g.stop_distance_pct || 0);
      const distStr = `${dist >= 0 ? "+" : ""}${dist.toFixed(1)}%`;

      const statusBadge = g.stop_hit
        ? `<span class="cp-badge cp-badge-stop-hit">trend stop hit</span>`
        : `<span class="cp-badge cp-badge-ok">Holding</span>`;

      let trendStr = "—";
      if (g.above_200d === true) {
        trendStr = `<span class="cp-trend-above">Above 200d</span>`;
      } else if (g.above_200d === false) {
        trendStr = `<span class="cp-trend-below">Below 200d</span>`;
      }

      const earningsStr = g.next_earnings ? formatDay(g.next_earnings) : "—";

      let activeSignalLink = "—";
      if (latestTradeSignals && Array.isArray(latestTradeSignals.cards)) {
        const match = latestTradeSignals.cards.find(c => c.symbol === g.symbol || c.key === g.key);
        if (match) {
          activeSignalLink = `<button type="button" class="cp-link-btn cp-guard-card-link" data-card-id="${escapeHtml(match.id)}">${match.side.toUpperCase()} ${escapeHtml(formatSetupName(match.setup))} →</button>`;
        }
      }

      return `
        <tr>
          <td><strong>${escapeHtml(g.symbol)}</strong></td>
          <td class="num">${formatTradePrice(g.close)}</td>
          <td class="num">${formatTradePrice(g.trailing_stop)}</td>
          <td class="num ${g.stop_hit ? "cp-dist-alert" : ""}">${distStr}</td>
          <td>${statusBadge}</td>
          <td>${trendStr}</td>
          <td>${earningsStr}</td>
          <td>${activeSignalLink}</td>
        </tr>
      `;
    }).join("");

    // Hook up active card links
    tbody.querySelectorAll(".cp-guard-card-link").forEach(btn => {
      btn.addEventListener("click", () => {
        const cardId = btn.getAttribute("data-card-id");
        switchTab("outlook", true);
        setTimeout(() => {
          const cardEl = document.getElementById(`card-${cardId}`);
          if (cardEl) {
            cardEl.scrollIntoView({ behavior: "smooth", block: "center" });
            cardEl.classList.add("cp-card-highlight");
            setTimeout(() => cardEl.classList.remove("cp-card-highlight"), 2500);
          }
        }, 80);
      });
    });
  } catch (err) {
    tbody.innerHTML = `<tr><td colspan="8" class="cp-empty">Holding guards unavailable (${escapeHtml(err.message)}).</td></tr>`;
  }
}

function setupSecRefresh() {
  const btn = document.getElementById("cpSecRefreshBtn");
  if (!btn) return;
  btn.addEventListener("click", async () => {
    btn.disabled = true;
    btn.textContent = "Checking SEC…";
    try {
      const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
      const res = await fetchJson(`/api/capital/sec/refresh${query}`, { method: "POST" });
      await Promise.all([loadFilings(), loadSecStatus()]);
      if (res.last_error) {
        showBanner(`SEC check stopped: ${res.last_error}`, true);
      } else {
        showBanner(`SEC check complete: ${res.issuers_checked || 0} issuers checked, ${res.new_evidence || 0} new filings stored.`);
      }
    } catch (err) {
      showBanner(`SEC refresh failed: ${err.message}`, true);
    } finally {
      btn.disabled = false;
      btn.textContent = "Check SEC now";
    }
  });
}

registerTab("portfolio", {
  setup() {
    setupGrantDialog();
    setupSecRefresh();
  },
  load: () => Promise.all([loadHoldingGuards(), loadGrants(), loadFilings(), loadSecStatus()]),
});

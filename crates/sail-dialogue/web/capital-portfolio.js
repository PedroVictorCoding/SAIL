// SAIL Capital — portfolio.
"use strict";

let latestPortfolio = null;
let latestHoldingGuards = null;

async function loadPortfolio() {
  setState("loading", "Loading…");
  if (typeof latestInsights !== "undefined") {
    latestInsights = null;
    latestInsightsAccount = null;
  }

  if (accountList.length === 0) {
    // Nothing imported yet: an empty state, not an error.
    latestPortfolio = null;
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
    latestPortfolio = null;
    renderEmptyPortfolio();
  }
}

function renderEmptyPortfolio() {
  const freshness = document.getElementById("cpFreshness");
  if (freshness) freshness.textContent = "XTB data as of — · imported —";

  const matchedNote = document.getElementById("cpReconMatchedNote");
  if (matchedNote) matchedNote.hidden = true;

  const reconSection = document.getElementById("cpReconciliation");
  if (reconSection) reconSection.hidden = true;

  const variancesDiv = document.getElementById("cpVariances");
  if (variancesDiv) variancesDiv.hidden = true;

  const grantsContainer = document.getElementById("cpGrants");
  if (grantsContainer) grantsContainer.hidden = true;

  const cfdPanel = document.getElementById("cpCfdPanel");
  if (cfdPanel) cfdPanel.hidden = true;

  const allocBody = document.getElementById("cpAllocationBody");
  if (allocBody) allocBody.innerHTML = `<p class="cp-empty">Import a report to see allocation details.</p>`;
  const allocFreshness = document.getElementById("cpAllocationFreshness");
  if (allocFreshness) allocFreshness.textContent = "Deterministic calculation from ledger";

  const insightsBody = document.getElementById("cpInsightsBody");
  if (insightsBody) insightsBody.innerHTML = `<p class="cp-empty">Import a report to see income details.</p>`;
  const insightsFreshness = document.getElementById("cpInsightsFreshness");
  if (insightsFreshness) insightsFreshness.textContent = "Deterministic calculation from ledger";

  const posBody = document.getElementById("cpPositionsBody");
  if (posBody) posBody.innerHTML = `<tr><td colspan="6" class="cp-empty">No portfolio data yet. Upload an XTB export to begin.</td></tr>`;
  const cfdBody = document.getElementById("cpCfdBody");
  if (cfdBody) cfdBody.innerHTML = `<tr><td colspan="5" class="cp-empty">No open CFD contracts.</td></tr>`;
  if (typeof renderDividendsTable === "function") renderDividendsTable([]);
  updateOutlookHoldings([]);
}

function renderPortfolio(p) {
  latestPortfolio = p;

  // Freshness bar
  const freshness = document.getElementById("cpFreshness");
  if (freshness) {
    const snapAsOf = p.snapshot_as_of ? formatDate(p.snapshot_as_of) : "not stated";
    const acct = accountList.find(a => a.account === p.account);
    const imported = acct && acct.last_import_at ? formatDate(acct.last_import_at) : "—";
    freshness.textContent = `XTB data as of: ${snapAsOf} · imported ${imported}`;
  }

  // Reconciliation
  const matchedNote = document.getElementById("cpReconMatchedNote");
  const reconSection = document.getElementById("cpReconciliation");
  const reconSummary = document.getElementById("cpReconSummary");
  const variancesDiv = document.getElementById("cpVariances");
  const variancesList = document.getElementById("cpVariancesList");
  const grantsContainer = document.getElementById("cpGrants");

  if (p.reconciliation) {
    const status = p.reconciliation.status || "unverified";
    const compared = p.reconciliation.compared || 0;
    const variances = p.reconciliation.variances || [];
    const varCount = variances.length;

    if (status === "matched") {
      if (matchedNote) {
        matchedNote.hidden = false;
        matchedNote.textContent = `Ledger matches XTB's figures (${compared} checked).`;
      }
      if (variancesDiv) variancesDiv.hidden = true;
      if (variancesList) variancesList.innerHTML = "";
      if (reconSection) {
        const hasGrants = grantsContainer && !grantsContainer.hidden;
        reconSection.hidden = !hasGrants;
        if (hasGrants && reconSummary) {
          reconSummary.textContent = "Declared received shares.";
        }
      }
    } else if (status === "variance" && varCount > 0) {
      if (matchedNote) matchedNote.hidden = true;
      if (reconSection) reconSection.hidden = false;
      if (reconSummary) {
        reconSummary.innerHTML = `<span class="cp-word-warn">${varCount} variance${varCount === 1 ? "" : "s"}</span> across ${compared} compared figures.`;
      }
      if (variancesDiv && variancesList) {
        variancesDiv.hidden = false;
        variancesList.innerHTML = "";
        for (const v of variances) {
          const li = document.createElement("li");
          li.className = "cp-row";

          const missingQty = getMissingQuantity(v);
          let actionBtnHtml = "";
          if (missingQty !== null) {
            const sym = resolveVarianceSymbol(v.subject, p);
            actionBtnHtml = `<button type="button" class="cp-button cp-btn-sm cp-record-grant-btn" data-symbol="${escapeHtml(sym)}" data-qty="${escapeHtml(missingQty)}">Record received shares</button>`;
          }

          li.innerHTML = `
            <span class="cp-row-title">${escapeHtml(v.subject)}</span>
            <span class="cp-row-value">${actionBtnHtml}</span>
            <span class="cp-row-meta">Ledger: ${escapeHtml(v.ledger || "—")} · Broker: ${escapeHtml(v.broker || "—")}${v.note ? ` · ${escapeHtml(v.note)}` : ""}</span>
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
      }
    } else {
      if (matchedNote) {
        matchedNote.hidden = false;
        matchedNote.textContent = "No snapshot available to verify ledger against broker figures.";
      }
      if (variancesDiv) variancesDiv.hidden = true;
      if (variancesList) variancesList.innerHTML = "";
      if (reconSection) {
        const hasGrants = grantsContainer && !grantsContainer.hidden;
        reconSection.hidden = !hasGrants;
        if (hasGrants && reconSummary) {
          reconSummary.textContent = "Declared received shares.";
        }
      }
    }
  }

  // Owned Positions Table
  renderPositionsTable("cpPositionsBody", p.positions || [], false);
  fillPositionChanges();

  // CFD Positions Table (hidden when empty)
  const cfds = p.cfd_positions || [];
  const cfdPanel = document.getElementById("cpCfdPanel");
  if (cfdPanel) cfdPanel.hidden = cfds.length === 0;
  renderPositionsTable("cpCfdBody", cfds, true);

  // Dividends Table
  renderDividendsTable(p.dividends || []);

  // The Outlook tab's holding picker.
  const heldSymbols = Array.from(new Set((p.positions || []).map(pos => (pos.instrument && pos.instrument.symbol) || pos.symbol).filter(Boolean))).sort();
  updateOutlookHoldings(heldSymbols);
}

function findGuardForPosition(pos) {
  if (!Array.isArray(latestHoldingGuards)) return null;
  const inst = pos.instrument || {};
  const sym = inst.symbol || pos.symbol;
  const key = pos.instrument_key;
  return latestHoldingGuards.find(g => (sym && g.symbol === sym) || (key && g.key === key)) || null;
}

function renderPositionsTable(tbodyId, positions, isCfd) {
  const tbody = document.getElementById(tbodyId);
  if (!tbody) return;

  const colSpan = isCfd ? 5 : 6;

  if (positions.length === 0) {
    tbody.innerHTML = `<tr><td colspan="${colSpan}" class="cp-empty">${isCfd ? "No open CFD contracts." : "No owned positions derived from the ledger."}</td></tr>`;
    return;
  }

  tbody.innerHTML = "";
  for (let i = 0; i < positions.length; i++) {
    const pos = positions[i];
    const inst = pos.instrument || {};
    const tr = document.createElement("tr");
    const guard = isCfd ? null : findGuardForPosition(pos);

    const lotCount = (pos.lots || []).length;
    const hasDetails = lotCount > 0 || Boolean(guard);
    const lotsBtn = hasDetails
      ? `<button type="button" class="cp-lots-toggle" data-idx="${i}" data-table="${tbodyId}">${lotCount} lot${lotCount === 1 ? "" : "s"}</button>`
      : "0";

    let stopCellHtml = "";
    if (!isCfd) {
      if (guard && guard.trailing_stop !== null && guard.trailing_stop !== undefined) {
        const dist = Number(guard.stop_distance_pct || 0);
        const distStr = `${dist >= 0 ? "+" : ""}${dist.toFixed(1)}%`;
        const stopPrice = typeof formatTradePrice === "function" ? formatTradePrice(guard.trailing_stop) : escapeHtml(guard.trailing_stop);
        stopCellHtml = guard.stop_hit
          ? `<td class="num">${stopPrice} <span class="cp-word-bad">${distStr} · stop hit</span></td>`
          : `<td class="num">${stopPrice} <span class="cp-pos-dist">${distStr}</span></td>`;
      } else {
        stopCellHtml = `<td class="num">—</td>`;
      }
    }

    tr.innerHTML = `
      <td>
        <span class="cp-inst">
          ${instrumentAvatar(inst.symbol, isCfd ? "index" : String(inst.kind || "stock").toLowerCase())}
          <span>
            <strong>${escapeHtml(inst.symbol || "UNKNOWN")}</strong>
            ${inst.name ? `<br><span class="cp-pos-name">${escapeHtml(inst.name)}</span>` : ""}
            ${inst.isin ? `<br><span class="cp-pos-isin">${escapeHtml(inst.isin)}</span>` : ""}
          </span>
        </span>
      </td>
      <td class="num">${escapeHtml(pos.quantity || "0")}</td>
      <td class="num">${pos.cost_basis ? formatMoney(pos.cost_basis) : "—"}</td>
      <td class="num">${pos.broker_market_value ? formatMoney(pos.broker_market_value) : "—"}${isCfd ? "" : `<br><span class="cp-pos-chg" data-symbol="${escapeHtml(inst.symbol || "")}"></span>`}</td>
      ${stopCellHtml}
      <td>${lotsBtn}</td>
    `;
    tbody.appendChild(tr);

    // Expandable lot + guard detail row
    if (hasDetails) {
      const lotTr = document.createElement("tr");
      lotTr.id = `${tbodyId}-lots-${i}`;
      lotTr.hidden = true;

      let guardHtml = "";
      if (guard) {
        const parts = [];
        if (guard.above_200d === true) {
          parts.push(`<span class="cp-word-good">Above 200d</span>`);
        } else if (guard.above_200d === false) {
          parts.push(`<span class="cp-word-bad">Below 200d</span>`);
        }
        if (guard.next_earnings) {
          parts.push(`Next earnings ${formatDay(guard.next_earnings)}`);
        }
        if (typeof latestTradeSignals !== "undefined" && latestTradeSignals && Array.isArray(latestTradeSignals.cards)) {
          const match = latestTradeSignals.cards.find(c => c.symbol === guard.symbol || c.key === guard.key);
          if (match) {
            const sideText = typeof signalSideLabel === "function" ? signalSideLabel(match.side) : match.side.toUpperCase();
            parts.push(`<button type="button" class="cp-link-btn cp-guard-card-link" data-card-id="${escapeHtml(match.id)}">${escapeHtml(sideText)} ${escapeHtml(formatSetupName(match.setup))} →</button>`);
          }
        }
        if (parts.length > 0) {
          guardHtml = `<div class="cp-pos-guard-meta">${parts.join(" · ")}</div>`;
        }
      }

      const lotsHtml = (pos.lots || []).map(l => {
        const isGranted = l.origin === "granted";
        const originText = isGranted ? ` · <span class="cp-word-wait">granted</span>` : "";
        return `<li>${formatDate(l.opened ? l.opened.at : null)} · <strong>${escapeHtml(l.quantity)}</strong> units @ ${l.unit_cost ? formatMoney(l.unit_cost) : "not stated"}${originText}</li>`;
      }).join("");

      lotTr.innerHTML = `
        <td colspan="${colSpan}" class="cp-lots-detail">
          ${guardHtml}
          ${lotsHtml ? `<ul class="cp-lots-list">${lotsHtml}</ul>` : ""}
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

  // Hook up active signal links
  tbody.querySelectorAll(".cp-guard-card-link").forEach(btn => {
    btn.addEventListener("click", () => {
      const cardId = btn.getAttribute("data-card-id");
      switchTab("outlook", true);
      setTimeout(() => {
        const cardEl = document.getElementById(`card-${cardId}`);
        if (cardEl) {
          const det = cardEl.querySelector("details") || (cardEl.tagName === "DETAILS" ? cardEl : null);
          if (det) det.open = true;
          cardEl.scrollIntoView({ behavior: "smooth", block: "center" });
        }
      }, 80);
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
        metaSpan.innerHTML = `<span class="cp-word-wait">Off</span> · SEC access not configured`;
      } else if (status.last_refresh_at && status.last_error) {
        metaSpan.innerHTML = `<span class="cp-word-bad">Error</span> · Last check ${formatDate(status.last_refresh_at)} failed: ${escapeHtml(status.last_error)}`;
      } else if (status.last_refresh_at) {
        metaSpan.innerHTML = `<span class="cp-word-good">Active</span> · Last check ${formatDate(status.last_refresh_at)} · ${status.new_evidence || 0} new filings`;
      } else {
        metaSpan.innerHTML = `<span class="cp-word-good">Active</span> · Ready to check SEC EDGAR`;
      }
    }
  } catch (err) {
    console.error("Failed to load SEC status:", err);
  }
}

async function loadFilings() {
  const list = document.getElementById("cpFilingsList");
  const unmatchedWrap = document.getElementById("cpUnmatchedWrap");
  const unmatchedCount = document.getElementById("cpUnmatchedCount");
  const unmatchedList = document.getElementById("cpUnmatchedList");
  if (!list) return;

  if (accountList.length === 0) {
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
      const ul = document.createElement("ul");
      ul.className = "cp-rows";
      for (const item of items) {
        const li = document.createElement("li");
        li.className = "cp-row";

        const isProposal = item.action === "research_proposal";
        const statusWord = isProposal
          ? `<span class="cp-word-good">Research proposal</span>`
          : `<span class="cp-word-wait">Watch</span>`;

        const match = matches.find(m => m.instrument_key === (item.exposure && item.exposure.instrument_key));
        const symbol = match ? match.symbol : (item.exposure && item.exposure.instrument_key) || "";
        const ticker = (match && match.match && match.match.ticker)
          ? match.match.ticker
          : symbol.replace(/\.US$/i, "");

        const exposure = item.exposure || {};
        const metaParts = [];
        if (item.reason) metaParts.push(escapeHtml(item.reason));
        if (exposure.quantity) metaParts.push(`Holding: ${escapeHtml(exposure.quantity)} units`);
        if (exposure.broker_market_value) metaParts.push(`Value: ${formatMoney(exposure.broker_market_value)}`);
        if (exposure.share_of_owned_value !== null && exposure.share_of_owned_value !== undefined) {
          const pct = (parseFloat(exposure.share_of_owned_value) * 100).toFixed(2);
          metaParts.push(`Share: ${pct}%`);
        }
        metaParts.push(`Published ${formatDate(item.published_at)} · first seen by SAIL ${formatDate(item.first_observed_at)}`);

        const url = (item.filing && item.filing.url && item.filing.url.startsWith("https://www.sec.gov/"))
          ? item.filing.url
          : null;
        const linkHtml = url
          ? `<a class="cp-filing-link" href="${escapeHtml(url)}" rel="noopener noreferrer" target="_blank">View on sec.gov ↗</a>`
          : "";

        const form = escapeHtml((item.filing && item.filing.form) || "");
        const issuer = escapeHtml(item.issuer_name || "");

        li.innerHTML = `
          <span class="cp-row-title">${statusWord} · ${form} · ${issuer} (${escapeHtml(ticker)})</span>
          <span class="cp-row-value">${linkHtml}</span>
          <span class="cp-row-meta">${metaParts.join(" · ")}</span>
        `;
        ul.appendChild(li);
      }
      list.innerHTML = "";
      list.appendChild(ul);
    }

    const uncovered = matches.filter(m => m.match && m.match.status !== "matched");
    if (unmatchedWrap && unmatchedList) {
      if (uncovered.length > 0) {
        unmatchedWrap.hidden = false;
        if (unmatchedCount) unmatchedCount.textContent = String(uncovered.length);
        unmatchedList.innerHTML = uncovered.map(m => {
          const reason = m.match.status === "not_us_listed"
            ? "Non-US listing (no .US suffix); SEC does not apply"
            : (m.match.reason || "Unresolved");
          return `
            <li class="cp-row">
              <span class="cp-row-title">${escapeHtml(m.symbol)}</span>
              <span class="cp-row-value cp-word-wait">${escapeHtml(reason)}</span>
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
      await loadInsights(true);
      await loadGrants();
    } catch (err) {
      showBanner(`Failed to record grant: ${err.message}`, true);
    } finally {
      submitBtn.disabled = false;
      submitBtn.textContent = "Record shares";
    }
  });
}

async function loadGrants() {
  const reconSection = document.getElementById("cpReconciliation");
  const reconSummary = document.getElementById("cpReconSummary");
  const variancesDiv = document.getElementById("cpVariances");
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
      if (reconSection && variancesDiv && variancesDiv.hidden) {
        reconSection.hidden = true;
      }
      return;
    }

    grantsContainer.hidden = false;
    if (reconSection) {
      reconSection.hidden = false;
      if (variancesDiv && variancesDiv.hidden && reconSummary) {
        reconSummary.textContent = "Declared received shares.";
      }
    }
    grantsList.innerHTML = "";
    for (const g of grants) {
      const li = document.createElement("li");
      li.className = "cp-row";
      const isRetracted = Boolean(g.retracted_at);
      const statusHtml = isRetracted
        ? `<span class="cp-word-wait">Retracted ${formatDate(g.retracted_at)}</span>`
        : `<button type="button" class="cp-button cp-btn-sm cp-retract-grant-btn" data-grant-id="${escapeHtml(g.id)}" data-symbol="${escapeHtml(g.symbol)}">Retract</button>`;

      const costText = g.cost && g.cost.amount && g.cost.amount !== "0"
        ? formatMoney(g.cost)
        : "Free (zero cost)";
      const kindLabel = (g.kind || "stock").toUpperCase();

      li.innerHTML = `
        <span class="cp-row-title">${escapeHtml(g.symbol)} · ${escapeHtml(kindLabel)} · ${escapeHtml(g.quantity)} units</span>
        <span class="cp-row-value">${statusHtml}</span>
        <span class="cp-row-meta">Received ${formatDay(g.received_at)} · Basis: ${costText}${g.note ? ` · ${escapeHtml(g.note)}` : ""}</span>
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
          await loadInsights(true);
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

async function loadHoldingGuards() {
  if (accountList.length === 0) {
    latestHoldingGuards = null;
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const guards = await fetchJson(`/api/capital/signals/trade/guards${query}`);
    latestHoldingGuards = Array.isArray(guards) ? guards : [];
    if (latestPortfolio) {
      renderPositionsTable("cpPositionsBody", latestPortfolio.positions || [], false);
    }
  } catch (err) {
    console.error("Failed to load holding guards:", err);
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

// Each owned position gains its last session's move from the shared market board.
function fillPositionChanges() {
  loadMarkets()
    .then((board) => {
      const bySymbol = new Map((board.tiles || []).map((t) => [t.symbol, t]));
      document.querySelectorAll(".cp-pos-chg[data-symbol]").forEach((el) => {
        const tile = bySymbol.get(el.dataset.symbol);
        el.innerHTML = tile ? `${changePill(tile.change_1d)} <span class="cp-muted">1D</span>` : "";
      });
    })
    .catch(() => {});
}

registerTab("portfolio", {
  setup() {
    setupGrantDialog();
    setupSecRefresh();
  },
  load: () => Promise.all([loadHoldingGuards(), loadGrants(), loadFilings(), loadSecStatus(), loadInsights()]),
});

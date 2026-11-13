// SAIL Capital — settings.
"use strict";

let modelsPollingTimer = null;

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

    const ul = document.createElement("ul");
    ul.className = "cp-rows";

    for (const b of batches) {
      const li = document.createElement("li");
      li.className = "cp-row";

      const shaShort = b.file_sha256 ? b.file_sha256.substring(0, 10) : "";
      const issues = b.issues || [];
      const issueCount = issues.length;

      let issuesHtml = "";
      if (issueCount > 0) {
        const items = issues.map(iss => {
          const sev = String(iss.severity || "info").toLowerCase();
          const sevClass = sev === "error" ? "cp-word-bad" : sev === "warning" ? "cp-word-warn" : "cp-word-wait";
          return `
            <li class="cp-row">
              <span class="cp-row-title"><span class="${sevClass}">[${escapeHtml(sev.toUpperCase())}]</span> ${escapeHtml(iss.code)}: ${escapeHtml(iss.message)}</span>
            </li>
          `;
        }).join("");
        issuesHtml = `
          <details class="cp-more cp-row-details">
            <summary>${issueCount} issue(s) recorded ${b.issues_dropped ? `(${b.issues_dropped} dropped)` : ""}</summary>
            <ul class="cp-rows">${items}</ul>
          </details>
        `;
      }

      li.innerHTML = `
        <span class="cp-row-title">${escapeHtml(b.file_name || "Statement export")}</span>
        <span class="cp-row-value cp-batch-sha">${shaShort}…</span>
        <span class="cp-row-meta">Imported ${formatDate(b.imported_at)} · Added: ${b.added || 0} · Duplicates: ${b.duplicates || 0} · Conflicts: ${b.conflicts || 0}</span>
        ${issuesHtml}
      `;
      ul.appendChild(li);
    }

    list.innerHTML = "";
    list.appendChild(ul);
  } catch (err) {
    list.innerHTML = `<p class="cp-empty cp-word-bad">Failed to load import status: ${escapeHtml(err.message)}</p>`;
  }
}

async function loadPriceStatus() {
  try {
    const status = await fetchJson("/api/capital/prices/status");
    const dataOptInNote = document.getElementById("cpDataPriceOptInNote");
    if (dataOptInNote) {
      dataOptInNote.hidden = Boolean(status.configured);
    }

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
    if (dataStatusLine) dataStatusLine.textContent = statusText;

    const dataUnmappedNote = document.getElementById("cpDataPriceUnmappedNote");
    if (dataUnmappedNote) {
      if (status.unmapped && status.unmapped.length > 0) {
        dataUnmappedNote.hidden = false;
        dataUnmappedNote.innerHTML = `<p>Unmapped instruments (CFD or unrecognised suffix): <strong>${status.unmapped.map(escapeHtml).join(", ")}</strong>. Track prices by uploading a price file.</p>`;
      } else {
        dataUnmappedNote.hidden = true;
        dataUnmappedNote.innerHTML = "";
      }
    }
  } catch (err) {
    console.error("Failed to load price status:", err);
  }
}

function setupPriceControls() {
  const refreshBtn = document.getElementById("cpDataPriceRefreshBtn");
  if (!refreshBtn) return;

  refreshBtn.addEventListener("click", async () => {
    refreshBtn.disabled = true;
    refreshBtn.textContent = "Fetching prices…";
    try {
      const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
      const res = await fetchJson(`/api/capital/prices/refresh${query}`, { method: "POST" });
      await loadPriceStatus();
      if (typeof invalidateTabs === "function") invalidateTabs("outlook", "record");
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

function setupPriceFileDialog() {
  const dialog = document.getElementById("cpPriceFileDialog");
  const openBtn = document.getElementById("cpDataPriceUploadBtn");
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
        if (typeof invalidateTabs === "function") invalidateTabs("outlook", "record");
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

async function loadPolicy() {
  const statusWord = document.getElementById("cpPolicyStatusWord");
  const historyList = document.getElementById("cpPolicyHistoryList");
  if (!statusWord && !historyList) return;

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
      setVal("cpLimitRiskPerTrade", l.risk_per_trade_pct !== undefined ? l.risk_per_trade_pct : "0.5");

      const isConfirmed = current.status === "confirmed";
      if (statusWord) {
        statusWord.textContent = isConfirmed ? "Confirmed" : "Draft";
        statusWord.className = isConfirmed ? "cp-word-good" : "cp-word-warn";
      }
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
            <li class="cp-row">
              <span class="cp-row-title">v${p.version} · <span class="${isConfirmed ? "cp-word-good" : "cp-word-warn"}">${isConfirmed ? "Confirmed" : "Draft"}</span></span>
              <span class="cp-row-value">${escapeHtml(dateStr)}</span>
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
    risk_per_trade_pct: (document.getElementById("cpLimitRiskPerTrade")?.value || "0.5").trim(),
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

function setupUploadButton() {
  const dataUploadBtn = document.getElementById("cpDataUploadBtn");
  if (dataUploadBtn) {
    dataUploadBtn.addEventListener("click", () => {
      const fileInput = document.getElementById("cpFileInput");
      if (fileInput) fileInput.click();
    });
  }
}

function renderModelsStatus(data) {
  const list = document.getElementById("cpModelsList");
  const licencesList = document.getElementById("cpModelsLicences");
  const runBtn = document.getElementById("cpRunModelsBtn");
  if (!list) return;

  if (runBtn) {
    if (data && data.running) {
      runBtn.disabled = true;
      runBtn.textContent = "Running models…";
    } else {
      runBtn.disabled = false;
      runBtn.textContent = "Run models";
    }
  }

  // Signal models store values; outlook models (round 12) store quantile paths.
  const outlookRows = ((data && data.outlook) || []).map(m => ({
    spec: { ...(m.spec || {}), name: `${(m.spec && m.spec.name) || "model"} · outlook` },
    enabled: m.enabled,
    last_run: m.last_run,
    stored: m.stored
      ? { values: m.stored[2], instruments: m.stored[1], latest: null, paths: true }
      : null,
  }));
  const models = ((data && data.models) || []).concat(outlookRows);
  if (models.length === 0) {
    list.innerHTML = `<li class="cp-empty">No models configured.</li>`;
    if (licencesList) licencesList.innerHTML = "";
    return;
  }

  list.innerHTML = models.map(m => {
    const spec = m.spec || {};
    const name = escapeHtml(spec.name || "Unknown");
    const model = escapeHtml(spec.model || "");
    const summary = escapeHtml(spec.summary || "");
    const statusWord = m.enabled
      ? `<span class="cp-word-good">Enabled</span>`
      : `<span class="cp-word-wait">Disabled</span>`;

    let storedStr = "None";
    if (m.stored && m.stored.values > 0) {
      const lat = m.stored.latest ? `, latest ${escapeHtml(m.stored.latest)}` : "";
      const unit = m.stored.paths ? "paths" : "values";
      storedStr = `${m.stored.values} ${unit} (${m.stored.instruments} instruments${lat})`;
    }

    let runStr = "Never run";
    if (m.last_run) {
      const at = formatDate(m.last_run.at);
      const added = m.last_run.added ?? 0;
      const skipped = m.last_run.skipped ?? 0;
      const err = m.last_run.error ? ` · <span class="cp-word-bad">Error: ${escapeHtml(m.last_run.error)}</span>` : "";
      runStr = `${at}: +${added} added, ${skipped} skipped${err}`;
    }

    return `
      <li class="cp-row">
        <span class="cp-row-title">${name} · ${statusWord}</span>
        <span class="cp-row-value">${storedStr}</span>
        <span class="cp-row-meta">${summary}${model ? ` · ${model}` : ""} · Last run: ${runStr}</span>
      </li>
    `;
  }).join("");

  if (licencesList) {
    licencesList.innerHTML = models.map(m => {
      const spec = m.spec || {};
      const name = escapeHtml(spec.name || "Unknown");
      const license = escapeHtml(spec.license || "not stated");
      return `
        <li class="cp-row">
          <span class="cp-row-title">${name}</span>
          <span class="cp-row-value cp-word-wait">${license}</span>
        </li>
      `;
    }).join("");
  }
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
    const list = document.getElementById("cpModelsList");
    if (list) {
      list.innerHTML = `<li class="cp-empty">Models status unavailable: ${escapeHtml(err.message)}</li>`;
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

let latestWatchlist = null;

const WATCHLIST_CLASS_LABELS = {
  fx: "FX",
  index: "Indices",
  stock: "Stocks",
  etf: "ETFs",
  commodity: "Commodities",
};

async function loadWatchlist() {
  const container = document.getElementById("cpWatchlistGroups");
  if (!container) return;

  try {
    const list = await fetchJson("/api/capital/universe/watchlist");
    latestWatchlist = list;

    if (!Array.isArray(list) || list.length === 0) {
      container.innerHTML = `<p class="cp-empty">Watchlist is empty.</p>`;
      return;
    }

    const groups = new Map();
    for (const key of Object.keys(WATCHLIST_CLASS_LABELS)) {
      groups.set(key, []);
    }
    for (const item of list) {
      const cls = String(item.class || "other").toLowerCase();
      if (!groups.has(cls)) groups.set(cls, []);
      groups.get(cls).push(item);
    }

    container.innerHTML = "";
    for (const [cls, items] of groups.entries()) {
      if (items.length === 0) continue;
      const label = WATCHLIST_CLASS_LABELS[cls] || (cls.charAt(0).toUpperCase() + cls.slice(1));
      const details = document.createElement("details");
      details.className = "cp-more";

      const rowsHtml = items.map(item => `
        <li class="cp-row">
          <span class="cp-row-title">${escapeHtml(item.symbol)}${item.name ? ` · ${escapeHtml(item.name)}` : ""}</span>
          <span class="cp-row-value">
            <button type="button" class="cp-button cp-btn-sm cp-watch-remove-btn" data-symbol="${escapeHtml(item.symbol)}">Remove</button>
          </span>
          <span class="cp-row-meta">${escapeHtml(item.yahoo)} · ${escapeHtml(item.currency)}</span>
        </li>
      `).join("");

      details.innerHTML = `
        <summary>${escapeHtml(label)} · ${items.length}</summary>
        <ul class="cp-rows">${rowsHtml}</ul>
      `;
      container.appendChild(details);
    }

    container.querySelectorAll(".cp-watch-remove-btn").forEach(btn => {
      btn.addEventListener("click", async () => {
        const symbol = btn.getAttribute("data-symbol");
        btn.disabled = true;
        btn.textContent = "Removing…";
        try {
          await fetchJson("/api/capital/universe/watchlist", {
            method: "POST",
            body: JSON.stringify({ op: "remove", symbol }),
          });
          showBanner(`Removed ${symbol} from watchlist.`);
          await loadWatchlist();
        } catch (err) {
          showBanner(`Failed to remove ${symbol}: ${err.message}`, true);
          btn.disabled = false;
          btn.textContent = "Remove";
        }
      });
    });
  } catch (err) {
    container.innerHTML = `<p class="cp-empty">Watchlist unavailable (${escapeHtml(err.message)}).</p>`;
  }
}

function setupWatchlistControls() {
  const form = document.getElementById("cpWatchlistAddForm");
  const errBox = document.getElementById("cpWatchlistError");
  if (!form) return;

  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    if (errBox) {
      errBox.hidden = true;
      errBox.textContent = "";
    }

    const symbol = (document.getElementById("cpWatchSymbol")?.value || "").trim().toUpperCase();
    const yahoo = (document.getElementById("cpWatchYahoo")?.value || "").trim();
    const assetClass = (document.getElementById("cpWatchClass")?.value || "stock").toLowerCase();
    const currency = (document.getElementById("cpWatchCurrency")?.value || "").trim().toUpperCase();
    const name = (document.getElementById("cpWatchName")?.value || "").trim();

    // Client-side validation per universe/mod.rs
    const symbolRegex = /^[A-Za-z0-9._-]+$/;
    const yahooRegex = /^[A-Za-z0-9._\-=^]+$/;

    if (!symbol || symbol.length > 24 || !symbolRegex.test(symbol)) {
      if (errBox) {
        errBox.hidden = false;
        errBox.textContent = "The XTB symbol may use letters, digits and . _ - only (max 24 chars).";
      }
      return;
    }

    if (!yahoo || yahoo.length > 24 || !yahooRegex.test(yahoo)) {
      if (errBox) {
        errBox.hidden = false;
        errBox.textContent = "The Yahoo symbol may use letters, digits and . _ - = ^ only (max 24 chars).";
      }
      return;
    }

    if (!currency || currency.length !== 3 || !/^[A-Za-z]{3}$/.test(currency)) {
      if (errBox) {
        errBox.hidden = false;
        errBox.textContent = "The currency must be a three-letter code.";
      }
      return;
    }

    if (name.length > 80) {
      if (errBox) {
        errBox.hidden = false;
        errBox.textContent = "The name is too long (max 80 chars).";
      }
      return;
    }

    const addBtn = document.getElementById("cpWatchAddBtn");
    if (addBtn) {
      addBtn.disabled = true;
      addBtn.textContent = "Adding…";
    }

    try {
      await fetchJson("/api/capital/universe/watchlist", {
        method: "POST",
        body: JSON.stringify({
          op: "add",
          item: {
            symbol,
            yahoo,
            class: assetClass,
            currency,
            name: name || symbol,
          },
        }),
      });

      showBanner(`Added ${symbol} to watchlist.`);
      form.reset();
      await loadWatchlist();
    } catch (err) {
      if (errBox) {
        errBox.hidden = false;
        errBox.textContent = err.message || "Failed to add watchlist item.";
      } else {
        showBanner(`Watchlist error: ${err.message}`, true);
      }
    } finally {
      if (addBtn) {
        addBtn.disabled = false;
        addBtn.textContent = "Add to watchlist";
      }
    }
  });
}

registerTab("settings", {
  setup() {
    setupPolicyControls();
    setupWatchlistControls();
    setupPriceControls();
    setupPriceFileDialog();
    setupUploadButton();
    setupModelsControls();
  },
  load: () =>
    Promise.all([loadPolicy(), loadWatchlist(), loadPriceStatus(), loadModelsStatus(), loadSyncStatus()]),
});

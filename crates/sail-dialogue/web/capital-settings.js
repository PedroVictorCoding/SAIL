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
  // Only the baseline-forecast pills: the outlook's horizon and replay pills share the
  // button class and have their own handlers.
  const horizonBtns = document.querySelectorAll(
    '.cp-horizon-pills[aria-label="Forecast horizon"] .cp-horizon-btn'
  );
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
      setVal("cpLimitRiskPerTrade", l.risk_per_trade_pct !== undefined ? l.risk_per_trade_pct : "0.5");

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

  // Signal models store values; outlook models (round 12) store quantile paths. Show
  // both in one table, with the outlook rows labelled.
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
      const unit = m.stored.paths ? "paths" : "values";
      storedStr = `${m.stored.values} ${unit} (${m.stored.instruments} instruments${lat})`;
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

let latestWatchlist = null;

async function loadWatchlist() {
  const tbody = document.getElementById("cpWatchlistBody");
  if (!tbody) return;

  try {
    const list = await fetchJson("/api/capital/universe/watchlist");
    latestWatchlist = list;

    if (!Array.isArray(list) || list.length === 0) {
      tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">Watchlist is empty.</td></tr>`;
      return;
    }

    tbody.innerHTML = list.map(item => `
      <tr>
        <td><strong>${escapeHtml(item.symbol)}</strong></td>
        <td><code>${escapeHtml(item.yahoo)}</code></td>
        <td><span class="cp-class-badge">${escapeHtml((item.class || "").toUpperCase())}</span></td>
        <td>${escapeHtml(item.currency)}</td>
        <td>${escapeHtml(item.name)}</td>
        <td class="num">
          <button type="button" class="cp-button cp-button-sm cp-watch-remove-btn" data-symbol="${escapeHtml(item.symbol)}">Remove</button>
        </td>
      </tr>
    `).join("");

    tbody.querySelectorAll(".cp-watch-remove-btn").forEach(btn => {
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
    tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">Watchlist unavailable (${escapeHtml(err.message)}).</td></tr>`;
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
        addBtn.textContent = "Add to Watchlist";
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
    setupDataTabControls();
    setupModelsControls();
  },
  load: () =>
    Promise.all([loadPolicy(), loadWatchlist(), loadPriceStatus(), loadModelsStatus(), loadSyncStatus()]),
});

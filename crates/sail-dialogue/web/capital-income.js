// SAIL Capital — income.
"use strict";

// The projection always looks 30 years ahead.
const PROJ_YEARS = 30;

let currentProjReinvestPct = 100;

let currentProjMonthlyContribution = 0;

let currentProjAllocation = "proportional";

let currentProjTopN = 5;

let currentProjReservePtTax = false;

let projDebounceTimer = null;

let currentProjectionData = null;

let showAllProjYears = false;

let latestInsights = null;
let latestInsightsAccount = null;
let insightsInFlight = null;

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
      <td class="num cp-word-warn">${d.withheld ? formatMoney(d.withheld) : "—"}</td>
      <td class="num cp-word-good"><strong>${d.net ? formatMoney(d.net) : "not stated"}</strong></td>
    `;
    tbody.appendChild(tr);
  }
}

async function loadInsights(force = false) {
  const incomeBody = document.getElementById("cpInsightsBody");
  const allocBody = document.getElementById("cpAllocationBody");
  const incomeFreshness = document.getElementById("cpInsightsFreshness");
  const allocFreshness = document.getElementById("cpAllocationFreshness");

  if (accountList.length === 0) {
    latestInsights = null;
    latestInsightsAccount = null;
    if (incomeBody) incomeBody.innerHTML = `<p class="cp-empty">Import a report to see income details.</p>`;
    if (allocBody) allocBody.innerHTML = `<p class="cp-empty">Import a report to see allocation details.</p>`;
    if (incomeFreshness) incomeFreshness.textContent = "Deterministic calculation from ledger";
    if (allocFreshness) allocFreshness.textContent = "Deterministic calculation from ledger";
    return;
  }

  if (!force && latestInsights && latestInsightsAccount === currentAccount) {
    renderInsights(latestInsights);
    return;
  }

  if (!force && insightsInFlight && latestInsightsAccount === currentAccount) {
    await insightsInFlight;
    return;
  }

  const acctKey = currentAccount;
  latestInsightsAccount = acctKey;

  insightsInFlight = (async () => {
    try {
      const query = acctKey ? `?account=${encodeURIComponent(acctKey)}` : "";
      const ins = await fetchJson(`/api/capital/insights${query}`);
      if (!hasTodayAccount && ins.account) {
        updateAccountHeaderTiles(ins.account);
      }
      latestInsights = ins;
      renderInsights(ins);
    } catch (err) {
      latestInsights = null;
      const isNotFound = String(err.message).includes("404") || String(err.message).toLowerCase().includes("not found");
      if (isNotFound) {
        if (incomeBody) incomeBody.innerHTML = `<p class="cp-empty">Import a report to see income details.</p>`;
        if (allocBody) allocBody.innerHTML = `<p class="cp-empty">Import a report to see allocation details.</p>`;
        if (incomeFreshness) incomeFreshness.textContent = "Deterministic calculation from ledger";
        if (allocFreshness) allocFreshness.textContent = "Deterministic calculation from ledger";
      } else {
        if (incomeBody) incomeBody.innerHTML = `<p class="cp-empty">Income summary unavailable: ${escapeHtml(err.message)}</p>`;
        if (allocBody) allocBody.innerHTML = `<p class="cp-empty">Allocation unavailable: ${escapeHtml(err.message)}</p>`;
      }
    } finally {
      insightsInFlight = null;
    }
  })();

  await insightsInFlight;
}

function formatInsightsFreshness(f) {
  if (f && f.snapshot_as_of) {
    const age = f.snapshot_age_hours !== null && f.snapshot_age_hours !== undefined
      ? ` (${formatAge(f.snapshot_age_hours)})`
      : "";
    const staleWord = f.stale ? ` · <span class="cp-word-warn">Snapshot older than 72h</span>` : "";
    return `Data as of ${formatDate(f.snapshot_as_of)}${age}${staleWord}`;
  }
  return "No broker snapshot";
}

function renderInsights(ins) {
  renderAllocation(ins);
  renderIncomeSummary(ins);
}

function renderAllocation(ins) {
  const body = document.getElementById("cpAllocationBody");
  const freshnessSpan = document.getElementById("cpAllocationFreshness");
  if (!body) return;

  if (freshnessSpan) {
    freshnessSpan.innerHTML = formatInsightsFreshness(ins.freshness);
  }

  let html = "";

  // Concentration
  if (ins.concentration) {
    const c = ins.concentration;
    const effStr = c.effective_positions ? `${c.effective_positions}` : "—";
    const largestPct = (parseFloat(c.largest_share) * 100).toFixed(1);
    const topNPct = (parseFloat(c.top_n_share) * 100).toFixed(1);

    html += `
      <p class="cp-note">
        Portfolio concentration is as concentrated as ${escapeHtml(effStr)} equal holdings across ${c.positions} valued position${c.positions === 1 ? "" : "s"}.
      </p>
      <ul class="cp-rows">
        <li class="cp-row">
          <span class="cp-row-title">Largest position</span>
          <span class="cp-row-value">${escapeHtml(c.largest_symbol)} (${largestPct}%)</span>
        </li>
        <li class="cp-row">
          <span class="cp-row-title">Top 5 holdings</span>
          <span class="cp-row-value">${topNPct}% of portfolio</span>
        </li>
        <li class="cp-row">
          <span class="cp-row-title">Effective positions</span>
          <span class="cp-row-value">${escapeHtml(effStr)} equal holdings</span>
        </li>
        <li class="cp-row">
          <span class="cp-row-title">HHI index</span>
          <span class="cp-row-value">${escapeHtml(c.hhi)}</span>
        </li>
      </ul>
    `;
  }

  // Allocation by position
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

    html += `
      <div class="cp-alloc-section">
        <h3 class="cp-section-subhead">By position (${ins.allocation.length})</h3>
        <div class="cp-alloc-bars">${rows}</div>
      </div>
    `;
  }

  // By instrument kind
  if (ins.by_kind && ins.by_kind.length > 0) {
    const kindRows = ins.by_kind.map(k => {
      const shareVal = parseFloat(k.share) || 0;
      const pct = (shareVal * 100).toFixed(1);
      const kindLabel = k.kind.charAt(0).toUpperCase() + k.kind.slice(1);
      return `
        <li class="cp-row">
          <span class="cp-row-title">${escapeHtml(kindLabel)} · ${k.positions} pos</span>
          <span class="cp-row-value">${formatMoney(k.value)} (${pct}%)</span>
        </li>
      `;
    }).join("");

    html += `
      <h3 class="cp-section-subhead">By instrument kind</h3>
      <ul class="cp-rows">${kindRows}</ul>
    `;
  }

  if (ins.unvalued && ins.unvalued.length > 0) {
    html += `<p class="cp-note">Positions without broker market value (excluded from allocation): ${ins.unvalued.map(escapeHtml).join(", ")}</p>`;
  }

  body.innerHTML = html || `<p class="cp-empty">No valued positions to show allocation.</p>`;
}

function renderIncomeSummary(ins) {
  const body = document.getElementById("cpInsightsBody");
  const freshnessSpan = document.getElementById("cpInsightsFreshness");
  if (!body) return;

  if (freshnessSpan) {
    freshnessSpan.innerHTML = formatInsightsFreshness(ins.freshness);
  }

  const signClass = (money) =>
    money && String(money.amount).startsWith("-") ? "cp-word-bad" : "cp-word-good";
  let html = "";

  // Return since the first deposit
  if (ins.total_return) {
    const tr = ins.total_return;
    const ratio = tr.gain_ratio !== null && tr.gain_ratio !== undefined
      ? ` (${(parseFloat(tr.gain_ratio) * 100).toFixed(2)}%)`
      : "";
    html += `
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

  // Income & Distributions
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
      monthRows = `<p class="cp-empty">No income recorded in the last 24 months.</p>`;
    }

    html += `
      <ul class="cp-rows">
        <li class="cp-row">
          <span class="cp-row-title">Trailing 12M net</span>
          <span class="cp-row-value">${trailing12m}</span>
        </li>
        <li class="cp-row">
          <span class="cp-row-title">Trailing yield</span>
          <span class="cp-row-value">${yieldStr}</span>
        </li>
      </ul>
      <div class="cp-insights-block">
        <div class="cp-income-months">${monthRows}</div>
      </div>
    `;
  }

  // Warnings
  if (ins.warnings && ins.warnings.length > 0) {
    html += ins.warnings.map(w => `<p class="cp-note">${escapeHtml(w)}</p>`).join("");
  }

  body.innerHTML = html || `<p class="cp-empty">Import a report to see income details.</p>`;
}

function formatProjMoney(val, currency = "") {
  if (val === null || val === undefined || isNaN(val)) return "—";
  const curr = currency || (currentAccount && accountList.find(a => a.account === currentAccount)?.currency) || "";
  const num = Number(val);
  const formatted = groupDecimal(num.toFixed(2));
  return curr ? `${formatted} ${curr}` : formatted;
}

function projCurrency() {
  return (currentAccount && accountList.find(a => a.account === currentAccount)?.currency) || "€";
}

function formatProjAllowance(value) {
  return `${groupDecimal(Number(value).toFixed(0))} ${projCurrency()}`;
}

function updateProjTopNVisibility() {
  const group = document.getElementById("cpProjTopNGroup");
  if (!group) return;
  group.hidden = !(currentProjAllocation === "top_value" || currentProjAllocation === "top_yield");
}

function setupDividendProjectionControls() {
  const reinvestInput = document.getElementById("cpProjReinvest");
  const reinvestVal = document.getElementById("cpProjReinvestVal");
  if (reinvestInput) {
    reinvestInput.addEventListener("input", (e) => {
      currentProjReinvestPct = parseFloat(e.target.value);
      if (isNaN(currentProjReinvestPct)) currentProjReinvestPct = 100;
      if (reinvestVal) reinvestVal.textContent = `${currentProjReinvestPct}%`;
      if (projDebounceTimer) clearTimeout(projDebounceTimer);
      projDebounceTimer = setTimeout(() => {
        loadDividendProjection(true);
      }, 250);
    });
  }

  const contribInput = document.getElementById("cpProjContrib");
  const contribVal = document.getElementById("cpProjContribVal");
  if (contribInput) {
    contribInput.addEventListener("input", (e) => {
      currentProjMonthlyContribution = parseFloat(e.target.value);
      if (isNaN(currentProjMonthlyContribution)) currentProjMonthlyContribution = 0;
      if (contribVal) contribVal.textContent = formatProjAllowance(currentProjMonthlyContribution);
      if (projDebounceTimer) clearTimeout(projDebounceTimer);
      projDebounceTimer = setTimeout(() => {
        loadDividendProjection(true);
      }, 250);
    });
    contribVal.textContent = formatProjAllowance(currentProjMonthlyContribution);
  }

  const allocBtns = document.querySelectorAll(".cp-proj-alloc-pills .cp-horizon-btn");
  allocBtns.forEach((btn) => {
    btn.addEventListener("click", () => {
      allocBtns.forEach((b) => {
        b.classList.remove("cp-active");
        b.setAttribute("aria-checked", "false");
      });
      btn.classList.add("cp-active");
      btn.setAttribute("aria-checked", "true");
      currentProjAllocation = btn.dataset.allocation || "proportional";
      updateProjTopNVisibility();
      renderAllocHelp();
      markCompareSelection();
      loadDividendProjection();
    });
  });

  const topNInput = document.getElementById("cpProjTopN");
  const topNVal = document.getElementById("cpProjTopNVal");
  if (topNInput) {
    topNInput.addEventListener("input", (e) => {
      currentProjTopN = parseInt(e.target.value, 10) || 5;
      if (topNVal) topNVal.textContent = currentProjTopN;
      if (projDebounceTimer) clearTimeout(projDebounceTimer);
      projDebounceTimer = setTimeout(() => {
        loadDividendProjection(true);
      }, 250);
    });
  }
  updateProjTopNVisibility();
  renderAllocHelp();

  const reserveTaxCheckbox = document.getElementById("cpProjReserveTax");
  if (reserveTaxCheckbox) {
    reserveTaxCheckbox.addEventListener("change", (e) => {
      currentProjReservePtTax = e.target.checked;
      loadDividendProjection(true);
    });
  }

  const refreshBtn = document.getElementById("cpProjRefreshBtn");
  if (refreshBtn) {
    refreshBtn.addEventListener("click", async () => {
      refreshBtn.disabled = true;
      const origText = refreshBtn.textContent;
      refreshBtn.textContent = "Updating dividend history…";
      try {
        const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
        const res = await fetchJson(`/api/capital/dividends/refresh${query}`, { method: "POST" });
        if (res && res.failed && res.failed.length > 0) {
          showBanner(`Dividend histories refreshed (${res.fetched || 0} fetched, ${res.failed.length} failed: ${res.failed.join(", ")})`, true);
        } else {
          showBanner(`Dividend histories refreshed (${(res && res.fetched) || 0} fetched).`);
        }
        await loadDividendProjection(true);
      } catch (err) {
        showBanner(`Dividend history refresh failed: ${err.message}`, true);
      } finally {
        refreshBtn.disabled = false;
        refreshBtn.textContent = origText;
      }
    });
  }

  const toggleYearsBtn = document.getElementById("cpProjToggleYearsBtn");
  if (toggleYearsBtn) {
    toggleYearsBtn.addEventListener("click", () => {
      showAllProjYears = !showAllProjYears;
      if (currentProjectionData) {
        renderProjYearsTable(currentProjectionData);
      }
    });
  }
}

function projectionUrl(allocation) {
  const queryAcc = currentAccount ? `account=${encodeURIComponent(currentAccount)}&` : "";
  return `/api/capital/dividends/projection?${queryAcc}years=${PROJ_YEARS}&reinvest_pct=${currentProjReinvestPct}&allocation=${allocation}&monthly_contribution=${currentProjMonthlyContribution}&top_n=${currentProjTopN}&reserve_pt_tax=${currentProjReservePtTax}`;
}

// `compare` also re-runs the side-by-side table (the shared settings changed).
async function loadDividendProjection(compare = false) {
  if (compare || !lastCompare) loadProjectionCompare();
  const statusEl = document.getElementById("cpProjStatus");
  const chartWrap = document.getElementById("cpProjChartWrap");
  if (!chartWrap) return;

  const url = projectionUrl(currentProjAllocation);

  try {
    const data = await fetchJson(url);
    if (statusEl) statusEl.hidden = true;
    currentProjectionData = data;
    renderDividendProjection(data);
    if (!compare && typeof CapitalRetirement !== "undefined") {
      CapitalRetirement.reloadFromControls();
    }
  } catch (err) {
    if (statusEl) {
      statusEl.hidden = false;
      statusEl.textContent = `Dividend projection unavailable: ${err.message}`;
    }
  }
}

function renderDividendProjection(data) {
  if (!data) return;
  renderProjHeadlines(data);
  renderProjChart(data);
  renderProjMonths(data);
  renderProjYearsTable(data);
  renderProjHoldingsTable(data);
  renderProjAssumptions(data);
}

function renderProjHeadlines(data) {
  const scenarios = data.scenarios || [];
  const base = scenarios.find(s => s.scenario === "base") || scenarios[0] || { years: [] };
  const low = scenarios.find(s => s.scenario === "low");
  const high = scenarios.find(s => s.scenario === "high");

  const baseYears = base.years || [];
  const y1 = baseYears[0];
  const yEnd = baseYears[baseYears.length - 1];

  const y1NetEl = document.getElementById("cpProjTileYear1Net");
  if (y1NetEl) {
    y1NetEl.textContent = y1 ? formatProjMoney(y1.net) : "—";
  }

  const finalNetLabel = document.getElementById("cpProjTileFinalNetLabel");
  const finalNetVal = document.getElementById("cpProjTileFinalNet");
  const finalNetRange = document.getElementById("cpProjTileFinalNetRange");
  if (finalNetLabel && yEnd) {
    finalNetLabel.textContent = `Year ${yEnd.year} Net Income`;
  }
  if (finalNetVal) {
    finalNetVal.textContent = yEnd ? formatProjMoney(yEnd.net) : "—";
  }
  if (finalNetRange) {
    const lowEnd = low?.years?.[low.years.length - 1];
    const highEnd = high?.years?.[high.years.length - 1];
    if (lowEnd && highEnd) {
      finalNetRange.textContent = `Low ${formatProjMoney(lowEnd.net)} · High ${formatProjMoney(highEnd.net)}`;
    } else {
      finalNetRange.textContent = "Base scenario";
    }
  }

  const totalReinvestedEl = document.getElementById("cpProjTileTotalReinvested");
  if (totalReinvestedEl) {
    const totalInvested = baseYears.reduce(
      (sum, y) => sum + (y.reinvested || 0) + (y.contributed || 0),
      0,
    );
    totalReinvestedEl.textContent = formatProjMoney(totalInvested);
  }

  const doublingEl = document.getElementById("cpProjTileDoubling");
  const doublingSub = document.getElementById("cpProjTileDoublingSub");
  if (doublingEl) {
    if (base.doubling_year !== null && base.doubling_year !== undefined) {
      doublingEl.textContent = `Doubles in Year ${base.doubling_year}`;
      if (doublingSub) doublingSub.textContent = `Net income reaches 2× Year 1`;
    } else {
      doublingEl.textContent = `Doesn't double`;
      if (doublingSub) doublingSub.textContent = `Within ${data.settings?.years || PROJ_YEARS} years`;
    }
  }
}

function renderProjChart(data) {
  const wrap = document.getElementById("cpProjChartWrap");
  if (!wrap) return;

  const scenarios = data.scenarios || [];
  const base = scenarios.find(s => s.scenario === "base") || scenarios[0] || { years: [] };
  const low = scenarios.find(s => s.scenario === "low") || { years: [] };
  const high = scenarios.find(s => s.scenario === "high") || { years: [] };
  const without = data.without_reinvestment || { years: [] };

  const baseYears = base.years || [];
  const lowYears = low.years || [];
  const highYears = high.years || [];
  const withoutYears = without.years || [];

  const N = baseYears.length;
  if (N === 0) {
    wrap.innerHTML = `<p class="cp-empty" id="cpProjChartEmpty">No projected dividend data available.</p>`;
    return;
  }

  const W = 860;
  const H = 380;
  const ml = 70;
  const mr = 75;
  const mt = 50;
  const mb = 45;
  const pw = W - ml - mr;
  const ph = H - mt - mb;
  const xLeft = ml;
  const xRight = ml + pw;
  const yBottom = mt + ph;

  // Compute Scales
  const allIncomes = [
    ...baseYears.map(y => y.net || 0),
    ...lowYears.map(y => y.net || 0),
    ...highYears.map(y => y.net || 0),
    ...withoutYears.map(y => y.net || 0),
  ];
  const maxIncome = Math.max(1, ...allIncomes) * 1.15;
  function yInc(val) {
    return yBottom - (Math.max(0, val) / maxIncome) * ph;
  }

  const allValues = baseYears.map(y => y.value || 0);
  const maxValue = Math.max(1, ...allValues) * 1.15;
  function yVal(val) {
    return yBottom - (Math.max(0, val) / maxValue) * ph;
  }

  const colW = pw / N;
  const barW = Math.max(4, Math.min(26, colW * 0.55));
  function xCenter(i) {
    return ml + (i + 0.5) * colW;
  }

  // Grid lines & Axis values (5 steps)
  const gridLines = [];
  const curr = (currentAccount && accountList.find(a => a.account === currentAccount)?.currency) || "€";
  for (let k = 0; k <= 4; k++) {
    const frac = k / 4;
    const yLine = yBottom - frac * ph;
    const incVal = frac * maxIncome;
    const portVal = frac * maxValue;
    gridLines.push(`
      <line x1="${xLeft}" y1="${yLine.toFixed(1)}" x2="${xRight}" y2="${yLine.toFixed(1)}" stroke="rgba(255,255,255,0.06)" stroke-width="1" />
      <text x="${(xLeft - 8).toFixed(1)}" y="${(yLine + 3).toFixed(1)}" text-anchor="end" class="cp-proj-axis-label">${groupDecimal(Math.round(incVal))} ${curr}</text>
      <text x="${(xRight + 8).toFixed(1)}" y="${(yLine + 3).toFixed(1)}" text-anchor="start" class="cp-proj-axis-label" fill="#ffd69f">${groupDecimal(Math.round(portVal))} ${curr}</text>
    `);
  }

  // Shaded low-high band
  let bandHtml = "";
  if (lowYears.length === N && highYears.length === N) {
    if (N === 1) {
      const x0 = xCenter(0);
      const yH = yInc(highYears[0]?.net || 0);
      const yL = yInc(lowYears[0]?.net || 0);
      bandHtml = `<rect x="${(x0 - barW).toFixed(1)}" y="${yH.toFixed(1)}" width="${(barW * 2).toFixed(1)}" height="${Math.max(1, yL - yH).toFixed(1)}" fill="rgba(78, 145, 252, 0.16)" stroke="none" />`;
    } else {
      const topHigh = highYears.map((h, i) => `${xCenter(i).toFixed(1)} ${yInc(h.net || 0).toFixed(1)}`);
      const botLow = [...lowYears].reverse().map((l, j) => {
        const idx = N - 1 - j;
        return `${xCenter(idx).toFixed(1)} ${yInc(l.net || 0).toFixed(1)}`;
      });
      const bandPath = `M ${topHigh.join(" L ")} L ${botLow.join(" L ")} Z`;
      bandHtml = `<path d="${bandPath}" fill="rgba(78, 145, 252, 0.16)" stroke="none" />`;
    }
  }

  // Base scenario bars
  const barsHtml = baseYears.map((b, i) => {
    const bx = xCenter(i) - barW / 2;
    const by = yInc(b.net || 0);
    const bh = Math.max(0, yBottom - by);
    return `<rect x="${bx.toFixed(1)}" y="${by.toFixed(1)}" width="${barW.toFixed(1)}" height="${bh.toFixed(1)}" rx="3" fill="#4e91fc" opacity="0.85" />`;
  }).join("");

  // Dashed line: without reinvestment
  let withoutLineHtml = "";
  if (withoutYears.length > 0) {
    if (withoutYears.length === 1) {
      const x0 = xCenter(0);
      const y0 = yInc(withoutYears[0].net || 0);
      withoutLineHtml = `<circle cx="${x0.toFixed(1)}" cy="${y0.toFixed(1)}" r="4" fill="#e17055" />`;
    } else {
      const pts = withoutYears.map((w, i) => `${xCenter(i).toFixed(1)} ${yInc(w.net || 0).toFixed(1)}`);
      withoutLineHtml = `<path d="M ${pts.join(" L ")}" fill="none" stroke="#e17055" stroke-width="2" stroke-dasharray="4 4" />`;
    }
  }

  // Secondary line (right axis): Value of paying holdings
  let valueLineHtml = "";
  if (baseYears.length === 1) {
    const x0 = xCenter(0);
    const y0 = yVal(baseYears[0].value || 0);
    valueLineHtml = `<circle cx="${x0.toFixed(1)}" cy="${y0.toFixed(1)}" r="4" fill="#ffd69f" />`;
  } else {
    const pts = baseYears.map((b, i) => `${xCenter(i).toFixed(1)} ${yVal(b.value || 0).toFixed(1)}`);
    const dots = baseYears.map((b, i) => `<circle cx="${xCenter(i).toFixed(1)}" cy="${yVal(b.value || 0).toFixed(1)}" r="3" fill="#ffd69f" />`).join("");
    valueLineHtml = `<path d="M ${pts.join(" L ")}" fill="none" stroke="#ffd69f" stroke-width="2.5" />${dots}`;
  }

  // X-axis Year labels
  const xLabels = [];
  const stride = N > 15 ? 5 : 1;
  for (let i = 0; i < N; i++) {
    const yr = baseYears[i].year;
    if (stride === 1 || yr === 1 || yr === N || yr % stride === 0) {
      xLabels.push(`
        <text x="${xCenter(i).toFixed(1)}" y="${(yBottom + 18).toFixed(1)}" text-anchor="middle" class="cp-proj-axis-label">Y${yr}</text>
      `);
    }
  }

  // Accessible summary
  const y1Net = formatProjMoney(baseYears[0]?.net);
  const yEndNet = formatProjMoney(baseYears[N - 1]?.net);
  const withoutEndNet = formatProjMoney(withoutYears[N - 1]?.net);
  const yEndVal = formatProjMoney(baseYears[N - 1]?.value);
  const allowance = data.settings?.monthly_contribution || 0;
  const allowanceDesc = allowance > 0
    ? ` A salary allowance of ${formatProjMoney(allowance)} a month is invested on top of dividends.`
    : "";

  const svg = `
    <svg viewBox="0 0 ${W} ${H}" role="img" aria-labelledby="cpProjSvgTitle cpProjSvgDesc" preserveAspectRatio="xMidYMid meet">
      <title id="cpProjSvgTitle">Projected Dividend Income and Portfolio Value over ${N} Years</title>
      <desc id="cpProjSvgDesc">Projection from Year 1 net income of ${y1Net} to Year ${N} net income of ${yEndNet} with ${data.settings?.reinvest_pct ?? currentProjReinvestPct}% reinvestment.${allowanceDesc} Without reinvesting dividends, Year ${N} net income would be ${withoutEndNet}. Paying portfolio value reaches ${yEndVal}.</desc>

      <!-- Legend -->
      <g class="cp-chart-legend" transform="translate(${ml}, 20)">
        <rect x="0" y="-8" width="12" height="12" rx="2" fill="#4e91fc" opacity="0.85" />
        <text x="18" y="-1" class="cp-proj-legend-text">Base Net Income</text>

        <rect x="135" y="-8" width="14" height="12" rx="2" fill="rgba(78, 145, 252, 0.25)" stroke="rgba(78, 145, 252, 0.5)" stroke-width="1" />
        <text x="155" y="-1" class="cp-proj-legend-text">Low–High Scenarios</text>

        <line x1="285" y1="-2" x2="305" y2="-2" stroke="#e17055" stroke-width="2" stroke-dasharray="4 4" />
        <text x="312" y="-1" class="cp-proj-legend-text">Without Reinvesting Dividends</text>

        <line x1="500" y1="-2" x2="520" y2="-2" stroke="#ffd69f" stroke-width="2.5" />
        <circle cx="510" cy="-2" r="3" fill="#ffd69f" />
        <text x="528" y="-1" class="cp-proj-legend-text" fill="#ffd69f">Portfolio Value (Right Axis)</text>
      </g>

      <!-- Grid lines -->
      ${gridLines.join("")}

      <!-- Low-High shaded band -->
      ${bandHtml}

      <!-- Base bars -->
      ${barsHtml}

      <!-- Without reinvestment dashed line -->
      ${withoutLineHtml}

      <!-- Portfolio value line -->
      ${valueLineHtml}

      <!-- X-axis labels -->
      ${xLabels.join("")}
    </svg>
  `;

  wrap.innerHTML = svg;
}

function renderProjYearsTable(data) {
  const tbody = document.getElementById("cpProjYearsBody");
  const toggleBtn = document.getElementById("cpProjToggleYearsBtn");
  if (!tbody) return;

  const base = (data.scenarios || []).find(s => s.scenario === "base") || { years: [] };
  const years = base.years || [];

  if (years.length === 0) {
    tbody.innerHTML = `<tr><td colspan="10" class="cp-empty">No projection rows available.</td></tr>`;
    if (toggleBtn) toggleBtn.hidden = true;
    return;
  }

  if (toggleBtn) {
    if (years.length > 5) {
      toggleBtn.hidden = false;
      toggleBtn.textContent = showAllProjYears ? "Show first 5 years" : `Show all ${years.length} years`;
      toggleBtn.setAttribute("aria-expanded", String(showAllProjYears));
    } else {
      toggleBtn.hidden = true;
    }
  }

  const rowsToShow = showAllProjYears ? years : years.slice(0, 5);
  tbody.innerHTML = rowsToShow.map(y => {
    const yieldCostStr = (y.yield_on_cost !== null && y.yield_on_cost !== undefined)
      ? `${(y.yield_on_cost * 100).toFixed(2)}%`
      : "—";
    return `
      <tr>
        <td><strong>Year ${y.year}</strong></td>
        <td class="num">${formatProjMoney(y.gross)}</td>
        <td class="num">${formatProjMoney(y.withheld)}</td>
        <td class="num">${formatProjMoney(y.pt_tax)}</td>
        <td class="num"><strong>${formatProjMoney(y.net)}</strong></td>
        <td class="num">${formatProjMoney(y.reinvested)}</td>
        <td class="num">${formatProjMoney(y.contributed)}</td>
        <td class="num">${formatProjMoney(y.cash)}</td>
        <td class="num">${formatProjMoney(y.value)}</td>
        <td class="num">${yieldCostStr}</td>
      </tr>
    `;
  }).join("");
}

function renderProjHoldingsTable(data) {
  const tbody = document.getElementById("cpProjHoldingsBody");
  const nonPayingBox = document.getElementById("cpProjNonPaying");
  const nonPayingList = document.getElementById("cpProjNonPayingList");
  if (!tbody) return;

  const holdings = data.holdings || [];
  if (holdings.length === 0) {
    tbody.innerHTML = `<tr><td colspan="7" class="cp-empty">No paying holdings in portfolio.</td></tr>`;
  } else {
    tbody.innerHTML = holdings.map(h => {
      const growth = h.growth || {};
      const gPct = (growth.g !== undefined) ? `${growth.g >= 0 ? "+" : ""}${(growth.g * 100).toFixed(1)}%` : "0.0%";
      const rawPct = (growth.raw !== null && growth.raw !== undefined) ? `${growth.raw >= 0 ? "+" : ""}${(growth.raw * 100).toFixed(1)}%` : "n/a";
      const yearsCount = growth.annual ? Math.max(0, growth.annual.length - 1) : 0;
      const growthSub = `<span class="cp-proj-growth-raw">raw: ${rawPct} (${yearsCount}y)</span>`;

      let withRate = 0;
      let withSrc = "default";
      if (Array.isArray(h.withholding)) {
        withRate = h.withholding[0] || 0;
        withSrc = h.withholding[1] || "default";
      } else if (typeof h.withholding === "number") {
        withRate = h.withholding;
      }
      const withStr = `${(withRate * 100).toFixed(1)}% <span class="cp-sub-notice">(${escapeHtml(withSrc)})</span>`;

      return `
        <tr>
          <td><strong>${escapeHtml(h.symbol)}</strong></td>
          <td class="num" title="Per share, in the listing's own currency">${Number(growth.ttm_per_share || 0).toLocaleString(undefined, { maximumFractionDigits: 4 })}</td>
          <td class="num">${gPct} ${growthSub}</td>
          <td>${withStr}</td>
          <td class="num">${formatProjMoney(h.first_year_net)}</td>
          <td class="num"><strong>${formatProjMoney(h.last_year_net)}</strong></td>
          <td class="num">${groupDecimal(Number(h.end_units || 0).toFixed(2))}</td>
        </tr>
      `;
    }).join("");
  }

  const nonPaying = data.non_paying || [];
  if (nonPayingBox && nonPayingList) {
    if (nonPaying.length > 0) {
      nonPayingBox.hidden = false;
      nonPayingList.textContent = `${nonPaying.join(", ")}: doesn't pay (accumulating or no history)`;
    } else {
      nonPayingBox.hidden = true;
    }
  }
}

function renderProjAssumptions(data) {
  const list = document.getElementById("cpProjNotesList");
  if (!list) return;

  const defaultNotes = [
    "Dividend growth estimated from up to 5 complete calendar years, shrunk toward zero by n/(n+3) and capped at ±15%.",
    "Share prices are assumed to grow at each holding's dividend growth rate, holding yields constant.",
    "Exchange rates between instrument trading currencies and account currency are held constant at current spot rates.",
    "Low and High scenarios represent ±1 standard deviation of historical annual payout changes.",
    "Withholding tax reflects observed ledger rates over the past 24 months, else standard country defaults.",
    "Portuguese tax reserve applies a 28% flat rate offset by foreign tax withholding (simplified foreign tax credit).",
    "A monthly allowance, when set, is invested each month after that month's dividends and is not taxed as a dividend."
  ];

  const notes = (data.notes && data.notes.length > 0) ? data.notes : defaultNotes;
  list.innerHTML = notes.map(n => `<li>${escapeHtml(n)}</li>`).join("");
}

registerTab("income", {
  setup: () => {
    setupDividendProjectionControls();
    if (typeof CapitalRetirement !== "undefined") CapitalRetirement.setup();
  },
  load: () =>
    Promise.all([
      loadDividendProjection(true),
      loadInsights(),
      typeof CapitalRetirement !== "undefined" ? CapitalRetirement.load() : Promise.resolve(),
    ]),
});

// --- Choosing how to reinvest ---------------------------------------------------------

const ALLOC_INFO = {
  proportional: {
    label: "Proportional to value",
    help: "Spreads each dividend across your paying holdings by their current value, so your mix stays as it is.",
    tradeoff: "Keeps your current mix. The default.",
  },
  same_holding: {
    label: "Back into the payer (DRIP)",
    help: "Each dividend buys more of the holding that paid it, like a broker's DRIP; the monthly allowance is spread by current value.",
    tradeoff: "Simple; high payers grow their weight over time.",
  },
  top_value: {
    label: "Largest holdings",
    help: "Everything goes into your largest holdings by value.",
    tradeoff: "Concentrates further in what is already biggest.",
  },
  equal_weight: {
    label: "Toward equal sizes",
    help: "Spreads dividends so paying holdings drift toward equal sizes.",
    tradeoff: "Diversifies; small holdings grow fastest.",
  },
  top_yield: {
    label: "Highest yields",
    help: "Everything goes into your highest-yielding holdings.",
    tradeoff: "Most income on paper, but very high yields are the most likely to be cut.",
  },
  underweight: {
    label: "Smallest positions first",
    help: "Tops up the paying holdings that are smallest relative to an equal split.",
    tradeoff: "Rebalances without selling anything.",
  },
};

let lastCompare = null;
let compareSeq = 0;

function renderAllocHelp() {
  const el = document.getElementById("cpProjAllocHelp");
  if (el) el.textContent = ALLOC_INFO[currentProjAllocation]?.help || "";
}

function monthlyAt(years, year) {
  const row = (years || []).find((y) => y.year === year);
  return row ? row.net / 12 : null;
}

async function loadProjectionCompare() {
  const body = document.getElementById("cpProjCompareBody");
  if (!body) return;
  const seq = ++compareSeq;
  const keys = Object.keys(ALLOC_INFO);
  const results = await Promise.allSettled(keys.map((k) => fetchJson(projectionUrl(k))));
  if (seq !== compareSeq) return;
  const rows = [];
  let without = null;
  results.forEach((r, i) => {
    if (r.status !== "fulfilled") return;
    const base = (r.value.scenarios || []).find((s) => s.scenario === "base");
    if (!base) return;
    without = without || r.value.without_reinvestment;
    rows.push({
      key: keys[i],
      y1: monthlyAt(base.years, 1),
      y10: monthlyAt(base.years, 10),
      y30: monthlyAt(base.years, PROJ_YEARS),
    });
  });
  lastCompare = { rows, without };
  renderProjectionCompare();
}

function renderProjectionCompare() {
  const body = document.getElementById("cpProjCompareBody");
  const note = document.getElementById("cpProjCompareNote");
  if (!body || !lastCompare) return;
  const { rows, without } = lastCompare;
  if (rows.length === 0) {
    body.innerHTML = `<tr><td colspan="5" class="cp-empty">No paying holdings to compare yet.</td></tr>`;
    if (note) note.textContent = "";
    return;
  }
  const best = rows.reduce((a, b) => ((b.y30 ?? -1) > (a.y30 ?? -1) ? b : a));
  body.innerHTML = rows.map((r) => {
    const info = ALLOC_INFO[r.key];
    const tags = [];
    if (r.key === "proportional") tags.push(`<span class="cp-chg" data-dir="flat">Default</span>`);
    if (r.key === best.key) tags.push(`<span class="cp-chg" data-dir="up">Most income</span>`);
    return `<tr class="cp-proj-choice" data-allocation="${r.key}" tabindex="0" aria-label="Use ${escapeHtml(info.label)}">
        <td><strong>${escapeHtml(info.label)}</strong> ${tags.join(" ")}</td>
        <td class="num">${formatProjMoney(r.y1)}</td>
        <td class="num">${formatProjMoney(r.y10)}</td>
        <td class="num"><strong>${formatProjMoney(r.y30)}</strong></td>
        <td class="cp-muted">${escapeHtml(info.tradeoff)}</td>
      </tr>`;
  }).join("");
  if (without) {
    body.innerHTML += `<tr class="cp-proj-choice-ref">
        <td><strong>Don't reinvest</strong></td>
        <td class="num">${formatProjMoney(monthlyAt(without.years, 1))}</td>
        <td class="num">${formatProjMoney(monthlyAt(without.years, 10))}</td>
        <td class="num"><strong>${formatProjMoney(monthlyAt(without.years, PROJ_YEARS))}</strong></td>
        <td class="cp-muted">Take every dividend as cash, for comparison.</td>
      </tr>`;
  }
  body.querySelectorAll(".cp-proj-choice").forEach((tr) => {
    const pick = () => {
      const btn = document.querySelector(`.cp-proj-alloc-pills [data-allocation="${tr.dataset.allocation}"]`);
      if (btn) btn.click();
    };
    tr.addEventListener("click", pick);
    tr.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        pick();
      }
    });
  });
  markCompareSelection();
  if (note) {
    const gap = best.key !== "proportional"
      ? rows.find((r) => r.key === "proportional")
      : null;
    note.textContent = gap && Number.isFinite(best.y30) && Number.isFinite(gap.y30)
      ? `${ALLOC_INFO[best.key].label} gives ${formatProjMoney(best.y30 - gap.y30)} more a month by year ${PROJ_YEARS} than keeping your mix. Weigh that against its trade-off; the difference rests on past payouts continuing.`
      : `Keeping your mix also gives the most income here.`;
  }
}

function markCompareSelection() {
  document.querySelectorAll(".cp-proj-choice").forEach((tr) => {
    const on = tr.dataset.allocation === currentProjAllocation;
    tr.classList.toggle("cp-proj-choice-on", on);
    tr.setAttribute("aria-current", on ? "true" : "false");
  });
}

// --- Month by month, every year ---------------------------------------------------------

const MONTH_LABELS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function compactMoney(value) {
  if (!Number.isFinite(value) || value <= 0) return "";
  if (value >= 10000) return `${(value / 1000).toFixed(0)}k`;
  if (value >= 1000) return `${(value / 1000).toFixed(1)}k`;
  return value >= 100 ? value.toFixed(0) : value.toFixed(value >= 10 ? 0 : 1);
}

function renderProjMonths(data) {
  const wrap = document.getElementById("cpProjMonths");
  if (!wrap) return;
  const base = (data.scenarios || []).find((s) => s.scenario === "base");
  const monthly = base?.monthly_net || [];
  const start = String(data.settings?.start || "");
  const [sy, sm] = start.split("-").map(Number);
  if (monthly.length === 0 || !sy || !sm) {
    wrap.innerHTML = `<p class="cp-empty">No expected payments yet.</p>`;
    return;
  }
  const byYear = new Map();
  monthly.forEach((value, i) => {
    const m0 = sm - 1 + i;
    const year = sy + Math.floor(m0 / 12);
    const month = m0 % 12;
    if (!byYear.has(year)) byYear.set(year, new Array(12).fill(null));
    byYear.get(year)[month] = value;
  });
  const max = Math.max(...monthly, 0) || 1;
  const head = `<tr><th scope="col">Year</th>${MONTH_LABELS.map((m) => `<th scope="col" class="num">${m}</th>`).join("")}<th scope="col" class="num">Total</th></tr>`;
  const rows = [...byYear.entries()].map(([year, months]) => {
    const total = months.reduce((a, v) => a + (v || 0), 0);
    const cells = months.map((v, m) => {
      if (v === null) return `<td class="cp-mcell" data-out></td>`;
      const heat = v > 0 ? Math.max(0.08, Math.sqrt(v / max)) : 0;
      return `<td class="cp-mcell num" style="--heat:${heat.toFixed(3)}" title="${MONTH_LABELS[m]} ${year}: ${formatProjMoney(v)}">${compactMoney(v)}</td>`;
    }).join("");
    return `<tr><th scope="row">${year}</th>${cells}<td class="num cp-mtotal">${formatProjMoney(total)}</td></tr>`;
  }).join("");
  wrap.innerHTML = `<table class="cp-table cp-months-table"><thead>${head}</thead><tbody>${rows}</tbody></table>`;
}

// --- Round 24: safety chips, specials note, income fan, and Maximize income panel -----

(function initR24Income() {
  let r24FanSeq = 0;
  let r24MaxPollTimer = null;
  let r24Candidates = [];
  let r24LimitsPrefilled = false;

  function r24Query() {
    return currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
  }

  function retireAssumptions() {
    const num = (id, fallback, min, max) => {
      const el = document.getElementById(id);
      const v = el ? Number(el.value) : NaN;
      return Number.isFinite(v) ? Math.min(max, Math.max(min, v)) : fallback;
    };
    const currentAge = Math.round(num("cpRetireCurrentAge", 35, 16, 95));
    const retireAge = Math.round(num("cpRetireRetireAge", 55, 16, 95));
    const targetMonthly = num("cpRetireTarget", 2500, 1, 100000);
    const inflation = num("cpRetireInflation", 2.0, 0, 10);
    const years = Math.max(1, Math.min(PROJ_YEARS, retireAge - currentAge));
    const nominalMonthly = targetMonthly * Math.pow(1 + inflation / 100, years);
    return { years, nominalMonthly };
  }

  function safetyLabel(grade) {
    const g = String(grade || "unknown").toLowerCase();
    return g === "unknown" ? "?" : g.toUpperCase();
  }

  function createSafetyChip(safety, symbol) {
    const g = String((safety && safety.grade) || "unknown").toLowerCase();
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "cp-safety-chip";
    btn.setAttribute("data-grade", g);
    btn.textContent = `Safety ${safetyLabel(g)}`;
    const reasons = Array.isArray(safety && safety.reasons) ? safety.reasons : [];
    const titleParts = [`Safety ${safetyLabel(g)}`];
    if (safety && typeof safety.fcf_cover === "number") {
      titleParts.push(`FCF cover ${safety.fcf_cover.toFixed(1)}×`);
    }
    if (safety && typeof safety.streak_years === "number" && safety.streak_years > 0) {
      titleParts.push(`${safety.streak_years}y streak`);
    }
    if (reasons.length > 0) titleParts.push(reasons.join(" "));
    btn.title = titleParts.join(" · ");
    btn.setAttribute("aria-label", `${symbol} dividend safety ${safetyLabel(g)}: show the reasons`);
    return btn;
  }

  // Enhance holdings table with safety grade chips and expandable reason rows.
  if (typeof renderProjHoldingsTable === "function") {
    const origHoldings = renderProjHoldingsTable;
    renderProjHoldingsTable = function (data) {
      origHoldings(data);
      const tbody = document.getElementById("cpProjHoldingsBody");
      const safetyMap = (data && data.safety && typeof data.safety === "object") ? data.safety : {};
      const holdings = (data && Array.isArray(data.holdings)) ? data.holdings : [];
      if (tbody && holdings.length > 0) {
        const rows = Array.from(tbody.querySelectorAll("tr"));
        rows.forEach((tr, idx) => {
          const h = holdings[idx];
          if (!h) return;
          const s = safetyMap[h.symbol];
          if (!s) return;
          const cell = tr.querySelector("td");
          if (!cell) return;
          const chip = createSafetyChip(s, h.symbol);
          chip.addEventListener("click", () => {
            const next = tr.nextElementSibling;
            if (next && next.classList.contains("cp-safety-detail-row")) {
              next.remove();
              chip.setAttribute("aria-expanded", "false");
              return;
            }
            const detailTr = document.createElement("tr");
            detailTr.className = "cp-safety-detail-row";
            const detailTd = document.createElement("td");
            detailTd.colSpan = 7;
            const parts = [];
            if (typeof s.fcf_cover === "number") parts.push(`FCF cover ${s.fcf_cover.toFixed(2)}×`);
            if (typeof s.earnings_cover === "number") parts.push(`Earnings cover ${s.earnings_cover.toFixed(2)}×`);
            if (typeof s.streak_years === "number") parts.push(`Streak ${s.streak_years}y`);
            const head = document.createElement("div");
            head.className = "cp-safety-detail-head";
            head.textContent = `${h.symbol} · Grade ${safetyLabel(s.grade)}${parts.length ? " · " + parts.join(" · ") : ""}`;
            detailTd.appendChild(head);
            const reasons = Array.isArray(s.reasons) ? s.reasons : [];
            if (reasons.length > 0) {
              const ul = document.createElement("ul");
              ul.className = "cp-safety-reasons";
              for (const r of reasons) {
                const li = document.createElement("li");
                li.textContent = r;
                ul.appendChild(li);
              }
              detailTd.appendChild(ul);
            }
            detailTr.appendChild(detailTd);
            tr.parentNode.insertBefore(detailTr, tr.nextSibling);
            chip.setAttribute("aria-expanded", "true");
          });
          cell.appendChild(document.createTextNode(" "));
          cell.appendChild(chip);
        });
      }

      // Specials note
      const nonPayingBox = document.getElementById("cpProjNonPaying");
      if (nonPayingBox && nonPayingBox.parentNode) {
        let specialsEl = document.getElementById("cpProjSpecialsNote");
        if (!specialsEl) {
          specialsEl = document.createElement("p");
          specialsEl.id = "cpProjSpecialsNote";
          specialsEl.className = "cp-note cp-specials-note";
          nonPayingBox.parentNode.insertBefore(specialsEl, nonPayingBox.nextSibling);
        }
        const specials = (data && Array.isArray(data.specials)) ? data.specials : [];
        if (specials.length > 0) {
          specialsEl.hidden = false;
          const items = specials.map((sp) => `${sp.symbol} (${sp.ex_date}: ${Number(sp.per_share || 0).toFixed(2)})`);
          specialsEl.textContent = `Special dividends excluded from TTM and growth: ${items.join(", ")}.`;
        } else {
          specialsEl.hidden = true;
          specialsEl.textContent = "";
        }
      }
    };
  }

  // --- Income fan (Monte Carlo) -------------------------------------------------------
  function ensureFanWrap() {
    const chartWrap = document.getElementById("cpProjChartWrap");
    if (!chartWrap || !chartWrap.parentNode) return null;
    let wrap = document.getElementById("cpIncomeFanWrap");
    if (!wrap) {
      wrap = document.createElement("div");
      wrap.id = "cpIncomeFanWrap";
      wrap.className = "cp-income-fan-wrap";
      chartWrap.parentNode.insertBefore(wrap, chartWrap.nextSibling);
    }
    return wrap;
  }

  function renderIncomeFan(dist) {
    const wrap = ensureFanWrap();
    if (!wrap) return;
    wrap.innerHTML = "";

    const header = document.createElement("div");
    header.className = "cp-fan-header";
    const title = document.createElement("h3");
    title.className = "cp-fan-title";
    title.textContent = "Monte Carlo income fan (p10 / p50 / p90)";
    header.appendChild(title);

    if (dist && typeof dist.p_target === "number" && Array.isArray(dist.target)) {
      const badge = document.createElement("span");
      badge.className = "cp-fan-target-badge";
      badge.textContent = `P(income ≥ target at Year ${dist.target[0]}): ${Math.round(dist.p_target * 100)}%`;
      header.appendChild(badge);
    }
    wrap.appendChild(header);

    const years = (dist && Array.isArray(dist.years)) ? dist.years : [];
    if (years.length === 0) {
      const empty = document.createElement("p");
      empty.className = "cp-empty";
      empty.textContent = "No Monte Carlo distribution available.";
      wrap.appendChild(empty);
      return;
    }

    const W = 820;
    const H = 220;
    const ml = 68;
    const mr = 24;
    const mt = 20;
    const mb = 30;
    const pw = W - ml - mr;
    const ph = H - mt - mb;
    const yBottom = mt + ph;

    const targetAnnual = (dist.target && typeof dist.target[1] === "number") ? dist.target[1] : null;
    const maxY = Math.max(
      1,
      ...years.map((y) => (y.net && y.net.p90) || 0),
      targetAnnual || 0,
    ) * 1.1;

    const xPos = (i) => ml + (years.length === 1 ? pw / 2 : (i / (years.length - 1)) * pw);
    const yPos = (v) => yBottom - (Math.max(0, v) / maxY) * ph;

    const p90Pts = years.map((y, i) => `${xPos(i).toFixed(1)},${yPos(y.net?.p90 || 0).toFixed(1)}`);
    const p10Pts = years.map((y, i) => `${xPos(i).toFixed(1)},${yPos(y.net?.p10 || 0).toFixed(1)}`).reverse();
    const p50Pts = years.map((y, i) => `${xPos(i).toFixed(1)},${yPos(y.net?.p50 || 0).toFixed(1)}`);

    const svgNS = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(svgNS, "svg");
    svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
    svg.setAttribute("class", "cp-fan-svg");
    svg.setAttribute("role", "img");
    svg.setAttribute("aria-label", "Monte Carlo p10 to p90 net dividend income fan");

    for (let k = 0; k <= 3; k++) {
      const frac = k / 3;
      const yLine = yBottom - frac * ph;
      const line = document.createElementNS(svgNS, "line");
      line.setAttribute("x1", String(ml));
      line.setAttribute("y1", yLine.toFixed(1));
      line.setAttribute("x2", String(W - mr));
      line.setAttribute("y2", yLine.toFixed(1));
      line.setAttribute("stroke", "rgba(255,255,255,0.07)");
      svg.appendChild(line);

      const lbl = document.createElementNS(svgNS, "text");
      lbl.setAttribute("x", String(ml - 8));
      lbl.setAttribute("y", (yLine + 3).toFixed(1));
      lbl.setAttribute("text-anchor", "end");
      lbl.setAttribute("class", "cp-proj-axis-label");
      lbl.textContent = `${groupDecimal(Math.round(frac * maxY))}`;
      svg.appendChild(lbl);
    }

    const poly = document.createElementNS(svgNS, "polygon");
    poly.setAttribute("points", p90Pts.concat(p10Pts).join(" "));
    poly.setAttribute("fill", "rgba(78, 145, 252, 0.22)");
    svg.appendChild(poly);

    const med = document.createElementNS(svgNS, "polyline");
    med.setAttribute("points", p50Pts.join(" "));
    med.setAttribute("fill", "none");
    med.setAttribute("stroke", "#7cb7ff");
    med.setAttribute("stroke-width", "2");
    svg.appendChild(med);

    if (targetAnnual !== null && targetAnnual > 0) {
      const ty = yPos(targetAnnual);
      const tLine = document.createElementNS(svgNS, "line");
      tLine.setAttribute("x1", String(ml));
      tLine.setAttribute("y1", ty.toFixed(1));
      tLine.setAttribute("x2", String(W - mr));
      tLine.setAttribute("y2", ty.toFixed(1));
      tLine.setAttribute("stroke", "#ffd69f");
      tLine.setAttribute("stroke-dasharray", "5 4");
      tLine.setAttribute("stroke-width", "1.5");
      svg.appendChild(tLine);

      const tLbl = document.createElementNS(svgNS, "text");
      tLbl.setAttribute("x", String(W - mr));
      tLbl.setAttribute("y", (ty - 5).toFixed(1));
      tLbl.setAttribute("text-anchor", "end");
      tLbl.setAttribute("class", "cp-fan-target-lbl");
      tLbl.textContent = `Target: ${groupDecimal(Math.round(targetAnnual))}/yr`;
      svg.appendChild(tLbl);
    }

    wrap.appendChild(svg);

    const last = years[years.length - 1];
    if (last && last.net) {
      const foot = document.createElement("p");
      foot.className = "cp-note";
      foot.textContent = `Year ${last.year}: p10 ${formatProjMoney(last.net.p10)} · p50 ${formatProjMoney(last.net.p50)} · p90 ${formatProjMoney(last.net.p90)} (${dist.paths || 2000} paths).`;
      wrap.appendChild(foot);
    }
  }

  async function loadIncomeFan() {
    const wrap = ensureFanWrap();
    if (!wrap) return;
    const seq = ++r24FanSeq;
    wrap.innerHTML = `<div class="cp-fan-header"><h3 class="cp-fan-title">Monte Carlo income fan</h3><span class="cp-muted">Simulating…</span></div>`;

    const ret = retireAssumptions();
    const body = {
      years: PROJ_YEARS,
      reinvest_pct: currentProjReinvestPct,
      allocation: currentProjAllocation,
      monthly_contribution: currentProjMonthlyContribution,
      top_n: currentProjTopN,
      reserve_pt_tax: currentProjReservePtTax,
      paths: 2000,
      target: [ret.years, ret.nominalMonthly],
    };

    try {
      const dist = await fetchJson(`/api/capital/dividends/distribution${r24Query()}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (seq !== r24FanSeq) return;
      renderIncomeFan(dist);
    } catch (err) {
      if (seq !== r24FanSeq) return;
      wrap.innerHTML = "";
      const p = document.createElement("p");
      p.className = "cp-empty";
      p.textContent = `Income distribution unavailable: ${err.message}`;
      wrap.appendChild(p);
    }
  }

  // --- Maximize income panel ----------------------------------------------------------
  // Round 25: one run compares all three goals on the same simulated futures.
  const MAX_GOALS = [
    { kind: "robust_income", label: "Most income in a bad year" },
    { kind: "income_with_wealth_floor", label: "Most typical income, keeping wealth" },
    { kind: "smooth_months", label: "Steadiest monthly income" },
  ];

  function ensureMaximizePanel() {
    const projPanel = document.getElementById("cpProjectionPanel");
    if (!projPanel || !projPanel.parentNode) return null;
    let panel = document.getElementById("cpMaximizePanel");
    if (!panel) {
      panel = document.createElement("section");
      panel.id = "cpMaximizePanel";
      panel.className = "cp-panel cp-maximize-panel";
      panel.setAttribute("aria-labelledby", "cpMaximizeTitle");

      const ret = retireAssumptions();
      panel.innerHTML = `
        <div class="cp-panel-header">
          <h2 id="cpMaximizeTitle">Maximize income</h2>
          <button type="button" class="cp-button cp-btn-primary" id="cpMaxRunBtn" title="Search the best way to invest your dividends and allowance for each of the three goals, then compare them">Compare all three goals</button>
        </div>
        <p class="cp-muted cp-max-intro">One run searches the best allocation for each goal on the same simulated futures: the most income in a bad year, the most typical income without giving up wealth, and the steadiest months. It then compares each with the six fixed rules.</p>
        <div class="cp-max-controls">
          <label class="cp-max-field">
            <span>Year to maximize (from now)</span>
            <input type="number" id="cpMaxYear" class="cp-input" min="1" max="30" value="${ret.years}">
          </label>
          <label class="cp-max-field">
            <span>Keep at least this much wealth (% of the proportional rule)</span>
            <input type="number" id="cpMaxFloorPct" class="cp-input" min="50" max="100" value="90">
          </label>
          <label class="cp-max-field">
            <span>Largest single holding (% of portfolio)</span>
            <input type="number" id="cpMaxPosPct" class="cp-input" min="5" max="100" value="25">
          </label>
          <label class="cp-max-field">
            <span>High-yield names, over 12% (max % of portfolio)</span>
            <input type="number" id="cpMaxHyPct" class="cp-input" min="0" max="100" value="20">
          </label>
          <label class="cp-max-field">
            <span>Outside your account currency (max %)</span>
            <input type="number" id="cpMaxFxPct" class="cp-input" min="0" max="100" value="60">
          </label>
          <label class="cp-max-field">
            <span>Lowest dividend safety allowed</span>
            <select id="cpMaxMinSafety" class="cp-input">
              <option value="a">A: safest only</option>
              <option value="b">B or better</option>
              <option value="c" selected>C or better</option>
              <option value="d">D or better</option>
              <option value="e">E: any graded payer</option>
            </select>
          </label>
          <label class="cp-max-field">
            <span>Most holdings to buy</span>
            <input type="number" id="cpMaxTopN" class="cp-input" min="1" max="30" value="10">
          </label>
        </div>
        <div class="cp-max-candidates" id="cpMaxCandidatesWrap">
          <h3 class="cp-max-sub">Watchlist names the plan may buy</h3>
          <div id="cpMaxCandidatesList" class="cp-max-cand-list"></div>
        </div>
        <div id="cpMaxResult" class="cp-max-result" aria-live="polite"></div>
      `;
      projPanel.parentNode.insertBefore(panel, projPanel.nextSibling);

      const runBtn = document.getElementById("cpMaxRunBtn");
      if (runBtn) {
        runBtn.addEventListener("click", startMaximizeJob);
      }
    }
    return panel;
  }

  async function prefillLimits() {
    if (r24LimitsPrefilled) return;
    r24LimitsPrefilled = true;
    try {
      const limits = await fetchJson(`/api/capital/limits${r24Query()}`);
      const pos = document.getElementById("cpMaxPosPct");
      if (pos && limits && typeof limits.max_position_pct === "number") {
        pos.value = String(limits.max_position_pct);
      }
    } catch (_) {}
  }

  function renderCandidates(list) {
    const wrap = document.getElementById("cpMaxCandidatesList");
    if (!wrap) return;
    wrap.innerHTML = "";
    if (!Array.isArray(list) || list.length === 0) {
      const p = document.createElement("p");
      p.className = "cp-muted";
      p.textContent = "No watchlist dividend payers to choose from yet. Add stocks or ETFs to the watchlist.";
      wrap.appendChild(p);
      return;
    }

    for (const c of list) {
      const row = document.createElement("label");
      row.className = "cp-max-cand-item";
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = !!c.enabled;
      cb.setAttribute("aria-label", `Let the plan buy ${c.symbol}`);
      row.title = `Tick to let the maximizer buy ${c.symbol}`;
      cb.addEventListener("change", async () => {
        cb.disabled = true;
        try {
          const updated = await fetchJson(`/api/capital/dividends/candidates${r24Query()}`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ symbol: c.symbol, enabled: cb.checked }),
          });
          r24Candidates = updated;
          renderCandidates(updated);
        } catch (_) {
          cb.checked = !cb.checked;
        } finally {
          cb.disabled = false;
        }
      });

      const name = document.createElement("strong");
      name.textContent = c.symbol;
      const meta = document.createElement("span");
      meta.className = "cp-muted";
      const yld = typeof c.trailing_yield === "number" ? `${(c.trailing_yield * 100).toFixed(1)}% yield` : "—";
      meta.textContent = `${yld} · ${c.years_of_history || 0}y`;

      row.appendChild(cb);
      row.appendChild(name);
      if (c.safety) row.appendChild(createSafetyChip(c.safety, c.symbol));
      row.appendChild(meta);
      wrap.appendChild(row);
    }
  }

  async function loadCandidates() {
    ensureMaximizePanel();
    try {
      const list = await fetchJson(`/api/capital/dividends/candidates${r24Query()}`);
      r24Candidates = list;
      renderCandidates(list);
    } catch (_) {}
  }

  function jobPlans(job) {
    if (job && Array.isArray(job.plans) && job.plans.length > 0) return job.plans;
    return job && job.plan ? [job.plan] : [];
  }

  function goalLabel(plan) {
    if (plan && plan.objective_label) return plan.objective_label;
    const kind = plan && plan.objective && plan.objective.kind;
    const goal = MAX_GOALS.find((g) => g.kind === kind);
    return goal ? goal.label : kind || "Goal";
  }

  function policyLabel(policy) {
    if (policy === "plan") return "Maximized plan";
    return (ALLOC_INFO[policy] && ALLOC_INFO[policy].label) || policy || "fixed rule";
  }

  // The row of the policy the plan recommends: the plan itself, or the best fixed rule.
  function recommendedScore(plan) {
    const scores = Array.isArray(plan.scores) ? plan.scores : [];
    const rec = plan.recommendation || {};
    const policy = rec.kind === "mode" ? rec.mode : "plan";
    return { policy, score: scores.find((s) => s.policy === policy) || null };
  }

  function moneyOrDash(v) {
    return typeof v === "number" && Number.isFinite(v) ? formatProjMoney(v) : "—";
  }

  function cell(tr, text, numeric) {
    const td = document.createElement("td");
    if (numeric) td.className = "num";
    td.textContent = text;
    tr.appendChild(td);
  }

  function renderComparison(out, plans, total) {
    const year = plans[0] && plans[0].objective && plans[0].objective.year;
    const h3 = document.createElement("h3");
    h3.className = "cp-max-sub";
    h3.textContent = year
      ? `How each goal maximizes income in year ${year}`
      : "How each goal maximizes income";
    out.appendChild(h3);

    const scroll = document.createElement("div");
    scroll.className = "cp-max-table-scroll";
    const table = document.createElement("table");
    table.className = "cp-table cp-max-compare";
    const thead = document.createElement("thead");
    const htr = document.createElement("tr");
    for (const col of [
      "Goal",
      "Best choice",
      "Bad-year income (p10, a year)",
      "Typical income (p50, a year)",
      "Typical portfolio value",
      "Gain over the best fixed rule",
    ]) {
      const th = document.createElement("th");
      th.scope = "col";
      th.textContent = col;
      htr.appendChild(th);
    }
    thead.appendChild(htr);
    table.appendChild(thead);

    const tbody = document.createElement("tbody");
    for (let i = 0; i < Math.max(total, plans.length); i++) {
      const plan = plans[i];
      const tr = document.createElement("tr");
      if (!plan) {
        cell(tr, (MAX_GOALS[i] && MAX_GOALS[i].label) || "Goal", false);
        cell(tr, "Searching…", false);
        for (let k = 0; k < 4; k++) cell(tr, "—", true);
        tbody.appendChild(tr);
        continue;
      }
      const { policy, score } = recommendedScore(plan);
      cell(tr, goalLabel(plan), false);
      cell(tr, policyLabel(policy), false);
      cell(tr, moneyOrDash(score && score.p10), true);
      cell(tr, moneyOrDash(score && score.p50), true);
      cell(tr, moneyOrDash(score && score.value_p50), true);
      const proven = policy === "plan" && typeof plan.margin === "number";
      cell(
        tr,
        proven
          ? `+${formatProjMoney(plan.margin)} (± ${moneyOrDash(plan.margin_se)})`
          : "Not proven",
        true,
      );
      tbody.appendChild(tr);
    }
    table.appendChild(tbody);
    scroll.appendChild(table);
    out.appendChild(scroll);

    const legend = document.createElement("ul");
    legend.className = "cp-max-goal-legend";
    for (const plan of plans) {
      if (!plan.objective_explainer) continue;
      const li = document.createElement("li");
      const strong = document.createElement("strong");
      strong.textContent = `${goalLabel(plan)}: `;
      li.appendChild(strong);
      li.appendChild(document.createTextNode(plan.objective_explainer));
      legend.appendChild(li);
    }
    if (legend.childNodes.length > 0) out.appendChild(legend);
    const gainNote = document.createElement("p");
    gainNote.className = "cp-note";
    gainNote.textContent =
      "Gain is the goal's value for the plan minus the best fixed rule, with its standard error. A plan is recommended only when it wins by more than two standard errors; otherwise the best fixed rule is.";
    out.appendChild(gainNote);
  }

  function renderPlanDetail(plan, container) {
    const rec = plan.recommendation || {};
    const banner = document.createElement("div");
    banner.className = "cp-max-rec";
    if (rec.kind === "mode") {
      const label = policyLabel(rec.mode);
      banner.textContent = `No allocation beats ${label} by more than 2 standard errors. Recommended rule: ${label}.`;
    } else {
      banner.textContent = "Recommended: the maximized allocation below.";
    }
    container.appendChild(banner);

    const thisMonth = Array.isArray(plan.this_month) ? plan.this_month : [];
    if (thisMonth.length > 0) {
      const h4 = document.createElement("h4");
      h4.textContent = "This month's buy list";
      container.appendChild(h4);
      const ul = document.createElement("ul");
      ul.className = "cp-max-buylist";
      for (const item of thisMonth) {
        const li = document.createElement("li");
        const tag = item.candidate ? " (new name: consider buying)" : "";
        li.textContent = `${item.symbol}: ${formatProjMoney(item.amount)}${tag}`;
        ul.appendChild(li);
      }
      container.appendChild(ul);
    }

    const scores = Array.isArray(plan.scores) ? plan.scores : [];
    if (scores.length > 0) {
      const scroll = document.createElement("div");
      scroll.className = "cp-max-table-scroll";
      const table = document.createElement("table");
      table.className = "cp-table cp-max-scores";
      const thead = document.createElement("thead");
      const htr = document.createElement("tr");
      for (const col of [
        "Rule",
        "Goal value",
        "Bad-year income (p10)",
        "Typical income (p50)",
        "Typical portfolio value",
      ]) {
        const th = document.createElement("th");
        th.scope = "col";
        th.textContent = col;
        htr.appendChild(th);
      }
      thead.appendChild(htr);
      table.appendChild(thead);
      const tbody = document.createElement("tbody");
      for (const s of scores) {
        const tr = document.createElement("tr");
        cell(tr, policyLabel(s.policy), false);
        cell(tr, moneyOrDash(s.objective), true);
        cell(tr, moneyOrDash(s.p10), true);
        cell(tr, moneyOrDash(s.p50), true);
        cell(tr, moneyOrDash(s.value_p50), true);
        tbody.appendChild(tr);
      }
      table.appendChild(tbody);
      scroll.appendChild(table);
      container.appendChild(scroll);
    }

    if (plan.sensitivity && typeof plan.sensitivity === "object") {
      const sensWrap = document.createElement("div");
      sensWrap.className = "cp-max-sens";
      const entries = [
        ["If dividends grow slower", plan.sensitivity.low_growth],
        ["If no dividend is cut", plan.sensitivity.no_cuts],
        ["If exchange rates stay flat", plan.sensitivity.fx_flat],
      ];
      for (const [lbl, v] of entries) {
        if (typeof v !== "number") continue;
        const chip = document.createElement("span");
        chip.className = "cp-max-sens-chip";
        chip.textContent = `${lbl}: ${formatProjMoney(v)}`;
        sensWrap.appendChild(chip);
      }
      if (sensWrap.childNodes.length > 0) container.appendChild(sensWrap);
    }

    const notes = Array.isArray(plan.notes) ? plan.notes : [];
    for (const n of notes) {
      const p = document.createElement("p");
      p.className = "cp-note";
      p.textContent = n;
      container.appendChild(p);
    }
  }

  function renderMaximizeJob(job) {
    const out = document.getElementById("cpMaxResult");
    const runBtn = document.getElementById("cpMaxRunBtn");
    if (!out) return;
    out.innerHTML = "";

    const state = (job && job.state) || "idle";
    const plans = jobPlans(job);
    const total = Math.max((job && job.objectives_total) || 0, plans.length);
    if (runBtn) {
      runBtn.disabled = state === "running";
      runBtn.textContent = state === "running"
        ? `Searching… ${plans.length} of ${total || MAX_GOALS.length} goals done`
        : plans.length > 0 ? "Run all three goals again" : "Compare all three goals";
    }

    if (state === "idle") {
      const p = document.createElement("p");
      p.className = "cp-empty";
      p.textContent = "Set the limits above, then press Compare all three goals to search the best allocation for each goal.";
      out.appendChild(p);
      return;
    }

    if (state === "running") {
      const p = document.createElement("p");
      p.className = "cp-max-running";
      p.textContent = `Searching allocations across simulated futures… ${plans.length} of ${total || MAX_GOALS.length} goals done.`;
      out.appendChild(p);
      if (plans.length > 0) renderComparison(out, plans, total);
      return;
    }

    if (state === "failed") {
      const p = document.createElement("p");
      p.className = "cp-word-bad";
      p.textContent = `Maximizer failed: ${(job && job.error) || "unknown error"}`;
      out.appendChild(p);
      if (plans.length > 0) renderComparison(out, plans, plans.length);
      return;
    }

    if (plans.length === 0) {
      const p = document.createElement("p");
      p.className = "cp-empty";
      p.textContent = "No plan returned.";
      out.appendChild(p);
      return;
    }

    renderComparison(out, plans, total);
    plans.forEach((plan, i) => {
      const details = document.createElement("details");
      details.className = "cp-max-goal";
      if (i === 0) details.open = true;
      const summary = document.createElement("summary");
      summary.textContent = `${goalLabel(plan)}: details and buy list`;
      details.appendChild(summary);
      const body = document.createElement("div");
      body.className = "cp-max-goal-body";
      if (plan.objective_explainer) {
        const p = document.createElement("p");
        p.className = "cp-muted";
        p.textContent = plan.objective_explainer;
        body.appendChild(p);
      }
      renderPlanDetail(plan, body);
      details.appendChild(body);
      out.appendChild(details);
    });
  }

  function clearMaxPoll() {
    if (r24MaxPollTimer) {
      clearTimeout(r24MaxPollTimer);
      r24MaxPollTimer = null;
    }
  }

  async function pollMaximizeJob() {
    clearMaxPoll();
    ensureMaximizePanel();
    try {
      const job = await fetchJson(`/api/capital/dividends/maximize${r24Query()}`);
      renderMaximizeJob(job);
      if (job && job.state === "running") {
        r24MaxPollTimer = setTimeout(pollMaximizeJob, 2000);
      }
    } catch (err) {
      renderMaximizeJob({ state: "failed", error: err.message });
    }
  }

  async function startMaximizeJob() {
    clearMaxPoll();
    const year = Math.max(1, Math.min(30, Number(document.getElementById("cpMaxYear")?.value) || 20));
    const floorPct = Math.max(50, Math.min(100, Number(document.getElementById("cpMaxFloorPct")?.value) || 90));

    // No `objective`: the server runs all three goals at `objective_year`.
    const body = {
      years: PROJ_YEARS,
      reinvest_pct: currentProjReinvestPct,
      allocation: currentProjAllocation,
      monthly_contribution: currentProjMonthlyContribution,
      top_n: currentProjTopN,
      reserve_pt_tax: currentProjReservePtTax,
      objective_year: year,
      floor_pct: floorPct,
      constraints: {
        max_position_pct: Number(document.getElementById("cpMaxPosPct")?.value) || 25,
        max_high_yield_pct: Number(document.getElementById("cpMaxHyPct")?.value) || 20,
        max_foreign_pct: Number(document.getElementById("cpMaxFxPct")?.value) || 60,
        min_safety: document.getElementById("cpMaxMinSafety")?.value || "c",
        top_n: Number(document.getElementById("cpMaxTopN")?.value) || 10,
      },
    };

    try {
      renderMaximizeJob({ state: "running", objectives_total: MAX_GOALS.length });
      const res = await fetchJson(`/api/capital/dividends/maximize${r24Query()}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      const job = (res && res.job) || res;
      renderMaximizeJob(job);
      if (job && job.state === "running") {
        r24MaxPollTimer = setTimeout(pollMaximizeJob, 2000);
      }
    } catch (err) {
      renderMaximizeJob({ state: "failed", error: err.message });
    }
  }

  if (typeof loadDividendProjection === "function") {
    const origLoadProj = loadDividendProjection;
    loadDividendProjection = async function () {
      const result = await origLoadProj.apply(this, arguments);
      ensureMaximizePanel();
      prefillLimits();
      loadIncomeFan();
      loadCandidates();
      pollMaximizeJob();
      return result;
    };
  }
})();

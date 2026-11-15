// SAIL Capital — Signals: every holding and watchlist stock or ETF side by side, with
// its price-signal readings, the blended outlook, any trade setup, one read, a stock drawer,
// and the forward record.
"use strict";

const SIGB_SIGNALS = ["momentum12_1", "trend", "low_volatility", "short_term_reversal"];
const SIGB_SIGNAL_NAMES = {
  momentum12_1: "Momentum",
  trend: "Trend",
  low_volatility: "Calm",
  short_term_reversal: "Reversal",
};
const SIGB_LEAN = {
  favoured: "Favoured",
  disfavoured: "Against",
  neutral: "Neutral",
  mixed: "Mixed",
  no_data: "No data",
};
// Sort order for the read column: favoured first.
const SIGB_LEAN_RANK = { favoured: 0, neutral: 1, mixed: 2, disfavoured: 3, no_data: 4 };

let sigbBoard = null;
let sigbRecord = null;
let sigbScope = "all";
let sigbLeanFilter = "any";
let sigbHorizon = 63;
let sigbSort = { key: "read", dir: 1 };

let sigbDrawerSymbol = null;
let sigbDrawerTrigger = null;

// Lazy caches cleared on Refresh (loadSignalBoard).
let sigbOutlookCache = new Map();
let sigbGuardsCache = null;
let sigbDividendsCache = null;
let sigbInsightsCache = null;

function sigbQuery() {
  return currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
}

function sigbReading(row, signal) {
  return (row.readings || []).find((r) => r.signal === signal) || null;
}

function sigbOutlook(row, horizon = sigbHorizon) {
  return (row.outlook || []).find((o) => o.horizon_sessions === horizon) || null;
}

function sigbPct(fraction, digits = 1) {
  if (!Number.isFinite(fraction)) return "—";
  const v = fraction * 100;
  const sign = v > 0.05 ? "+" : v < -0.05 ? "−" : "";
  return `${sign}${Math.abs(v).toFixed(digits)}%`;
}

function sigbHorizonWord(sessions) {
  return sessions >= 60 ? "3 months" : "1 month";
}

function sigbHorizonShort(sessions) {
  return sessions >= 60 ? "3M" : "1M";
}

function sigbSignalLabel(sig) {
  if (SIGB_SIGNAL_NAMES[sig]) return SIGB_SIGNAL_NAMES[sig];
  if (typeof formatSignalName === "function") return formatSignalName(sig);
  return String(sig || "");
}

// A percentile cell: the number and a short bar, coloured by side of the middle.
function sigbMeter(reading) {
  if (!reading) return `<span class="cp-chg-none" title="Not enough history">—</span>`;
  const p = Math.round(reading.percentile);
  const side = p >= 67 ? "up" : p <= 33 ? "down" : "mid";
  return `<span class="cp-sigb-meter" data-side="${side}" title="${escapeHtml(reading.text)}">
      <span class="cp-sigb-meter-num">${p}</span>
      <span class="cp-sigb-meter-bar" aria-hidden="true"><span style="width:${Math.max(4, p)}%"></span></span>
    </span>`;
}

function sigbLeanPill(read) {
  const lean = read?.lean || "no_data";
  const proof = read?.lean === "no_data"
    ? ""
    : `<span class="cp-proof" data-proven="${read?.proven ? "yes" : "no"}">${read?.proven ? "validated" : "unproven"}</span>`;
  return `<span class="cp-sigb-read"><span class="cp-lean" data-lean="${escapeHtml(lean)}">${escapeHtml(SIGB_LEAN[lean] || lean)}</span>${proof}</span>`;
}

function sigbSetup(setup) {
  if (!setup) return `<span class="cp-chg-none">—</span>`;
  const side = setup.side === "long" ? "Buy" : "Sell";
  const title = setup.published
    ? "Published: the setup has earned a record"
    : setup.candidate
      ? "A candidate: positive after costs in both periods, not yet clear of luck"
      : "Fired, but the setup hasn't earned a record, so it didn't publish";
  const tag = setup.published ? "" : setup.candidate ? " (candidate)" : " (unproven)";
  return `<span class="cp-sigb-setup" data-published="${setup.published ? "yes" : "no"}" title="${title}">${escapeHtml(side)} · ${escapeHtml(formatSetupName(setup.setup))}${tag}</span>`;
}

function sigbSortValue(row, key) {
  switch (key) {
    case "symbol": return row.symbol;
    case "read": {
      const lean = SIGB_LEAN_RANK[row.read?.lean] ?? 5;
      const net = (row.read?.for_count || 0) - (row.read?.against_count || 0);
      return lean * 100 - net;
    }
    case "median": return sigbOutlook(row)?.median ?? null;
    case "p_up": return sigbOutlook(row)?.p_up ?? null;
    default: return sigbReading(row, key)?.percentile ?? null;
  }
}

function sigbRows() {
  if (!sigbBoard) return [];
  let rows = sigbBoard.rows || [];
  if (sigbScope === "held") rows = rows.filter((r) => r.held);
  if (sigbScope === "watch") rows = rows.filter((r) => !r.held);
  if (sigbLeanFilter !== "any") rows = rows.filter((r) => r.read?.lean === sigbLeanFilter);
  const { key, dir } = sigbSort;
  // Numbers sort high-first by default; text and the read sort ascending.
  const textual = key === "symbol" || key === "read";
  return [...rows].sort((a, b) => {
    const va = sigbSortValue(a, key);
    const vb = sigbSortValue(b, key);
    if (va === null && vb === null) return a.symbol.localeCompare(b.symbol);
    if (va === null) return 1;
    if (vb === null) return -1;
    let c = typeof va === "string" ? va.localeCompare(vb) : va - vb;
    if (!textual) c = -c;
    return c * dir || a.symbol.localeCompare(b.symbol);
  });
}

function renderSigbTable() {
  const body = document.getElementById("cpSigbBody");
  if (!body) return;
  const rows = sigbRows();
  if (!sigbBoard) return;
  if (rows.length === 0) {
    body.innerHTML = `<tr><td colspan="9" class="cp-empty">${(sigbBoard.rows || []).length === 0
      ? "No instruments with prices yet. Turn on price fetching in Settings → Prices."
      : "Nothing matches these filters."}</td></tr>`;
    return;
  }
  body.innerHTML = rows.map((row) => {
    const o = sigbOutlook(row);
    const isOpen = sigbDrawerSymbol === row.symbol;
    const outlookCell = o
      ? `<span class="cp-sigb-out"><span class="cp-sigb-out-med" data-dir="${changeDir(o.median)}">${sigbPct(o.median)}</span><span class="cp-sigb-out-range">${sigbPct(o.low, 0)} … ${sigbPct(o.high, 0)}</span></span>`
      : `<span class="cp-chg-none">—</span>`;
    const chance = o ? `${Math.round(o.p_up * 100)}%` : "—";
    return `<tr class="cp-sigb-row${isOpen ? " cp-sigb-open" : ""}" data-symbol="${escapeHtml(row.symbol)}" tabindex="0" aria-expanded="${isOpen}">
        <td>
          <span class="cp-sigb-inst">
            ${instrumentAvatar(row.symbol, row.class)}
            <span class="cp-sigb-inst-text">
              <span class="cp-sigb-sym">${escapeHtml(row.symbol)}${row.held ? ` <span class="cp-sigb-held">Held</span>` : ""}</span>
              <span class="cp-sigb-name">${escapeHtml(row.name || "")}</span>
            </span>
            <span class="cp-sigb-px">${formatPrice(row.close)}<br>${changePill(row.change_1m, 1)}</span>
          </span>
        </td>
        <td>${sigbLeanPill(row.read)}</td>
        ${SIGB_SIGNALS.map((sig) => `<td class="num">${sigbMeter(sigbReading(row, sig))}</td>`).join("")}
        <td class="num">${outlookCell}</td>
        <td class="num">${chance}</td>
        <td>${sigbSetup(row.setup)}</td>
      </tr>`;
  }).join("");

  body.querySelectorAll(".cp-sigb-row").forEach((tr) => {
    const open = () => openSigbDrawer(tr.dataset.symbol, tr);
    tr.addEventListener("click", open);
    tr.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        open();
      }
    });
  });
  document.querySelectorAll(".cp-sigb-sort").forEach((btn) => {
    const th = btn.closest("th");
    const on = btn.dataset.sort === sigbSort.key;
    const textual = sigbSort.key === "symbol" || sigbSort.key === "read";
    const descending = (textual ? -sigbSort.dir : sigbSort.dir) === 1;
    if (th) th.setAttribute("aria-sort", on ? (descending ? "descending" : "ascending") : "none");
    btn.classList.toggle("cp-sigb-sort-on", on);
  });
}

// --- Stock drawer ----------------------------------------------------------------------

function renderDrawerFanSvg(res) {
  const outlook = res && res.outlook;
  const fan = (outlook && outlook.fan) || [];
  if (!outlook || fan.length === 0) return "";

  const history = res.history || [];
  const originClose = Number(outlook.origin_close) || 0;
  const prices = [originClose];
  for (const h of history) {
    const c = Number(Array.isArray(h) ? h[1] : (h.close ?? h[1]));
    if (Number.isFinite(c)) prices.push(c);
  }
  for (const fp of fan) {
    for (const p of fp.prices || []) {
      if (Number.isFinite(Number(p))) prices.push(Number(p));
    }
  }
  let minP = Math.min(...prices);
  let maxP = Math.max(...prices);
  if (minP === maxP) {
    minP *= 0.95;
    maxP *= 1.05;
  }
  const span = maxP - minP;
  const pMin = minP - span * 0.08;
  const pMax = maxP + span * 0.08;

  const W = 420;
  const H = 190;
  const ml = 50;
  const mr = 12;
  const mt = 14;
  const mb = 24;
  const pw = W - ml - mr;
  const ph = H - mt - mb;
  const yBottom = mt + ph;

  const yPrice = (p) => yBottom - ((p - pMin) / (pMax - pMin)) * ph;
  const nHist = Math.max(1, history.length);
  const nFan = Math.max(1, outlook.horizon || fan.length);
  const histRatio = Math.min(0.42, Math.max(0.28, nHist / (nHist + nFan)));
  const xLeft = ml;
  const xOrigin = ml + pw * histRatio;
  const xRight = ml + pw;
  const xHist = (i) => (nHist <= 1 ? xOrigin : xLeft + (i / (nHist - 1)) * (xOrigin - xLeft));
  const xFan = (step) => xOrigin + (step / nFan) * (xRight - xOrigin);
  const yOrigin = yPrice(originClose);

  let histPath = "";
  if (history.length > 0) {
    histPath = history.map((h, i) => {
      const c = Number(Array.isArray(h) ? h[1] : (h.close ?? h[1]));
      return `${i === 0 ? "M" : "L"} ${xHist(i).toFixed(1)} ${yPrice(c).toFixed(1)}`;
    }).join(" ") + ` L ${xOrigin.toFixed(1)} ${yOrigin.toFixed(1)}`;
  }

  const fanSteps = [
    { step: 0, prices: [originClose, originClose, originClose, originClose, originClose] },
    ...fan,
  ];
  const band90 = `M ${fanSteps.map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[4]).toFixed(1)}`).join(" L ")} L ${[...fanSteps].reverse().map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[0]).toFixed(1)}`).join(" L ")} Z`;
  const band68 = `M ${fanSteps.map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[3]).toFixed(1)}`).join(" L ")} L ${[...fanSteps].reverse().map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[1]).toFixed(1)}`).join(" L ")} Z`;
  const medPath = "M " + fanSteps.map(f => `${xFan(f.step).toFixed(1)} ${yPrice(f.prices[2]).toFixed(1)}`).join(" L ");

  const midP = (pMin + pMax) / 2;
  const ticks = [pMax, midP, pMin].map(v => `
    <line x1="${xLeft}" y1="${yPrice(v).toFixed(1)}" x2="${xRight}" y2="${yPrice(v).toFixed(1)}" stroke="rgba(255,255,255,0.06)" stroke-width="1" />
    <text x="${(xLeft - 6).toFixed(1)}" y="${(yPrice(v) + 3).toFixed(1)}" text-anchor="end" class="cp-sigb-fan-label">${formatPrice(v)}</text>
  `).join("");

  return `
    <svg viewBox="0 0 ${W} ${H}" class="cp-sigb-fan-svg" role="img" aria-label="3-month outlook fan for ${escapeHtml(res.symbol || "")}">
      ${ticks}
      <line x1="${xOrigin.toFixed(1)}" y1="${mt}" x2="${xOrigin.toFixed(1)}" y2="${yBottom}" stroke="rgba(255,255,255,0.18)" stroke-dasharray="3 3" stroke-width="1" />
      <path d="${band90}" fill="rgba(143, 140, 255, 0.14)" />
      <path d="${band68}" fill="rgba(143, 140, 255, 0.24)" />
      ${histPath ? `<path d="${histPath}" fill="none" stroke="rgba(255,255,255,0.55)" stroke-width="1.6" />` : ""}
      <path d="${medPath}" fill="none" stroke="#8f8cff" stroke-width="2" />
      <circle cx="${xOrigin.toFixed(1)}" cy="${yOrigin.toFixed(1)}" r="2.8" fill="#ffd69f" />
      <text x="${xOrigin.toFixed(1)}" y="${H - 6}" text-anchor="middle" class="cp-sigb-fan-label">${outlook.origin ? formatDay(outlook.origin) : "Now"}</text>
      <text x="${xRight.toFixed(1)}" y="${H - 6}" text-anchor="end" class="cp-sigb-fan-label">+${nFan}d</text>
    </svg>
  `;
}

async function fetchDrawerOutlook(symbol) {
  if (sigbOutlookCache.has(symbol)) return sigbOutlookCache.get(symbol);
  try {
    const q = sigbQuery();
    const sep = q ? "&" : "?";
    const res = await fetchJson(`/api/capital/outlook${q}${sep}symbol=${encodeURIComponent(symbol)}&horizon=63`);
    sigbOutlookCache.set(symbol, res);
    return res;
  } catch (_) {
    sigbOutlookCache.set(symbol, null);
    return null;
  }
}

async function fetchDrawerGuards() {
  if (sigbGuardsCache) return sigbGuardsCache;
  if (typeof latestHoldingGuards !== "undefined" && Array.isArray(latestHoldingGuards)) {
    sigbGuardsCache = latestHoldingGuards;
    return sigbGuardsCache;
  }
  try {
    sigbGuardsCache = await fetchJson(`/api/capital/signals/trade/guards${sigbQuery()}`);
  } catch (_) {
    sigbGuardsCache = [];
  }
  return sigbGuardsCache;
}

async function fetchDrawerDividends() {
  if (sigbDividendsCache) return sigbDividendsCache;
  try {
    sigbDividendsCache = await fetchJson(`/api/capital/dividends${sigbQuery()}`);
  } catch (_) {
    sigbDividendsCache = [];
  }
  return sigbDividendsCache;
}

async function fetchDrawerInsights() {
  if (sigbInsightsCache) return sigbInsightsCache;
  if (typeof latestInsights !== "undefined" && latestInsights) {
    sigbInsightsCache = latestInsights;
    return sigbInsightsCache;
  }
  try {
    sigbInsightsCache = await fetchJson(`/api/capital/insights${sigbQuery()}`);
  } catch (_) {
    sigbInsightsCache = null;
  }
  return sigbInsightsCache;
}

function renderDrawerStatic(row) {
  const idBox = document.getElementById("cpSigbDrawerId");
  const body = document.getElementById("cpSigbDrawerBody");
  if (!idBox || !body) return;

  idBox.innerHTML = `
    <div class="cp-sigb-drawer-top">
      ${instrumentAvatar(row.symbol, row.class)}
      <div class="cp-sigb-drawer-titles">
        <h2 id="cpSigbDrawerTitle">${escapeHtml(row.symbol)}${row.held ? ` <span class="cp-sigb-held">Held</span>` : ""}</h2>
        <span class="cp-sigb-drawer-name">${escapeHtml(row.name || "")}</span>
      </div>
      <div class="cp-sigb-drawer-px">
        <span>${formatPrice(row.close)}</span>
        ${changePill(row.change_1m, 1)}
      </div>
    </div>
    <div class="cp-sigb-drawer-read">
      ${sigbLeanPill(row.read)}
      <p class="cp-sigb-d-read">${escapeHtml(row.read?.sentence || "")}</p>
    </div>
  `;

  const readings = (row.readings || []).map((r) => `
    <li class="cp-row">
      <span class="cp-row-title">${escapeHtml(sigbSignalLabel(r.signal))}</span>
      <span class="cp-row-value">Percentile ${Math.round(r.percentile)}</span>
      <span class="cp-row-meta">${escapeHtml(r.text)}</span>
    </li>
  `).join("");

  const setupHtml = row.setup
    ? `<section class="cp-sigb-drawer-sec">
        <h3>Trade setup</h3>
        <p class="cp-sigb-d-line">${row.setup.published
          ? `A ${row.setup.side === "long" ? "buy" : "sell"} setup (${escapeHtml(formatSetupName(row.setup.setup))}) published on the latest session. Its entry, stop and targets are on Outlook → Trade signals.`
          : row.setup.candidate
            ? `A ${row.setup.side === "long" ? "buy" : "sell"} setup (${escapeHtml(formatSetupName(row.setup.setup))}) fired as a candidate: positive after costs in both periods, not yet clear of luck. Its levels are on Outlook → Trade signals.`
            : `A ${row.setup.side === "long" ? "buy" : "sell"} setup (${escapeHtml(formatSetupName(row.setup.setup))}) fired on the latest session but hasn't earned a record, so it didn't publish.`}</p>
      </section>`
    : "";

  body.innerHTML = `
    <section class="cp-sigb-drawer-sec" id="cpSigbDrawerOutlookSec">
      <div class="cp-sigb-drawer-sec-head">
        <h3>3-month outlook</h3>
        <button type="button" class="cp-link-btn cp-sigb-open-outlook" data-symbol="${escapeHtml(row.symbol)}">Open full outlook →</button>
      </div>
      <div id="cpSigbDrawerFan" class="cp-sigb-drawer-fan">
        <p class="cp-empty">Loading outlook…</p>
      </div>
    </section>

    <section class="cp-sigb-drawer-sec">
      <h3>What the prices show</h3>
      ${readings ? `<ul class="cp-rows">${readings}</ul>` : `<p class="cp-empty">No signal readings yet.</p>`}
    </section>

    <section class="cp-sigb-drawer-sec" id="cpSigbDrawerPosSec" hidden></section>
    <section class="cp-sigb-drawer-sec" id="cpSigbDrawerDivSec" hidden></section>
    ${setupHtml}
  `;

  body.querySelector(".cp-sigb-open-outlook")?.addEventListener("click", () => {
    const sym = row.symbol;
    closeSigbDrawer(false);
    if (typeof openOutlookFor === "function") openOutlookFor(sym);
  });
}

async function hydrateDrawer(row) {
  const sym = row.symbol;
  const [outlookRes, guards, dividends, insights] = await Promise.all([
    fetchDrawerOutlook(sym),
    row.held ? fetchDrawerGuards() : Promise.resolve([]),
    fetchDrawerDividends(),
    row.held ? fetchDrawerInsights() : Promise.resolve(null),
  ]);

  // Bail if the user switched stocks or closed the drawer while fetching.
  if (sigbDrawerSymbol !== sym) return;

  // 1. Outlook fan
  const outlookSec = document.getElementById("cpSigbDrawerOutlookSec");
  const fanBox = document.getElementById("cpSigbDrawerFan");
  if (outlookSec && fanBox) {
    const svg = renderDrawerFanSvg(outlookRes);
    const o3 = sigbOutlook(row, 63) || sigbOutlook(row, 21);
    if (svg) {
      const summary = o3
        ? `<p class="cp-note">${sigbHorizonWord(o3.horizon_sessions)}: median ${sigbPct(o3.median)}, 90% range ${sigbPct(o3.low, 0)} to ${sigbPct(o3.high, 0)}, chance up ${Math.round(o3.p_up * 100)}%.</p>`
        : "";
      fanBox.innerHTML = `${svg}${summary}`;
    } else if (o3) {
      fanBox.innerHTML = `<p class="cp-note">${sigbHorizonWord(o3.horizon_sessions)}: median ${sigbPct(o3.median)}, 90% range ${sigbPct(o3.low, 0)} to ${sigbPct(o3.high, 0)}, chance up ${Math.round(o3.p_up * 100)}%.</p>`;
    } else {
      fanBox.innerHTML = "";
      outlookSec.hidden = true;
    }
  }

  // 2. Your position (holdings only)
  const posSec = document.getElementById("cpSigbDrawerPosSec");
  if (posSec && row.held) {
    const positions = (typeof latestPortfolio !== "undefined" && latestPortfolio && latestPortfolio.positions) || [];
    const pos = positions.find(p => (p.instrument && p.instrument.symbol) === sym || p.symbol === sym);
    const guard = (guards || []).find(g => g.symbol === sym);
    const allocSlice = insights && Array.isArray(insights.allocation)
      ? insights.allocation.find(a => a.symbol === sym)
      : null;

    if (pos) {
      const rows = [
        `<li class="cp-row"><span class="cp-row-title">Units</span><span class="cp-row-value">${escapeHtml(pos.quantity || "0")}</span></li>`,
        `<li class="cp-row"><span class="cp-row-title">Value</span><span class="cp-row-value">${pos.broker_market_value ? formatMoney(pos.broker_market_value) : "—"}</span></li>`,
      ];
      if (allocSlice && allocSlice.share !== undefined && allocSlice.share !== null) {
        rows.push(`<li class="cp-row"><span class="cp-row-title">Share of account</span><span class="cp-row-value">${(parseFloat(allocSlice.share) * 100).toFixed(1)}%</span></li>`);
      }
      rows.push(`<li class="cp-row"><span class="cp-row-title">Cost basis</span><span class="cp-row-value">${pos.cost_basis ? formatMoney(pos.cost_basis) : "—"}</span></li>`);
      if (pos.broker_market_value && pos.cost_basis && pos.broker_market_value.currency === pos.cost_basis.currency) {
        const openAmt = sumDecimals([pos.broker_market_value.amount, "-" + pos.cost_basis.amount]);
        const isNeg = String(openAmt).startsWith("-");
        rows.push(`<li class="cp-row"><span class="cp-row-title">Open result</span><span class="cp-row-value ${isNeg ? "cp-word-bad" : "cp-word-good"}">${formatMoney({ amount: openAmt, currency: pos.broker_market_value.currency })}</span></li>`);
      }
      if (guard && guard.trailing_stop !== null && guard.trailing_stop !== undefined) {
        const dist = Number(guard.stop_distance_pct || 0);
        const distStr = `${dist >= 0 ? "+" : ""}${dist.toFixed(1)}%`;
        const stopVal = `${formatPrice(guard.trailing_stop)} (${distStr}${guard.stop_hit ? " · stop hit" : ""})`;
        rows.push(`<li class="cp-row"><span class="cp-row-title">Trailing stop</span><span class="cp-row-value ${guard.stop_hit ? "cp-word-bad" : ""}">${stopVal}</span></li>`);
      }
      posSec.hidden = false;
      posSec.innerHTML = `<h3>Your position</h3><ul class="cp-rows">${rows.join("")}</ul>`;
    } else {
      posSec.hidden = true;
    }
  }

  // 3. Dividends (last two received for this symbol)
  const divSec = document.getElementById("cpSigbDrawerDivSec");
  if (divSec) {
    const matching = (Array.isArray(dividends) ? dividends : [])
      .filter(d => d.instrument && d.instrument.symbol === sym)
      .sort((a, b) => String(b.paid?.at || "").localeCompare(String(a.paid?.at || "")))
      .slice(0, 2);

    if (matching.length > 0) {
      divSec.hidden = false;
      divSec.innerHTML = `
        <h3>Recent dividends</h3>
        <ul class="cp-rows">
          ${matching.map(d => `
            <li class="cp-row">
              <span class="cp-row-title">${formatDay(d.paid?.at)}</span>
              <span class="cp-row-value cp-word-good">${d.net ? formatMoney(d.net) : "—"}</span>
              <span class="cp-row-meta">Gross: ${formatMoney(d.gross)}</span>
            </li>
          `).join("")}
        </ul>
      `;
    } else {
      divSec.hidden = true;
    }
  }
}

function openSigbDrawer(symbol, triggerEl) {
  const row = (sigbBoard?.rows || []).find(r => r.symbol === symbol);
  if (!row) return;

  sigbDrawerSymbol = symbol;
  sigbDrawerTrigger = triggerEl || document.activeElement;

  const drawer = document.getElementById("cpSigbDrawer");
  const backdrop = document.getElementById("cpSigbBackdrop");
  if (!drawer || !backdrop) return;

  renderDrawerStatic(row);
  drawer.hidden = false;
  backdrop.hidden = false;
  renderSigbTable();

  const closeBtn = document.getElementById("cpSigbDrawerClose");
  if (closeBtn) closeBtn.focus();

  hydrateDrawer(row);
}

function closeSigbDrawer(restoreFocus = true) {
  const drawer = document.getElementById("cpSigbDrawer");
  const backdrop = document.getElementById("cpSigbBackdrop");
  if (!drawer || drawer.hidden) return;

  const prevSym = sigbDrawerSymbol;
  sigbDrawerSymbol = null;
  drawer.hidden = true;
  if (backdrop) backdrop.hidden = true;
  renderSigbTable();

  if (restoreFocus) {
    const target = (sigbDrawerTrigger && document.body.contains(sigbDrawerTrigger))
      ? sigbDrawerTrigger
      : (prevSym ? document.querySelector(`.cp-sigb-row[data-symbol="${CSS.escape(prevSym)}"]`) : null);
    if (target && typeof target.focus === "function") target.focus();
  }
  sigbDrawerTrigger = null;
}

function setupSigbDrawer() {
  document.getElementById("cpSigbDrawerClose")?.addEventListener("click", () => closeSigbDrawer(true));
  document.getElementById("cpSigbBackdrop")?.addEventListener("click", () => closeSigbDrawer(true));

  document.addEventListener("keydown", (e) => {
    const drawer = document.getElementById("cpSigbDrawer");
    if (!drawer || drawer.hidden) return;

    if (e.key === "Escape") {
      e.preventDefault();
      closeSigbDrawer(true);
      return;
    }

    if (e.key === "Tab") {
      const focusables = Array.from(
        drawer.querySelectorAll('button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])')
      ).filter(el => el.offsetParent !== null);
      if (focusables.length === 0) return;
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  });
}

// --- Summary, notes, and record panel --------------------------------------------------

function renderSigbSummary() {
  const box = document.getElementById("cpSigbSummary");
  if (!box || !sigbBoard) return;
  const rows = sigbBoard.rows || [];
  const count = (lean, held) => rows.filter((r) => r.read?.lean === lean && (held === undefined || r.held === held)).length;
  const passed = sigbBoard.passed || [];
  const rec = (sigbBoard.outlook_record || []).find((r) => r.horizon_sessions === sigbHorizon)
    || (sigbBoard.outlook_record || [])[0];
  const tiles = [
    {
      label: "Validated signals",
      value: passed.length ? String(passed.length) : "None yet",
      sub: passed.length ? passed.join(" · ") : "Reads describe the past until a signal passes out-of-sample checks",
      tone: passed.length ? "up" : "flat",
    },
    {
      label: "Your holdings",
      value: `${count("favoured", true)} favoured · ${count("disfavoured", true)} against`,
      sub: `${sigbBoard.held} holdings on the board`,
      tone: "flat",
    },
    {
      label: "Watchlist",
      value: `${count("favoured", false)} favoured`,
      sub: `${sigbBoard.watched} watchlist stocks and ETFs`,
      tone: "flat",
    },
    {
      label: `Outlook record (${rec ? sigbHorizonWord(rec.horizon_sessions) : "—"})`,
      value: rec ? `${Math.round(rec.direction_hit_rate * 100)}% direction` : "Not enough history",
      sub: rec
        ? `Over ${rec.replays} past replays on your holdings; ${Math.round(rec.inside_90 * 100)}% landed inside the 90% range`
        : "Replays need a few years of prices",
      tone: rec && rec.direction_hit_rate > 0.55 ? "up" : "flat",
    },
  ];
  box.innerHTML = tiles.map((t) => `<div class="cp-card cp-sigb-tile" data-tone="${t.tone}">
      <p class="cp-sigb-tile-label">${escapeHtml(t.label)}</p>
      <p class="cp-sigb-tile-value">${escapeHtml(t.value)}</p>
      <p class="cp-sigb-tile-sub">${escapeHtml(t.sub)}</p>
    </div>`).join("");
}

function renderSigbNotes() {
  const box = document.getElementById("cpSigbNotes");
  if (!box || !sigbBoard) return;
  const notes = sigbBoard.notes || [];
  box.innerHTML = notes.map((n) => `<p class="cp-note">${escapeHtml(n)}</p>`).join("");
}

function renderBoardRecord(rec) {
  const body = document.getElementById("cpSigbRecordBody");
  const csvBtn = document.getElementById("cpSigbRecordCsv");
  if (!body) return;

  const recent = (rec && rec.recent) || [];
  if (csvBtn) csvBtn.disabled = recent.length === 0;

  if (!rec || rec.graded === 0 || (rec.by_lean || []).length === 0) {
    const firstNote = (rec && rec.notes && rec.notes[0]) || "No reads graded yet.";
    const recLine = rec && rec.first_session
      ? `Recording since ${formatDay(rec.first_session)} · ${rec.sessions || 0} sessions`
      : `${(rec && rec.sessions) || 0} sessions recorded`;
    body.innerHTML = `
      <p class="cp-empty">${escapeHtml(firstNote)}</p>
      <p class="cp-note">${escapeHtml(recLine)}</p>
    `;
    return;
  }

  const leanRows = (rec.by_lean || []).map((r) => `
    <tr>
      <td>${sigbLeanPill({ lean: r.lean, proven: r.proven })}</td>
      <td>${sigbHorizonShort(r.horizon_sessions)}</td>
      <td class="num">${r.reads}</td>
      <td class="num">${Math.round(r.beat_board_rate * 100)}% of ${r.reads}</td>
      <td class="num">${sigbPct(r.mean_excess)} <span class="cp-sigb-rec-ci">(${sigbPct(r.excess_ci[0])} to ${sigbPct(r.excess_ci[1])})</span></td>
    </tr>
  `).join("");

  const calRows = (rec.outlook || []).map((o) => {
    const better = o.brier < o.brier_base_rate;
    const brierClass = better ? "cp-word-good" : "cp-word-wait";
    return `
      <li class="cp-row">
        <span class="cp-row-title">Outlook · ${sigbHorizonShort(o.horizon_sessions)} (${o.graded} graded)</span>
        <span class="cp-row-value">Direction hit ${Math.round(o.direction_hit_rate * 100)}%</span>
        <span class="cp-row-meta">Inside 90% range: ${Math.round(o.inside_90 * 100)}% (aim 90%) · <span class="${brierClass}">chance-up Brier ${o.brier.toFixed(3)} vs ${o.brier_base_rate.toFixed(3)} for the base rate</span></span>
      </li>
    `;
  }).join("");

  const recentRows = recent.map((g) => `
    <tr>
      <td class="cp-nowrap">${formatDay(g.session)}</td>
      <td><strong>${escapeHtml(g.symbol)}</strong></td>
      <td>${sigbLeanPill({ lean: g.lean, proven: g.proven })}</td>
      <td>${sigbHorizonShort(g.horizon_sessions)}</td>
      <td class="num">${sigbPct(g.outcome)}</td>
      <td class="num">${sigbPct(g.board_return)}</td>
      <td class="num">${sigbPct(g.excess)}</td>
      <td><span class="${g.beat_board ? "cp-word-good" : "cp-word-wait"}">${g.beat_board ? "Yes" : "No"}</span></td>
    </tr>
  `).join("");

  const recentHtml = recent.length > 0
    ? `<details class="cp-more">
        <summary>Show recent graded reads (${recent.length})</summary>
        <div class="cp-table-wrap">
          <table class="cp-table">
            <thead>
              <tr>
                <th scope="col">Date</th>
                <th scope="col">Symbol</th>
                <th scope="col">Read</th>
                <th scope="col">Horizon</th>
                <th scope="col" class="num">Outcome</th>
                <th scope="col" class="num">Board</th>
                <th scope="col" class="num">Excess</th>
                <th scope="col">Beat</th>
              </tr>
            </thead>
            <tbody>${recentRows}</tbody>
          </table>
        </div>
      </details>`
    : "";

  const notesHtml = (rec.notes || []).map((n) => `<p class="cp-note">${escapeHtml(n)}</p>`).join("");

  body.innerHTML = `
    <div class="cp-table-wrap">
      <table class="cp-table">
        <thead>
          <tr>
            <th scope="col">Read</th>
            <th scope="col">Horizon</th>
            <th scope="col" class="num">Reads</th>
            <th scope="col" class="num">Beat the board</th>
            <th scope="col" class="num">Mean excess (95% CI)</th>
          </tr>
        </thead>
        <tbody>${leanRows}</tbody>
      </table>
    </div>
    ${calRows ? `<ul class="cp-rows">${calRows}</ul>` : ""}
    ${recentHtml}
    ${notesHtml}
  `;
}

// --- CSV exports -----------------------------------------------------------------------

function downloadCsv(filename, lines) {
  const blob = new Blob([lines.join("\n") + "\n"], { type: "text/csv" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function csvCell(v) {
  if (v === null || v === undefined) return "";
  const s = String(v);
  return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

function sigbCsv() {
  if (!sigbBoard) return;
  const num = (v, digits = 4) => (Number.isFinite(v) ? v.toFixed(digits) : "");
  const allSignals = [...SIGB_SIGNALS];
  for (const row of sigbBoard.rows || []) {
    for (const r of row.readings || []) {
      if (r.signal && !allSignals.includes(r.signal)) allSignals.push(r.signal);
    }
  }

  const head = [
    "symbol", "name", "class", "held", "session", "close", "change_1m",
    "lean", "validated", "read",
    ...allSignals.flatMap((s) => [`${s}_raw`, `${s}_percentile`, `${s}_text`]),
    "outlook_1m_median", "outlook_1m_low5", "outlook_1m_high95", "outlook_1m_chance_up",
    "outlook_3m_median", "outlook_3m_low5", "outlook_3m_high95", "outlook_3m_chance_up",
    "signal_1m_score", "signal_1m_alpha", "signal_3m_score", "signal_3m_alpha",
    "setup", "setup_side", "setup_published",
  ];
  const lines = [head.join(",")];
  for (const row of sigbBoard.rows || []) {
    const o1 = sigbOutlook(row, 21);
    const o3 = sigbOutlook(row, 63);
    const c1 = (row.combined || []).find((c) => c.horizon_sessions === 20);
    const c3 = (row.combined || []).find((c) => c.horizon_sessions === 60);
    const values = [
      row.symbol, row.name, row.class, row.held, row.session, num(row.close), num(row.change_1m),
      row.read?.lean, row.read?.proven, row.read?.sentence,
      ...allSignals.flatMap((s) => {
        const r = sigbReading(row, s);
        return r ? [num(r.raw, 6), num(r.percentile, 1), r.text] : ["", "", ""];
      }),
      num(o1?.median), num(o1?.low), num(o1?.high), num(o1?.p_up, 3),
      num(o3?.median), num(o3?.low), num(o3?.high), num(o3?.p_up, 3),
      num(c1?.score, 3), num(c1?.alpha, 5), num(c3?.score, 3), num(c3?.alpha, 5),
      row.setup ? formatSetupName(row.setup.setup) : "", row.setup?.side || "", row.setup ? row.setup.published : "",
    ];
    lines.push(values.map(csvCell).join(","));
  }
  downloadCsv(`sail-signals-${sigbBoard.session || "latest"}.csv`, lines);
}

function sigbRecordCsv() {
  const recent = (sigbRecord && sigbRecord.recent) || [];
  if (recent.length === 0) return;
  const num = (v, digits = 4) => (Number.isFinite(v) ? v.toFixed(digits) : "");
  const head = [
    "session", "symbol", "held", "lean", "validated", "horizon_sessions",
    "outcome", "board_return", "excess", "beat_board", "p_up", "median", "inside_90",
  ];
  const lines = [head.join(",")];
  for (const g of recent) {
    lines.push([
      g.session, g.symbol, g.held, g.lean, g.proven, g.horizon_sessions,
      num(g.outcome), num(g.board_return), num(g.excess), g.beat_board,
      num(g.p_up, 3), num(g.median), g.inside_90 === null || g.inside_90 === undefined ? "" : g.inside_90,
    ].map(csvCell).join(","));
  }
  downloadCsv(`sail-graded-reads-${sigbRecord.first_session || "latest"}.csv`, lines);
}

// --- Load & setup ----------------------------------------------------------------------

async function loadSignalBoard() {
  const body = document.getElementById("cpSigbBody");
  const csv = document.getElementById("cpSigbCsv");
  const recBody = document.getElementById("cpSigbRecordBody");
  const recCsv = document.getElementById("cpSigbRecordCsv");

  sigbOutlookCache.clear();
  sigbGuardsCache = null;
  sigbDividendsCache = null;
  sigbInsightsCache = null;
  closeSigbDrawer(false);

  if (accountList.length === 0) {
    if (body) body.innerHTML = `<tr><td colspan="9" class="cp-empty">Import a report to see the signal board.</td></tr>`;
    if (recBody) recBody.innerHTML = `<p class="cp-empty">Import a report to see the forward record.</p>`;
    if (recCsv) recCsv.disabled = true;
    return;
  }
  if (body && !sigbBoard) body.innerHTML = `<tr><td colspan="9" class="cp-empty">Ranking your holdings and watchlist…</td></tr>`;

  const [boardRes, recordRes] = await Promise.allSettled([
    fetchJson(`/api/capital/signals/board${sigbQuery()}`),
    fetchJson(`/api/capital/signals/board/record${sigbQuery()}`),
  ]);

  if (boardRes.status === "fulfilled") {
    sigbBoard = boardRes.value;
    if (csv) csv.disabled = (sigbBoard.rows || []).length === 0;
    renderSigbSummary();
    renderSigbTable();
    renderSigbNotes();
  } else {
    sigbBoard = null;
    if (csv) csv.disabled = true;
    if (body) body.innerHTML = `<tr><td colspan="9" class="cp-empty">Couldn't load the signal board: ${escapeHtml(boardRes.reason?.message || "error")}</td></tr>`;
  }

  if (recordRes.status === "fulfilled") {
    sigbRecord = recordRes.value;
    renderBoardRecord(sigbRecord);
  } else {
    sigbRecord = null;
    if (recCsv) recCsv.disabled = true;
    if (recBody) recBody.innerHTML = `<p class="cp-empty">Couldn't load the forward record: ${escapeHtml(recordRes.reason?.message || "error")}</p>`;
  }
}

function setupSigbSeg(id, attr, apply) {
  const group = document.getElementById(id);
  if (!group) return;
  group.querySelectorAll(".cp-seg-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      group.querySelectorAll(".cp-seg-btn").forEach((b) => {
        const on = b === btn;
        b.classList.toggle("cp-seg-on", on);
        b.setAttribute("aria-checked", String(on));
      });
      apply(btn.dataset[attr]);
      renderSigbSummary();
      renderSigbTable();
    });
  });
}

registerTab("signals", {
  setup() {
    setupSigbSeg("cpSigbScope", "scope", (v) => { sigbScope = v; });
    setupSigbSeg("cpSigbLean", "lean", (v) => { sigbLeanFilter = v; });
    setupSigbSeg("cpSigbHorizon", "h", (v) => { sigbHorizon = Number(v); });
    document.querySelectorAll(".cp-sigb-sort").forEach((btn) => {
      btn.addEventListener("click", () => {
        const key = btn.dataset.sort;
        sigbSort = sigbSort.key === key ? { key, dir: -sigbSort.dir } : { key, dir: 1 };
        renderSigbTable();
      });
    });
    document.getElementById("cpSigbCsv")?.addEventListener("click", sigbCsv);
    document.getElementById("cpSigbRecordCsv")?.addEventListener("click", sigbRecordCsv);
    setupSigbDrawer();
  },
  load: loadSignalBoard,
});

// --- Round 24: card expectations, portfolio heat bar, regime chips --------------------

(function initR24TradeSignals() {
  function fmtPct(v) {
    if (typeof v !== "number" || !Number.isFinite(v)) return "—";
    return `${Math.round(v * 100)}%`;
  }

  function fmtR(v) {
    if (typeof v !== "number" || !Number.isFinite(v)) return "—";
    const sign = v > 0.005 ? "+" : v < -0.005 ? "−" : "";
    return `${sign}${Math.abs(v).toFixed(2)}R`;
  }

  function formatExpectationsLine(exp) {
    if (!exp || typeof exp !== "object") return "";
    const ci = Array.isArray(exp.mean_r_ci) && exp.mean_r_ci.length === 2
      ? ` [${fmtR(exp.mean_r_ci[0])}, ${fmtR(exp.mean_r_ci[1])}]`
      : "";
    const med = typeof exp.median_sessions === "number" ? Math.round(exp.median_sessions) : "—";
    return `Reached 1R ${fmtPct(exp.p_t1)} · 2R ${fmtPct(exp.p_t2)} · 3R ${fmtPct(exp.p_t3)} · stop ${fmtPct(exp.p_stop)} · median ${med} sessions · ${fmtR(exp.mean_r)}${ci}`;
  }

  if (typeof renderSignalCardMarkup === "function") {
    const origRenderCard = renderSignalCardMarkup;
    renderSignalCardMarkup = function (c, idPrefix = "") {
      let html = origRenderCard(c, idPrefix);
      if (!c || typeof html !== "string") return html;

      const extraLines = [];
      const expText = formatExpectationsLine(c.expectations);
      if (expText) {
        extraLines.push(
          `<p class="cp-sig-line cp-sig-expect"><span class="cp-sug-label">Expect</span> ${escapeHtml(expText)}</p>`,
        );
      }
      if (c.tier === "candidate" && typeof c.sessions_to_verdict === "number" && Number.isFinite(c.sessions_to_verdict)) {
        extraLines.push(
          `<p class="cp-sig-line cp-sig-verdict-eta">≈ ${Math.round(c.sessions_to_verdict)} trades to a verdict</p>`,
        );
      }
      if (extraLines.length > 0) {
        const marker = '<div class="cp-sig-body">';
        const idx = html.indexOf(marker);
        if (idx !== -1) {
          const insertAt = idx + marker.length;
          html = html.slice(0, insertAt) + extraLines.join("") + html.slice(insertAt);
        }
      }
      return html;
    };
  }

  function ensureContextBar() {
    const cardsGrid = document.getElementById("cpSignalsCardsGrid");
    if (!cardsGrid) return null;
    let bar = document.getElementById("cpTradeContextBar");
    if (!bar) {
      bar = document.createElement("div");
      bar.id = "cpTradeContextBar";
      bar.className = "cp-trade-context-bar";
      bar.hidden = true;
      const emptyWrap = document.getElementById("cpSignalsEmpty");
      const anchor = emptyWrap && emptyWrap.parentNode === cardsGrid.parentNode ? emptyWrap : cardsGrid;
      cardsGrid.parentNode.insertBefore(bar, anchor);
    }
    return bar;
  }

  function renderTradeHeatAndRegime(day) {
    const bar = ensureContextBar();
    if (!bar) return;
    bar.innerHTML = "";

    const heat = day && day.heat && typeof day.heat === "object" ? day.heat : null;
    const regime = day && day.regime && typeof day.regime === "object" ? day.regime : null;
    const regimeEntries = regime ? Object.entries(regime) : [];

    if (!heat && regimeEntries.length === 0) {
      bar.hidden = true;
      return;
    }
    bar.hidden = false;

    if (heat) {
      const heatWrap = document.createElement("div");
      heatWrap.className = "cp-heat-wrap";

      const used = Number(heat.total_risk_pct) || 0;
      const limit = Number(heat.limit_pct) || 3;
      const ratio = limit > 0 ? Math.min(1, Math.max(0, used / limit)) : 0;

      const labelRow = document.createElement("div");
      labelRow.className = "cp-heat-header";
      const label = document.createElement("span");
      label.className = "cp-heat-label";
      label.textContent = "Portfolio heat";
      const val = document.createElement("span");
      val.className = "cp-heat-val";
      val.textContent = `${used.toFixed(2)}% / ${limit.toFixed(1)}%`;
      labelRow.appendChild(label);
      labelRow.appendChild(val);
      heatWrap.appendChild(labelRow);

      const track = document.createElement("div");
      track.className = "cp-heat-track";
      track.setAttribute("role", "progressbar");
      track.setAttribute("aria-valuenow", used.toFixed(2));
      track.setAttribute("aria-valuemin", "0");
      track.setAttribute("aria-valuemax", limit.toFixed(1));
      const fill = document.createElement("div");
      fill.className = "cp-heat-fill";
      if (ratio >= 0.85) fill.setAttribute("data-warn", "high");
      fill.style.width = `${(ratio * 100).toFixed(1)}%`;
      track.appendChild(fill);
      heatWrap.appendChild(track);

      const clusters = Array.isArray(heat.clusters) ? heat.clusters : [];
      if (clusters.length > 0) {
        const clusterRow = document.createElement("div");
        clusterRow.className = "cp-heat-clusters";
        for (const cl of clusters) {
          const chip = document.createElement("span");
          chip.className = "cp-heat-cluster-chip";
          const keys = Array.isArray(cl.keys) ? cl.keys.join(" · ") : "Cluster";
          const cRisk = typeof cl.risk_pct === "number" ? `${cl.risk_pct.toFixed(2)}%` : "—";
          chip.textContent = `${keys}: ${cRisk}`;
          clusterRow.appendChild(chip);
        }
        heatWrap.appendChild(clusterRow);
      }

      bar.appendChild(heatWrap);
    }

    if (regimeEntries.length > 0) {
      const regWrap = document.createElement("div");
      regWrap.className = "cp-regime-chips";
      for (const [group, reg] of regimeEntries) {
        if (!reg || typeof reg !== "object") continue;
        const chip = document.createElement("span");
        chip.className = "cp-regime-chip";
        const trend = reg.trend === "down" ? "down" : "up";
        const vol = reg.vol === "stressed" ? "stressed" : "calm";
        chip.setAttribute("data-trend", trend);
        chip.setAttribute("data-vol", vol);
        const trendWord = trend === "up" ? "Uptrend" : "Downtrend";
        const volWord = vol === "calm" ? "Calm" : "Stressed";
        chip.textContent = `${group}: ${trendWord} · ${volWord}`;
        regWrap.appendChild(chip);
      }
      bar.appendChild(regWrap);
    }
  }

  if (typeof loadTradeSignals === "function") {
    const origLoadTradeSignals = loadTradeSignals;
    loadTradeSignals = async function () {
      const result = await origLoadTradeSignals.apply(this, arguments);
      renderTradeHeatAndRegime(typeof latestTradeSignals !== "undefined" ? latestTradeSignals : null);
      return result;
    };
  }
})();

// SAIL Capital — outlook.
"use strict";

let currentOutlookSymbol = "";

let currentOutlookHorizon = 21;

let currentOutlookTrim = 0;

let currentOutlookSessions = [];

const COMPONENT_LABELS = {
  gbm: "Geometric Brownian Motion (GBM)",
  ewma: "EWMA Volatility (No-Change)",
  historical: "Historical Simulation",
  chronos: "Chronos-Bolt (Pretrained Model)",
  gbdt: "Gradient Boosted Trees (GBDT)",
};

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

// Opens the outlook chart for any symbol on the signal board, holding or watchlist.
function openOutlookFor(symbol) {
  const select = document.getElementById("cpOutlookSymbolSelect");
  if (select && ![...select.options].some((o) => o.value === symbol)) {
    let group = select.querySelector("optgroup[data-watch]");
    if (!group) {
      group = document.createElement("optgroup");
      group.label = "Watchlist";
      group.dataset.watch = "1";
      select.appendChild(group);
    }
    const opt = document.createElement("option");
    opt.value = symbol;
    opt.textContent = symbol;
    group.appendChild(opt);
  }
  if (select) select.value = symbol;
  currentOutlookSymbol = symbol;
  currentOutlookTrim = 0;
  currentOutlookSessions = [];
  const replay = document.getElementById("cpOutlookReplaySelect");
  if (replay) replay.value = "0";
  switchTab("outlook", true);
  loadOutlook();
}

async function loadOutlook() {
  const wrap = document.getElementById("cpOutlookChartWrap");
  const replayBox = document.getElementById("cpOutlookReplaySummary");
  const statsBox = document.getElementById("cpOutlookStats");
  const details = document.getElementById("cpOutlookDetails");
  if (!wrap) return;

  function hideSupportingPanels() {
    if (replayBox) replayBox.hidden = true;
    if (statsBox) {
      statsBox.hidden = true;
      statsBox.innerHTML = "";
    }
    if (details) details.hidden = true;
  }

  if (!currentOutlookSymbol) {
    wrap.innerHTML = `<p class="cp-empty">Select a holding to see its blended outlook.</p>`;
    hideSupportingPanels();
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
      hideSupportingPanels();
      return;
    }

    // Check status
    const reason = getStatusReason(outlook.status);
    if (reason && (!outlook.fan || outlook.fan.length === 0)) {
      wrap.innerHTML = `
        <p class="cp-empty"><span class="cp-word-wait">Not enough data:</span> ${escapeHtml(reason)}</p>
      `;
      hideSupportingPanels();
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

    // Model detail stays out of the first reading path.
    if (details) details.hidden = false;
    renderComponentWeights(outlook.weights || []);
    renderTilt(outlook.tilt, outlook.tilt_signals);

  } catch (err) {
    wrap.innerHTML = `<p class="cp-empty">Outlook unavailable: ${escapeHtml(err.message)}</p>`;
    hideSupportingPanels();
  }
}

function renderOutlookChart(res) {
  const wrap = document.getElementById("cpOutlookChartWrap");
  if (!wrap) return;

  const outlook = res.outlook || {};
  const history = res.history || [];
  const fan = outlook.fan || [];
  const actual = outlook.actual || [];
  const narrow = window.innerWidth <= 700;

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

  // ViewBox coordinates. The narrow layout drops the end labels and gives the
  // fan the width they used, so the whole forecast fits a phone without a
  // sideways-scrolling chart.
  const W = narrow ? 520 : 820;
  const H = 360;
  const ml = narrow ? 60 : 65;
  const mr = narrow ? 24 : 135;
  const mt = 40;
  const mb = narrow ? 48 : 36;
  const pw = W - ml - mr;
  const ph = H - mt - mb;
  const yTop = mt;
  const yBottom = mt + ph;

  function yPrice(p) {
    return yBottom - ((p - pMin) / (pMax - pMin)) * ph;
  }

  const nHist = Math.max(1, history.length);
  const nFan = Math.max(1, outlook.horizon || 21);
  const histRatio = Math.min(narrow ? 0.34 : 0.45, Math.max(0.3, nHist / (nHist + nFan)));
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

    if (!narrow) {
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
  }

  // Three numbers replace the old five-card summary. The full breakdown stays
  // behind the "How this fan was built" disclosure.
  const statsBox = document.getElementById("cpOutlookStats");
  if (statsBox) {
    if (lastFan) {
      const p68Low = lastFan.prices[1];
      const p68High = lastFan.prices[3];
      const p90Low = lastFan.prices[0];
      const p90High = lastFan.prices[4];
      const pctMed = ((p50End - originClose) / originClose) * 100;
      const signMed = pctMed >= 0 ? "+" : "";
      const statsHorizon = outlook.horizon || nFan;
      statsBox.innerHTML = `
        <div class="cp-outlook-stat">
          <span class="cp-outlook-stat-label">Median at +${statsHorizon}d</span>
          <span class="cp-outlook-stat-val">${groupDecimal(p50End.toFixed(2))} <span class="cp-outlook-stat-delta">${signMed}${pctMed.toFixed(1)}%</span></span>
        </div>
        <div class="cp-outlook-stat">
          <span class="cp-outlook-stat-label">68% range</span>
          <span class="cp-outlook-stat-val">${groupDecimal(p68Low.toFixed(2))} – ${groupDecimal(p68High.toFixed(2))}</span>
        </div>
        <div class="cp-outlook-stat">
          <span class="cp-outlook-stat-label">90% range</span>
          <span class="cp-outlook-stat-val">${groupDecimal(p90Low.toFixed(2))} – ${groupDecimal(p90High.toFixed(2))}</span>
        </div>
        <p class="cp-outlook-stat-note">Dashed lines: median (50%) and mean. Bands: 68% and 90% of outcomes.</p>`;
      statsBox.hidden = false;
    } else {
      statsBox.hidden = true;
      statsBox.innerHTML = "";
    }
  }

  // Accessible summary text
  const sym = escapeHtml(outlook.symbol || currentOutlookSymbol);
  const horizonLabelMap = { 21: "1 month", 63: "3 months", 126: "6 months", 252: "1 year" };
  const hLabel = horizonLabelMap[outlook.horizon] || `${outlook.horizon} sessions`;
  const accessibleSummary = `Blended outlook fan chart for ${sym} across ${hLabel}. Origin price ${groupDecimal(originClose.toFixed(2))} on ${originDate}. Median expectation at horizon is ${groupDecimal(p50End.toFixed(2))}. 68% range is ${groupDecimal((lastFan ? lastFan.prices[1] : 0).toFixed(2))} to ${groupDecimal((lastFan ? lastFan.prices[3] : 0).toFixed(2))}; 90% range is ${groupDecimal(p5End.toFixed(2))} to ${groupDecimal(p95End.toFixed(2))}.`;

  const svgHtml = `
    <svg viewBox="0 0 ${W} ${H}" width="100%" style="height:auto;display:block" role="img" aria-labelledby="cpFanTitle cpFanDesc" xmlns="http://www.w3.org/2000/svg">
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
      <text x="${(xLeft + 6).toFixed(1)}" y="${(yOrigin - 6).toFixed(1)}" class="cp-chart-axis-text" fill="var(--sail-text-subtle)" stroke="rgba(10, 13, 18, 0.9)" stroke-width="3" paint-order="stroke">Origin: ${groupDecimal(originClose.toFixed(2))}</text>

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
    </svg>
    <p class="cp-sr-only">${escapeHtml(accessibleSummary)}</p>
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
      if (document.getElementById("cpOutlookDetails")?.open) loadTrackRecord();
    });
  });

  const replaySelect = document.getElementById("cpOutlookReplaySelect");
  if (replaySelect) {
    replaySelect.addEventListener("change", (e) => {
      currentOutlookTrim = parseInt(e.target.value, 10) || 0;
      loadOutlook();
    });
  }
}

let latestTradeSignals = null;

function formatTradePrice(val) {
  if (val === null || val === undefined || isNaN(val)) return "—";
  const num = Number(val);
  const abs = Math.abs(num);
  if (abs === 0) return "0.00";
  if (abs < 0.001) return num.toFixed(6);
  if (abs < 10) {
    const s = String(num);
    if (s.includes(".") && s.split(".")[1].length > 2) {
      return num.toFixed(Math.min(5, s.split(".")[1].length));
    }
  }
  return num.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 4 });
}

function formatDistPct(targetPrice, entryMid) {
  if (!entryMid || !targetPrice) return "0.0";
  const pct = ((targetPrice - entryMid) / entryMid) * 100;
  return (pct >= 0 ? "+" : "") + pct.toFixed(1);
}

function formatCardSize(size, riskPct) {
  const rPct = String(Number((riskPct !== undefined && riskPct !== null ? Number(riskPct) : 0.5).toFixed(2)));
  if (size && Array.isArray(size) && size.length >= 2) {
    const units = Number(size[0]).toLocaleString(undefined, { maximumFractionDigits: 2 });
    // The value is a plain number in the account currency, not a Money object.
    const value = Number(size[1]).toLocaleString(undefined, { maximumFractionDigits: 0 });
    return `${units} units (worth ≈ ${value}) at ${rPct}% risk`;
  }
  return `At ${rPct}% risk to stop (account size pending)`;
}

function formatEvidenceLine(verdict) {
  if (!verdict) return "No evidence record available.";
  const sel = verdict.selection || {};
  const j = verdict.judgement;
  if (j) {
    const pooled = j.pooled || {};
    const wf = j.walk_forward || {};
    const live = j.live || {};
    const postMean = Number(j.posterior_mean ?? pooled.posterior_mean ?? 0);
    const postSd = Number(j.posterior_sd ?? pooled.posterior_sd ?? 0);
    const margin = 1.96 * postSd;
    const postStr = `${postMean >= 0 ? "+" : "−"}${Math.abs(postMean).toFixed(2)}R ± ${margin.toFixed(2)}R`;
    const trades = sel.trades || 0;
    const oosR = Number(wf.oos_mean_r || 0);
    const oosStr = `${oosR >= 0 ? "+" : "−"}${Math.abs(oosR).toFixed(2)}R`;
    const oosTrades = wf.oos_trades || 0;
    const liveR = Number(live.mean_r || 0);
    const liveStr = live.trades > 0
      ? `${liveR >= 0 ? "+" : "−"}${Math.abs(liveR).toFixed(2)}R over ${live.trades}`
      : "none yet";
    return `${postStr} pooled over ${trades} trades · out of sample ${oosStr} over ${oosTrades} · live ${liveStr}`;
  }
  const hold = verdict.holdout || {};

  const hitPct = ((sel.hit_target1 || 0) * 100).toFixed(0);
  const trades = sel.trades || 0;
  const ciMin = Array.isArray(sel.hit_target1_ci) ? (sel.hit_target1_ci[0] * 100).toFixed(0) : "0";
  const ciMax = Array.isArray(sel.hit_target1_ci) ? (sel.hit_target1_ci[1] * 100).toFixed(0) : "0";

  const expR = Number(sel.expectancy_r || 0);
  const expRStr = (expR >= 0 ? "+" : "") + expR.toFixed(2);

  const expFrom = Array.isArray(sel.expectancy_ci) ? Number(sel.expectancy_ci[0] || 0) : 0;
  const expFromStr = (expFrom >= 0 ? "+" : "") + expFrom.toFixed(2);

  const holdR = Number(hold.expectancy_r || 0);
  const holdRStr = (holdR >= 0 ? "+" : "") + holdR.toFixed(2);
  const holdTrades = hold.trades || 0;

  return `Reached target 1 in ${hitPct}% of ${trades} past trades (95%: ${ciMin}–${ciMax}%) · ${expRStr}R a trade after costs (from ${expFromStr}R) · holdout ${holdRStr}R over ${holdTrades}`;
}

function renderMiniSvgChart(card, closes) {
  const width = 340;
  const height = 120;
  const padTop = 18;
  const padBottom = 22;
  const padLeft = 8;
  const padRight = 50;

  const entryLow = Math.min(card.entry_zone[0], card.entry_zone[1]);
  const entryHigh = Math.max(card.entry_zone[0], card.entry_zone[1]);
  const entryMid = (entryLow + entryHigh) / 2;

  let allPrices = [entryLow, entryHigh, card.stop, ...card.targets];
  if (Array.isArray(closes) && closes.length > 0) {
    allPrices = allPrices.concat(closes);
  }

  let minP = Math.min(...allPrices);
  let maxP = Math.max(...allPrices);
  const range = maxP - minP || 1;
  minP -= range * 0.08;
  maxP += range * 0.08;

  const y = (p) => {
    return padTop + (1 - (p - minP) / (maxP - minP)) * (height - padTop - padBottom);
  };

  const yEntryLow = y(entryLow);
  const yEntryHigh = y(entryHigh);
  const yEntryMid = y(entryMid);
  const yStop = y(card.stop);
  const yT1 = y(card.targets[0]);
  const yT2 = y(card.targets[1]);
  const yT3 = y(card.targets[2]);

  let svg = `<svg class="cp-sparkline-svg" viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" role="img" aria-label="Signal price levels for ${card.symbol}">`;

  // Entry zone shaded band
  const topY = Math.min(yEntryLow, yEntryHigh);
  const bandHeight = Math.max(Math.abs(yEntryLow - yEntryHigh), 2);
  svg += `<rect x="${padLeft}" y="${topY.toFixed(1)}" width="${width - padLeft - padRight}" height="${bandHeight.toFixed(1)}" fill="rgba(255, 255, 255, 0.08)" rx="2" />`;

  // Horizontal guide lines & labels
  // Stop Loss line (red)
  svg += `<line x1="${padLeft}" y1="${yStop.toFixed(1)}" x2="${width - padRight}" y2="${yStop.toFixed(1)}" stroke="#ef4444" stroke-width="1.2" stroke-dasharray="3,3" />`;
  svg += `<text x="${width - padRight + 4}" y="${(yStop + 3.5).toFixed(1)}" fill="#ef4444" font-size="9" font-family="monospace">SL</text>`;

  // Entry line (neutral)
  svg += `<line x1="${padLeft}" y1="${yEntryMid.toFixed(1)}" x2="${width - padRight}" y2="${yEntryMid.toFixed(1)}" stroke="rgba(255,255,255,0.4)" stroke-width="1" stroke-dasharray="2,2" />`;
  svg += `<text x="${width - padRight + 4}" y="${(yEntryMid + 3.5).toFixed(1)}" fill="rgba(255,255,255,0.6)" font-size="9" font-family="monospace">Entry</text>`;

  // Targets (green)
  svg += `<line x1="${padLeft}" y1="${yT1.toFixed(1)}" x2="${width - padRight}" y2="${yT1.toFixed(1)}" stroke="#22c55e" stroke-width="1" stroke-dasharray="2,2" opacity="0.8" />`;
  svg += `<text x="${width - padRight + 4}" y="${(yT1 + 3.5).toFixed(1)}" fill="#22c55e" font-size="9" font-family="monospace">TP1</text>`;

  svg += `<line x1="${padLeft}" y1="${yT2.toFixed(1)}" x2="${width - padRight}" y2="${yT2.toFixed(1)}" stroke="#22c55e" stroke-width="1" stroke-dasharray="2,2" opacity="0.6" />`;
  svg += `<text x="${width - padRight + 4}" y="${(yT2 + 3.5).toFixed(1)}" fill="#22c55e" font-size="9" font-family="monospace">TP2</text>`;

  svg += `<line x1="${padLeft}" y1="${yT3.toFixed(1)}" x2="${width - padRight}" y2="${yT3.toFixed(1)}" stroke="#22c55e" stroke-width="1" stroke-dasharray="2,2" opacity="0.4" />`;
  svg += `<text x="${width - padRight + 4}" y="${(yT3 + 3.5).toFixed(1)}" fill="#22c55e" font-size="9" font-family="monospace">TP3</text>`;

  // Price closes path if available
  if (Array.isArray(closes) && closes.length > 1) {
    const xStep = (width - padLeft - padRight) / (closes.length - 1);
    let pathD = "";
    closes.forEach((c, idx) => {
      const cx = padLeft + idx * xStep;
      const cy = y(c);
      pathD += (idx === 0 ? "M" : " L") + `${cx.toFixed(1)},${cy.toFixed(1)}`;
    });
    svg += `<path d="${pathD}" fill="none" stroke="#60a5fa" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />`;
    const lastX = padLeft + (closes.length - 1) * xStep;
    const lastY = y(closes[closes.length - 1]);
    svg += `<circle cx="${lastX.toFixed(1)}" cy="${lastY.toFixed(1)}" r="3" fill="#60a5fa" stroke="#0f172a" stroke-width="1" />`;
  }

  svg += `</svg>`;
  return svg;
}


function signalLevel(label, price, entryMid) {
  const dist = entryMid ? ` <span class="cp-sig-dist">${formatDistPct(price, entryMid)}%</span>` : "";
  return `<div class="cp-sig-level"><dt>${label}</dt><dd>${formatTradePrice(price)}${dist}</dd></div>`;
}

function renderSignalCardMarkup(c, idPrefix = "") {
  const entryLow = Math.min(c.entry_zone[0], c.entry_zone[1]);
  const entryHigh = Math.max(c.entry_zone[0], c.entry_zone[1]);
  const entryMid = (entryLow + entryHigh) / 2;
  const meta = [formatSetupName(c.setup), `up to ${c.max_sessions} sessions`];
  if (c.needs_cfd) meta.push("trades as a CFD");
  const warnings = (c.warnings || [])
    .map((w) => `<p class="cp-sig-line cp-word-warn">${escapeHtml(w)}</p>`)
    .join("");

  return `<li class="cp-sig" id="${idPrefix}card-${c.id}">
      <details>
        <summary>
          <span class="cp-sig-title">${escapeHtml(c.symbol)} <span class="${c.side === "long" ? "cp-word-good" : "cp-word-bad"}">${signalSideLabel(c.side)}</span>${c.tier === "candidate" ? ` <span class="cp-sig-tier" title="Positive after costs in both periods, not yet clear of luck">Candidate · unproven</span>` : ""}</span>
          <span class="cp-sig-entry">entry ${formatTradePrice(entryLow)} – ${formatTradePrice(entryHigh)}</span>
          <span class="cp-sig-meta">${escapeHtml(meta.join(" · "))}</span>
        </summary>
        <div class="cp-sig-body">
          <dl class="cp-sig-levels">
            <div class="cp-sig-level"><dt>Entry zone</dt><dd>${formatTradePrice(entryLow)} – ${formatTradePrice(entryHigh)}</dd></div>
            ${signalLevel("Stop", c.stop, entryMid)}
            ${signalLevel("Target 1 (1R)", c.targets[0], entryMid)}
            ${signalLevel("Target 2 (2R)", c.targets[1], entryMid)}
            ${signalLevel("Target 3 (3R)", c.targets[2], entryMid)}
          </dl>
          <p class="cp-sig-line"><span class="cp-sug-label">Size</span> ${escapeHtml(formatCardSize(c.size, c.risk_pct))}</p>
          <p class="cp-sig-line"><span class="cp-sug-label">Record</span> ${escapeHtml(formatEvidenceLine(c.verdict))}</p>
          ${c.relation ? `<p class="cp-sig-line">${escapeHtml(c.relation)}</p>` : ""}
          ${warnings}
          <div class="cp-sig-chart" id="chart-wrap-${idPrefix}${c.id}">${renderMiniSvgChart(c, null)}</div>
        </div>
      </details>
    </li>`;
}

async function loadTradeSignals() {
  const cardsGrid = document.getElementById("cpSignalsCardsGrid");
  const emptyWrap = document.getElementById("cpSignalsEmpty");
  const metaSpan = document.getElementById("cpSignalsSessionMeta");

  if (!cardsGrid) return;

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const raw = await fetchJson(`/api/capital/signals/trade${query}`);
    // The route returns {day: SignalDay | null, run_at, next_run_at, notes}; the rest of
    // this view reads the day's fields directly.
    const res = Object.assign({}, (raw && raw.day) || {}, {
      run_at: raw && raw.run_at,
      next_run_at: raw && raw.next_run_at,
      view_notes: (raw && raw.notes) || [],
      running: !!(raw && raw.running),
      last_error: raw && raw.last_error,
    });
    latestTradeSignals = res;

    const sessionDate = res.session && res.session !== "1970-01-01" && !res.session.startsWith("-") ? formatDay(res.session) : "—";
    const lastRun = res.run_at ? formatDate(res.run_at) : (res.session ? formatDate(res.session) : "—");
    if (metaSpan) {
      metaSpan.textContent = `Session ${sessionDate} · last run ${lastRun}`;
    }

    const cards = Array.isArray(res.cards) ? res.cards : [];
    const candidates = Array.isArray(res.candidates) ? res.candidates : [];
    if (emptyWrap) {
      emptyWrap.hidden = cards.length > 0;
      emptyWrap.textContent = candidates.length > 0
        ? `No setup has earned a verified signal today. ${candidates.length === 1 ? "One candidate is" : `${candidates.length} candidates are`} below: positive after costs, not yet clear of luck.`
        : "No setup has earned a signal today, and none fired as a candidate. That's the honest answer, not a fault.";
    }
    cardsGrid.innerHTML = cards.map((c) => renderSignalCardMarkup(c)).join("");
    const candWrap = document.getElementById("cpSignalsCandidates");
    const candGrid = document.getElementById("cpSignalsCandidatesGrid");
    const candCount = document.getElementById("cpSignalsCandidatesCount");
    if (candWrap && candGrid) {
      candWrap.hidden = candidates.length === 0;
      candGrid.innerHTML = candidates.map((c) => renderSignalCardMarkup(c)).join("");
      if (candCount) candCount.textContent = candidates.length ? `· ${candidates.length}` : "";
    }
    // Each card's chart gains its recent closes once they arrive.
    cards.concat(candidates).forEach((c) => {
      fetchJson(`/api/capital/prices?symbol=${encodeURIComponent(c.symbol)}`)
        .then((rows) => {
          if (!Array.isArray(rows) || rows.length === 0) return;
          const wrap = document.getElementById(`chart-wrap-${c.id}`);
          if (wrap) wrap.innerHTML = renderMiniSvgChart(c, rows.slice(-60).map((r) => r.close));
        })
        .catch(() => {});
    });

    renderScoreboard(Array.isArray(res.scoreboard) ? res.scoreboard : []);
  } catch (err) {
    if (cardsGrid) {
      cardsGrid.innerHTML = `<li class="cp-empty">Signals unavailable: ${escapeHtml(err.message)}</li>`;
    }
  }
}

// Asset search and a one-off signal check.
let signalSearchRows = [];

let signalSearchController = null;

let signalCheckController = null;

function setupSignalSearchControls() {
  const form = document.getElementById("cpSignalSearchForm");
  const input = document.getElementById("cpSignalSearchInput");
  if (!form || !input) return;

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    runSignalSearch(input.value);
  });

  let timer = null;
  input.addEventListener("input", () => {
    clearTimeout(timer);
    const query = input.value.trim();
    if (query.length < 2) {
      clearSignalSearchResults();
      return;
    }
    timer = setTimeout(() => runSignalSearch(query), 350);
  });

  input.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      clearSignalSearchResults();
      input.blur();
    }
  });
}

function clearSignalSearchResults() {
  const status = document.getElementById("cpSignalSearchStatus");
  const results = document.getElementById("cpSignalSearchResults");
  const check = document.getElementById("cpSignalCheck");
  if (signalSearchController) signalSearchController.abort();
  if (signalCheckController) signalCheckController.abort();
  signalSearchRows = [];
  if (status) status.textContent = "";
  if (results) {
    results.hidden = true;
    results.innerHTML = "";
  }
  if (check) {
    check.hidden = true;
    check.innerHTML = "";
  }
}

async function runSignalSearch(rawQuery) {
  const status = document.getElementById("cpSignalSearchStatus");
  const results = document.getElementById("cpSignalSearchResults");
  const check = document.getElementById("cpSignalCheck");
  if (!status || !results) return;

  const query = (rawQuery || "").trim();
  if (query.length < 2) {
    clearSignalSearchResults();
    return;
  }
  if (signalSearchController) signalSearchController.abort();
  signalSearchController = new AbortController();
  if (signalCheckController) {
    signalCheckController.abort();
    signalCheckController = null;
  }
  if (check) {
    check.hidden = true;
    check.innerHTML = "";
  }
  status.textContent = "Searching Yahoo Finance…";
  status.hidden = false;

  try {
    const rows = await fetchJson(
      `/api/capital/signals/trade/search?q=${encodeURIComponent(query)}`,
      { signal: signalSearchController.signal }
    );
    signalSearchRows = Array.isArray(rows) ? rows : [];
    renderSignalSearchResults();
    status.textContent = signalSearchRows.length
      ? "Pick an asset to check its setups."
      : "No matching asset found.";
  } catch (err) {
    if (err && err.name === "AbortError") return;
    signalSearchRows = [];
    results.hidden = true;
    results.innerHTML = "";
    status.textContent = `Search failed: ${err.message}`;
  }
}

function renderSignalSearchResults() {
  const results = document.getElementById("cpSignalSearchResults");
  if (!results) return;
  if (signalSearchRows.length === 0) {
    results.hidden = true;
    results.innerHTML = "";
    return;
  }
  results.hidden = false;
  results.innerHTML = signalSearchRows.map((row, index) => `
    <li class="cp-signal-search-result">
      <button type="button" class="cp-signal-search-pick" data-index="${index}">
        <span class="cp-signal-search-symbol">${escapeHtml(row.symbol)}</span>
        <span class="cp-signal-search-name">${escapeHtml(row.name || row.symbol)}</span>
        <span class="cp-signal-search-meta">${escapeHtml(row.exchange || "")} · ${escapeHtml((row.class || "").toUpperCase())}</span>
      </button>
    </li>
  `).join("");
  results.querySelectorAll(".cp-signal-search-pick").forEach((button) => {
    button.addEventListener("click", () => {
      const row = signalSearchRows[Number(button.dataset.index)];
      if (row) checkSignalFor(row);
    });
  });
}

async function checkSignalFor(row) {
  const status = document.getElementById("cpSignalSearchStatus");
  const results = document.getElementById("cpSignalSearchResults");
  const check = document.getElementById("cpSignalCheck");
  if (!check) return;

  if (signalCheckController) signalCheckController.abort();
  signalCheckController = new AbortController();
  if (signalSearchController) {
    signalSearchController.abort();
    signalSearchController = null;
  }
  if (results) results.hidden = true;
  if (status) status.textContent = "";
  check.hidden = false;
  check.innerHTML = `<p class="cp-signal-check-loading" role="status">Fetching daily bars and checking ${escapeHtml(row.symbol)} against the verified setups…</p>`;

  const params = new URLSearchParams({
    symbol: row.symbol,
    yahoo: row.symbol,
    name: row.name || row.symbol,
    exchange: row.exchange || "",
    class: row.class || "stock",
  });
  if (currentAccount) params.set("account", currentAccount);

  try {
    const result = await fetchJson(`/api/capital/signals/trade/check?${params.toString()}`, {
      signal: signalCheckController.signal,
    });
    renderSignalCheck(result);
  } catch (err) {
    if (err && err.name === "AbortError") return;
    check.innerHTML = `<p class="cp-form-error">The check failed: ${escapeHtml(err.message)}</p>`;
  }
}

function renderSignalCheck(check) {
  const wrap = document.getElementById("cpSignalCheck");
  if (!wrap) return;
  const cards = Array.isArray(check.cards) ? check.cards : [];
  const candidates = Array.isArray(check.candidates) ? check.candidates : [];
  const suppressed = Array.isArray(check.suppressed) ? check.suppressed : [];
  const verdicts = Array.isArray(check.verdicts) ? check.verdicts : [];
  const closes = (check.recent_closes || []).map((pair) => pair[1]);
  const publishing = verdicts.filter((verdict) => verdict.publishes).length;
  const heading = check.name && check.name !== check.symbol
    ? `${escapeHtml(check.symbol)} · ${escapeHtml(check.name)}`
    : escapeHtml(check.symbol);
  const session = check.session ? formatDay(check.session) : "—";

  let html = `
    <div class="cp-signal-check-head">
      <div class="cp-signal-check-title">
        <strong>${heading}</strong>
        <span class="cp-signal-check-meta">${escapeHtml(check.exchange || "")}${check.currency ? ` · ${escapeHtml(check.currency)}` : ""} · ${escapeHtml((check.class || "").toUpperCase())} · ${check.bars_used || 0} bars · latest ${session} · close ${formatTradePrice(check.close)}</span>
      </div>
      <span class="cp-signal-check-meta">${[check.held ? "You hold it" : "", check.on_watchlist ? "On the watchlist" : ""].filter(Boolean).join(" · ")}</span>
    </div>`;

  if (cards.length > 0) {
    html += `<p class="cp-signal-check-result cp-signal-check-yes">Potential verified signal on ${escapeHtml(check.symbol)}.</p>`;
    html += `<ul class="cp-rows">${cards.map((card) => renderSignalCardMarkup(card, "check-")).join("")}</ul>`;
  } else {
    html += `<p class="cp-signal-check-result">No verified setup fires today for ${escapeHtml(check.symbol)}.</p>`;
    if (suppressed.length === 0 && candidates.length === 0) {
      html += `<p class="cp-signal-check-sub">None of the six setups fired on its latest session.</p>`;
    }
  }

  if (candidates.length > 0) {
    html += `<p class="cp-signal-check-sub">${candidates.length === 1 ? "A candidate fires" : `${candidates.length} candidates fire`}: positive after costs in both periods, not yet clear of luck.</p>`;
    html += `<ul class="cp-rows">${candidates.map((card) => renderSignalCardMarkup(card, "check-")).join("")}</ul>`;
  }

  if (suppressed.length > 0) {
    html += `
      <div class="cp-table-wrap cp-signal-check-suppressed">
        <table class="cp-table">
          <thead><tr><th scope="col">Setup</th><th scope="col">Side</th><th scope="col">Why it didn't publish</th></tr></thead>
          <tbody>
            ${suppressed.map((item) => `
              <tr>
                <td>${escapeHtml(formatSetupName(item.setup))}</td>
                <td>${escapeHtml(signalSideLabel(item.side))}</td>
                <td>${escapeHtml(item.reason || "—")}</td>
              </tr>`).join("")}
          </tbody>
        </table>
      </div>`;
  }

  html += `<p class="cp-signal-check-evidence">${verdicts.length === 0
    ? `The latest run has no evidence for ${escapeHtml(check.class || "")} assets. Run the signals again.`
    : publishing > 0
      ? `${publishing} of ${verdicts.length} ${escapeHtml(check.class || "")} setups have passed validation.`
      : `No ${escapeHtml(check.class || "")} setup has passed validation yet.`}</p>`;

  if (check.notes && check.notes.length > 0) {
    html += check.notes.map((note) => `<p class="cp-note">${escapeHtml(note)}</p>`).join("");
  }

  wrap.innerHTML = html;
  cards.concat(candidates).forEach((card) => {
    const chart = document.getElementById(`chart-wrap-check-${card.id}`);
    if (chart) chart.innerHTML = renderMiniSvgChart(card, closes.length ? closes : null);
  });
}

function setupSignalsControls() {
  const runBtn = document.getElementById("cpSignalsRunBtn");
  if (runBtn) {
    runBtn.addEventListener("click", async () => {
      runBtn.disabled = true;
      runBtn.textContent = "Computing signals…";
      setState("busy", "Running trade signals (a few minutes)…");
      const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
      try {
        // The run takes minutes, longer than the tunnel keeps a request open, so the
        // server runs it in the background and we poll until it's done.
        let view = await fetchJson(`/api/capital/signals/trade/run${query}`, { method: "POST" });
        const started = Date.now();
        while (view && view.running && Date.now() - started < 20 * 60 * 1000) {
          const mins = Math.floor((Date.now() - started) / 60000);
          runBtn.textContent = `Computing signals… ${mins} min`;
          await new Promise(resolve => setTimeout(resolve, 5000));
          view = await fetchJson(`/api/capital/signals/trade${query}`);
        }
        if (view && view.last_error) {
          showBanner(`Signals run failed: ${view.last_error}`, true);
        } else if (view && view.running) {
          showBanner("Signals are still computing; they'll show when the run finishes.");
        } else {
          const n = view && view.day && view.day.cards ? view.day.cards.length : 0;
          showBanner(`Signals computed for session ${(view && view.day && view.day.session) || "today"}: ${n} signal${n === 1 ? "" : "s"}.`);
        }
        await loadTradeSignals();
        await loadTradeRecord();
        await loadHoldingGuards();
      } catch (err) {
        showBanner(`Signals run failed: ${err.message}`, true);
      } finally {
        runBtn.disabled = false;
        runBtn.textContent = "Run now";
        setState("idle", "Ready");
      }
    });
  }
}

registerTab("outlook", {
  setup() {
    // The replay track record sits inside "Show how this range was built": fetch it
    // only when that opens.
    document.getElementById("cpOutlookDetails")?.addEventListener("toggle", (e) => {
      if (e.target.open) loadTrackRecord();
    });
    setupOutlookControls();
    setupSignalsControls();
    setupSignalSearchControls();
  },
  load: () => Promise.all([loadOutlook(), loadTradeSignals()]),
});

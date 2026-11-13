// SAIL Capital — record.
"use strict";

let currentHorizon = 20;

let currentModel = "no_change";

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

function formatSignalName(sig) {
  switch (sig) {
    case "momentum12_1":
    case "momentum_12_1": return "12-1 Momentum";
    case "short_term_reversal": return "Short-term reversal";
    case "low_volatility": return "Low volatility";
    case "trend": return "Trend (200d)";
    case "chart_pattern": return "Chart patterns";
    case "foundation_forecast": return "Foundation forecast";
    case "filing_sentiment": return "Filing sentiment";
    case "insider_buying": return "Insider buying";
    case "chart_trend": return "Chart trend (YOLO)";
    default: return sig ? sig.replace(/_/g, " ") : "—";
  }
}

async function loadSignals() {
  const tbody = document.getElementById("cpSignalsBody");
  const notesWrap = document.getElementById("cpSignalsNotes");
  if (!tbody) return;

  if (accountList.length === 0) {
    tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">Import a report to see signal skill.</td></tr>`;
    if (notesWrap) notesWrap.hidden = true;
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const report = await fetchJson(`/api/capital/signals${query}`);

    if (!report.skill || report.skill.length === 0) {
      tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">No signal report yet.</td></tr>`;
      if (notesWrap) notesWrap.hidden = true;
      return;
    }

    tbody.innerHTML = report.skill.map(s => {
      const isDimmed = s.weight === 0 || s.ic_used === 0;
      const icStr = s.ic_mean >= 0 ? `+${s.ic_mean.toFixed(3)}` : s.ic_mean.toFixed(3);
      const tStr = s.t_stat >= 0 ? `+${s.t_stat.toFixed(2)}` : s.t_stat.toFixed(2);
      const icUsedStr = s.ic_used >= 0 ? `+${s.ic_used.toFixed(3)}` : s.ic_used.toFixed(3);
      const weightStr = `${(s.weight * 100).toFixed(1)}%`;

      return `
        <tr class="${isDimmed ? "cp-signal-dimmed" : ""}">
          <td><strong>${escapeHtml(formatSignalName(s.signal))}</strong></td>
          <td>${s.horizon_sessions} sessions</td>
          <td class="num">${icStr}</td>
          <td class="num">${tStr}</td>
          <td class="num">${icUsedStr}</td>
          <td class="num">${weightStr}</td>
        </tr>
      `;
    }).join("");

    if (notesWrap) {
      if (report.notes && report.notes.length > 0) {
        notesWrap.hidden = false;
        notesWrap.innerHTML = report.notes.map(n => `<div class="cp-scorecard-note">${escapeHtml(n)}</div>`).join("");
      } else {
        notesWrap.hidden = true;
        notesWrap.innerHTML = "";
      }
    }
  } catch (_) {
    tbody.innerHTML = `<tr><td colspan="6" class="cp-empty">No signal report yet.</td></tr>`;
    if (notesWrap) notesWrap.hidden = true;
  }
}

function formatSetupName(setup) {
  switch (setup) {
    case "trend_pullback":
    case "TrendPullback":
      return "Trend Pullback";
    case "squeeze_breakout":
    case "SqueezeBreakout":
      return "Squeeze Breakout";
    case "donchian55":
    case "Donchian55":
      return "Donchian 55";
    case "bollinger_reversion":
    case "BollingerReversion":
      return "Bollinger Reversion";
    case "inside_bar_break":
    case "InsideBarBreak":
      return "Inside Bar Break";
    case "three_day_reversal":
    case "ThreeDayReversal":
      return "3-Day Reversal";
    default:
      return setup ? String(setup).replace(/([A-Z])/g, " $1").replace(/_/g, " ").trim() : "—";
  }
}

async function loadTradeRecord() {
  const closedEl = document.getElementById("cpRecordTotalClosed");
  const hitT1El = document.getElementById("cpRecordTotalHitT1");
  const meanREl = document.getElementById("cpRecordTotalMeanR");
  const cumREl = document.getElementById("cpRecordTotalCumR");
  const tbody = document.getElementById("cpRecordBody");

  if (!tbody) return;

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const res = await fetchJson(`/api/capital/signals/trade/record${query}`);

    const totals = res.totals || {};
    if (closedEl) closedEl.textContent = totals.closed !== undefined ? String(totals.closed) : "0";
    if (hitT1El) {
      hitT1El.textContent = totals.hit_target1 !== undefined ? `${(totals.hit_target1 * 100).toFixed(1)}%` : "—";
    }
    if (meanREl) {
      const m = Number(totals.mean_r || 0);
      meanREl.textContent = totals.mean_r !== undefined ? `${m >= 0 ? "+" : ""}${m.toFixed(2)}R` : "—";
      meanREl.className = `cp-stat-val ${m >= 0 ? "cp-val-pos" : "cp-val-neg"}`;
    }
    if (cumREl) {
      const c = Number(totals.cumulative_r || 0);
      cumREl.textContent = totals.cumulative_r !== undefined ? `${c >= 0 ? "+" : ""}${c.toFixed(2)}R` : "—";
      cumREl.className = `cp-stat-val ${c >= 0 ? "cp-val-pos" : "cp-val-neg"}`;
    }

    const cards = Array.isArray(res.cards) ? res.cards : [];
    if (cards.length === 0) {
      tbody.innerHTML = `<tr><td colspan="5" class="cp-empty">No past signals recorded yet.</td></tr>`;
      return;
    }

    tbody.innerHTML = cards.map(item => {
      const c = item.card || {};
      const status = item.status || "open";
      const graded = item.graded;

      let exitLabel = status;
      if (status === "closed" && graded && graded.exit) {
        exitLabel = graded.exit.toUpperCase();
      }

      let rDisplay = "—";
      let rClass = "";
      if (status === "closed" && graded && graded.r_multiple !== undefined) {
        const r = Number(graded.r_multiple);
        rDisplay = `${r >= 0 ? "+" : ""}${r.toFixed(2)}R`;
        rClass = r >= 0 ? "cp-r-pos" : "cp-r-neg";
      }

      const sideBadge = c.side === "long" ? "cp-side-long" : "cp-side-short";

      return `
        <tr>
          <td>${formatDate(c.signal_session)}</td>
          <td><strong>${escapeHtml(c.symbol || "—")}</strong></td>
          <td><span class="cp-badge ${sideBadge}">${(c.side || "").toUpperCase()}</span></td>
          <td><span class="cp-badge cp-badge-${status}">${escapeHtml(exitLabel)}</span></td>
          <td class="num ${rClass}"><strong>${rDisplay}</strong></td>
        </tr>
      `;
    }).join("");
  } catch (err) {
    tbody.innerHTML = `<tr><td colspan="5" class="cp-empty">Record data unavailable (${escapeHtml(err.message)}).</td></tr>`;
  }
}

registerTab("record", {
  load: () =>
    Promise.all([loadTradeSignals(), loadTradeRecord(), loadSignals(), loadForecasts(), loadEvaluation()]),
});

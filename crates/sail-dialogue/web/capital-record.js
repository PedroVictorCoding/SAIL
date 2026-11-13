// SAIL Capital — Track record: has anything earned trust, and the evidence behind it.
"use strict";

const STANDING_WORDS = {
  earned: ["Earned", "cp-word-good"],
  not_yet: ["Not yet", "cp-word-wait"],
  failed: ["Failed", "cp-word-bad"],
};

function accountQuery(prefix = "?") {
  return currentAccount ? `${prefix}account=${encodeURIComponent(currentAccount)}` : "";
}

function signed(value, digits = 2, suffix = "") {
  const n = Number(value || 0);
  return `${n >= 0 ? "+" : "−"}${Math.abs(n).toFixed(digits)}${suffix}`;
}

function pct(value, digits = 0) {
  return `${(Number(value || 0) * 100).toFixed(digits)}%`;
}

function horizonLabel(sessions) {
  if (!sessions) return "";
  const named = { 5: "1 week", 20: "1 month", 21: "1 month", 60: "1 quarter", 63: "3 months", 126: "6 months", 252: "1 year" };
  return named[sessions] || `${sessions} sessions`;
}

function renderNotes(container, notes) {
  if (!container) return;
  container.innerHTML = (notes || []).map((n) => `<p class="cp-note">${escapeHtml(n)}</p>`).join("");
}

// --- The scorecard ------------------------------------------------------------------

async function loadScorecard() {
  const summary = document.getElementById("cpScorecardSummary");
  const list = document.getElementById("cpScorecardRows");
  if (!list) return;
  if (accountList.length === 0) {
    if (summary) summary.textContent = "Import a report to build a track record.";
    list.innerHTML = "";
    return;
  }
  try {
    const card = await fetchJson(`/api/capital/scorecard${accountQuery()}`);
    if (summary) summary.textContent = card.summary || "";
    const rows = Array.isArray(card.rows) ? card.rows : [];
    if (rows.length === 0) {
      list.innerHTML = `<li class="cp-empty">Nothing has been evaluated yet. Evaluations need daily prices (Settings → Prices) and a signals run (Outlook).</li>`;
      return;
    }
    list.innerHTML = rows.map((row) => {
      const [word, cls] = STANDING_WORDS[row.standing] || [row.standing, "cp-word-wait"];
      const horizon = row.horizon_sessions ? ` · ${horizonLabel(row.horizon_sessions)}` : "";
      return `<li class="cp-row" data-standing="${escapeHtml(row.standing)}">
          <span class="cp-row-title">${escapeHtml(row.name)}<span class="cp-row-horizon">${escapeHtml(horizon)}</span></span>
          <span class="cp-row-value ${cls}">${word}</span>
          <span class="cp-row-meta">${escapeHtml(row.headline)}</span>
        </li>`;
    }).join("");
  } catch (err) {
    if (summary) summary.textContent = "The track record is unavailable.";
    list.innerHTML = `<li class="cp-empty">${escapeHtml(err.message)}</li>`;
  }
}

// --- Trade setups (from the latest signals run) --------------------------------------

function renderScoreboard(verdicts) {
  const body = document.getElementById("cpScoreboardBody");
  const count = document.getElementById("cpScoreboardCount");
  if (count) {
    const passing = verdicts.filter((v) => v.publishes).length;
    count.textContent = verdicts.length ? `· ${passing} of ${verdicts.length} pass` : "";
  }
  if (!body) return;
  if (verdicts.length === 0) {
    body.innerHTML = `<tr><td colspan="8" class="cp-empty">No run yet. Run the signals on Outlook.</td></tr>`;
    return;
  }
  body.innerHTML = verdicts.map((v) => {
    const sel = v.selection || {};
    const hold = v.holdout || {};
    const hitCi = Array.isArray(sel.hit_target1_ci) ? `${pct(sel.hit_target1_ci[0])}–${pct(sel.hit_target1_ci[1])}` : "—";
    const expCi = Array.isArray(sel.expectancy_ci) ? `${signed(sel.expectancy_ci[0])} to ${signed(sel.expectancy_ci[1])}` : "—";
    return `<tr>
        <td>${escapeHtml(formatSetupName(sel.setup))}</td>
        <td>${escapeHtml(signalSideLabel(sel.side))}</td>
        <td>${escapeHtml(v.group || "—")}</td>
        <td class="num">${sel.trades || 0}</td>
        <td class="num">${pct(sel.hit_target1)} <span class="cp-ci">${hitCi}</span></td>
        <td class="num">${signed(sel.expectancy_r, 2, "R")} <span class="cp-ci">${expCi}</span></td>
        <td class="num">${signed(hold.expectancy_r, 2, "R")} <span class="cp-ci">${hold.trades || 0} trades</span></td>
        <td><span class="${v.publishes ? "cp-word-good" : "cp-word-wait"}">${v.publishes ? "Publishes" : "Doesn't"}</span> <span class="cp-ci">${escapeHtml(v.reason || "")}</span></td>
      </tr>`;
  }).join("");
}

// --- Published signals, graded live ---------------------------------------------------

const EXIT_WORDS = {
  stop: "Stopped out",
  target1: "Target 1",
  target2: "Target 2",
  target3: "Target 3",
  time: "Time exit",
  timeout: "Time exit",
};

async function loadTradeRecord() {
  const totalsEl = document.getElementById("cpRecordTotals");
  const count = document.getElementById("cpRecordCount");
  const body = document.getElementById("cpRecordBody");
  if (!body) return;
  try {
    const res = await fetchJson(`/api/capital/signals/trade/record${accountQuery()}`);
    const totals = res.totals || {};
    const cards = Array.isArray(res.cards) ? res.cards : [];
    if (count) count.textContent = cards.length ? `· ${cards.length} published` : "";
    if (totalsEl) {
      totalsEl.textContent = totals.closed
        ? `${totals.closed} closed: reached target 1 in ${pct(totals.hit_target1)}, mean ${signed(totals.mean_r, 2, "R")}, cumulative ${signed(totals.cumulative_r, 2, "R")} after costs.`
        : "No closed signals yet.";
    }
    if (cards.length === 0) {
      body.innerHTML = `<tr><td colspan="5" class="cp-empty">Nothing has been published yet.</td></tr>`;
      return;
    }
    body.innerHTML = cards.map((item) => {
      const c = item.card || {};
      const status = item.status || "open";
      const graded = item.graded;
      let outcome = status === "open" ? "Open" : status === "unfilled" ? "Not filled" : "Closed";
      let result = "—";
      let resultClass = "";
      if (status === "closed" && graded) {
        outcome = EXIT_WORDS[String(graded.exit || "").toLowerCase()] || outcome;
        if (graded.r_multiple !== undefined) {
          result = signed(graded.r_multiple, 2, "R");
          resultClass = Number(graded.r_multiple) >= 0 ? "positive" : "negative";
        }
      }
      return `<tr>
          <td>${formatDay(c.signal_session)}</td>
          <td>${escapeHtml(c.symbol || "—")}</td>
          <td>${escapeHtml(signalSideLabel(c.side))}</td>
          <td>${escapeHtml(outcome)}</td>
          <td class="num ${resultClass}">${result}</td>
        </tr>`;
    }).join("");
  } catch (err) {
    body.innerHTML = `<tr><td colspan="5" class="cp-empty">Record unavailable: ${escapeHtml(err.message)}</td></tr>`;
  }
}

// --- Baseline forecasts, walk-forward -------------------------------------------------

function modelDisplayName(m) {
  switch (m) {
    case "no_change": return "No change (benchmark)";
    case "trailing_drift": return "Trailing drift";
    case "ewma_volatility": return "EWMA volatility";
    case "empirical_horizon": return "Past returns";
    default: return m;
  }
}

async function loadEvaluation() {
  const wrap = document.getElementById("cpEvalWrap");
  if (!wrap) return;
  if (accountList.length === 0) {
    wrap.innerHTML = `<p class="cp-empty">No evaluation yet.</p>`;
    return;
  }
  try {
    renderEvaluation(await fetchJson(`/api/capital/forecasts/evaluation${accountQuery()}`));
  } catch (err) {
    wrap.innerHTML = /404|not found/i.test(String(err.message))
      ? `<p class="cp-empty">No evaluation yet.</p>`
      : `<p class="cp-empty">Evaluation unavailable: ${escapeHtml(err.message)}</p>`;
    renderNotes(document.getElementById("cpEvalNotes"), []);
  }
}

function renderEvaluation(report) {
  const wrap = document.getElementById("cpEvalWrap");
  const meta = document.getElementById("cpEvalMeta");
  const notes = document.getElementById("cpEvalNotes");
  if (!wrap) return;
  if (!report || !report.scores || report.scores.length === 0) {
    wrap.innerHTML = `<p class="cp-empty">Scoring needs at least 252 daily returns per instrument.</p>`;
    if (meta) meta.textContent = "";
    renderNotes(notes, []);
    return;
  }
  if (meta) {
    const n = report.instruments ? report.instruments.length : 0;
    meta.textContent = `· ${n} instrument${n === 1 ? "" : "s"}`;
  }

  const rankIc = new Map((report.rank_ic || []).map((r) => [`${r.model}:${r.horizon_sessions}`, r]));
  const horizons = (report.config && report.config.horizons) || [5, 20, 60];
  const rows = [];
  for (const h of horizons) {
    for (const s of report.scores.filter((x) => x.horizon_sessions === h)) {
      const ic = rankIc.get(`${s.model}:${h}`);
      const skill = s.model === "no_change"
        ? `<span class="cp-word-wait">benchmark</span>`
        : `<span class="${s.crps_skill > 0.0005 ? "cp-word-good" : s.crps_skill < -0.0005 ? "cp-word-bad" : "cp-word-wait"}">${signed(s.crps_skill * 100, 1, "%")}</span>`;
      rows.push(`<tr>
          <td>${escapeHtml(modelDisplayName(s.model))}</td>
          <td>${horizonLabel(h)}</td>
          <td class="num">${s.forecasts}</td>
          <td class="num">${skill}</td>
          <td class="num">${pct(s.interval_90, 1)}</td>
          <td class="num">${pct(s.direction_hit_rate, 1)}</td>
          <td>${s.calibrated ? `<span class="cp-word-good">Yes</span>` : `<span class="cp-word-wait">No</span>`}</td>
          <td class="num">${ic && ic.origins > 0 ? signed(ic.mean, 3) : "—"}</td>
        </tr>`);
    }
  }
  wrap.innerHTML = `<div class="cp-table-wrap">
      <table class="cp-table">
        <thead><tr>
          <th scope="col">Model</th><th scope="col">Horizon</th><th scope="col" class="num">Forecasts</th>
          <th scope="col" class="num">Skill</th><th scope="col" class="num">Inside 90%</th>
          <th scope="col" class="num">Direction</th><th scope="col">Calibrated</th><th scope="col" class="num">Rank IC</th>
        </tr></thead>
        <tbody>${rows.join("")}</tbody>
      </table>
    </div>`;

  const lines = [];
  if (report.skipped && report.skipped.length > 0) {
    lines.push(`Skipped: ${report.skipped.map(([sym, why]) => `${sym} (${why})`).join(", ")}.`);
  }
  renderNotes(notes, lines.concat(report.notes || []));
}

// --- Signal skill ---------------------------------------------------------------------

function formatSignalName(sig) {
  switch (sig) {
    case "momentum12_1":
    case "momentum_12_1": return "12-1 momentum";
    case "short_term_reversal": return "Short-term reversal";
    case "low_volatility": return "Low volatility";
    case "trend": return "Trend";
    case "chart_pattern": return "Chart patterns (YOLO)";
    case "chart_trend": return "Chart trend (YOLO)";
    case "foundation_forecast": return "Foundation model (Chronos)";
    case "filing_sentiment": return "Filing sentiment";
    case "insider_buying": return "Insider buying";
    default: return sig ? sig.replace(/_/g, " ") : "—";
  }
}

async function loadSignals() {
  const body = document.getElementById("cpSignalsBody");
  const notes = document.getElementById("cpSignalsNotes");
  if (!body) return;
  const empty = (text) => {
    body.innerHTML = `<tr><td colspan="6" class="cp-empty">${escapeHtml(text)}</td></tr>`;
    renderNotes(notes, []);
  };
  if (accountList.length === 0) return empty("Import a report to see signal skill.");
  try {
    const report = await fetchJson(`/api/capital/signals${accountQuery()}`);
    if (!report.skill || report.skill.length === 0) return empty("No signal report yet.");
    body.innerHTML = report.skill.map((s) => {
      const off = s.weight === 0 || s.ic_used === 0;
      return `<tr class="${off ? "cp-row-off" : ""}">
          <td>${escapeHtml(formatSignalName(s.signal))}</td>
          <td>${horizonLabel(s.horizon_sessions)}</td>
          <td class="num">${signed(s.ic_mean, 3)}</td>
          <td class="num">${signed(s.t_stat, 2)}</td>
          <td class="num">${signed(s.ic_used, 3)}</td>
          <td class="num">${pct(s.weight, 1)}</td>
        </tr>`;
    }).join("");
    renderNotes(notes, report.notes);
  } catch (_) {
    empty("No signal report yet.");
  }
}

registerTab("record", {
  load: () =>
    Promise.all([loadScorecard(), loadTradeSignals(), loadTradeRecord(), loadSignals(), loadEvaluation()]),
});

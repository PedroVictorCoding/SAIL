// SAIL Capital — Signals: every holding and watchlist stock or ETF side by side, with
// its price-signal readings, the blended outlook, any trade setup, and one read.
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
let sigbScope = "all";
let sigbLeanFilter = "any";
let sigbHorizon = 63;
let sigbSort = { key: "read", dir: 1 };
let sigbOpen = new Set();

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
  return `<span class="cp-sigb-setup" data-published="${setup.published ? "yes" : "no"}" title="${setup.published ? "Published: the setup has earned a record" : "Fired, but the setup hasn't earned a record, so it didn't publish"}">${escapeHtml(side)} · ${escapeHtml(formatSetupName(setup.setup))}${setup.published ? "" : " (unproven)"}</span>`;
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

function sigbDetail(row) {
  const readings = SIGB_SIGNALS.map((sig) => {
    const r = sigbReading(row, sig);
    return `<li><span class="cp-sigb-d-name">${SIGB_SIGNAL_NAMES[sig]}</span>
        <span>${r ? `${escapeHtml(r.text)} · percentile ${Math.round(r.percentile)}` : "Not enough history yet"}</span></li>`;
  }).join("");
  const outlooks = (row.outlook || []).map((o) => `<li><span class="cp-sigb-d-name">${sigbHorizonWord(o.horizon_sessions)}</span>
      <span>Median ${sigbPct(o.median)}, 90% range ${sigbPct(o.low, 0)} to ${sigbPct(o.high, 0)}, chance of ending higher ${Math.round(o.p_up * 100)}%</span></li>`).join("")
    || `<li><span>No outlook: it needs about a year of prices.</span></li>`;
  const combined = (row.combined || []).filter((c) => c.alpha !== 0).map((c) => `<li><span class="cp-sigb-d-name">${sigbHorizonWord(c.horizon_sessions)}</span>
      <span>Score ${c.score >= 0 ? "+" : "−"}${Math.abs(c.score).toFixed(2)} SD, expected ${sigbPct(Math.exp(c.alpha) - 1)} against the board</span></li>`).join("");
  const setup = row.setup
    ? `<p class="cp-sigb-d-line">${row.setup.published
        ? `A ${row.setup.side === "long" ? "buy" : "sell"} setup (${escapeHtml(formatSetupName(row.setup.setup))}) published on the latest session. Its entry, stop and targets are on Outlook → Trade signals.`
        : `A ${row.setup.side === "long" ? "buy" : "sell"} setup (${escapeHtml(formatSetupName(row.setup.setup))}) fired on the latest session but hasn't earned a record, so it didn't publish.`}</p>`
    : "";
  return `<div class="cp-sigb-detail">
      <p class="cp-sigb-d-read">${escapeHtml(row.read?.sentence || "")}</p>
      <div class="cp-sigb-d-grid">
        <div><h3 class="cp-sigb-d-head">What the prices show</h3><ul class="cp-sigb-d-list">${readings}</ul></div>
        <div><h3 class="cp-sigb-d-head">Outlook</h3><ul class="cp-sigb-d-list">${outlooks}</ul>
          ${combined ? `<h3 class="cp-sigb-d-head">Validated signals</h3><ul class="cp-sigb-d-list">${combined}</ul>` : ""}
        </div>
      </div>
      ${setup}
      <div class="cp-sigb-d-actions">
        <button type="button" class="cp-button cp-btn-sm cp-sigb-chart" data-symbol="${escapeHtml(row.symbol)}">Open outlook chart</button>
      </div>
    </div>`;
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
    const open = sigbOpen.has(row.symbol);
    const outlookCell = o
      ? `<span class="cp-sigb-out"><span class="cp-sigb-out-med" data-dir="${changeDir(o.median)}">${sigbPct(o.median)}</span><span class="cp-sigb-out-range">${sigbPct(o.low, 0)} … ${sigbPct(o.high, 0)}</span></span>`
      : `<span class="cp-chg-none">—</span>`;
    const chance = o ? `${Math.round(o.p_up * 100)}%` : "—";
    return `<tr class="cp-sigb-row${open ? " cp-sigb-open" : ""}" data-symbol="${escapeHtml(row.symbol)}" tabindex="0" aria-expanded="${open}">
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
      </tr>
      ${open ? `<tr class="cp-sigb-detail-row"><td colspan="9">${sigbDetail(row)}</td></tr>` : ""}`;
  }).join("");

  body.querySelectorAll(".cp-sigb-row").forEach((tr) => {
    const toggle = () => {
      const sym = tr.dataset.symbol;
      if (sigbOpen.has(sym)) sigbOpen.delete(sym);
      else sigbOpen.add(sym);
      renderSigbTable();
    };
    tr.addEventListener("click", toggle);
    tr.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggle();
      }
    });
  });
  body.querySelectorAll(".cp-sigb-chart").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (typeof openOutlookFor === "function") openOutlookFor(btn.dataset.symbol);
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

function sigbCsv() {
  if (!sigbBoard) return;
  const cell = (v) => {
    if (v === null || v === undefined) return "";
    const s = String(v);
    return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  const num = (v, digits = 4) => (Number.isFinite(v) ? v.toFixed(digits) : "");
  const head = [
    "symbol", "name", "class", "held", "session", "close", "change_1m",
    "lean", "validated", "read",
    ...SIGB_SIGNALS.flatMap((s) => [`${s}_raw`, `${s}_percentile`, `${s}_text`]),
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
      ...SIGB_SIGNALS.flatMap((s) => {
        const r = sigbReading(row, s);
        return r ? [num(r.raw, 6), num(r.percentile, 1), r.text] : ["", "", ""];
      }),
      num(o1?.median), num(o1?.low), num(o1?.high), num(o1?.p_up, 3),
      num(o3?.median), num(o3?.low), num(o3?.high), num(o3?.p_up, 3),
      num(c1?.score, 3), num(c1?.alpha, 5), num(c3?.score, 3), num(c3?.alpha, 5),
      row.setup ? formatSetupName(row.setup.setup) : "", row.setup?.side || "", row.setup ? row.setup.published : "",
    ];
    lines.push(values.map(cell).join(","));
  }
  const blob = new Blob([lines.join("\n") + "\n"], { type: "text/csv" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `sail-signals-${sigbBoard.session || "latest"}.csv`;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

async function loadSignalBoard() {
  const body = document.getElementById("cpSigbBody");
  const csv = document.getElementById("cpSigbCsv");
  if (accountList.length === 0) {
    if (body) body.innerHTML = `<tr><td colspan="9" class="cp-empty">Import a report to see the signal board.</td></tr>`;
    return;
  }
  if (body && !sigbBoard) body.innerHTML = `<tr><td colspan="9" class="cp-empty">Ranking your holdings and watchlist…</td></tr>`;
  try {
    sigbBoard = await fetchJson(`/api/capital/signals/board${sigbQuery()}`);
    if (csv) csv.disabled = (sigbBoard.rows || []).length === 0;
    renderSigbSummary();
    renderSigbTable();
    renderSigbNotes();
  } catch (err) {
    sigbBoard = null;
    if (csv) csv.disabled = true;
    if (body) body.innerHTML = `<tr><td colspan="9" class="cp-empty">Couldn't load the signal board: ${escapeHtml(err.message)}</td></tr>`;
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
  },
  load: loadSignalBoard,
});

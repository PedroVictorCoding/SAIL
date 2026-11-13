// SAIL Capital — Dashboard: the account at a glance, markets, what needs the owner,
// the calendar and the market overview.
"use strict";

const TAB_TITLES = {
  today: "Dashboard",
  portfolio: "Portfolio",
  income: "Income",
  signals: "Signals",
  outlook: "Outlook",
  record: "Track record",
  settings: "Settings",
};

const KIND_LABELS = { attention: "Attention", policy: "Limits", signal: "Signals", idea: "Idea" };

const LEAN_WORDS = { favoured: "Favoured", disfavoured: "Against", neutral: "Neutral", mixed: "Mixed", no_data: "No data" };

// Index tiles, in order of preference: the first four the watchlist has.
const INDEX_TILES = [
  ["US500", "S&P 500"],
  ["US100", "Nasdaq 100"],
  ["GOLD", "Gold"],
  ["EURUSD", "EUR/USD"],
  ["DE40", "DAX"],
  ["EU50", "Euro Stoxx 50"],
];

let marketFilter = "all";
let marketShowAll = false;
const MARKET_ROWS = 12;
let lastBoard = null;

function dashQuery() {
  return currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
}

// --- What needs you ------------------------------------------------------------------

function setStance(text, label = "hold") {
  const stance = document.getElementById("cpStanceSentence");
  if (!stance) return;
  stance.textContent = text;
  stance.dataset.stance = label;
}

function setTodayEmpty(message) {
  const list = document.getElementById("cpSuggestionsList");
  if (list) list.innerHTML = `<li class="cp-empty">${escapeHtml(message)}</li>`;
  const hidden = document.getElementById("cpSuggestionsHidden");
  if (hidden) hidden.hidden = true;
}

async function loadToday() {
  if (accountList.length === 0) {
    hasTodayAccount = false;
    setStance("No statements imported yet.");
    setTodayEmpty("Upload an official XTB report to see what needs you.");
    return;
  }

  try {
    const today = await fetchJson(`/api/capital/today${dashQuery()}`);

    hasTodayAccount = Boolean(today.account);
    if (today.account) updateAccountHeaderTiles(today.account);

    setStance(today.stance?.sentence || "", today.stance?.label || "hold");
    renderSuggestions(today.suggestions || [], today.account);

    const hidden = document.getElementById("cpSuggestionsHidden");
    if (hidden) {
      const count = today.hidden || 0;
      hidden.hidden = count === 0;
      hidden.textContent = `${count} dismissed or done ${count === 1 ? "item is" : "items are"} hidden.`;
    }
  } catch (err) {
    hasTodayAccount = false;
    if (/404|not found/i.test(String(err.message))) {
      setStance("No statements imported yet.");
      setTodayEmpty("Upload an official XTB report to see what needs you.");
    } else {
      setStance("Today is unavailable.");
      setTodayEmpty(`Couldn't load suggestions: ${err.message}`);
    }
  }
}

function suggestionChanges(changes, currency) {
  if (!changes || changes.length === 0) return "";
  const rows = changes.map((c) => {
    const fromPct = typeof c.from_pct === "number" ? `${c.from_pct.toFixed(1)}%` : "—";
    const toPct = typeof c.to_pct === "number" ? `${c.to_pct.toFixed(1)}%` : "—";
    const amount = typeof c.amount === "number" ? c.amount : 0;
    const sign = amount >= 0 ? "+" : "−";
    const formatted = `${sign}${groupDecimal(Math.abs(amount).toFixed(2))} ${currency}`;
    return `<tr>
        <td>${escapeHtml(c.symbol)}</td>
        <td class="num">${fromPct}</td>
        <td class="num">${toPct}</td>
        <td class="num ${amount >= 0 ? "positive" : "negative"}">${escapeHtml(formatted)}</td>
      </tr>`;
  }).join("");
  return `<div class="cp-table-wrap">
      <table class="cp-table cp-sug-changes">
        <thead><tr>
          <th scope="col">Symbol</th><th scope="col" class="num">From</th>
          <th scope="col" class="num">To</th><th scope="col" class="num">Amount</th>
        </tr></thead>
        <tbody>${rows}</tbody>
      </table>
    </div>`;
}

function renderSuggestions(suggestions, account) {
  const list = document.getElementById("cpSuggestionsList");
  if (!list) return;

  if (suggestions.length === 0) {
    list.innerHTML = `<li class="cp-empty">Nothing needs you today.</li>`;
    return;
  }

  const currency = account?.account_value?.currency || "EUR";

  list.innerHTML = suggestions.map((s) => {
    const severity = String(s.severity || "low").toLowerCase();
    const kind = String(s.kind || "attention").toLowerCase();
    const tab = normalizeTab(s.tab || "today");
    const meta = KIND_LABELS[kind] || kind;
    const confidence = s.confidence
      ? `<p class="cp-sug-line"><span class="cp-sug-label">Confidence</span> ${escapeHtml(s.confidence)}</p>`
      : "";
    const evidence = (s.evidence || []).length
      ? `<p class="cp-sug-line"><span class="cp-sug-label">Evidence</span> ${s.evidence.map(escapeHtml).join(" · ")}</p>`
      : "";
    const ifNothing = s.if_nothing
      ? `<p class="cp-sug-line"><span class="cp-sug-label">If you do nothing</span> ${escapeHtml(s.if_nothing)}</p>`
      : "";
    const stocks = (s.stocks || []).length
      ? `<ul class="cp-sug-stocks">${s.stocks.map((st) => `<li>
          <span class="cp-sug-stock-head">
            <span class="cp-sug-stock-sym">${escapeHtml(st.symbol)}</span>
            <span class="cp-lean" data-lean="${escapeHtml(st.lean)}">${escapeHtml(LEAN_WORDS[st.lean] || st.lean)}</span>
            <span class="cp-proof" data-proven="${st.proven ? "yes" : "no"}">${st.proven ? "validated" : "unproven"}</span>
          </span>
          <span class="cp-sug-stock-line">${escapeHtml(st.line)}</span>
        </li>`).join("")}</ul>`
      : "";
    const open = tab !== "today"
      ? `<button type="button" class="cp-button cp-btn-sm cp-sug-open" data-tab="${escapeHtml(tab)}">Open ${escapeHtml(TAB_TITLES[tab] || tab)}</button>`
      : "";
    return `<li class="cp-sug" data-severity="${escapeHtml(severity)}">
        <details>
          <summary>
            <span class="cp-sug-dot" aria-hidden="true"></span>
            <span class="cp-sug-title">${escapeHtml(s.title)}</span>
            <span class="cp-sug-kind">${escapeHtml(meta)}</span>
          </summary>
          <div class="cp-sug-body">
            <p class="cp-sug-why">${escapeHtml(s.why)}</p>
            ${ifNothing}
            ${stocks}
            ${suggestionChanges(s.changes, currency)}
            ${confidence}
            ${evidence}
            <div class="cp-sug-actions">
              ${open}
              <button type="button" class="cp-button cp-btn-sm cp-sug-done" data-id="${escapeHtml(s.id)}">Mark done</button>
              <button type="button" class="cp-button cp-btn-sm cp-sug-dismiss" data-id="${escapeHtml(s.id)}">Dismiss</button>
            </div>
          </div>
        </details>
      </li>`;
  }).join("");

  list.querySelectorAll(".cp-sug-dismiss").forEach((btn) => {
    btn.addEventListener("click", () => handleSuggestionAction(btn.dataset.id, "dismissed"));
  });
  list.querySelectorAll(".cp-sug-done").forEach((btn) => {
    btn.addEventListener("click", () => handleSuggestionAction(btn.dataset.id, "done"));
  });
  list.querySelectorAll(".cp-sug-open").forEach((btn) => {
    btn.addEventListener("click", () => switchTab(btn.dataset.tab, true));
  });
}

async function handleSuggestionAction(id, action) {
  try {
    await fetchJson(`/api/capital/suggestions/${encodeURIComponent(id)}/action${dashQuery()}`, {
      method: "POST",
      body: JSON.stringify({ action }),
    });
    await loadToday();
  } catch (err) {
    showBanner(`Action failed: ${err.message}`, true);
  }
}

// --- Markets: hero line, price cards, index tiles, breadth, overview -------------------

function renderHero(board) {
  const spark = document.getElementById("cpHeroSpark");
  const change = document.getElementById("cpHeroChange");
  const note = document.getElementById("cpHeroChangeNote");
  const values = (board.holdings_value || []).map((p) => p[1]);
  if (spark) spark.innerHTML = sparklineSvg(values, { width: 600, height: 120, color: "#a9a6ff" });
  if (values.length >= 2) {
    if (change) change.innerHTML = changePill(values[values.length - 1] / values[0] - 1);
    if (note) note.textContent = `today's holdings over the last ${values.length} sessions`;
  } else {
    if (change) change.innerHTML = "";
    if (note) note.textContent = board.holdings_value_note || "Free funds + positions";
  }
}

function priceCard(t) {
  return `<article class="cp-card cp-price-card" data-class="${escapeHtml(t.class)}">
      <div class="cp-price-top">
        ${instrumentAvatar(t.symbol, t.class)}
        <div class="cp-price-id">
          <span class="cp-price-sym">${escapeHtml(t.symbol)}${t.currency ? ` · ${escapeHtml(t.currency)}` : ""}</span>
          <span class="cp-price-name">${escapeHtml(t.name)}</span>
        </div>
        <span class="cp-price-held">${formatDay(t.session)}</span>
      </div>
      <p class="cp-price-label">Last close</p>
      <p class="cp-price-val">${formatPrice(t.close)}</p>
      <div class="cp-price-chg">${changePill(t.change_1d)}<span class="cp-muted">1D</span>${changePill(t.change_1m)}<span class="cp-muted">1M</span></div>
      <div class="cp-price-spark">${sparklineSvg(t.spark, { width: 320, height: 90 })}</div>
    </article>`;
}

function renderPriceCards(board) {
  const row = document.getElementById("cpPriceCards");
  if (!row) return;
  const held = board.tiles.filter((t) => t.held);
  row.closest("section").hidden = held.length === 0;
  row.innerHTML = held.map(priceCard).join("");
}

function renderIndexTiles(board) {
  const wrap = document.getElementById("cpIndexTiles");
  if (!wrap) return;
  const bySymbol = new Map(board.tiles.map((t) => [t.symbol.toUpperCase(), t]));
  const picks = INDEX_TILES.filter(([sym]) => bySymbol.has(sym)).slice(0, 4);
  wrap.closest("section").hidden = picks.length === 0;
  wrap.innerHTML = picks.map(([sym, label]) => {
    const t = bySymbol.get(sym);
    const dir = changeDir(t.change_1d);
    const pctText = Number.isFinite(t.change_1d)
      ? `${dir === "up" ? "↗ +" : dir === "down" ? "↘ −" : ""}${Math.abs(t.change_1d * 100).toFixed(2)}%`
      : "—";
    return `<div class="cp-card cp-index" data-dir="${dir}">
        <span class="cp-index-name">${escapeHtml(label)}</span>
        <span class="cp-index-chg" data-dir="${dir}">${pctText}</span>
        <span class="cp-index-px">${formatPrice(t.close)} · ${formatDay(t.session)}</span>
      </div>`;
  }).join("");
}

function renderBreadth(board) {
  const gauge = document.getElementById("cpBreadthGauge");
  const note = document.getElementById("cpBreadthNote");
  if (!gauge) return;
  const { above = 0, total = 0 } = board.breadth || {};
  if (total === 0) {
    gauge.innerHTML = `<p class="cp-muted cp-center">Needs 200 sessions of prices.</p>`;
    return;
  }
  const share = above / total;
  const angle = Math.PI * (1 - share);
  const cx = 100, cy = 100, r = 78;
  const px = cx + r * Math.cos(angle);
  const py = cy - r * Math.sin(angle);
  const arc = (from, to, color) => {
    const a0 = Math.PI * (1 - from), a1 = Math.PI * (1 - to);
    return `<path d="M${(cx + r * Math.cos(a0)).toFixed(1)},${(cy - r * Math.sin(a0)).toFixed(1)} A${r},${r} 0 0 1 ${(cx + r * Math.cos(a1)).toFixed(1)},${(cy - r * Math.sin(a1)).toFixed(1)}" stroke="${color}" stroke-width="14" fill="none" stroke-linecap="round"/>`;
  };
  const word = share < 0.35 ? "Weak" : share > 0.65 ? "Strong" : "Mixed";
  gauge.innerHTML = `<svg viewBox="0 0 200 112" role="img" aria-label="${above} of ${total} above their 200-day average">
      ${arc(0.01, 0.33, "rgb(255 107 107 / .55)")}
      ${arc(0.35, 0.65, "rgb(240 180 90 / .5)")}
      ${arc(0.67, 0.99, "rgb(61 220 151 / .55)")}
      <circle cx="${px.toFixed(1)}" cy="${py.toFixed(1)}" r="9" fill="#0c0e14" stroke="#f2f5fa" stroke-width="2.5"/>
    </svg>
    <div class="cp-gauge-read">
      <div class="cp-gauge-num">${Math.round(share * 100)}</div>
      <div class="cp-gauge-word">${word}</div>
    </div>`;
  if (note) note.textContent = `${above} of ${total} tracked instruments closed above their 200-day average.`;
}

function renderMarketTable(board) {
  const body = document.getElementById("cpMarketBody");
  if (!body) return;
  const rows = board.tiles.filter((t) =>
    marketFilter === "all" ? true : marketFilter === "held" ? t.held : !t.held);
  if (rows.length === 0) {
    body.innerHTML = `<tr><td colspan="7" class="cp-empty">No prices yet. Fetch them in Settings → Prices.</td></tr>`;
    return;
  }
  const more = document.getElementById("cpMarketMore");
  if (more) {
    more.hidden = rows.length <= MARKET_ROWS;
    more.textContent = marketShowAll ? "Show fewer" : `Show all ${rows.length}`;
  }
  const shown = marketShowAll ? rows : rows.slice(0, MARKET_ROWS);
  body.innerHTML = shown.map((t, i) => `<tr>
      <td class="cp-no">#${i + 1}</td>
      <td><span class="cp-inst">${instrumentAvatar(t.symbol, t.class)}<span><span class="cp-inst-name">${escapeHtml(t.name)}</span><span class="cp-inst-sym">${escapeHtml(t.symbol)}</span>${t.held ? `<span class="cp-inst-held">held</span>` : ""}</span></span></td>
      <td class="num">${formatPrice(t.close)}</td>
      <td class="num">${changePill(t.change_1d)}</td>
      <td class="num">${changePill(t.change_1w)}</td>
      <td class="num">${changePill(t.change_1m)}</td>
      <td><div class="cp-row-spark">${sparklineSvg(t.spark, { width: 120, height: 30, area: false, dot: false })}</div></td>
    </tr>`).join("");
}

async function loadMarketBoard() {
  try {
    const board = await loadMarkets();
    lastBoard = board;
    const updated = document.getElementById("cpDashUpdated");
    if (updated) updated.textContent = board.session ? `Last close ${formatDay(board.session)}` : "No prices yet";
    renderHero(board);
    renderPriceCards(board);
    renderIndexTiles(board);
    renderBreadth(board);
    renderMarketTable(board);
  } catch (err) {
    const body = document.getElementById("cpMarketBody");
    if (body) body.innerHTML = `<tr><td colspan="7" class="cp-empty">Markets unavailable: ${escapeHtml(err.message)}</td></tr>`;
  }
}

function setupMarketFilter() {
  document.getElementById("cpMarketMore")?.addEventListener("click", () => {
    marketShowAll = !marketShowAll;
    if (lastBoard) renderMarketTable(lastBoard);
  });
  document.querySelectorAll(".cp-market .cp-seg-btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      marketFilter = btn.dataset.filter;
      document.querySelectorAll(".cp-market .cp-seg-btn").forEach((b) => {
        b.classList.toggle("cp-seg-on", b === btn);
        b.setAttribute("aria-pressed", String(b === btn));
      });
      if (lastBoard) renderMarketTable(lastBoard);
    });
  });
}

// --- Calendar: earnings, dividends paid and SEC filings --------------------------------

const CAL_LABELS = { earnings: "earnings", dividend: "dividend paid", filing: "filing" };

function isoDay(date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

async function loadCalendar() {
  const grid = document.getElementById("cpCalGrid");
  if (!grid) return;
  const now = new Date();
  const from = new Date(now.getFullYear(), now.getMonth(), 1);
  const events = [];
  const results = await Promise.allSettled([
    fetchJson(`/api/capital/signals/trade/guards${dashQuery()}`),
    fetchJson(`/api/capital/dividends${dashQuery()}${dashQuery() ? "&" : "?"}from=${isoDay(new Date(now.getFullYear(), now.getMonth() - 1, 1))}T00:00:00Z`),
    fetchJson(`/api/capital/filings${dashQuery()}`),
  ]);
  if (results[0].status === "fulfilled") {
    for (const g of results[0].value || []) {
      if (g.next_earnings) events.push({ day: g.next_earnings, kind: "earnings", what: g.symbol });
    }
  }
  if (results[1].status === "fulfilled") {
    for (const d of results[1].value || []) {
      const at = d.paid?.at;
      if (at) events.push({ day: at.slice(0, 10), kind: "dividend", what: d.instrument?.symbol || "Dividend" });
    }
  }
  if (results[2].status === "fulfilled") {
    for (const item of results[2].value?.items || []) {
      if (item.filing?.filing_date) {
        events.push({ day: item.filing.filing_date, kind: "filing", what: `${item.issuer_name} ${item.filing.form}` });
      }
    }
  }
  renderCalendar(from, events);
}

function renderCalendar(monthStart, events) {
  const grid = document.getElementById("cpCalGrid");
  const next = document.getElementById("cpCalNext");
  const todayLabel = document.getElementById("cpCalToday");
  const today = isoDay(new Date());
  if (todayLabel) todayLabel.textContent = `Today · ${formatDay(today)}`;

  const byDay = new Map();
  for (const e of events) {
    if (!byDay.has(e.day)) byDay.set(e.day, []);
    byDay.get(e.day).push(e);
  }

  const upcoming = events.filter((e) => e.day >= today).sort((a, b) => a.day.localeCompare(b.day))[0];
  if (next) {
    next.innerHTML = upcoming
      ? `<span class="cp-muted">Next · ${formatDay(upcoming.day)}</span><strong>${escapeHtml(upcoming.what)} ${CAL_LABELS[upcoming.kind]}</strong>`
      : `<span class="cp-muted">Nothing scheduled</span><strong>No upcoming earnings or filings known.</strong>`;
  }

  const year = monthStart.getFullYear();
  const month = monthStart.getMonth();
  const offset = (new Date(year, month, 1).getDay() + 6) % 7;
  const cells = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
    .map((d) => `<span class="cp-cal-dow">${d}</span>`);
  for (let i = 0; i < 42; i++) {
    const date = new Date(year, month, 1 - offset + i);
    if (i >= 35 && date.getMonth() !== month) break;
    const key = isoDay(date);
    const dayEvents = byDay.get(key) || [];
    const kinds = [...new Set(dayEvents.map((e) => e.kind))];
    const label = dayEvents.length ? `${formatDay(key)}: ${dayEvents.map((e) => `${e.what} ${CAL_LABELS[e.kind]}`).join(", ")}` : "";
    cells.push(`<span class="cp-cal-day"${date.getMonth() !== month ? " data-out" : ""}${key === today ? " data-today" : ""}${dayEvents.length ? ` data-has title="${escapeHtml(label)}"` : ""}>
        ${date.getDate()}${kinds.length ? `<span class="cp-cal-dots">${kinds.map((k) => `<span class="cp-cal-dot" data-kind="${k}"></span>`).join("")}</span>` : ""}
      </span>`);
  }
  grid.innerHTML = cells.join("");
}

registerTab("today", {
  setup: setupMarketFilter,
  load: () => Promise.all([loadMarketBoard(), loadCalendar()]),
});

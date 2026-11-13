// SAIL Capital — Today: the stance and what needs the owner.
"use strict";

const TAB_TITLES = {
  today: "Today",
  portfolio: "Portfolio",
  income: "Income",
  outlook: "Outlook",
  record: "Track record",
  settings: "Settings",
};

const KIND_LABELS = { attention: "Attention", policy: "Limits", idea: "Idea" };

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
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const today = await fetchJson(`/api/capital/today${query}`);

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
    const meta = [KIND_LABELS[kind] || kind];
    if (s.confidence) meta.push(`confidence ${s.confidence}`);
    const evidence = (s.evidence || []).length
      ? `<p class="cp-sug-line"><span class="cp-sug-label">Evidence</span> ${s.evidence.map(escapeHtml).join(" · ")}</p>`
      : "";
    const ifNothing = s.if_nothing
      ? `<p class="cp-sug-line"><span class="cp-sug-label">If you do nothing</span> ${escapeHtml(s.if_nothing)}</p>`
      : "";
    const open = tab !== "today"
      ? `<button type="button" class="cp-button cp-btn-sm cp-sug-open" data-tab="${escapeHtml(tab)}">Open ${escapeHtml(TAB_TITLES[tab] || tab)}</button>`
      : "";
    return `<li class="cp-sug" data-severity="${escapeHtml(severity)}">
        <details>
          <summary>
            <span class="cp-sug-title">${escapeHtml(s.title)}</span>
            <span class="cp-sug-kind">${escapeHtml(meta.join(" · "))}</span>
          </summary>
          <div class="cp-sug-body">
            <p class="cp-sug-why">${escapeHtml(s.why)}</p>
            ${ifNothing}
            ${suggestionChanges(s.changes, currency)}
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
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    await fetchJson(`/api/capital/suggestions/${encodeURIComponent(id)}/action${query}`, {
      method: "POST",
      body: JSON.stringify({ action }),
    });
    await loadToday();
  } catch (err) {
    showBanner(`Action failed: ${err.message}`, true);
  }
}

registerTab("today", { load: loadToday });

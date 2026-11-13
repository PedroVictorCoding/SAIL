// SAIL Capital — today.
"use strict";

async function loadToday() {
  const stanceBanner = document.getElementById("cpStanceBanner");
  const stanceSentence = document.getElementById("cpStanceSentence");
  const draftNotice = document.getElementById("cpPolicyDraftNotice");
  const suggestionsList = document.getElementById("cpSuggestionsList");
  const hiddenCount = document.getElementById("cpSuggestionsHidden");

  if (!suggestionsList) return;

  if (accountList.length === 0) {
    if (stanceSentence) stanceSentence.textContent = "No accounts recorded yet.";
    if (stanceBanner) stanceBanner.setAttribute("data-stance", "hold");
    if (draftNotice) draftNotice.hidden = true;
    suggestionsList.innerHTML = `<p class="cp-empty">Import an official XTB report above to see suggestions.</p>`;
    if (hiddenCount) hiddenCount.hidden = true;
    return;
  }

  try {
    const query = currentAccount ? `?account=${encodeURIComponent(currentAccount)}` : "";
    const today = await fetchJson(`/api/capital/today${query}`);

    // 1. Account header tiles
    if (today.account) {
      hasTodayAccount = true;
      updateAccountHeaderTiles(today.account);
    } else {
      hasTodayAccount = false;
    }

    // 2. Stance banner
    if (today.stance) {
      if (stanceSentence) stanceSentence.textContent = today.stance.sentence || "";
      if (stanceBanner) stanceBanner.setAttribute("data-stance", today.stance.label || "hold");
    }

    // 3. Policy draft notice
    if (draftNotice) {
      draftNotice.hidden = !(today.policy && today.policy.status === "draft");
    }

    // 4. Suggestions list
    renderSuggestions(today.suggestions || [], today.account);

    // 5. Hidden count
    if (hiddenCount) {
      const count = today.hidden || 0;
      hiddenCount.textContent = `${count} hidden`;
      hiddenCount.hidden = false;
    }
  } catch (err) {
    hasTodayAccount = false;
    // 404 indicates no XTB report imported yet
    if (String(err.message).includes("404") || String(err.message).toLowerCase().includes("not found")) {
      if (stanceSentence) stanceSentence.textContent = "No statements imported yet.";
      if (stanceBanner) stanceBanner.setAttribute("data-stance", "hold");
      if (draftNotice) draftNotice.hidden = true;
      suggestionsList.innerHTML = `<p class="cp-empty">Upload an official XTB export above to see suggestions.</p>`;
      if (hiddenCount) hiddenCount.hidden = true;
    } else {
      if (stanceSentence) stanceSentence.textContent = "Today suggestions unavailable.";
      suggestionsList.innerHTML = `<p class="cp-empty" style="color:var(--sail-text-subtle)">Could not load suggestions: ${escapeHtml(err.message)}</p>`;
    }
  }
}

function renderSuggestions(suggestions, account) {
  const container = document.getElementById("cpSuggestionsList");
  if (!container) return;

  if (suggestions.length === 0) {
    container.innerHTML = `<p class="cp-empty">No active suggestions right now.</p>`;
    return;
  }

  const accountCurrency = (account && account.account_value && account.account_value.currency) || "EUR";

  container.innerHTML = suggestions.map((s) => {
    const kindLabel = s.kind.charAt(0).toUpperCase() + s.kind.slice(1);
    const severity = (s.severity || "low").toLowerCase();
    const kind = (s.kind || "attention").toLowerCase();

    // Sized changes table
    let changesHtml = "";
    if (s.changes && s.changes.length > 0) {
      const rows = s.changes.map(c => {
        const fromPct = typeof c.from_pct === "number" ? c.from_pct.toFixed(1) : "—";
        const toPct = typeof c.to_pct === "number" ? c.to_pct.toFixed(1) : "—";
        const isPos = typeof c.amount === "number" && c.amount >= 0;
        const sign = isPos ? "+" : "-";
        const absAmt = typeof c.amount === "number" ? Math.abs(c.amount).toFixed(2) : "0.00";
        const formattedAmt = `${sign}${groupDecimal(absAmt)} ${accountCurrency}`;
        return `
          <tr>
            <td><strong>${escapeHtml(c.symbol)}</strong></td>
            <td class="num">${fromPct}%</td>
            <td class="num">${toPct}%</td>
            <td class="num ${isPos ? "positive" : "negative"}">${escapeHtml(formattedAmt)}</td>
          </tr>
        `;
      }).join("");

      changesHtml = `
        <div class="cp-changes-table-wrap">
          <table class="cp-changes-table">
            <thead>
              <tr>
                <th scope="col">Symbol</th>
                <th scope="col" class="num">From</th>
                <th scope="col" class="num">To</th>
                <th scope="col" class="num">Amount</th>
              </tr>
            </thead>
            <tbody>${rows}</tbody>
          </table>
        </div>
      `;
    }

    // Evidence chips
    let evidenceHtml = "";
    if (s.evidence && s.evidence.length > 0) {
      evidenceHtml = `
        <div class="cp-evidence-chips">
          ${s.evidence.map(e => `<span class="cp-evidence-chip">${escapeHtml(e)}</span>`).join("")}
        </div>
      `;
    }

    // Confidence
    let confidenceHtml = "";
    if (s.confidence) {
      confidenceHtml = `
        <span class="cp-confidence-tag">Confidence: <strong>${escapeHtml(s.confidence)}</strong></span>
      `;
    }

    // If nothing
    let ifNothingHtml = "";
    if (s.if_nothing) {
      ifNothingHtml = `
        <div class="cp-suggestion-if-nothing">
          <strong>If you do nothing:</strong> ${escapeHtml(s.if_nothing)}
        </div>
      `;
    }

    const targetTab = (s.tab || "today").toLowerCase();
    const tabName = targetTab.charAt(0).toUpperCase() + targetTab.slice(1);

    return `
      <article class="cp-suggestion-card cp-card-collapsed" data-id="${escapeHtml(s.id)}">
        <header class="cp-suggestion-head" role="button" tabindex="0" aria-expanded="false" aria-label="Toggle suggestion: ${escapeHtml(s.title)}">
          <span class="cp-severity-dot ${severity}" title="Severity: ${severity}"></span>
          <span class="cp-kind-chip ${kind}">${escapeHtml(kindLabel)}</span>
          <span class="cp-suggestion-title">${escapeHtml(s.title)}</span>
          <svg class="cp-suggestion-chevron" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
        </header>
        <div class="cp-suggestion-body">
          <p class="cp-suggestion-why">${escapeHtml(s.why)}</p>
          ${ifNothingHtml}
          ${changesHtml}
          <div class="cp-suggestion-meta-row">
            ${confidenceHtml}
            ${evidenceHtml}
          </div>
          <div class="cp-suggestion-actions">
            <button type="button" class="cp-button cp-btn-sm cp-action-dismiss" data-id="${escapeHtml(s.id)}">Dismiss</button>
            <button type="button" class="cp-button cp-btn-sm cp-action-done" data-id="${escapeHtml(s.id)}">Mark done</button>
            <button type="button" class="cp-button cp-btn-sm cp-action-open" data-tab="${escapeHtml(targetTab)}">Open ${escapeHtml(tabName)}</button>
          </div>
        </div>
      </article>
    `;
  }).join("");

  // Attach toggle listeners
  container.querySelectorAll(".cp-suggestion-card").forEach(card => {
    const head = card.querySelector(".cp-suggestion-head");
    head.addEventListener("click", () => {
      const isCollapsed = card.classList.toggle("cp-card-collapsed");
      head.setAttribute("aria-expanded", String(!isCollapsed));
    });
    head.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        const isCollapsed = card.classList.toggle("cp-card-collapsed");
        head.setAttribute("aria-expanded", String(!isCollapsed));
      }
    });
  });

  // Attach button actions
  container.querySelectorAll(".cp-action-dismiss").forEach(btn => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      handleSuggestionAction(btn.getAttribute("data-id"), "dismissed");
    });
  });

  container.querySelectorAll(".cp-action-done").forEach(btn => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      handleSuggestionAction(btn.getAttribute("data-id"), "done");
    });
  });

  container.querySelectorAll(".cp-action-open").forEach(btn => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      const tab = btn.getAttribute("data-tab");
      switchTab(tab, true);
    });
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

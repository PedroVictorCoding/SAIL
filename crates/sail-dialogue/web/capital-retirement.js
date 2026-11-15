// SAIL Capital — early-retirement plan and the researched strategy brief.
// Loaded after capital-income.js: it reads the projection controls and the selected
// account from that module's globals, and re-runs whenever the projection changes.
"use strict";

const CapitalRetirement = (() => {
  const STORAGE_KEY = "sail_capital_retirement";
  const RESEARCH_KEY = "sail_capital_retirement_research_key";
  const DEFAULTS = {
    currentAge: 35,
    retireAge: 55,
    targetMonthly: 2500,
    inflation: 2,
    withdrawal: 4,
  };

  let plan = null;
  let research = null;
  let planTimer = null;
  let researchTimer = null;
  let pollTimer = null;
  let lastQueryKey = null;
  let loaded = false;

  function numberValue(id, fallback, min, max) {
    const input = document.getElementById(id);
    const value = input ? Number(input.value) : NaN;
    if (!Number.isFinite(value)) return fallback;
    return Math.min(max, Math.max(min, value));
  }

  function inputs() {
    return {
      currentAge: Math.round(numberValue("cpRetireCurrentAge", DEFAULTS.currentAge, 16, 95)),
      retireAge: Math.round(numberValue("cpRetireRetireAge", DEFAULTS.retireAge, 16, 95)),
      targetMonthly: numberValue("cpRetireTarget", DEFAULTS.targetMonthly, 1, 100000),
      inflation: numberValue("cpRetireInflation", DEFAULTS.inflation, 0, 10),
      withdrawal: numberValue("cpRetireWithdrawal", DEFAULTS.withdrawal, 1, 10),
    };
  }

  function persistInputs() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(inputs()));
    } catch (_) {}
  }

  function restoreInputs() {
    let saved = null;
    try {
      saved = JSON.parse(localStorage.getItem(STORAGE_KEY) || "null");
    } catch (_) {
      saved = null;
    }
    if (!saved || typeof saved !== "object") return;
    const set = (id, value) => {
      const input = document.getElementById(id);
      if (input && Number.isFinite(value)) input.value = value;
    };
    set("cpRetireCurrentAge", saved.currentAge);
    set("cpRetireRetireAge", saved.retireAge);
    set("cpRetireTarget", saved.targetMonthly);
    set("cpRetireInflation", saved.inflation);
    set("cpRetireWithdrawal", saved.withdrawal);
  }

  function query() {
    const values = inputs();
    const params = new URLSearchParams({
      years: String(PROJ_YEARS),
      reinvest_pct: String(currentProjReinvestPct),
      allocation: currentProjAllocation,
      monthly_contribution: String(currentProjMonthlyContribution),
      top_n: String(currentProjTopN),
      reserve_pt_tax: String(currentProjReservePtTax),
      current_age: String(values.currentAge),
      retire_age: String(values.retireAge),
      target_monthly: String(values.targetMonthly),
      inflation_pct: String(values.inflation),
      withdrawal_rate_pct: String(values.withdrawal),
    });
    if (currentAccount) params.set("account", currentAccount);
    return params.toString();
  }

  function money(value, decimals = 0) {
    if (value === null || value === undefined || !Number.isFinite(Number(value))) return "—";
    return `${groupDecimal(Number(value).toFixed(decimals))} ${projCurrency()}`.trim();
  }

  function monthly(value) {
    return `${money(value, 0)}/mo`;
  }

  function percent(value) {
    if (!Number.isFinite(Number(value))) return "—";
    return `${Number(value).toFixed(1)}%`;
  }

  function ageText(age) {
    return age === null || age === undefined ? "not in the window" : `age ${age}`;
  }

  function statusElement() {
    return document.getElementById("cpRetireStatus");
  }

  function setStatus(message, isError = false) {
    const element = statusElement();
    if (!element) return;
    element.hidden = !message;
    element.textContent = message || "";
    element.classList.toggle("cp-word-warn", isError);
  }

  function errorText(error) {
    const message = String(error && error.message ? error.message : error);
    if (message.includes("401") || message.toLowerCase().includes("owner")) {
      return "Owner sign-in needed.";
    }
    if (message.includes("503")) {
      return "Research is not available in this SAIL process.";
    }
    if (message.toLowerCase().includes("failed to fetch") || message.toLowerCase().includes("network")) {
      return "Couldn't reach SAIL — showing the last known plan.";
    }
    return message;
  }

  function renderPlan(data) {
    plan = data;
    const base = (data.scenarios || []).find((scenario) => scenario.scenario === "base") || {};
    const set = (id, text) => {
      const element = document.getElementById(id);
      if (element) element.textContent = text;
    };
    set("cpRetireTileYear", `Year ${data.retirement_year}`);
    set(
      "cpRetireTileYearSub",
      `age ${data.assumptions.retire_age} · ${data.years_to_retirement} years away`,
    );
    set(
      "cpRetireTileProgress",
      `${monthly(data.base_monthly_net_today)} / ${monthly(data.assumptions.target_monthly_net)}`,
    );
    set("cpRetireTileProgressSub", `${percent(data.progress_pct)} of target · trailing dividends`);
    set("cpRetireTileIncome", monthly(base.monthly_net_today));
    set(
      "cpRetireTileIncomeSub",
      `base · ${monthly(base.monthly_net_at_retirement)} nominal then`,
    );
    set("cpRetireTileCapital", money(data.required_capital));
    set(
      "cpRetireTileCapitalSub",
      `at ${percent(data.assumptions.withdrawal_rate_pct)} · projected ${money(base.value_at_retirement)}`,
    );
    const needed = data.required_monthly_contribution;
    let contribution = "—";
    let contributionSub = "base scenario";
    if (needed === null || needed === undefined) {
      contribution = "Above the cap";
      contributionSub = "even the maximum allowance does not reach the target";
    } else if (needed <= 0) {
      contribution = "Already covered";
      contributionSub = "no extra allowance needed in the base scenario";
    } else {
      contribution = monthly(needed);
      contributionSub = `base scenario · now ${monthly(data.current_monthly_contribution)}`;
    }
    set("cpRetireTileContribution", contribution);
    set("cpRetireTileContributionSub", contributionSub);

    const body = document.getElementById("cpRetireScenarioBody");
    if (body) {
      const rows = (data.scenarios || []).map((scenario) => {
        const incomeState = scenario.funded_by_income
          ? `<span class="cp-retire-verdict cp-retire-yes">Covered ${ageText(scenario.income_reached_age)}</span>`
          : `<span class="cp-retire-verdict cp-retire-no">Not by ${data.assumptions.retire_age}</span>`;
        const capitalState = scenario.funded_by_capital
          ? `<span class="cp-retire-verdict cp-retire-yes">Covered</span>`
          : `<span class="cp-retire-verdict cp-retire-no">Below target</span>`;
        return `<tr>
          <th scope="row">${scenarioName(scenario.scenario)}</th>
          <td class="num">${money(scenario.monthly_net_today)}/mo</td>
          <td class="num">${money(scenario.value_at_retirement)}</td>
          <td>${incomeState}</td>
          <td>${capitalState}</td>
        </tr>`;
      });
      body.innerHTML = rows.length
        ? rows.join("")
        : `<tr><td colspan="5" class="cp-empty">No scenarios available.</td></tr>`;
    }

    const notes = document.getElementById("cpRetireNotes");
    if (notes) {
      notes.innerHTML = (data.notes || [])
        .map((note) => `<li>${escapeHtml(note)}</li>`)
        .join("");
    }
    lastQueryKey = query();
  }

  function scenarioName(scenario) {
    if (scenario === "low") return "Low";
    if (scenario === "high") return "High";
    return "Base";
  }

  async function loadPlan({ quiet = false } = {}) {
    if (!currentAccount) {
      setStatus("Import an XTB report to plan an early retirement.", true);
      return;
    }
    if (!quiet) setStatus("Working out the plan…");
    const url = `/api/capital/dividends/retirement?${query()}`;
    try {
      const data = await fetchJson(url);
      renderPlan(data);
      setStatus("");
      await loadResearch();
    } catch (error) {
      setStatus(`Couldn't work out the plan: ${errorText(error)}`, true);
    }
  }

  function schedulePlan() {
    persistInputs();
    if (planTimer) clearTimeout(planTimer);
    planTimer = setTimeout(() => {
      planTimer = null;
      loadPlan({ quiet: true });
    }, 300);
  }

  function researchStateText(snapshot) {
    if (!snapshot) return "Not run yet";
    switch (snapshot.status) {
      case "running":
        return "Researching…";
      case "done":
        return "Ready";
      case "failed":
        return "Last run failed";
      default:
        return "Not run yet";
    }
  }

  function citedNumbers(brief) {
    const cited = new Set();
    for (const strategy of brief.strategies || []) {
      for (const number of strategy.citations || []) cited.add(number);
    }
    return cited;
  }

  function sourceLink(number, title, url) {
    return `<a class="cp-retire-cite" href="${escapeHtml(url)}" target="_blank" rel="noopener noreferrer" aria-label="Source ${number}: ${escapeHtml(title)}">[S${number}]</a>`;
  }

  function renderStrategy(strategy, sources) {
    const byNumber = new Map((sources || []).map((source) => [source.number, source]));
    const citations = (strategy.citations || [])
      .map((number) => {
        const source = byNumber.get(number);
        return source ? sourceLink(number, source.title, source.url) : "";
      })
      .join(" ");
    const actions = (strategy.actions || []).map((item) => `<li>${escapeHtml(item)}</li>`).join("");
    const tradeoffs = (strategy.tradeoffs || [])
      .map((item) => `<li>${escapeHtml(item)}</li>`)
      .join("");
    return `<article class="cp-retire-strategy">
      <h4>${escapeHtml(strategy.name)}</h4>
      <p>${escapeHtml(strategy.why)}</p>
      ${actions ? `<p class="cp-retire-strategy-label">What to do</p><ul>${actions}</ul>` : ""}
      ${tradeoffs ? `<p class="cp-retire-strategy-label">What it costs or risks</p><ul>${tradeoffs}</ul>` : ""}
      ${citations ? `<p class="cp-retire-strategy-cites">Sources: ${citations}</p>` : ""}
    </article>`;
  }

  function renderResearchList(title, items) {
    if (!items || items.length === 0) return "";
    return `<section class="cp-retire-research-block">
      <h4>${escapeHtml(title)}</h4>
      <ul>${items.map((item) => `<li>${escapeHtml(item)}</li>`).join("")}</ul>
    </section>`;
  }

  function briefHtml(snapshot, stale) {
    const brief = snapshot.brief;
    if (!brief) return "";
    const sources = brief.sources || [];
    const cited = citedNumbers(brief);
    const sourceRows = sources
      .map(
        (source) => `<li>
          ${sourceLink(source.number, source.title, source.url)}
          <a href="${escapeHtml(source.url)}" target="_blank" rel="noopener noreferrer">${escapeHtml(source.title)}</a>
          <span class="cp-muted">· retrieved ${formatDate(source.retrieved_at)}</span>
          <span class="cp-retire-pill cp-retire-pill-small">${cited.has(source.number) ? "Cited" : "Read, not cited"}</span>
        </li>`,
      )
      .join("");
    const strategies = (brief.strategies || []).map((strategy) => renderStrategy(strategy, sources)).join("");
    return `
      <div class="cp-retire-brief">
        <p class="cp-retire-headline">${escapeHtml(brief.headline)}</p>
        <p>${escapeHtml(brief.summary)}</p>
        ${stale ? `<p class="cp-retire-stale">Your projection or plan settings changed after this brief was written. Run it again to bring it up to date.</p>` : ""}
        ${strategies ? `<h4>Strategies</h4>${strategies}` : ""}
        ${renderResearchList("Risks and limits", brief.risks)}
        ${renderResearchList("Next steps", brief.next_steps)}
        ${renderResearchList("Assumptions", brief.assumptions)}
        <section class="cp-retire-research-block">
          <h4>Sources fetched live</h4>
          <ul class="cp-retire-sources">${sourceRows || "<li>No source was kept.</li>"}</ul>
        </section>
        <p class="cp-retire-meta">
          Written by ${escapeHtml(brief.model || "the local model")} · ${formatDate(brief.generated_at)}
          ${brief.structured ? "" : " · the answer was not structured JSON, so citations were not checked"}
        </p>
        <ul class="cp-retire-notes">${(brief.notes || []).map((note) => `<li>${escapeHtml(note)}</li>`).join("")}</ul>
      </div>`;
  }

  function renderResearch() {
    const body = document.getElementById("cpRetireResearchBody");
    const state = document.getElementById("cpRetireResearchState");
    if (!body || !state) return;
    const snapshot = research;
    state.textContent = researchStateText(snapshot);
    state.classList.toggle("cp-retire-pill-warn", snapshot && snapshot.status === "failed");
    const button = document.getElementById("cpRetireResearchBtn");
    const running = snapshot && snapshot.status === "running";
    if (button) {
      button.disabled = running;
      button.textContent = running ? "Researching…" : snapshot && snapshot.brief ? "Research again" : "Research a strategy";
    }
    if (running) {
      const sources = snapshot.sources || [];
      const fetchedLine = sources.length
        ? `${sources.filter((source) => source.ok).length} of ${sources.length} sources fetched.`
        : "Fetching the sources…";
      body.innerHTML = `<p class="cp-retire-running" role="status">Fetching public sources and asking the local SAIL model. This can take a few minutes. ${escapeHtml(fetchedLine)}</p>${
        snapshot.brief
          ? `<p class="cp-muted">Showing the previous brief until the new one is ready.</p>${briefHtml(snapshot, isStale())}`
          : ""
      }`;
      return;
    }
    if (snapshot && snapshot.status === "failed") {
      body.innerHTML = `<p class="cp-retire-failed" role="alert">The last research run failed: ${escapeHtml(snapshot.last_error || "unknown reason")}</p>${
        snapshot.brief
          ? briefHtml(snapshot, isStale())
          : `<p class="cp-muted">Try again when this host can reach the pages and the local model is free.</p>`
      }`;
      return;
    }
    if (snapshot && snapshot.brief) {
      body.innerHTML = briefHtml(snapshot, isStale());
      return;
    }
    body.innerHTML = "";
  }

  function isStale() {
    try {
      const key = localStorage.getItem(RESEARCH_KEY);
      return Boolean(key && plan && key !== lastQueryKey);
    } catch (_) {
      return false;
    }
  }

  async function loadResearch() {
    if (!currentAccount) return;
    try {
      const snapshot = await fetchJson("/api/capital/dividends/retirement/research");
      research = snapshot;
      renderResearch();
      if (snapshot && snapshot.status === "running") schedulePoll();
    } catch (_) {
      // The plan still works without research; the button explains the 503.
    }
  }

  function schedulePoll() {
    if (pollTimer) return;
    const delay = document.visibilityState === "visible" ? 3000 : 15000;
    pollTimer = setTimeout(async () => {
      pollTimer = null;
      try {
        const snapshot = await fetchJson("/api/capital/dividends/retirement/research");
        research = snapshot;
        renderResearch();
        if (snapshot.status === "running") {
          schedulePoll();
        } else if (snapshot.status === "done" || snapshot.status === "failed") {
          setStatus("");
          if (snapshot.status === "done") await loadPlan({ quiet: true });
        }
      } catch (error) {
        setStatus(`Research status unavailable: ${errorText(error)}`, true);
      }
    }, delay);
  }

  async function startResearch() {
    if (!currentAccount) {
      setStatus("Import an XTB report first.", true);
      return;
    }
    const button = document.getElementById("cpRetireResearchBtn");
    if (button) button.disabled = true;
    setStatus("Starting the research…");
    try {
      const key = query();
      const response = await fetchJson(`/api/capital/dividends/retirement/research?${key}`, {
        method: "POST",
      });
      try {
        localStorage.setItem(RESEARCH_KEY, key);
      } catch (_) {}
      research = response.research || research;
      setStatus("");
      renderResearch();
      schedulePoll();
    } catch (error) {
      const message = errorText(error);
      if (message.includes("not available")) {
        setStatus("Research needs the local SAIL model running in this SAIL process.", true);
      } else {
        setStatus(`Couldn't start the research: ${message}`, true);
      }
      renderResearch();
    }
  }

  function setup() {
    restoreInputs();
    const ids = [
      "cpRetireCurrentAge",
      "cpRetireRetireAge",
      "cpRetireTarget",
      "cpRetireInflation",
      "cpRetireWithdrawal",
    ];
    for (const id of ids) {
      const input = document.getElementById(id);
      if (input) input.addEventListener("input", schedulePlan);
    }
    document.getElementById("cpRetireResearchBtn")?.addEventListener("click", startResearch);
    const notes = document.getElementById("cpRetireNotes");
    if (notes && !notes.childElementCount) {
      notes.innerHTML =
        "<li>Loads when the Income tab opens; the plan uses the projection's current settings.</li>";
    }
  }

  function reloadFromControls() {
    if (!loaded) return;
    if (planTimer) clearTimeout(planTimer);
    planTimer = setTimeout(() => {
      planTimer = null;
      loadPlan({ quiet: true });
    }, 300);
  }

  async function load() {
    loaded = true;
    await loadPlan();
  }

  return { setup, load, reloadFromControls, startResearch };
})();

if (typeof window !== "undefined") window.CapitalRetirement = CapitalRetirement;

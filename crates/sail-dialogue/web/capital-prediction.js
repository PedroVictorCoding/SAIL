// Presentation only: Rust owns input policy, prices, forecasts, charts and jobs.
"use strict";
(() => {
  const el = id => document.getElementById(id);
  let timer = null;
  let latest = null;
  let submitting = false;
  let displayedJob = null;
  function error(message) {
    el("cpPredError").hidden = !message;
    el("cpPredError").textContent = message || "";
  }
  function render(job) {
    if (!job) return;
    if (displayedJob !== job.id) {
      for (const [key, id] of [["symbol", "cpPredSymbol"], ["period", "cpPredPeriod"], ["interval", "cpPredInterval"], ["horizon", "cpPredHorizon"]]) {
        if (job.request[key] !== undefined) el(id).value = job.request[key];
      }
      displayedJob = job.id;
    }
    const running = job.state === "running";
    el("cpPredRun").disabled = running || submitting;
    el("cpPredStatus").textContent = `${job.request.symbol} · ${job.request.interval} · ${job.request.horizon} steps · ${job.state}${running ? " — runs on this host even if you close the tab" : ""}`;
    error(job.error);
    latest = job.result;
    el("cpPredResult").hidden = !latest;
    if (latest) {
      el("cpPredMeta").textContent = `${latest.symbol} · ${latest.currency || "currency unknown"} · ${latest.source} · fetched ${new Date(latest.fetched_at).toLocaleString()} · ${latest.context_points} model input bars`;
      // Server generates this SVG from validated finite numbers and fixed text only.
      el("cpPredChart").innerHTML = latest.chart_svg;
      el("cpPredNotes").replaceChildren(...latest.notes.map(note => {
        const li = document.createElement("li"); li.textContent = note; return li;
      }));
      el("cpPredProvenance").textContent = `${latest.model}@${latest.model_revision} · ${latest.adjustment} · exchange timezone ${latest.exchange_timezone}. All nine deciles and observed prices are included in the JSON export.`;
      const number = v => v.toLocaleString(undefined, { maximumFractionDigits: 4 });
      el("cpPredRows").replaceChildren(...latest.forecast.map(p => {
        const tr = document.createElement("tr");
        for (const text of [p.step, p.calendar_time.replace("T", " ").replace("Z", ""), number(p.deciles[0]), number(p.median), number(p.deciles[8])]) {
          const td = document.createElement("td"); td.textContent = text; tr.append(td);
        }
        return tr;
      }));
    }
    return running;
  }
  async function load() {
    clearTimeout(timer);
    try {
      const data = await fetchJson("/api/capital/prediction");
      el("cpPredConfig").textContent = !data.enabled ? "TimesFM is disabled. Install and enable the local worker using docs/CAPITAL_STOCK_FORECAST.md." : !data.yahoo_enabled ? "Yahoo access is disabled. Enable the Yahoo price source in the host configuration." : "Local TimesFM enabled. Weights must already be installed; inference never downloads them.";
      el("cpPredRun").disabled = !data.enabled || !data.yahoo_enabled || submitting;
      if (data.job) {
        const running = render(data.job);
        el("cpPredRun").disabled ||= !data.enabled || !data.yahoo_enabled;
        if (running) timer = setTimeout(load, 2000);
      }
    } catch (err) {
      error(`Could not read forecast status: ${err.message}`);
      // Keep polling after transient transport errors, without resubmitting a job.
      timer = setTimeout(load, 5000);
    }
  }
  function setup() {
    el("cpPredForm").addEventListener("submit", async event => {
      event.preventDefault();
      if (submitting) return;
      submitting = true;
      el("cpPredRun").disabled = true;
      error("");
      let submitError = null;
      try {
        const request = { symbol: el("cpPredSymbol").value, period: el("cpPredPeriod").value, interval: el("cpPredInterval").value, horizon: Number(el("cpPredHorizon").value) };
        const job = await fetchJson("/api/capital/prediction", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(request) });
        render(job);
      } catch (err) { submitError = err.message; }
      finally { submitting = false; await load(); if (submitError) error(submitError); }
    });
    el("cpPredExport").addEventListener("click", () => {
      if (!latest) return;
      const url = URL.createObjectURL(new Blob([JSON.stringify(latest, null, 2)], {type: "application/json"}));
      const a = document.createElement("a"); a.href = url; a.download = "capital-stock-forecast.json"; a.click(); URL.revokeObjectURL(url);
    });
  }
  registerTab("prediction", { setup, load });
})();

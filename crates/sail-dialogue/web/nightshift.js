// SAIL Night Shift — tell SAIL in the day, review its work in the morning.
"use strict";

(() => {
  const MAX_ASK_CHARS = 2000;
  const COLLAPSE_DIFF_LINES = 400; // files over 400 lines collapsed by default (spec)

  let ownerTokenValue = null;
  let state = {
    settings: null,
    tonight: { window: "01:00–07:00", plan: { items: [], deferred: [] } },
    errands: [],
    morning: null,
    mesh: null,
    external: { provider: null, used_tonight: 0, allowed: 0 },
  };
  let diffsByErrand = new Map();
  let bannerMsg = "";
  let bannerIsError = false;
  let openNoteBox = null; // { id, mode: "reshape" | "redo" }

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
  }

  async function ownerToken(force) {
    if (!force && ownerTokenValue) return ownerTokenValue;
    if (!force) {
      try {
        const stored = localStorage.getItem("sail_owner_token");
        if (stored && stored.trim().length >= 32) return (ownerTokenValue = stored.trim());
      } catch (_) {}
    }
    const r = await fetch("/api/guardian/token", { cache: "no-store" });
    if (!r.ok) throw new Error("Owner token unavailable.");
    const data = await r.json();
    ownerTokenValue = data.token;
    try {
      localStorage.setItem("sail_owner_token", ownerTokenValue);
    } catch (_) {}
    return ownerTokenValue;
  }

  async function fetchAuth(url, options = {}) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const token = await ownerToken(attempt > 0);
      const res = await fetch(url, {
        ...options,
        cache: "no-store",
        headers: { ...(options.headers || {}), Authorization: `Bearer ${token}` },
      });
      if (res.status === 401 && attempt === 0) {
        ownerTokenValue = null;
        try { localStorage.removeItem("sail_owner_token"); } catch (_) {}
        continue;
      }
      return res;
    }
    throw new Error("Owner authentication failed.");
  }

  async function fetchJson(url, options = {}) {
    const res = await fetchAuth(url, options);
    const data = await res.json().catch(() => ({}));
    if (!res.ok) {
      const msg = data.error || (Array.isArray(data.reasons) ? data.reasons.join("; ") : "") || `HTTP ${res.status}`;
      throw new Error(msg);
    }
    return data;
  }

  async function fetchText(url) {
    const res = await fetchAuth(url);
    const text = await res.text();
    if (!res.ok) throw new Error(text || `HTTP ${res.status}`);
    return text;
  }

  function setBanner(msg, isError = false) {
    bannerMsg = msg || "";
    bannerIsError = isError;
  }

  function safePrHref(rawUrl) {
    try {
      const u = new URL(String(rawUrl));
      if (u.protocol === "https:" || u.protocol === "http:") return u.href;
    } catch (_) {}
    return null;
  }

  function formatRanOn(ranOn) {
    if (!ranOn) return "";
    if (ranOn.kind === "external") return `Sent to ${ranOn.name} · ${ranOn.model}`;
    if (ranOn.kind === "mesh") return `PC (${ranOn.name}) · ${ranOn.model}`;
    return `Host · ${ranOn.model}`;
  }

  function stateBadge(errandState) {
    const s = errandState?.state || "unknown";
    switch (s) {
      case "shaping": return { label: "Shaping", cls: "live" };
      case "proposed": return { label: "SAIL's draft", cls: "warn" };
      case "unshapeable": return { label: "Couldn't shape", cls: "err" };
      case "queued": return { label: "Queued", cls: "good" };
      case "working": return { label: `Working · ${errandState.stage || "running"}`, cls: "live" };
      case "ready": return { label: "Ready", cls: "good" };
      case "needs_you": return { label: "Needs you", cls: "warn" };
      case "failed": return { label: "Failed", cls: "err" };
      case "shipping": return { label: "Shipping…", cls: "live" };
      case "shipped": return { label: "Shipped", cls: "good" };
      case "dropped": return { label: "Dropped", cls: "" };
      default: return { label: s, cls: "" };
    }
  }

  function parseUnifiedDiff(diffText) {
    const lines = String(diffText || "").split("\n");
    const files = [];
    let current = null;

    for (const line of lines) {
      if (line.startsWith("diff --git ")) {
        if (current) files.push(current);
        const parts = line.slice(11).split(" b/");
        const path = parts.length > 1 ? parts[1] : line.slice(11);
        current = { path, added: 0, removed: 0, lines: [line] };
      } else {
        if (!current) current = { path: "patch", added: 0, removed: 0, lines: [] };
        current.lines.push(line);
        if (line.startsWith("+") && !line.startsWith("+++")) current.added += 1;
        else if (line.startsWith("-") && !line.startsWith("---")) current.removed += 1;
      }
    }
    if (current && current.lines.length > 0) files.push(current);
    return files;
  }

  function renderDiffViewer(diffText) {
    const wrap = el("div", "ns-diff-viewer");
    const files = parseUnifiedDiff(diffText);
    if (files.length === 0) {
      wrap.appendChild(el("div", "ns-meta", "Empty diff."));
      return wrap;
    }

    const fileList = el("div", "ns-diff-filelist");
    for (const f of files) {
      const chip = el("span", "ns-chip");
      chip.appendChild(el("span", "", f.path));
      chip.appendChild(el("span", "ns-cnt-add", `+${f.added}`));
      chip.appendChild(el("span", "ns-cnt-rem", `−${f.removed}`));
      fileList.appendChild(chip);
    }
    wrap.appendChild(fileList);

    for (const f of files) {
      const fileCard = el("div", "ns-diff-file");
      const collapsedByDefault = f.lines.length > COLLAPSE_DIFF_LINES;
      const hdr = el("button", "ns-diff-file-hdr");
      hdr.type = "button";
      hdr.setAttribute("aria-expanded", collapsedByDefault ? "false" : "true");

      const pathSpan = el("span", "ns-diff-file-path", f.path);
      const counts = el("span", "ns-diff-counts");
      counts.appendChild(el("span", "ns-cnt-add", `+${f.added}`));
      counts.appendChild(el("span", "ns-cnt-rem", `−${f.removed}`));
      if (collapsedByDefault) counts.appendChild(el("span", "ns-meta", `(${f.lines.length} lines · collapsed)`));
      hdr.appendChild(pathSpan);
      hdr.appendChild(counts);

      const linesBox = el("div", "ns-diff-lines");
      linesBox.hidden = collapsedByDefault;
      for (const line of f.lines) {
        let cls = "ns-diff-line";
        if (line.startsWith("+") && !line.startsWith("+++")) cls += " add";
        else if (line.startsWith("-") && !line.startsWith("---")) cls += " del";
        else if (line.startsWith("@@")) cls += " hunk";
        else if (line.startsWith("diff ") || line.startsWith("index ") || line.startsWith("---") || line.startsWith("+++")) cls += " hdr";
        linesBox.appendChild(el("div", cls, line));
      }

      hdr.addEventListener("click", () => {
        linesBox.hidden = !linesBox.hidden;
        hdr.setAttribute("aria-expanded", linesBox.hidden ? "false" : "true");
      });

      fileCard.appendChild(hdr);
      fileCard.appendChild(linesBox);
      wrap.appendChild(fileCard);
    }
    return wrap;
  }

  function renderProposedEditor(errand) {
    const order = errand.state.order;
    const draft = {
      title: order.title || "",
      why: order.why || "",
      acceptance: [...(order.acceptance || [])],
      target_files: [...(order.target_files || [])],
      size: order.size || "s",
      notes_for_solver: order.notes_for_solver || "",
    };

    const box = el("div", "ns-draft-box");
    const hdrRow = el("div", "ns-card-top");
    hdrRow.appendChild(el("strong", "", "SAIL's draft work order"));
    hdrRow.appendChild(el("span", "ns-pill", `Size ${String(draft.size).toUpperCase()}`));
    box.appendChild(hdrRow);

    const titleLbl = el("label", "ns-field-label", "Title");
    const titleInput = el("input", "ns-input");
    titleInput.type = "text";
    titleInput.value = draft.title;
    titleInput.maxLength = 100;
    titleInput.addEventListener("input", () => { draft.title = titleInput.value; });
    box.appendChild(titleLbl);
    box.appendChild(titleInput);

    const whyLbl = el("label", "ns-field-label", "Why");
    const whyInput = el("textarea", "ns-textarea");
    whyInput.rows = 2;
    whyInput.maxLength = 600;
    whyInput.value = draft.why;
    whyInput.addEventListener("input", () => { draft.why = whyInput.value; });
    box.appendChild(whyLbl);
    box.appendChild(whyInput);

    box.appendChild(el("span", "ns-field-label", "Acceptance checks"));
    const accList = el("div", "ns-plan-list");
    function drawAcc() {
      accList.replaceChildren();
      draft.acceptance.forEach((item, idx) => {
        const row = el("div", "ns-acceptance-row");
        const inp = el("input", "ns-input");
        inp.type = "text";
        inp.value = item;
        inp.maxLength = 200;
        inp.addEventListener("input", () => { draft.acceptance[idx] = inp.value; });
        const rm = el("button", "ns-btn", "×");
        rm.type = "button";
        rm.setAttribute("aria-label", "Remove acceptance check");
        rm.disabled = draft.acceptance.length <= 1;
        rm.addEventListener("click", () => {
          draft.acceptance.splice(idx, 1);
          drawAcc();
        });
        row.appendChild(inp);
        row.appendChild(rm);
        accList.appendChild(row);
      });
    }
    drawAcc();
    box.appendChild(accList);

    const addAccBtn = el("button", "ns-btn", "+ Add check");
    addAccBtn.type = "button";
    addAccBtn.addEventListener("click", () => {
      if (draft.acceptance.length < 8) {
        draft.acceptance.push("");
        drawAcc();
      }
    });
    box.appendChild(addAccBtn);

    box.appendChild(el("span", "ns-field-label", "Target files"));
    const chipsWrap = el("div", "ns-chip-row");
    function drawChips() {
      chipsWrap.replaceChildren();
      draft.target_files.forEach((path, idx) => {
        const chip = el("span", "ns-chip");
        chip.appendChild(el("span", "", path));
        if (draft.target_files.length > 1) {
          const rm = el("button", "", "×");
          rm.type = "button";
          rm.setAttribute("aria-label", `Remove ${path}`);
          rm.addEventListener("click", () => {
            draft.target_files.splice(idx, 1);
            drawChips();
          });
          chip.appendChild(rm);
        }
        chipsWrap.appendChild(chip);
      });
    }
    drawChips();
    box.appendChild(chipsWrap);

    const actions = el("div", "ns-actions-row");
    const approveBtn = el("button", "ns-btn primary", "Approve for tonight");
    approveBtn.type = "button";
    approveBtn.addEventListener("click", async () => {
      approveBtn.disabled = true;
      try {
        const updated = await fetchJson(`/api/nightshift/errands/${encodeURIComponent(errand.id)}/approve`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ order: draft }),
        });
        upsertErrand(updated);
        setBanner("Approved for tonight.");
      } catch (err) {
        setBanner(err.message, true);
      } finally {
        render();
      }
    });

    const reshapeBtn = el("button", "ns-btn", "Reshape with a note");
    reshapeBtn.type = "button";
    reshapeBtn.addEventListener("click", () => {
      openNoteBox = { id: errand.id, mode: "reshape" };
      render();
    });

    const dropBtn = el("button", "ns-btn danger", "Drop");
    dropBtn.type = "button";
    dropBtn.addEventListener("click", () => doDrop(errand.id));

    actions.appendChild(approveBtn);
    actions.appendChild(reshapeBtn);
    actions.appendChild(dropBtn);
    box.appendChild(actions);

    return box;
  }

  function renderResultDetails(errand, result) {
    const wrap = el("div", "ns-draft-box");
    wrap.appendChild(el("div", "ns-meta", formatRanOn(result.ran_on)));
    wrap.appendChild(el("p", "ns-ask-quote", result.summary));

    if (result.tests) {
      const t = result.tests;
      const cls = t.exit_ok ? "ns-pill good" : "ns-pill err";
      wrap.appendChild(el("span", cls, `Tests: ${t.passed} passed, ${t.failed} failed, ${t.ignored} ignored (${t.duration_secs}s)`));
    } else {
      wrap.appendChild(el("span", "ns-pill warn", "Tests did not run"));
    }

    if (Array.isArray(result.gates) && result.gates.length > 0) {
      const gates = el("div", "ns-gates-list");
      for (const g of result.gates) {
        const pill = el("span", `ns-pill ${g.passed ? "good" : "err"}`, `${g.name}: ${g.detail}`);
        gates.appendChild(pill);
      }
      wrap.appendChild(gates);
    }

    if (result.review && Array.isArray(result.review.concerns) && result.review.concerns.length > 0) {
      const rev = el("div", "ns-callout");
      rev.appendChild(el("strong", "", "Self-review concerns:"));
      for (const c of result.review.concerns) {
        rev.appendChild(el("div", "", `• ${c}`));
      }
      wrap.appendChild(rev);
    }

    if (result.model_note) {
      const mn = el("div", "ns-model-note");
      mn.appendChild(el("span", "ns-model-note-label", "Model-written note"));
      mn.appendChild(el("div", "", result.model_note));
      wrap.appendChild(mn);
    }

    const diffBtn = el("button", "ns-btn", diffsByErrand.has(errand.id) ? "Hide diff" : "View diff");
    diffBtn.type = "button";
    diffBtn.addEventListener("click", async () => {
      if (diffsByErrand.has(errand.id)) {
        diffsByErrand.delete(errand.id);
        render();
        return;
      }
      diffBtn.disabled = true;
      try {
        const diffText = await fetchText(`/api/nightshift/errands/${encodeURIComponent(errand.id)}/diff`);
        diffsByErrand.set(errand.id, diffText);
      } catch (err) {
        setBanner(err.message, true);
      } finally {
        render();
      }
    });
    wrap.appendChild(diffBtn);

    if (diffsByErrand.has(errand.id)) {
      wrap.appendChild(renderDiffViewer(diffsByErrand.get(errand.id)));
    }
    return wrap;
  }

  function renderNoteInline(errand, mode) {
    const form = el("form", "ns-inline-note");
    const inp = el("input", "ns-input");
    inp.type = "text";
    inp.maxLength = 1000;
    inp.placeholder = mode === "reshape" ? "Note for reshaping this work order…" : "Note for tonight's redo…";
    const send = el("button", "ns-btn primary", mode === "reshape" ? "Send reshape" : "Queue redo");
    send.type = "submit";
    const cancel = el("button", "ns-btn", "Cancel");
    cancel.type = "button";
    cancel.addEventListener("click", () => { openNoteBox = null; render(); });

    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const note = inp.value.trim();
      if (!note) return;
      send.disabled = true;
      try {
        const updated = await fetchJson(`/api/nightshift/errands/${encodeURIComponent(errand.id)}/${mode}`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ note }),
        });
        openNoteBox = null;
        upsertErrand(updated);
        setBanner(mode === "reshape" ? "Sent back for reshaping." : "Queued for redo tonight.");
      } catch (err) {
        setBanner(err.message, true);
      } finally {
        render();
      }
    });

    form.appendChild(inp);
    form.appendChild(send);
    form.appendChild(cancel);
    return form;
  }

  async function doShip(id) {
    try {
      const updated = await fetchJson(`/api/nightshift/errands/${encodeURIComponent(id)}/ship`, { method: "POST" });
      upsertErrand(updated);
      setBanner("Shipping branch and opening draft PR…");
    } catch (err) {
      setBanner(err.message, true);
    } finally {
      render();
    }
  }

  async function doDrop(id) {
    try {
      const updated = await fetchJson(`/api/nightshift/errands/${encodeURIComponent(id)}/drop`, { method: "POST" });
      upsertErrand(updated);
      setBanner("Errand dropped.");
    } catch (err) {
      setBanner(err.message, true);
    } finally {
      render();
    }
  }

  function askAboutErrand(errand) {
    const title = errand.state?.order?.title || errand.ask;
    const composer = document.getElementById("composerInput");
    close();
    if (composer) {
      composer.value = `About Night Shift errand ${errand.id} ("${title}"): `;
      composer.focus();
    }
  }

  function renderErrandCard(errand) {
    const card = el("article", "ns-card");
    const top = el("div", "ns-card-top");
    const badges = el("div", "ns-badges");
    const badge = stateBadge(errand.state);
    badges.appendChild(el("span", `ns-pill ${badge.cls}`, badge.label));
    badges.appendChild(el("span", "ns-pill", errand.source));
    if (errand.external_used) {
      const cliName = errand.state?.result?.ran_on?.name || "Claude Code";
      badges.appendChild(el("span", "ns-pill external", `Sent to ${cliName}`));
    }
    top.appendChild(badges);
    top.appendChild(el("span", "ns-meta", errand.id));
    card.appendChild(top);

    card.appendChild(el("p", "ns-ask-quote", errand.ask));

    if (Array.isArray(errand.notes) && errand.notes.length > 0) {
      const notesBox = el("div", "ns-meta");
      notesBox.textContent = `Owner notes: ${errand.notes.map((n) => n.text).join(" · ")}`;
      card.appendChild(notesBox);
    }

    const s = errand.state || {};
    if (s.state === "shaping") {
      card.appendChild(el("div", "ns-meta", "Shaping work order in the background…"));
    } else if (s.state === "proposed") {
      card.appendChild(renderProposedEditor(errand));
    } else if (s.state === "unshapeable") {
      const box = el("div", "ns-callout error");
      for (const r of s.reasons || []) box.appendChild(el("div", "", `• ${r}`));
      card.appendChild(box);
      const row = el("div", "ns-actions-row");
      const rBtn = el("button", "ns-btn", "Split or reshape");
      rBtn.type = "button";
      rBtn.addEventListener("click", () => { openNoteBox = { id: errand.id, mode: "reshape" }; render(); });
      const dBtn = el("button", "ns-btn danger", "Drop");
      dBtn.type = "button";
      dBtn.addEventListener("click", () => doDrop(errand.id));
      row.appendChild(rBtn);
      row.appendChild(dBtn);
      card.appendChild(row);
    } else if (s.state === "queued") {
      card.appendChild(el("div", "ns-meta", `Work order: ${s.order?.title || ""} (Size ${String(s.order?.size || "").toUpperCase()})`));
      const dBtn = el("button", "ns-btn danger", "Drop");
      dBtn.type = "button";
      dBtn.addEventListener("click", () => doDrop(errand.id));
      const row = el("div", "ns-actions-row");
      row.appendChild(dBtn);
      card.appendChild(row);
    } else if (s.state === "working") {
      card.appendChild(el("div", "ns-meta", `${s.order?.title || ""} · ${formatRanOn(s.ran_on)}`));
    } else if (s.state === "ready" || s.state === "needs_you" || s.state === "failed" || s.state === "shipping" || s.state === "shipped") {
      if (s.state === "needs_you" && s.question) {
        const q = el("div", "ns-callout");
        q.appendChild(el("strong", "", "Decision needed: "));
        q.appendChild(el("span", "", s.question));
        card.appendChild(q);
      }
      if (s.state === "failed" && s.reason) {
        card.appendChild(el("div", "ns-callout error", s.reason));
      }
      if (s.state === "shipped" && s.pr_url) {
        const href = safePrHref(s.pr_url);
        if (href) {
          const link = el("a", "ns-btn primary", "Open draft PR");
          link.href = href;
          link.target = "_blank";
          link.rel = "noopener noreferrer";
          const row = el("div", "ns-actions-row");
          row.appendChild(link);
          card.appendChild(row);
        }
      }
      if (s.result) card.appendChild(renderResultDetails(errand, s.result));

      if (s.state !== "shipped" && s.state !== "shipping") {
        const row = el("div", "ns-actions-row");
        if (s.state === "ready" || s.state === "needs_you") {
          const shipBtn = el("button", "ns-btn primary", "Ship");
          shipBtn.type = "button";
          shipBtn.addEventListener("click", () => doShip(errand.id));
          row.appendChild(shipBtn);
        }
        const redoBtn = el("button", "ns-btn", "Redo with a note");
        redoBtn.type = "button";
        redoBtn.addEventListener("click", () => { openNoteBox = { id: errand.id, mode: "redo" }; render(); });
        const askBtn = el("button", "ns-btn", "Ask about this");
        askBtn.type = "button";
        askBtn.addEventListener("click", () => askAboutErrand(errand));
        const dropBtn = el("button", "ns-btn danger", "Drop");
        dropBtn.type = "button";
        dropBtn.addEventListener("click", () => doDrop(errand.id));
        row.appendChild(redoBtn);
        row.appendChild(askBtn);
        row.appendChild(dropBtn);
        card.appendChild(row);
      }
    }

    if (openNoteBox && openNoteBox.id === errand.id) {
      card.appendChild(renderNoteInline(errand, openNoteBox.mode));
    }
    return card;
  }

  function render() {
    const page = document.getElementById("nightshiftPage");
    if (!page) return;
    page.replaceChildren();

    const shell = el("div", "ns-shell");

    // Header
    const hdr = el("header", "ns-header");
    const titleWrap = el("div", "ns-header-title");
    titleWrap.appendChild(el("h1", "", "Night Shift"));
    titleWrap.appendChild(el("p", "", "Tell SAIL in the day, review its work in the morning."));
    hdr.appendChild(titleWrap);

    const hdrActions = el("div", "ns-header-actions");
    const settingsBtn = el("button", "ns-btn", "Settings");
    settingsBtn.type = "button";
    settingsBtn.addEventListener("click", () => {
      document.getElementById("settingsOpen")?.click();
      const tab = document.querySelector('[data-settings-tab="nightshift"]');
      tab?.click();
    });
    const closeBtn = el("button", "ns-btn", "Close");
    closeBtn.type = "button";
    closeBtn.addEventListener("click", close);
    hdrActions.appendChild(settingsBtn);
    hdrActions.appendChild(closeBtn);
    hdr.appendChild(hdrActions);

    const statusRow = el("div", "ns-status-row");
    const win = state.tonight?.window || "01:00–07:00";
    statusRow.appendChild(el("span", "ns-pill", `Window ${win}`));
    if (state.mesh) {
      if (state.mesh.resting) {
        statusRow.appendChild(el("span", "ns-pill warn", `PC (${state.mesh.name}) resting · using host`));
      } else if (state.mesh.available) {
        statusRow.appendChild(el("span", "ns-pill good", `PC (${state.mesh.name}) ready`));
      } else {
        statusRow.appendChild(el("span", "ns-pill", "Host only"));
      }
    } else {
      statusRow.appendChild(el("span", "ns-pill", "Host only"));
    }
    if (state.external && state.external.allowed > 0) {
      statusRow.appendChild(el("span", "ns-pill external", `External: ${state.external.used_tonight}/${state.external.allowed}`));
    }
    hdr.appendChild(statusRow);
    shell.appendChild(hdr);

    if (bannerMsg) {
      shell.appendChild(el("div", `ns-banner${bannerIsError ? " error" : ""}`, bannerMsg));
    }

    // Ask composer
    const askCard = el("section", "ns-card");
    const askForm = el("form", "ns-ask-form");
    const askLbl = el("label", "ns-field-label", "Queue something for tonight");
    askLbl.htmlFor = "nsAskInput";
    const askInput = el("textarea");
    askInput.id = "nsAskInput";
    askInput.rows = 2;
    askInput.maxLength = MAX_ASK_CHARS;
    askInput.placeholder = "SAIL, tonight: make the Capital Today tab show the next ex-dividend date.";
    const askFooter = el("div", "ns-ask-footer");
    const countSpan = el("span", "ns-char-count", `0 / ${MAX_ASK_CHARS}`);
    askInput.addEventListener("input", () => {
      countSpan.textContent = `${askInput.value.length} / ${MAX_ASK_CHARS}`;
    });
    const askSubmit = el("button", "ns-btn primary", "Ask for tonight");
    askSubmit.type = "submit";
    askFooter.appendChild(countSpan);
    askFooter.appendChild(askSubmit);
    askForm.appendChild(askLbl);
    askForm.appendChild(askInput);
    askForm.appendChild(askFooter);
    askForm.addEventListener("submit", async (e) => {
      e.preventDefault();
      const ask = askInput.value.trim();
      if (!ask) return;
      askSubmit.disabled = true;
      try {
        const created = await fetchJson("/api/nightshift/errands", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ask, source: "web" }),
        });
        askInput.value = "";
        upsertErrand(created);
        setBanner("Got it. SAIL is drafting a work order.");
      } catch (err) {
        setBanner(err.message, true);
      } finally {
        render();
      }
    });
    askCard.appendChild(askForm);
    shell.appendChild(askCard);

    // Morning report
    if (state.morning) {
      const m = state.morning;
      const mCard = el("section", "ns-card");
      const mTop = el("div", "ns-card-top");
      mTop.appendChild(el("strong", "", `Morning report · ${m.date}`));
      const mCounts = el("div", "ns-badges");
      mCounts.appendChild(el("span", "ns-pill good", `${m.ready} ready`));
      if (m.needs_you > 0) mCounts.appendChild(el("span", "ns-pill warn", `${m.needs_you} needs you`));
      if (m.failed > 0) mCounts.appendChild(el("span", "ns-pill err", `${m.failed} failed`));
      if (m.not_reached > 0) mCounts.appendChild(el("span", "ns-pill", `${m.not_reached} not reached`));
      mTop.appendChild(mCounts);
      mCard.appendChild(mTop);
      if (m.spoken) mCard.appendChild(el("p", "ns-morning-spoken", m.spoken));
      if (m.halted) mCard.appendChild(el("div", "ns-callout error", m.halted));
      if (Array.isArray(m.items) && m.items.length > 0) {
        const list = el("div", "ns-morning-items");
        for (const item of m.items) {
          const row = el("div", "ns-plan-row");
          row.appendChild(el("span", "", `${item.title} — ${item.line}`));
          row.appendChild(el("span", "ns-pill", item.outcome));
          list.appendChild(row);
        }
        mCard.appendChild(list);
      }
      shell.appendChild(mCard);
    }

    // Tonight's plan
    const plan = state.tonight?.plan;
    if (plan && ((plan.items && plan.items.length > 0) || (plan.deferred && plan.deferred.length > 0))) {
      const pCard = el("section", "ns-card");
      pCard.appendChild(el("strong", "", "Tonight's plan"));
      const list = el("div", "ns-plan-list");
      for (const it of plan.items || []) {
        const label = it.kind === "errand" ? `Errand ${it.id}` : `GitHub issue #${it.number}`;
        list.appendChild(el("div", "ns-plan-row", `Planned: ${label}`));
      }
      for (const def of plan.deferred || []) {
        const label = def.item?.kind === "errand" ? `Errand ${def.item.id}` : `GitHub issue #${def.item?.number}`;
        const row = el("div", "ns-plan-row");
        row.appendChild(el("span", "", `Deferred: ${label}`));
        row.appendChild(el("span", "ns-meta", def.reason));
        list.appendChild(row);
      }
      pCard.appendChild(list);
      shell.appendChild(pCard);
    }

    // Errands
    if (!state.errands || state.errands.length === 0) {
      const emptyCard = el("section", "ns-card");
      emptyCard.appendChild(el("div", "ns-meta", "No Night Shift errands yet. Tell SAIL what you'd like built above."));
      shell.appendChild(emptyCard);
    } else {
      for (const errand of state.errands) {
        shell.appendChild(renderErrandCard(errand));
      }
    }

    page.appendChild(shell);
  }

  function syncSettingsForm() {
    const s = state.settings;
    if (!s) return;
    const setVal = (id, prop, val) => {
      const node = document.getElementById(id);
      if (node) node[prop] = val;
    };
    setVal("nsSetEnabled", "checked", !!s.enabled);
    setVal("nsSetStartHour", "value", s.window_start_hour ?? 1);
    setVal("nsSetEndHour", "value", s.window_end_hour ?? 7);
    setVal("nsSetMaxItems", "value", s.max_items ?? 5);
    setVal("nsSetIncludeIssues", "checked", !!s.include_forge_ready_issues);
    setVal("nsSetUseMesh", "checked", !!s.use_mesh);
    setVal("nsSetExternalMax", "value", s.external_solves_per_night ?? 0);
    setVal("nsSetExternalProvider", "value", s.external_provider || "");
    setVal("nsSetMorningGreeting", "checked", !!s.morning_greeting);
  }

  function wireSettingsForm() {
    const form = document.getElementById("nightshiftSettingsForm");
    if (!form || form.dataset.wired) return;
    form.dataset.wired = "1";
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const status = document.getElementById("nsSetStatus");
      const btn = document.getElementById("nsSetSaveBtn");
      const payload = {
        enabled: !!document.getElementById("nsSetEnabled")?.checked,
        window_start_hour: Number(document.getElementById("nsSetStartHour")?.value ?? 1),
        window_end_hour: Number(document.getElementById("nsSetEndHour")?.value ?? 7),
        max_items: Number(document.getElementById("nsSetMaxItems")?.value ?? 5),
        include_forge_ready_issues: !!document.getElementById("nsSetIncludeIssues")?.checked,
        use_mesh: !!document.getElementById("nsSetUseMesh")?.checked,
        external_solves_per_night: Number(document.getElementById("nsSetExternalMax")?.value ?? 0),
        external_provider: document.getElementById("nsSetExternalProvider")?.value || null,
        morning_greeting: !!document.getElementById("nsSetMorningGreeting")?.checked,
      };
      if (btn) btn.disabled = true;
      if (status) status.textContent = "Saving…";
      try {
        const saved = await fetchJson("/api/nightshift/settings", {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(payload),
        });
        state.settings = saved;
        if (status) status.textContent = "Saved.";
      } catch (err) {
        if (status) status.textContent = err.message;
      } finally {
        if (btn) btn.disabled = false;
      }
    });
  }

  function upsertErrand(errand) {
    if (!errand || !errand.id) return;
    const idx = state.errands.findIndex((e) => e.id === errand.id);
    if (idx >= 0) state.errands[idx] = errand;
    else state.errands.unshift(errand);
  }

  async function refresh() {
    try {
      const data = await fetchJson("/api/nightshift");
      state = { ...state, ...data };
      syncSettingsForm();
      render();
    } catch (err) {
      setBanner(err.message, true);
      render();
    }
  }

  async function open() {
    const page = document.getElementById("nightshiftPage");
    if (!page) return;
    page.hidden = false;
    render();
    await refresh();
  }

  function close() {
    const page = document.getElementById("nightshiftPage");
    if (page) page.hidden = true;
  }

  function onErrandUpdate(errand) {
    upsertErrand(errand);
    const page = document.getElementById("nightshiftPage");
    if (page && !page.hidden) render();
  }

  function onMorningReport(report) {
    state.morning = report;
    const page = document.getElementById("nightshiftPage");
    if (page && !page.hidden) render();
  }

  function init() {
    const navBtn = document.getElementById("nightshiftNavBtn");
    if (navBtn) {
      navBtn.addEventListener("click", () => {
        const page = document.getElementById("nightshiftPage");
        if (page && page.hidden) open();
        else close();
      });
    }
    wireSettingsForm();
    if (window.location.hash === "#nightshift") open();
  }

  window.SailNightShift = { open, close, refresh, onErrandUpdate, onMorningReport };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();

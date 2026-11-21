// SAIL Calendar — local calendar, read-only ICS feeds, .ics import/export.
"use strict";

(() => {
  let ownerTokenValue = null;
  let prefs = {
    time_zone: "Europe/Lisbon",
    first_weekday: "monday",
    default_view: "week",
    day_start_hour: 7,
    day_end_hour: 23,
  };
  let sources = [];
  let windowData = { occurrences: [], truncated: false, sources: [], revision: 0 };
  let activeView = window.innerWidth < 640 ? "agenda" : "week";
  let anchorDate = new Date();
  let includeCancelled = false;
  let showSourcesPanel = false;
  let banner = null; // { text, level: "info"|"warn"|"err" }
  let activeModal = null; // null | { kind: "add"|"edit", event?, dateStr? } | { kind: "detail", event, occurrenceKey }
  let importPreview = null;

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
    try { localStorage.setItem("sail_owner_token", ownerTokenValue); } catch (_) {}
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
    if (res.status === 204) return null;
    const data = await res.json().catch(() => ({}));
    if (!res.ok) {
      const msg = data?.error?.message || data?.error || `HTTP ${res.status}`;
      throw new Error(msg);
    }
    return data;
  }

  function tzDateStr(date, tz = prefs.time_zone) {
    const parts = new Intl.DateTimeFormat("en-CA", {
      timeZone: tz,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    }).formatToParts(date);
    const get = (t) => parts.find((p) => p.type === t)?.value || "";
    return `${get("year")}-${get("month")}-${get("day")}`;
  }

  function tzHm(isoStr, tz = prefs.time_zone) {
    const d = new Date(isoStr);
    const parts = new Intl.DateTimeFormat("en-GB", {
      timeZone: tz,
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    }).formatToParts(d);
    const h = Number(parts.find((p) => p.type === "hour")?.value || 0);
    const m = Number(parts.find((p) => p.type === "minute")?.value || 0);
    return { h, m, label: `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}` };
  }

  function addDays(dateStr, delta) {
    const [y, m, d] = dateStr.split("-").map(Number);
    const dt = new Date(Date.UTC(y, m - 1, d + delta));
    return dt.toISOString().slice(0, 10);
  }

  function startOfWeek(dateStr) {
    const [y, m, d] = dateStr.split("-").map(Number);
    const dt = new Date(Date.UTC(y, m - 1, d));
    const dow = dt.getUTCDay(); // 0=Sun..6=Sat
    const first = prefs.first_weekday === "sunday" ? 0 : 1;
    const diff = (dow - first + 7) % 7;
    return addDays(dateStr, -diff);
  }

  function formatDayHeader(dateStr) {
    const [y, m, d] = dateStr.split("-").map(Number);
    const dt = new Date(Date.UTC(y, m - 1, d, 12, 0, 0));
    return new Intl.DateTimeFormat("en-GB", {
      weekday: "short",
      day: "numeric",
      month: "short",
      timeZone: "UTC",
    }).format(dt);
  }

  function sourceById(id) {
    return sources.find((s) => s.id === id) || { id, name: "Calendar", color: "#496f9d", freshness: "fresh" };
  }

  function visibleRange() {
    const anchorStr = tzDateStr(anchorDate);
    if (activeView === "day") {
      return { startDate: anchorStr, endDate: addDays(anchorStr, 1), days: [anchorStr] };
    }
    if (activeView === "month") {
      const [y, m] = anchorStr.split("-").map(Number);
      const firstOfMonth = `${y}-${String(m).padStart(2, "0")}-01`;
      const gridStart = startOfWeek(firstOfMonth);
      const days = Array.from({ length: 42 }, (_, i) => addDays(gridStart, i));
      return { startDate: gridStart, endDate: addDays(gridStart, 42), days, month: m };
    }
    const ws = startOfWeek(anchorStr);
    const days = Array.from({ length: 7 }, (_, i) => addDays(ws, i));
    return { startDate: ws, endDate: addDays(ws, 7), days };
  }

  function periodTitle(range) {
    if (activeView === "month") {
      const [y, m] = tzDateStr(anchorDate).split("-").map(Number);
      return new Intl.DateTimeFormat("en-GB", { month: "long", year: "numeric", timeZone: "UTC" })
        .format(new Date(Date.UTC(y, m - 1, 15)));
    }
    if (activeView === "day") return formatDayHeader(range.startDate);
    return `${formatDayHeader(range.startDate)} – ${formatDayHeader(addDays(range.endDate, -1))}`;
  }

  function occursOnDay(occ, dayStr) {
    if (occ.time.type === "all_day") {
      return occ.time.start_date <= dayStr && dayStr < occ.time.end_date;
    }
    const sDay = tzDateStr(new Date(occ.time.start));
    const eDay = tzDateStr(new Date(new Date(occ.time.end).getTime() - 1));
    return sDay <= dayStr && dayStr <= eDay;
  }

  function layoutDayTimedEvents(timedOccs) {
    const items = timedOccs.map((occ) => {
      const s = tzHm(occ.time.start);
      const e = tzHm(occ.time.end);
      const startMin = s.h * 60 + s.m;
      const endMin = Math.max(startMin + 20, e.h * 60 + e.m);
      return { occ, startMin, endMin, startLabel: s.label, endLabel: e.label, col: 0, cols: 1 };
    }).sort((a, b) => a.startMin - b.startMin || a.endMin - b.endMin);

    let cluster = [];
    let clusterEnd = -1;
    function finishCluster() {
      if (cluster.length === 0) return;
      const colEnds = [];
      for (const item of cluster) {
        let placed = false;
        for (let c = 0; c < colEnds.length; c++) {
          if (colEnds[c] <= item.startMin) {
            item.col = c;
            colEnds[c] = item.endMin;
            placed = true;
            break;
          }
        }
        if (!placed) {
          item.col = colEnds.length;
          colEnds.push(item.endMin);
        }
      }
      for (const item of cluster) item.cols = colEnds.length;
      cluster = [];
    }

    for (const item of items) {
      if (item.startMin >= clusterEnd) finishCluster();
      cluster.push(item);
      clusterEnd = Math.max(clusterEnd, item.endMin);
    }
    finishCluster();
    return items;
  }

  function syncedAgo(iso) {
    const then = Date.parse(iso || "");
    if (!Number.isFinite(then)) return "a while ago";
    const mins = Math.max(0, Math.round((Date.now() - then) / 60000));
    if (mins < 60) return `${mins} min ago`;
    const hours = Math.round(mins / 60);
    if (hours < 48) return `${hours} h ago`;
    return `${Math.round(hours / 24)} days ago`;
  }

  function makeEventPill(occ, extraClass = "") {
    const src = sourceById(occ.source_id);
    const pill = el("button", `cal-event-pill ${occ.status || "confirmed"} ${extraClass}`.trim());
    pill.type = "button";
    pill.style.borderLeftColor = src.color || "#496f9d";
    const title = el("span", "cal-event-title", occ.title);
    const timeText = occ.time.type === "all_day"
      ? "All day"
      : `${tzHm(occ.time.start).label}–${tzHm(occ.time.end).label}`;
    const subParts = [timeText, src.name];
    if (occ.prepare) subParts.push("Prepare");
    const sub = el("span", "cal-event-sub", subParts.join(" · "));
    pill.appendChild(title);
    pill.appendChild(sub);
    pill.addEventListener("click", () => openEventDetail(occ.event_id, occ.key));
    return pill;
  }

  function renderWeekOrDay(range) {
    const wrap = el("div", "cal-grid-wrap");
    const cols = range.days.length;
    const grid = el("div", "cal-week-grid");
    // One time gutter on the left instead of hour labels repeated in every column.
    grid.style.gridTemplateColumns = `44px repeat(${cols}, minmax(0, 1fr))`;

    const todayStr = tzDateStr(new Date());
    grid.appendChild(el("div", "cal-day-hdr cal-gutter", ""));
    for (const day of range.days) {
      const hdr = el("div", `cal-day-hdr${day === todayStr ? " today" : ""}`, formatDayHeader(day));
      grid.appendChild(hdr);
    }

    // All-day strip
    grid.appendChild(el("div", "cal-allday-cell cal-gutter", "All day"));
    for (const day of range.days) {
      const cell = el("div", "cal-allday-cell");
      const allDay = windowData.occurrences.filter((o) => o.time.type === "all_day" && occursOnDay(o, day));
      for (const occ of allDay) cell.appendChild(makeEventPill(occ));
      grid.appendChild(cell);
    }

    // Timed hours
    const startHr = prefs.day_start_hour ?? 7;
    const endHr = prefs.day_end_hour ?? 23;
    const hourCount = Math.max(1, endHr - startHr);
    const pxPerHour = 40;

    const gutter = el("div", "cal-day-col cal-gutter");
    gutter.style.height = `${hourCount * pxPerHour}px`;
    for (let h = startHr; h < endHr; h++) {
      gutter.appendChild(el("div", "cal-hour-slot", `${String(h).padStart(2, "0")}:00`));
    }
    grid.appendChild(gutter);

    for (const day of range.days) {
      const col = el("div", "cal-day-col");
      col.style.height = `${hourCount * pxPerHour}px`;
      for (let h = startHr; h < endHr; h++) {
        col.appendChild(el("div", "cal-hour-slot", ""));
      }
      const timed = windowData.occurrences.filter((o) => o.time.type === "timed" && occursOnDay(o, day));
      const laidOut = layoutDayTimedEvents(timed);
      for (const item of laidOut) {
        const topPx = Math.max(0, ((item.startMin - startHr * 60) / 60) * pxPerHour);
        const heightPx = Math.max(24, ((item.endMin - item.startMin) / 60) * pxPerHour);
        const widthPct = 100 / item.cols;
        const leftPct = item.col * widthPct;
        // Under ~50 min there is room for one line: title and time side by side.
        const pill = makeEventPill(item.occ, heightPx < 34 ? "timed compact" : "timed");
        pill.style.top = `${topPx}px`;
        pill.style.height = `${heightPx}px`;
        pill.style.left = `calc(${leftPct}% + 2px)`;
        pill.style.width = `calc(${widthPct}% - 4px)`;
        col.appendChild(pill);
      }
      grid.appendChild(col);
    }

    wrap.appendChild(grid);
    return wrap;
  }

  function renderMonth(range) {
    const grid = el("div", "cal-month-grid");
    for (const day of range.days) {
      const m = Number(day.split("-")[1]);
      const cell = el("div", `cal-month-cell${m !== range.month ? " muted" : ""}`);
      cell.appendChild(el("span", "cal-month-num", String(Number(day.split("-")[2]))));
      const dayOccs = windowData.occurrences.filter((o) => occursOnDay(o, day));
      for (const occ of dayOccs.slice(0, 3)) {
        cell.appendChild(makeEventPill(occ));
      }
      if (dayOccs.length > 3) {
        const more = el("button", "cal-more-btn", `+${dayOccs.length - 3} more`);
        more.type = "button";
        more.addEventListener("click", () => {
          const [y, mo, d] = day.split("-").map(Number);
          anchorDate = new Date(Date.UTC(y, mo - 1, d, 12, 0, 0));
          activeView = "day";
          refreshWindow();
        });
        cell.appendChild(more);
      }
      grid.appendChild(cell);
    }
    return grid;
  }

  function renderAgenda(range) {
    const wrap = el("div", "cal-agenda-list");
    if (!windowData.occurrences || windowData.occurrences.length === 0) {
      wrap.appendChild(el("div", "cal-banner", "No events this week."));
      return wrap;
    }
    for (const day of range.days) {
      const dayOccs = windowData.occurrences.filter((o) => occursOnDay(o, day));
      if (dayOccs.length === 0) continue;
      const dayBox = el("div");
      dayBox.appendChild(el("div", "cal-agenda-day-hdr", formatDayHeader(day)));
      const list = el("div", "cal-agenda-list");
      for (const occ of dayOccs) {
        const src = sourceById(occ.source_id);
        const row = el("button", `cal-agenda-row ${occ.status || "confirmed"}`);
        row.type = "button";
        row.style.borderLeftColor = src.color || "#496f9d";
        const left = el("div");
        left.appendChild(el("div", "cal-event-title", occ.title));
        const timeStr = occ.time.type === "all_day"
          ? "All day"
          : `${tzHm(occ.time.start).label}–${tzHm(occ.time.end).label}`;
        const meta = [timeStr, src.name, occ.location].filter(Boolean).join(" · ");
        left.appendChild(el("div", "cal-event-sub", meta));
        row.appendChild(left);
        if (occ.prepare) row.appendChild(el("span", "cal-pill", "Needs preparation"));
        row.addEventListener("click", () => openEventDetail(occ.event_id, occ.key));
        list.appendChild(row);
      }
      dayBox.appendChild(list);
      wrap.appendChild(dayBox);
    }
    if (wrap.children.length === 0) {
      wrap.appendChild(el("div", "cal-banner", "No events this week."));
    }
    return wrap;
  }

  function renderSourcesSection() {
    const card = el("section", "cal-card cal-sources-panel");
    const top = el("div", "cal-row");
    top.appendChild(el("strong", "", "Calendar sources & import"));
    card.appendChild(top);

    // Add feed form
    const feedForm = el("form", "cal-row");
    const nameInp = el("input", "cal-input");
    nameInp.placeholder = "Feed name (e.g. Personal)";
    nameInp.style.flex = "1 1 140px";
    const urlInp = el("input", "cal-input");
    urlInp.placeholder = "https://… or webcal://… (.ics URL)";
    urlInp.style.flex = "2 1 240px";
    const colorInp = el("input");
    colorInp.type = "color";
    colorInp.value = "#00b894";
    colorInp.setAttribute("aria-label", "Feed colour");
    const addFeedBtn = el("button", "cal-btn primary", "Add a calendar feed");
    addFeedBtn.type = "submit";
    feedForm.appendChild(nameInp);
    feedForm.appendChild(urlInp);
    feedForm.appendChild(colorInp);
    feedForm.appendChild(addFeedBtn);
    feedForm.addEventListener("submit", async (e) => {
      e.preventDefault();
      if (!nameInp.value.trim() || !urlInp.value.trim()) return;
      addFeedBtn.disabled = true;
      try {
        await fetchJson("/api/calendar/sources", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            name: nameInp.value.trim(),
            url: urlInp.value.trim(),
            color: colorInp.value,
          }),
        });
        nameInp.value = "";
        urlInp.value = "";
        banner = { text: "Calendar feed added and synced.", level: "info" };
        await refreshAll();
      } catch (err) {
        banner = { text: err.message, level: "err" };
        render();
      } finally {
        addFeedBtn.disabled = false;
      }
    });
    card.appendChild(feedForm);

    // Source list
    for (const s of sources) {
      const row = el("div", "cal-source-row");
      const left = el("div", "cal-row");
      const dot = el("span", "cal-source-dot");
      dot.style.backgroundColor = s.color || "#496f9d";
      const vis = el("input");
      vis.type = "checkbox";
      vis.checked = !!s.visible;
      vis.setAttribute("aria-label", `Show ${s.name}`);
      vis.addEventListener("change", async () => {
        try {
          await fetchJson(`/api/calendar/sources/${encodeURIComponent(s.id)}`, {
            method: "PATCH",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ visible: vis.checked }),
          });
          await refreshAll();
        } catch (err) {
          banner = { text: err.message, level: "err" };
          render();
        }
      });
      left.appendChild(vis);
      left.appendChild(dot);
      left.appendChild(el("strong", "", s.name));
      left.appendChild(el("span", "cal-pill", `${s.event_count} events`));
      left.appendChild(el("span", "cal-pill", s.freshness));
      if (s.url_hint) left.appendChild(el("span", "cal-pill", s.url_hint));
      if (s.last_error) left.appendChild(el("span", "cal-pill", s.last_error));
      row.appendChild(left);

      const right = el("div", "cal-row");
      if (s.kind === "ics_feed") {
        const syncBtn = el("button", "cal-btn", "Sync now");
        syncBtn.type = "button";
        syncBtn.addEventListener("click", async () => {
          syncBtn.disabled = true;
          try {
            await fetchJson(`/api/calendar/sources/${encodeURIComponent(s.id)}/sync`, { method: "POST" });
            banner = { text: `${s.name} synced.`, level: "info" };
            await refreshAll();
          } catch (err) {
            banner = { text: err.message, level: "err" };
            render();
          }
        });
        const rmBtn = el("button", "cal-btn danger", "Remove");
        rmBtn.type = "button";
        rmBtn.addEventListener("click", async () => {
          try {
            await fetchJson(`/api/calendar/sources/${encodeURIComponent(s.id)}`, { method: "DELETE" });
            await refreshAll();
          } catch (err) {
            banner = { text: err.message, level: "err" };
            render();
          }
        });
        right.appendChild(syncBtn);
        right.appendChild(rmBtn);
      }
      row.appendChild(right);
      card.appendChild(row);
    }

    // Import .ics
    const impRow = el("div", "cal-row");
    const fileInp = el("input");
    fileInp.type = "file";
    fileInp.accept = ".ics,text/calendar";
    fileInp.setAttribute("aria-label", "Import .ics file");
    fileInp.addEventListener("change", async () => {
      const f = fileInp.files?.[0];
      if (!f) return;
      try {
        const text = await f.text();
        importPreview = await fetchJson("/api/calendar/import/preview", {
          method: "POST",
          headers: { "Content-Type": "text/calendar" },
          body: text,
        });
        render();
      } catch (err) {
        banner = { text: err.message, level: "err" };
        render();
      }
    });
    impRow.appendChild(el("span", "", "Import .ics file:"));
    impRow.appendChild(fileInp);
    card.appendChild(impRow);

    if (importPreview) {
      const prevBox = el("div", "cal-banner");
      const newCount = (importPreview.events || []).filter((e) => !e.duplicate).length;
      const dupCount = (importPreview.events || []).filter((e) => e.duplicate).length;
      prevBox.appendChild(el("div", "", `Preview: ${newCount} new, ${dupCount} duplicates, ${(importPreview.skipped || []).length} skipped categories.`));
      for (const w of importPreview.warnings || []) {
        prevBox.appendChild(el("div", "", `Warning: ${w}`));
      }
      const commitBtn = el("button", "cal-btn primary", `Import ${newCount} events`);
      commitBtn.type = "button";
      commitBtn.addEventListener("click", async () => {
        try {
          const res = await fetchJson("/api/calendar/import/commit", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ preview_id: importPreview.preview_id }),
          });
          importPreview = null;
          banner = { text: `Imported ${res.imported} events (${res.duplicates} duplicates skipped).`, level: "info" };
          await refreshAll();
        } catch (err) {
          banner = { text: err.message, level: "err" };
          render();
        }
      });
      prevBox.appendChild(commitBtn);
      card.appendChild(prevBox);
    }

    return card;
  }

  function renderModal() {
    if (!activeModal) return null;
    const backdrop = el("div", "cal-modal-backdrop");
    const modal = el("div", "cal-modal");
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");

    if (activeModal.kind === "add" || activeModal.kind === "edit") {
      const ev = activeModal.event;
      const isEdit = activeModal.kind === "edit" && ev;
      modal.appendChild(el("strong", "", isEdit ? "Edit event" : "Add event"));

      const form = el("form", "cal-sources-panel");
      const titleInp = el("input", "cal-input");
      titleInp.required = true;
      titleInp.maxLength = 200;
      titleInp.value = ev?.title || "";

      const dateInp = el("input", "cal-input");
      dateInp.type = "date";
      dateInp.value = ev
        ? (ev.time.type === "all_day" ? ev.time.start_date : tzDateStr(new Date(ev.time.start)))
        : (activeModal.dateStr || tzDateStr(anchorDate));

      const allDayChk = el("input");
      allDayChk.type = "checkbox";
      allDayChk.checked = ev?.time?.type === "all_day";

      const startInp = el("input", "cal-input");
      startInp.type = "time";
      startInp.value = ev?.time?.type === "timed" ? tzHm(ev.time.start).label : "09:00";

      const endInp = el("input", "cal-input");
      endInp.type = "time";
      endInp.value = ev?.time?.type === "timed" ? tzHm(ev.time.end).label : "10:00";

      const repeatSel = el("select", "cal-select");
      for (const opt of ["none", "daily", "weekdays", "weekly", "monthly", "yearly"]) {
        const o = el("option", "", opt);
        o.value = opt;
        if ((ev?.repeat || "none") === opt) o.selected = true;
        repeatSel.appendChild(o);
      }

      const locInp = el("input", "cal-input");
      locInp.value = ev?.location || "";
      const notesInp = el("textarea", "cal-input");
      notesInp.rows = 2;
      notesInp.value = ev?.notes || "";

      const addField = (labelText, inputEl) => {
        const f = el("div", "cal-field");
        f.appendChild(el("label", "", labelText));
        f.appendChild(inputEl);
        form.appendChild(f);
      };
      addField("Title", titleInp);
      addField("Date", dateInp);
      const adRow = el("label", "cal-row");
      adRow.appendChild(allDayChk);
      adRow.appendChild(el("span", "", "All-day"));
      form.appendChild(adRow);
      addField("Start time", startInp);
      addField("End time", endInp);
      addField("Repeat", repeatSel);
      addField("Location", locInp);
      addField("Notes", notesInp);

      const btns = el("div", "cal-row");
      const saveBtn = el("button", "cal-btn primary", isEdit ? "Save changes" : "Save event");
      saveBtn.type = "submit";
      const cancelBtn = el("button", "cal-btn", "Cancel");
      cancelBtn.type = "button";
      cancelBtn.addEventListener("click", () => { activeModal = null; render(); });
      btns.appendChild(saveBtn);
      btns.appendChild(cancelBtn);
      form.appendChild(btns);

      form.addEventListener("submit", async (e) => {
        e.preventDefault();
        const dStr = dateInp.value;
        const time = allDayChk.checked
          ? { type: "all_day", start_date: dStr, end_date: addDays(dStr, 1) }
          : {
              type: "timed",
              start: new Date(`${dStr}T${startInp.value || "09:00"}:00`).toISOString(),
              end: new Date(`${dStr}T${endInp.value || "10:00"}:00`).toISOString(),
              tz: prefs.time_zone,
            };
        const payload = {
          title: titleInp.value.trim(),
          time,
          location: locInp.value.trim() || null,
          notes: notesInp.value.trim() || null,
          repeat: repeatSel.value,
          repeat_until: null,
        };
        try {
          if (isEdit) {
            await fetchJson(`/api/calendar/events/${encodeURIComponent(ev.id)}`, {
              method: "PATCH",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify(payload),
            });
          } else {
            await fetchJson("/api/calendar/events", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify(payload),
            });
          }
          activeModal = null;
          await refreshAll();
        } catch (err) {
          banner = { text: err.message, level: "err" };
          render();
        }
      });
      modal.appendChild(form);
    } else if (activeModal.kind === "detail") {
      const ev = activeModal.event;
      const src = sourceById(ev.source_id);
      modal.appendChild(el("strong", "", ev.title));
      const metaRow = el("div", "cal-row");
      metaRow.appendChild(el("span", "cal-pill", src.name));
      metaRow.appendChild(el("span", "cal-pill", ev.status));
      if (!ev.writable) metaRow.appendChild(el("span", "cal-pill", "Read-only feed"));
      modal.appendChild(metaRow);

      const whenStr = ev.time.type === "all_day"
        ? `All day: ${ev.time.start_date} (until ${ev.time.end_date})`
        : `${tzDateStr(new Date(ev.time.start))} ${tzHm(ev.time.start).label}–${tzHm(ev.time.end).label} (${ev.time.tz})`;
      modal.appendChild(el("div", "", whenStr));
      if (ev.location) modal.appendChild(el("div", "", `Location: ${ev.location}`));
      if (ev.notes) modal.appendChild(el("div", "", `Notes: ${ev.notes}`));

      const prepBtn = el("button", "cal-btn", ev.prepare ? "Needs preparation: On" : "Needs preparation: Off");
      prepBtn.type = "button";
      prepBtn.addEventListener("click", async () => {
        try {
          const updated = await fetchJson(`/api/calendar/events/${encodeURIComponent(ev.id)}`, {
            method: "PATCH",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ prepare: !ev.prepare }),
          });
          activeModal = { ...activeModal, event: updated };
          await refreshWindow();
        } catch (err) {
          banner = { text: err.message, level: "err" };
          render();
        }
      });

      const actions = el("div", "cal-row");
      actions.appendChild(prepBtn);
      if (ev.writable) {
        const editBtn = el("button", "cal-btn primary", "Edit");
        editBtn.type = "button";
        editBtn.addEventListener("click", () => {
          activeModal = { kind: "edit", event: ev };
          render();
        });
        const delBtn = el("button", "cal-btn danger", "Delete");
        delBtn.type = "button";
        delBtn.addEventListener("click", async () => {
          try {
            await fetchJson(`/api/calendar/events/${encodeURIComponent(ev.id)}?scope=all`, { method: "DELETE" });
            activeModal = null;
            await refreshAll();
          } catch (err) {
            banner = { text: err.message, level: "err" };
            render();
          }
        });
        actions.appendChild(editBtn);
        actions.appendChild(delBtn);
      }
      const closeBtn = el("button", "cal-btn", "Close");
      closeBtn.type = "button";
      closeBtn.addEventListener("click", () => { activeModal = null; render(); });
      actions.appendChild(closeBtn);
      modal.appendChild(actions);
    }

    backdrop.appendChild(modal);
    return backdrop;
  }

  function render() {
    const page = document.getElementById("calendarPage");
    if (!page) return;
    page.replaceChildren();

    const range = visibleRange();
    const shell = el("div", "cal-shell");

    // Header
    const hdr = el("header", "cal-header");
    const navGroup = el("div", "cal-nav-group");
    navGroup.appendChild(el("h1", "cal-period-title", periodTitle(range)));
    const prevBtn = el("button", "cal-btn", "‹");
    prevBtn.type = "button";
    prevBtn.setAttribute("aria-label", "Previous period");
    prevBtn.addEventListener("click", () => stepPeriod(-1));
    const todayBtn = el("button", "cal-btn", "Today");
    todayBtn.type = "button";
    todayBtn.addEventListener("click", () => { anchorDate = new Date(); refreshWindow(); });
    const nextBtn = el("button", "cal-btn", "›");
    nextBtn.type = "button";
    nextBtn.setAttribute("aria-label", "Next period");
    nextBtn.addEventListener("click", () => stepPeriod(1));
    navGroup.appendChild(prevBtn);
    navGroup.appendChild(todayBtn);
    navGroup.appendChild(nextBtn);
    hdr.appendChild(navGroup);

    const viewGroup = el("div", "cal-view-group");
    for (const [id, label] of [["month", "Month"], ["week", "Week"], ["day", "Day"], ["agenda", "Agenda"]]) {
      const b = el("button", `cal-btn${activeView === id ? " active" : ""}`, label);
      b.type = "button";
      b.addEventListener("click", () => { activeView = id; refreshWindow(); });
      viewGroup.appendChild(b);
    }
    hdr.appendChild(viewGroup);

    const actGroup = el("div", "cal-action-group");
    const addBtn = el("button", "cal-btn primary", "Add event");
    addBtn.type = "button";
    addBtn.addEventListener("click", () => { activeModal = { kind: "add", dateStr: tzDateStr(anchorDate) }; render(); });
    const srcBtn = el("button", `cal-btn${showSourcesPanel ? " active" : ""}`, "Sources");
    srcBtn.type = "button";
    srcBtn.addEventListener("click", () => { showSourcesPanel = !showSourcesPanel; render(); });
    const cancelBtn = el("button", `cal-btn${includeCancelled ? " active" : ""}`, includeCancelled ? "Hide cancelled" : "Show cancelled");
    cancelBtn.type = "button";
    cancelBtn.addEventListener("click", () => { includeCancelled = !includeCancelled; refreshWindow(); });
    const closeBtn = el("button", "cal-btn", "Close");
    closeBtn.type = "button";
    closeBtn.addEventListener("click", close);
    actGroup.appendChild(addBtn);
    actGroup.appendChild(srcBtn);
    actGroup.appendChild(cancelBtn);
    actGroup.appendChild(closeBtn);
    hdr.appendChild(actGroup);
    shell.appendChild(hdr);

    if (banner) {
      shell.appendChild(el("div", `cal-banner ${banner.level}`, banner.text));
    }

    if (windowData.truncated) {
      shell.appendChild(el("div", "cal-banner warn", "Showing the first 5,000 events in this window. Narrow the range to see all."));
    }

    for (const s of sources) {
      if (s.freshness === "stale") {
        shell.appendChild(el("div", "cal-banner warn", `${s.name}: Last synced ${syncedAgo(s.last_sync_at)} — retrying.`));
      } else if (s.freshness === "failed") {
        shell.appendChild(el("div", "cal-banner err", `${s.name}: Sync failed (${s.last_error || "unavailable"}).`));
      }
    }

    if (showSourcesPanel) {
      shell.appendChild(renderSourcesSection());
    }

    const bodyCard = el("section", "cal-card");
    if (activeView === "week" || activeView === "day") bodyCard.appendChild(renderWeekOrDay(range));
    else if (activeView === "month") bodyCard.appendChild(renderMonth(range));
    else bodyCard.appendChild(renderAgenda(range));
    shell.appendChild(bodyCard);

    page.appendChild(shell);
    const modal = renderModal();
    if (modal) page.appendChild(modal);
  }

  function stepPeriod(dir) {
    const str = tzDateStr(anchorDate);
    let nextStr = str;
    if (activeView === "month") {
      const [y, m] = str.split("-").map(Number);
      const dt = new Date(Date.UTC(y, m - 1 + dir, 15));
      anchorDate = dt;
      refreshWindow();
      return;
    }
    const delta = activeView === "week" ? dir * 7 : dir;
    nextStr = addDays(str, delta);
    const [y, m, d] = nextStr.split("-").map(Number);
    anchorDate = new Date(Date.UTC(y, m - 1, d, 12, 0, 0));
    refreshWindow();
  }

  async function openEventDetail(eventId, occurrenceKey) {
    try {
      const ev = await fetchJson(`/api/calendar/events/${encodeURIComponent(eventId)}`);
      activeModal = { kind: "detail", event: ev, occurrenceKey };
      render();
    } catch (err) {
      banner = { text: err.message, level: "err" };
      render();
    }
  }

  async function refreshWindow() {
    const range = visibleRange();
    const startIso = `${range.startDate}T00:00:00Z`;
    const endIso = `${range.endDate}T00:00:00Z`;
    try {
      windowData = await fetchJson(
        `/api/calendar/events?start=${encodeURIComponent(startIso)}&end=${encodeURIComponent(endIso)}&include_cancelled=${includeCancelled}`,
      );
      render();
    } catch (err) {
      banner = { text: err.message, level: "err" };
      render();
    }
  }

  async function refreshAll() {
    try {
      const [p, s] = await Promise.all([
        fetchJson("/api/calendar/preferences").catch(() => prefs),
        fetchJson("/api/calendar/sources"),
      ]);
      if (p) prefs = p;
      sources = s?.sources || [];
      await refreshWindow();
    } catch (err) {
      banner = { text: err.message, level: "err" };
      render();
    }
  }

  async function open() {
    const page = document.getElementById("calendarPage");
    if (!page) return;
    page.hidden = false;
    render();
    await refreshAll();
  }

  function close() {
    const page = document.getElementById("calendarPage");
    if (page) page.hidden = true;
    activeModal = null;
  }

  function onChanged(msg) {
    if (!msg || typeof msg.revision !== "number") return;
    if (msg.revision > (windowData.revision || 0)) {
      const page = document.getElementById("calendarPage");
      if (page && !page.hidden) refreshAll();
    }
  }

  function onKeyDown(e) {
    const page = document.getElementById("calendarPage");
    if (!page || page.hidden) return;
    const tag = (document.activeElement?.tagName || "").toLowerCase();
    if (tag === "input" || tag === "textarea" || tag === "select") {
      if (e.key === "Escape") { activeModal = null; render(); }
      return;
    }
    if (e.key === "ArrowLeft") { e.preventDefault(); stepPeriod(-1); }
    else if (e.key === "ArrowRight") { e.preventDefault(); stepPeriod(1); }
    else if (e.key === "t") { e.preventDefault(); anchorDate = new Date(); refreshWindow(); }
    else if (e.key === "n") { e.preventDefault(); activeModal = { kind: "add", dateStr: tzDateStr(anchorDate) }; render(); }
    else if (e.key === "Escape") {
      e.preventDefault();
      if (activeModal) { activeModal = null; render(); }
      else close();
    }
  }

  function init() {
    document.getElementById("calendarNavBtn")?.addEventListener("click", () => {
      const page = document.getElementById("calendarPage");
      if (page && page.hidden) open();
      else close();
    });
    document.addEventListener("keydown", onKeyDown);
    if (window.location.hash === "#calendar") open();
  }

  window.SailCalendar = { open, close, refreshAll, refreshWindow, onChanged };

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();

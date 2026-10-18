// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// LogLab's default web client. It uses only the Engine's published API
// (Principle I) and loads nothing from the internet (Principle V). The Engine
// is the authority: after every change this page shows what the Engine
// returned, not what it sent (Principle II). Every time is UTC (Principle VIII).
// Each QSO gets its unique ID when it is typed and waits in this browser's
// journal until the Engine has logged it, so a dropped link never loses one and
// sending it again never duplicates it (Principle XV).
// Each box on the page is a module the operator can move, widen, or hide. The
// Engine keeps their layout, so it follows them to any workstation.
"use strict";

(() => {
  const SESSION_KEY = "loglab.session";
  const JOURNAL_KEY = "loglab.journal";
  const RETRY_MS = 15000;
  const FADE_AFTER_MS = 4000; // a "Logged" notice fades; warnings stay until dealt with
  // ADIF 3.1.7 band and mode enumerations most used on the air.
  const BANDS = ["160m", "80m", "60m", "40m", "30m", "20m", "17m", "15m", "12m", "10m", "6m", "2m", "1.25m", "70cm"];
  const MODES = ["SSB", "CW", "FT8", "FT4", "FM", "AM", "RTTY", "PSK", "DIGITALVOICE", "JS8", "OLIVIA", "SSTV"];
  // FT4 is a submode of MFSK in ADIF; the entry form writes it correctly.
  const SUBMODE_OF = { FT4: "MFSK", JS8: "MFSK" };

  const MODULES = { entry: "New QSO", journal: "Not logged yet", log: "Log", detail: "QSO detail" };
  // The journal is never hidden: the operator must always see what is not
  // logged yet (Principle XV). The detail panel opens from the log.
  const HIDEABLE = new Set(["entry", "log"]);
  const DEFAULT_LAYOUT = Object.freeze({
    version: 1,
    modules: Object.keys(MODULES).map((id) => ({ id, width: "full", hidden: false })),
  });
  const SAVE_AFTER_MS = 400;

  const $ = (selector, root = document) => root.querySelector(selector);
  let session = null;
  let current = null; // the QSO open in the detail panel

  // -- API ---------------------------------------------------------------

  class ApiError extends Error {
    constructor(status, message) {
      super(message);
      this.status = status;
    }
  }

  async function api(method, path, body) {
    const headers = { Accept: "application/json" };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (session) headers.Authorization = `Bearer ${session.token}`;
    let response;
    try {
      response = await fetch(`/api/v1${path}`, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch {
      throw new ApiError(0, "The Engine cannot be reached. Nothing was changed.");
    }
    if (response.status === 401 && session) {
      forget();
      showSignIn("Your session has ended. Sign in again.");
      throw new ApiError(401, "Signed out");
    }
    if (response.status === 204) return null;
    const data = await response.json().catch(() => null);
    if (!response.ok) throw new ApiError(response.status, describe(data) || response.statusText);
    return data;
  }

  function describe(data) {
    if (!data || data.detail === undefined) return "";
    if (typeof data.detail === "string") return data.detail;
    if (Array.isArray(data.detail)) {
      return data.detail.map((d) => `${(d.loc || []).slice(1).join(".")}: ${d.msg}`).join("; ");
    }
    return JSON.stringify(data.detail);
  }

  // -- Journal: QSOs typed here that the Engine has not logged yet --------

  function newId() {
    const b = crypto.getRandomValues(new Uint8Array(16));
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
    const hex = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
    return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
  }

  function readJournal() {
    try {
      const saved = JSON.parse(localStorage.getItem(JOURNAL_KEY) || "[]");
      return Array.isArray(saved) ? saved : [];
    } catch { return []; }
  }

  // Returns false when this browser cannot keep a journal (private mode, full).
  function writeJournal(entries) {
    try {
      localStorage.setItem(JOURNAL_KEY, JSON.stringify(entries));
      return true;
    } catch { return false; }
  }

  function updateEntry(id, change) {
    const entries = readJournal();
    const next = change === null
      ? entries.filter((e) => e.id !== id)
      : entries.map((e) => (e.id === id ? { ...e, ...change } : e));
    writeJournal(next);
    renderJournal();
  }

  function mine(entry) {
    return session && entry.operator === session.operator && entry.station === session.station;
  }

  function summary(entry) {
    const value = (name) => (entry.fields.find((f) => f.name === name) || {}).value || "";
    const date = value("QSO_DATE");
    const time = value("TIME_ON");
    return [value("CALL") || "(no call)", value("BAND"), value("SUBMODE") || value("MODE"),
      date && `${date.slice(0, 4)}-${date.slice(4, 6)}-${date.slice(6, 8)}`,
      time && `${time.slice(0, 2)}:${time.slice(2, 4)} UTC`].filter(Boolean).join(" ");
  }

  // QSOs on their way to the Engine right now. They are already in the
  // journal, but are only shown as waiting if sending them fails, so storing a
  // QSO does not flash the "Not logged yet" box.
  const sending = new Set();

  function renderJournal() {
    const entries = readJournal().filter((e) => !sending.has(e.id));
    const list = $("#journal-list");
    list.replaceChildren();
    for (const entry of entries) {
      const item = document.createElement("li");
      const what = document.createElement("span");
      what.className = "what";
      what.textContent = summary(entry);
      let state;
      if (entry.problem) state = `refused by the Engine: ${entry.problem}`;
      else if (!mine(entry)) state = `waiting for ${entry.operator} to sign in at ${entry.station}`;
      else state = "waiting for the Engine";
      item.append(what, ` · typed by ${entry.operator} at ${entry.station} · ${state}`);
      if (entry.problem) {
        const discard = document.createElement("button");
        discard.type = "button";
        discard.className = "link";
        discard.textContent = "Discard";
        discard.addEventListener("click", () => {
          if (confirm(`Discard ${summary(entry)}? The Engine refused it, and it will be gone from this browser.`)) {
            updateEntry(entry.id, null);
          }
        });
        item.append(" ", discard);
      }
      list.append(item);
    }
    $("#journal").hidden = entries.length === 0;
    $("#journal-count").textContent = String(entries.length);
    const waiting = $("#sign-in-journal");
    waiting.hidden = entries.length === 0;
    waiting.textContent = entries.length === 1
      ? "This browser is holding 1 QSO the Engine has not logged yet. Sign in to send it."
      : `This browser is holding ${entries.length} QSOs the Engine has not logged yet. Sign in to send them.`;
  }

  // Send one journal entry. True when the Engine has logged it.
  async function send(entry) {
    sending.add(entry.id);
    try {
      const result = await api("POST", "/qsos", { id: entry.id, fields: entry.fields });
      sending.delete(entry.id);
      updateEntry(entry.id, null);
      return result;
    } catch (e) {
      sending.delete(entry.id);
      // Unreachable, an Engine error, or signed out: keep it and try again later.
      if (e.status === 0 || e.status >= 500 || e.status === 401) {
        renderJournal();
        return null;
      }
      updateEntry(entry.id, { problem: e.message });
      throw e;
    }
  }

  let flushing = false;

  async function flushJournal() {
    if (flushing || !session || !session.permissions.includes("log")) return;
    flushing = true;
    let sent = 0;
    try {
      for (const entry of readJournal()) {
        if (entry.problem || !mine(entry)) continue;
        let result = null;
        try { result = await send(entry); } catch { continue; }
        if (result === null) break; // the Engine is out of reach; try again later
        sent += 1;
      }
    } finally {
      flushing = false;
    }
    if (sent) await loadLog();
  }

  // -- Workspace: modules the operator arranges --------------------------

  let layout = structuredClone(DEFAULT_LAYOUT);
  let saveTimer = null;

  const moduleEl = (id) => $(`.module[data-module="${id}"]`);

  // Keep what is known from a saved layout, in its order; add anything new.
  function normalize(saved) {
    const seen = new Set();
    const modules = [];
    for (const m of (saved && saved.modules) || []) {
      if (!(m.id in MODULES) || seen.has(m.id)) continue;
      seen.add(m.id);
      modules.push({ id: m.id, width: m.width === "half" ? "half" : "full", hidden: HIDEABLE.has(m.id) && m.hidden === true });
    }
    for (const id of Object.keys(MODULES)) {
      if (!seen.has(id)) modules.push({ id, width: "full", hidden: false });
    }
    return { version: 1, modules };
  }

  // The page's order never changes, only where each box is drawn, so the
  // keyboard and screen readers keep a stable order.
  function applyLayout() {
    layout.modules.forEach((m, index) => {
      const el = moduleEl(m.id);
      el.style.order = String(index);
      el.classList.toggle("half", m.width === "half");
      el.classList.toggle("off", m.hidden);
      const head = $(".module-head", el);
      $("[data-act=earlier]", head).disabled = index === 0;
      $("[data-act=later]", head).disabled = index === layout.modules.length - 1;
      const width = $("[data-act=width]", head);
      width.textContent = m.width === "half" ? "Full width" : "Half width";
    });
    const tray = $("#tray");
    tray.replaceChildren();
    for (const m of layout.modules.filter((x) => x.hidden)) {
      const show = document.createElement("button");
      show.type = "button";
      show.className = "small secondary";
      show.textContent = `Show ${MODULES[m.id]}`;
      show.addEventListener("click", () => changeLayout(m.id, { hidden: false }));
      tray.append(show);
    }
  }

  function saveLayout() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(async () => {
      if (!session) return;
      try {
        await api("PUT", "/layout", layout);
        $("#arrange-hint").classList.remove("error");
      } catch (e) {
        $("#arrange-hint").textContent = `Your layout was not saved: ${e.message}`;
        $("#arrange-hint").classList.add("error");
      }
    }, SAVE_AFTER_MS);
  }

  function changeLayout(id, change) {
    layout.modules = layout.modules.map((m) => (m.id === id ? { ...m, ...change } : m));
    applyLayout();
    saveLayout();
  }

  function move(id, to) {
    const from = layout.modules.findIndex((m) => m.id === id);
    const clamped = Math.max(0, Math.min(layout.modules.length - 1, to));
    if (from === clamped) return false;
    const [m] = layout.modules.splice(from, 1);
    layout.modules.splice(clamped, 0, m);
    applyLayout();
    return true;
  }

  async function loadLayout() {
    try {
      const saved = await api("GET", "/layout");
      layout = normalize(saved.layout || DEFAULT_LAYOUT);
    } catch {
      layout = normalize(DEFAULT_LAYOUT); // shown as the default until the Engine answers
    }
    applyLayout();
  }

  function arrange(on) {
    document.body.classList.toggle("arranging", on);
    $("#arrange-bar").hidden = !on;
    $("#arrange").setAttribute("aria-pressed", String(on));
  }

  function toolButton(act, text, label) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "small secondary";
    button.dataset.act = act;
    button.textContent = text;
    if (label) button.setAttribute("aria-label", label);
    return button;
  }

  // Dragging uses pointer events, so it works with a mouse, a pen, or a finger.
  function startDrag(event, id) {
    if (event.button !== 0) return;
    event.preventDefault();
    const el = moduleEl(id);
    const handle = event.currentTarget;
    handle.setPointerCapture(event.pointerId);
    el.classList.add("dragging");
    const onMove = (e) => {
      el.style.pointerEvents = "none";
      const under = document.elementFromPoint(e.clientX, e.clientY);
      el.style.pointerEvents = "";
      const target = under && under.closest(".module");
      if (!target || target === el) return;
      const box = target.getBoundingClientRect();
      const after = target.classList.contains("half") && el.classList.contains("half")
        ? e.clientX > box.left + box.width / 2
        : e.clientY > box.top + box.height / 2;
      const ids = layout.modules.map((m) => m.id);
      const from = ids.indexOf(id);
      let to = ids.indexOf(target.dataset.module) + (after ? 1 : 0);
      if (from < to) to -= 1;
      move(id, to);
    };
    const onUp = () => {
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", onUp);
      handle.removeEventListener("pointercancel", onUp);
      el.classList.remove("dragging");
      saveLayout();
    };
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", onUp);
    handle.addEventListener("pointercancel", onUp);
  }

  function buildModuleHeads() {
    for (const [id, title] of Object.entries(MODULES)) {
      const head = document.createElement("div");
      head.className = "module-head";
      const grip = document.createElement("button");
      grip.type = "button";
      grip.className = "grip";
      grip.textContent = "⠿";
      grip.title = "Drag to move";
      grip.setAttribute("aria-label", `Move ${title}: drag, or use the arrow keys`);
      grip.addEventListener("pointerdown", (event) => startDrag(event, id));
      grip.addEventListener("keydown", (event) => {
        const step = { ArrowUp: -1, ArrowLeft: -1, ArrowDown: 1, ArrowRight: 1 }[event.key];
        if (!step) return;
        event.preventDefault();
        const at = layout.modules.findIndex((m) => m.id === id);
        if (move(id, at + step)) saveLayout();
        grip.focus();
      });
      const name = document.createElement("span");
      name.className = "module-title";
      name.textContent = title;
      const earlier = toolButton("earlier", "↑", `Move ${title} earlier`);
      earlier.addEventListener("click", () => {
        if (move(id, layout.modules.findIndex((m) => m.id === id) - 1)) saveLayout();
      });
      const later = toolButton("later", "↓", `Move ${title} later`);
      later.addEventListener("click", () => {
        if (move(id, layout.modules.findIndex((m) => m.id === id) + 1)) saveLayout();
      });
      const width = toolButton("width", "Half width");
      width.addEventListener("click", () => {
        const m = layout.modules.find((x) => x.id === id);
        changeLayout(id, { width: m.width === "half" ? "full" : "half" });
      });
      head.append(grip, name, earlier, later, width);
      if (HIDEABLE.has(id)) {
        const hide = toolButton("hide", "Hide");
        hide.addEventListener("click", () => changeLayout(id, { hidden: true }));
        head.append(hide);
      }
      const placeholder = document.createElement("p");
      placeholder.className = "hint placeholder";
      placeholder.textContent = id === "journal"
        ? "Appears here when a QSO is waiting to be logged."
        : "Appears here when you open a QSO from the log.";
      moduleEl(id).prepend(head);
      if (id === "journal" || id === "detail") head.after(placeholder);
    }
  }

  // -- Session -----------------------------------------------------------

  function remember(value) {
    session = value;
    try { localStorage.setItem(SESSION_KEY, JSON.stringify(value)); } catch { /* private mode */ }
  }

  function forget() {
    session = null;
    try { localStorage.removeItem(SESSION_KEY); } catch { /* private mode */ }
  }

  function restore() {
    try {
      const saved = JSON.parse(localStorage.getItem(SESSION_KEY) || "null");
      if (saved && typeof saved.token === "string") session = saved;
    } catch { session = null; }
  }

  function showSignIn(message = "") {
    $("#app").hidden = true;
    $("#who").hidden = true;
    $("#sign-out").hidden = true;
    $("#arrange").hidden = true;
    arrange(false);
    clearTimeout(saveTimer);
    layout = normalize(DEFAULT_LAYOUT);
    applyLayout();
    $("#sign-in").hidden = false;
    $("#sign-in-form .error").textContent = message;
    const station = $("#sign-in-form [name=station]");
    if (!station.value) station.value = localStorage.getItem("loglab.station") || "";
    $("#sign-in-form [name=callsign]").focus();
  }

  function showApp() {
    $("#sign-in").hidden = true;
    $("#app").hidden = false;
    $("#who").hidden = false;
    $("#sign-out").hidden = false;
    $("#arrange").hidden = false;
    $("#who").textContent = `${session.operator} at ${session.station}`;
    const mayLog = session.permissions.includes("log");
    for (const el of document.querySelectorAll("#entry button, #edit button[type=submit], #delete, #restore, #add-field")) {
      el.disabled = !mayLog;
    }
    if (!mayLog) setError("#entry", "You can view the log but not change it (no log permission).");
    resetEntry();
    renderJournal();
    loadLayout();
    loadLog();
    flushJournal();
  }

  async function signIn(event) {
    event.preventDefault();
    const form = event.target;
    const error = $(".error", form);
    error.textContent = "";
    const body = {
      callsign: form.callsign.value.trim().toUpperCase(),
      secret: form.secret.value,
      station: form.station.value.trim(),
    };
    try {
      remember(await api("POST", "/sessions", body));
      try { localStorage.setItem("loglab.station", body.station); } catch { /* private mode */ }
      form.secret.value = "";
      showApp();
    } catch (e) {
      error.textContent = e.message;
      form.secret.select();
    }
  }

  async function signOut() {
    try { await api("DELETE", "/sessions/current"); } catch { /* already ended */ }
    forget();
    closeDetail();
    showSignIn("Signed out.");
  }

  // -- Entering a QSO ----------------------------------------------------

  function utcNow() {
    const now = new Date();
    return {
      date: now.toISOString().slice(0, 10),
      time: now.toISOString().slice(11, 16).replace(":", ""),
    };
  }

  function fillSelect(select, values) {
    for (const value of values) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = value;
      select.append(option);
    }
  }

  function resetEntry() {
    const form = $("#entry");
    const { date, time } = utcNow();
    for (const name of ["CALL", "FREQ", "RST_SENT", "RST_RCVD", "NAME", "COMMENT"]) form[name].value = "";
    form.QSO_DATE.value = date;
    form.TIME_ON.value = time;
    form.CALL.focus();
  }

  function entryFields(form) {
    const fields = [];
    const add = (name, value) => {
      const text = value.trim();
      if (text) fields.push({ name, value: text, type: null });
    };
    add("CALL", form.CALL.value.toUpperCase());
    add("QSO_DATE", form.QSO_DATE.value.replaceAll("-", ""));
    add("TIME_ON", form.TIME_ON.value.replace(":", ""));
    add("BAND", form.BAND.value.toLowerCase());
    const mode = form.MODE.value;
    if (SUBMODE_OF[mode]) {
      add("MODE", SUBMODE_OF[mode]);
      add("SUBMODE", mode);
    } else {
      add("MODE", mode);
    }
    add("FREQ", form.FREQ.value);
    add("RST_SENT", form.RST_SENT.value);
    add("RST_RCVD", form.RST_RCVD.value);
    add("NAME", form.NAME.value);
    add("COMMENT", form.COMMENT.value);
    return fields;
  }

  async function storeQso(event) {
    event.preventDefault();
    const form = event.target;
    setError("#entry", "");
    if (form.FREQ.value.trim() && !/^\d+(\.\d+)?$/.test(form.FREQ.value.trim())) {
      setError("#entry", "Frequency is in MHz, like 14.205");
      return;
    }
    const button = $("button[type=submit]", form);
    button.disabled = true;
    const notice = $("#stored");
    hideNotice();
    const entry = {
      id: newId(),
      operator: session.operator,
      station: session.station,
      fields: entryFields(form),
      typedAt: new Date().toISOString(),
      problem: null,
    };
    // Into the journal before it goes anywhere (Principle XV).
    const journaled = writeJournal([...readJournal(), entry]);
    try {
      const result = journaled ? await send(entry) : await api("POST", "/qsos", { id: entry.id, fields: entry.fields });
      if (result === null) {
        // Not confirmed yet: this stays on screen (Principle XV).
        notice.textContent = `${summary(entry)} is saved in this browser but not logged yet: ` +
          (session ? "the Engine cannot be reached. It will be sent when the Engine is back."
            : "sign in again to send it.");
        notice.hidden = false;
      } else {
        notice.textContent = result.message;
        notice.classList.add("ok");
        notice.hidden = false;
        fadeTimer = setTimeout(() => notice.classList.add("fading"), FADE_AFTER_MS);
        await loadLog();
      }
      resetEntry();
    } catch (e) {
      // The Engine refused it: keep the form so it can be fixed and sent again.
      if (journaled) updateEntry(entry.id, null);
      setError("#entry", journaled || e.status !== 0 ? e.message : `${e.message} This browser cannot keep a journal, so the QSO is only in the form.`);
    } finally {
      button.disabled = !session || !session.permissions.includes("log");
    }
  }

  let fadeTimer = null;

  function hideNotice() {
    clearTimeout(fadeTimer);
    const notice = $("#stored");
    notice.hidden = true;
    notice.classList.remove("ok", "fading");
  }

  function setError(selector, message) {
    $(`${selector} .error`).textContent = message;
  }

  // -- The log -----------------------------------------------------------

  function field(qso, name) {
    const found = qso.fields.find((f) => f.name.toUpperCase() === name);
    return found ? found.value : "";
  }

  function utcDate(text) {
    return text ? text.slice(0, 10) : "";
  }

  function utcTime(text) {
    return text ? text.slice(11, 16) : "";
  }

  function cell(row, text, className) {
    const td = document.createElement("td");
    td.textContent = text;
    if (className) td.className = className;
    row.append(td);
  }

  let searchTimer = null;

  async function loadLog() {
    const params = new URLSearchParams({ limit: "100" });
    const call = $("#search").value.trim();
    if (call) params.set("call", call);
    if ($("#show-deleted").checked) params.set("include_deleted", "true");
    let qsos;
    try {
      qsos = await api("GET", `/qsos?${params}`);
    } catch (e) {
      $("#count").textContent = e.message;
      return;
    }
    const body = $("#log tbody");
    body.replaceChildren();
    for (const qso of qsos) {
      const row = document.createElement("tr");
      if (qso.deleted) row.className = "deleted";
      cell(row, utcDate(qso.qso_start) || field(qso, "QSO_DATE"));
      cell(row, utcTime(qso.qso_start) || field(qso, "TIME_ON"));
      cell(row, qso.call || "(no call)", "call");
      cell(row, qso.band || "");
      cell(row, field(qso, "SUBMODE") || qso.mode || "");
      cell(row, qso.operator);
      cell(row, String(qso.version));
      row.addEventListener("click", () => openDetail(qso.id));
      body.append(row);
    }
    $("#count").textContent = qsos.length === 100 ? "latest 100 shown" : `${qsos.length} shown`;
  }

  // -- One QSO: fields, edit, delete, restore, history -------------------

  function fieldRow(f) {
    const row = document.createElement("tr");
    const name = document.createElement("input");
    name.value = f.name;
    name.maxLength = 64;
    name.dataset.type = f.type || "";
    name.setAttribute("aria-label", "Field name");
    const value = document.createElement("input");
    value.value = f.value;
    value.setAttribute("aria-label", "Value");
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "link";
    remove.textContent = "Remove";
    remove.addEventListener("click", () => row.remove());
    for (const el of [name, value, remove]) {
      const td = document.createElement("td");
      td.append(el);
      row.append(td);
    }
    return row;
  }

  async function openDetail(id) {
    let qso;
    let history;
    try {
      [qso, history] = await Promise.all([api("GET", `/qsos/${id}`), api("GET", `/qsos/${id}/history`)]);
    } catch (e) {
      $("#count").textContent = e.message;
      return;
    }
    current = qso;
    $("#detail").hidden = false;
    $("#detail-title").textContent = `${qso.call || "QSO"}${qso.deleted ? " (deleted)" : ""}`;
    $("#detail-meta").textContent =
      `ID ${qso.id} · version ${qso.version} · last changed ${qso.changed_at.slice(0, 16).replace("T", " ")} UTC ` +
      `by ${qso.operator} at ${qso.station}`;
    const rows = $("#edit tbody");
    rows.replaceChildren(...qso.fields.map(fieldRow));
    $("#edit-note").value = "";
    setError("#edit", "");
    $("#delete").hidden = qso.deleted;
    $("#restore").hidden = !qso.deleted;
    $("#edit button[type=submit]").hidden = qso.deleted;
    $("#add-field").hidden = qso.deleted;
    renderHistory(history);
    $("#detail").scrollIntoView({ behavior: "smooth", block: "start" });
  }

  function closeDetail() {
    current = null;
    $("#detail").hidden = true;
  }

  function show(f) {
    return `<${f.name}${f.type ? `:${f.type}` : ""}> '${f.value}'`;
  }

  function renderHistory(versions) {
    const list = $("#history");
    list.replaceChildren();
    let previous = null;
    for (const v of versions) {
      const item = document.createElement("li");
      const what = document.createElement("div");
      const when = v.changed_at.slice(0, 19).replace("T", " ");
      const label = document.createElement("span");
      label.className = "what";
      label.textContent = `v${v.version} ${v.action}`;
      what.append(label, ` by ${v.operator} at ${v.station}, ${when} UTC`);
      if (v.import_file) what.append(` (import ${v.import_id}: ${v.import_file})`);
      if (v.note) what.append(` — “${v.note}”`);
      item.append(what);
      const changes = document.createElement("ul");
      if (previous === null) {
        const li = document.createElement("li");
        li.textContent = `${v.fields.length} fields as first entered`;
        changes.append(li);
      } else {
        const before = previous.fields.map(show);
        const after = v.fields.map(show);
        const removed = before.filter((x) => !after.includes(x));
        const added = after.filter((x) => !before.includes(x));
        for (const x of removed) {
          const li = document.createElement("li");
          li.className = "removed";
          li.textContent = `− ${x}`;
          changes.append(li);
        }
        for (const x of added) {
          const li = document.createElement("li");
          li.className = "added";
          li.textContent = `+ ${x}`;
          changes.append(li);
        }
        if (!removed.length && !added.length) {
          const li = document.createElement("li");
          li.textContent = { delete: "marked deleted; fields kept", restore: "brought back; fields kept" }[v.action] || "fields unchanged";
          changes.append(li);
        }
      }
      item.append(changes);
      list.append(item);
      previous = v;
    }
  }

  function editedFields() {
    const fields = [];
    for (const row of $("#edit tbody").rows) {
      const [name, value] = row.querySelectorAll("input");
      if (!name.value.trim()) continue;
      fields.push({ name: name.value.trim(), value: value.value, type: name.dataset.type || null });
    }
    return fields;
  }

  async function change(method, path, body) {
    setError("#edit", "");
    try {
      const result = await api(method, path, body);
      await loadLog();
      await openDetail(result.qso.id);
    } catch (e) {
      setError("#edit", e.status === 409 ? `${e.message} The latest version is shown now.` : e.message);
      if (e.status === 409 && current) await openDetail(current.id);
    }
  }

  function note() {
    const text = $("#edit-note").value.trim();
    return text || null;
  }

  // -- Start -------------------------------------------------------------

  async function start() {
    buildModuleHeads();
    applyLayout();
    fillSelect($("#entry [name=BAND]"), BANDS);
    fillSelect($("#entry [name=MODE]"), MODES);
    $("#entry [name=BAND]").value = "20m";

    $("#sign-in-form").addEventListener("submit", signIn);
    $("#sign-out").addEventListener("click", signOut);
    $("#arrange").addEventListener("click", () => arrange(!document.body.classList.contains("arranging")));
    $("#arrange-done").addEventListener("click", () => arrange(false));
    $("#arrange-reset").addEventListener("click", () => {
      layout = normalize(DEFAULT_LAYOUT);
      applyLayout();
      saveLayout();
    });
    $("#stored").addEventListener("transitionend", (event) => {
      if (event.propertyName === "opacity" && $("#stored").classList.contains("fading")) hideNotice();
    });
    $("#entry").addEventListener("submit", storeQso);
    $("#send-now").addEventListener("click", flushJournal);
    window.addEventListener("online", flushJournal);
    setInterval(flushJournal, RETRY_MS);
    window.addEventListener("storage", (event) => { if (event.key === JOURNAL_KEY) renderJournal(); });
    $("#now").addEventListener("click", () => {
      const { date, time } = utcNow();
      $("#entry [name=QSO_DATE]").value = date;
      $("#entry [name=TIME_ON]").value = time;
    });
    $("#search").addEventListener("input", () => {
      clearTimeout(searchTimer);
      searchTimer = setTimeout(loadLog, 250);
    });
    $("#show-deleted").addEventListener("change", loadLog);
    $("#close-detail").addEventListener("click", closeDetail);
    $("#add-field").addEventListener("click", () => {
      $("#edit tbody").append(fieldRow({ name: "", value: "", type: null }));
    });
    $("#edit").addEventListener("submit", (event) => {
      event.preventDefault();
      if (!current) return;
      change("PUT", `/qsos/${current.id}`, {
        base_version: current.version,
        fields: editedFields(),
        note: note(),
      });
    });
    $("#delete").addEventListener("click", () => {
      if (current && confirm("Delete this QSO? It is kept in the history and can be restored.")) {
        change("POST", `/qsos/${current.id}/delete`, { note: note() });
      }
    });
    $("#restore").addEventListener("click", () => {
      if (current) change("POST", `/qsos/${current.id}/restore`, { note: note() });
    });

    try {
      const info = await api("GET", "/status");
      $("#version").textContent = `${info.name} ${info.version}`;
      $("#source").href = info.source;
    } catch { /* shown when the first real request fails */ }

    restore();
    renderJournal();
    if (!session) {
      showSignIn();
      return;
    }
    try {
      const me = await api("GET", "/sessions/current");
      session = { ...session, permissions: me.permissions };
      showApp();
    } catch {
      if (!$("#sign-in").hidden) return;
      showSignIn();
    }
  }

  start();
})();

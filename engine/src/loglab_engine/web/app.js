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
// Engine keeps their layout, so it follows them to any workstation. Every
// table is shaped the same way: the operator picks its columns, drags them
// into order, and sorts it; the Engine does the sorting, over the whole log.
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
    modules: Object.keys(MODULES).map((id) => ({ id, width: "full", side: "left", hidden: false })),
    tables: {},
  });
  const SAVE_AFTER_MS = 400;
  // The same checks the Engine makes on a saved table (Principle XII).
  const COLUMN_ID = /^(date|time|call|band|mode|operator|station|version|changed_at|field:[A-Za-z][A-Za-z0-9_]{0,63})$/;
  const SORT_KEY = /^(entered|qso_start|call|band|mode|operator|station|version|changed_at|field:[A-Za-z][A-Za-z0-9_]{0,63})$/;
  const MAX_COLUMNS = 40;

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

  // lookup.js (the QRZ.com lookup) reaches the Engine through the same API.
  window.LogLab = Object.freeze({ api });

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
      modules.push({
        id: m.id,
        width: m.width === "half" ? "half" : "full",
        side: m.side === "right" ? "right" : "left",
        hidden: HIDEABLE.has(m.id) && m.hidden === true,
      });
    }
    for (const id of Object.keys(MODULES)) {
      if (!seen.has(id)) modules.push({ id, width: "full", side: "left", hidden: false });
    }
    const tables = {};
    for (const id of Object.keys(TABLES)) {
      const t = saved && saved.tables && saved.tables[id];
      if (!t || !Array.isArray(t.columns)) continue;
      const names = new Set();
      const columns = t.columns.filter((c) => {
        const ok = typeof c === "string" && COLUMN_ID.test(c) && !names.has(c.toLowerCase());
        if (ok) names.add(c.toLowerCase());
        return ok;
      }).slice(0, MAX_COLUMNS);
      if (!columns.length) continue;
      tables[id] = {
        columns,
        sort: typeof t.sort === "string" && SORT_KEY.test(t.sort) ? t.sort : "entered",
        descending: t.descending !== false,
      };
    }
    return { version: 1, modules, tables };
  }

  const arranging = () => document.body.classList.contains("arranging");

  // Is this box on the page now? Boxes that show only when needed (Not
  // logged yet, QSO detail) take their place while the page is unlocked.
  function shown(m) {
    return !m.hidden && (arranging() || !moduleEl(m.id).hidden);
  }

  // Place each box on the grid: a full-width box takes a row of its own; a
  // half-width box takes its side of the current row, or starts a new row
  // when that side is taken. The page's own order never changes, so the
  // keyboard and screen readers keep a stable order.
  function applyLayout() {
    let row = 1;
    let used = { left: false, right: false, full: false };
    const fresh = () => {
      if (used.left || used.right || used.full) row += 1;
      used = { left: false, right: false, full: false };
    };
    layout.modules.forEach((m, index) => {
      const el = moduleEl(m.id);
      el.style.order = String(index);
      el.classList.toggle("half", m.width === "half");
      el.classList.toggle("off", m.hidden);
      el.dataset.place = m.width === "full" ? "full" : m.side;
      if (!shown(m)) return;
      if (m.width === "full") {
        fresh();
        used.full = true;
        el.style.gridColumn = "1 / -1";
      } else {
        if (used.full || used[m.side]) fresh();
        used[m.side] = true;
        el.style.gridColumn = m.side === "left" ? "1" : "2";
      }
      el.style.gridRow = String(row);
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

  // Put box `id` at `index` in the order, `width` wide, on `side`. True if
  // anything changed.
  function place(id, index, width, side) {
    const from = layout.modules.findIndex((m) => m.id === id);
    const m = layout.modules[from];
    const to = Math.max(0, Math.min(layout.modules.length - 1, index));
    if (from === to && m.width === width && (width === "full" || m.side === side)) return false;
    layout.modules.splice(from, 1);
    layout.modules.splice(to, 0, { ...m, width, side: width === "half" ? side : m.side });
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
    for (const table of Object.values(TABLES)) table.refresh();
  }

  // Locked is the normal state. Unlocked, every box shows a handle to move it.
  function arrange(on) {
    document.body.classList.toggle("arranging", on);
    $("#arrange-bar").hidden = !on;
    const toggle = $("#arrange");
    toggle.setAttribute("aria-pressed", String(on));
    toggle.textContent = on ? "🔓 Unlocked" : "🔒 Locked";
    toggle.title = on ? "Lock the layout" : "Unlock to move boxes";
    applyLayout();
  }

  // Where a box dropped at (x, y) goes. Across the page: near the centre
  // line it is full width; left or right of that, half width on that side.
  // Down the page: before the first other box whose middle is below the
  // pointer. Positions are measured when the drag starts, so the boxes
  // moving aside as you drag never change the answer.
  const CENTRE_BAND = 0.2; // the middle fifth of the page counts as the centre line

  function dropTarget(x, y, others, area) {
    let width = "full";
    let side = "left";
    const single = getComputedStyle($("#workspace")).gridTemplateColumns.split(" ").length < 2;
    const across = (x - area.left) / area.width;
    if (!single && across < 0.5 - CENTRE_BAND / 2) width = "half";
    if (!single && across > 0.5 + CENTRE_BAND / 2) {
      width = "half";
      side = "right";
    }
    let index = layout.modules.length - 1;
    for (const o of others) {
      if (y < o.top + o.height / 2) {
        index = o.index;
        break;
      }
    }
    return { index, width, side };
  }

  // Dragging uses pointer events, so it works with a mouse, a pen, or a
  // finger. The box's place shows as an outline; a small card follows the
  // pointer like a window being carried.
  function startDrag(event, id) {
    if (event.button !== 0) return;
    event.preventDefault();
    const handle = event.currentTarget;
    const el = moduleEl(id);
    const before = structuredClone(layout.modules);
    const area = $("#workspace").getBoundingClientRect();
    // Where the other boxes are, in order, with each one's index once this
    // box is lifted out of the order.
    const rest = layout.modules.filter((m) => m.id !== id);
    const others = rest.map((m, index) => ({ m, index }))
      .filter(({ m }) => shown(m))
      .map(({ m, index }) => {
        const r = moduleEl(m.id).getBoundingClientRect();
        return { index, top: r.top, height: r.height };
      });
    const grab = handle.getBoundingClientRect();
    const dx = event.clientX - grab.left;
    const dy = event.clientY - grab.top;
    const ghost = document.createElement("div");
    ghost.className = "ghost";
    ghost.setAttribute("aria-hidden", "true");
    const ghostName = document.createElement("span");
    ghostName.textContent = `⠿ ${MODULES[id]}`;
    const ghostWhere = document.createElement("span");
    ghostWhere.className = "ghost-where";
    ghost.append(ghostName, ghostWhere);
    ghost.style.width = `${Math.min(grab.width, 420)}px`;
    document.body.append(ghost);
    handle.setPointerCapture(event.pointerId);
    el.classList.add("placing");
    document.body.classList.add("moving");
    const follow = (e) => {
      ghost.style.transform = `translate(${e.clientX - dx}px, ${e.clientY - dy}px)`;
    };
    follow(event);
    let last = "";
    const onMove = (e) => {
      follow(e);
      const t = dropTarget(e.clientX, e.clientY, others, area);
      const key = `${t.index}:${t.width}:${t.side}`;
      if (key === last) return;
      last = key;
      ghostWhere.textContent = t.width === "full" ? "Full width" : t.side === "left" ? "Left half" : "Right half";
      // `index` counts the boxes without this one, which is where `place`
      // puts it once it has been lifted out.
      place(id, t.index, t.width, t.side);
    };
    const finish = (keep) => {
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", onUp);
      handle.removeEventListener("pointercancel", onCancel);
      ghost.remove();
      el.classList.remove("placing");
      document.body.classList.remove("moving");
      if (keep) {
        saveLayout();
      } else {
        layout.modules = before;
        applyLayout();
      }
    };
    const onUp = () => finish(true);
    const onCancel = () => finish(false);
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", onUp);
    handle.addEventListener("pointercancel", onCancel);
  }

  // From the keyboard: up and down move a box; left and right step it
  // between left half, full width, and right half (L, C, and R jump there).
  function keyMove(event, id) {
    const at = layout.modules.findIndex((m) => m.id === id);
    const m = layout.modules[at];
    const spot = m.width === "full" ? "centre" : m.side;
    const spots = ["left", "centre", "right"];
    let target = null;
    if (event.key === "ArrowUp") target = { index: at - 1, spot };
    else if (event.key === "ArrowDown") target = { index: at + 1, spot };
    else if (event.key === "ArrowLeft") target = { index: at, spot: spots[Math.max(0, spots.indexOf(spot) - 1)] };
    else if (event.key === "ArrowRight") target = { index: at, spot: spots[Math.min(2, spots.indexOf(spot) + 1)] };
    else if ({ l: 1, c: 1, r: 1 }[event.key.toLowerCase()]) {
      target = { index: at, spot: { l: "left", c: "centre", r: "right" }[event.key.toLowerCase()] };
    }
    if (!target) return;
    event.preventDefault();
    const width = target.spot === "centre" ? "full" : "half";
    if (place(id, target.index, width, target.spot === "right" ? "right" : "left")) saveLayout();
    event.currentTarget.focus();
  }

  function buildModuleHeads() {
    for (const [id, title] of Object.entries(MODULES)) {
      const head = document.createElement("div");
      head.className = "module-head";
      const handle = document.createElement("button");
      handle.type = "button";
      handle.className = "handle";
      handle.title = "Drag to move. Drop on the centre line for full width, left or right of it for half.";
      handle.setAttribute("aria-label",
        `Move ${title}: drag it, or use the arrow keys (up and down to move, left and right for width)`);
      const grip = document.createElement("span");
      grip.className = "grip";
      grip.setAttribute("aria-hidden", "true");
      grip.textContent = "⠿";
      const name = document.createElement("span");
      name.className = "module-title";
      name.textContent = title;
      const where = document.createElement("span");
      where.className = "where hint";
      handle.append(grip, name, where);
      handle.addEventListener("pointerdown", (event) => startDrag(event, id));
      handle.addEventListener("keydown", (event) => keyMove(event, id));
      head.append(handle);
      if (HIDEABLE.has(id)) {
        const hide = document.createElement("button");
        hide.type = "button";
        hide.className = "small secondary";
        hide.dataset.act = "hide";
        hide.textContent = "Hide";
        hide.addEventListener("click", () => changeLayout(id, { hidden: true }));
        head.append(hide);
      }
      const placeholder = document.createElement("p");
      placeholder.className = "hint placeholder";
      placeholder.textContent = id === "journal"
        ? "Appears here when a QSO is waiting to be logged."
        : "Appears here when you open a QSO from the log.";
      const box = moduleEl(id);
      box.prepend(head);
      if (id === "journal" || id === "detail") head.after(placeholder);
      // These boxes come and go; place the rest around them each time.
      new MutationObserver(() => applyLayout()).observe(box, { attributes: true, attributeFilter: ["hidden"] });
    }
  }

  // -- Tables: columns the operator chooses, orders, and sorts -----------

  const TABLES = {};

  // Make `table` (a <table> with a <thead> and <tbody>) one the operator can
  // shape. `column(id)` describes a column: its label, the key the Engine
  // sorts it by (or null), and how to show a row in it. `suggested` lists
  // columns offered in the chooser; any ADIF field can be added by name.
  // `load()` fetches rows again, sorted as `state()` says.
  function makeTable({ id, table, toolbar, column, suggested, defaults, load }) {
    const head = table.tHead.rows[0];
    const chooserButton = document.createElement("button");
    chooserButton.type = "button";
    chooserButton.className = "small secondary";
    chooserButton.textContent = "Columns";
    chooserButton.setAttribute("aria-expanded", "false");
    const chooser = document.createElement("div");
    chooser.className = "chooser card";
    chooser.hidden = true;
    chooser.setAttribute("role", "dialog");
    chooser.setAttribute("aria-label", "Choose columns");
    chooserButton.setAttribute("aria-controls", (chooser.id = `${id}-columns`));
    toolbar.append(chooserButton, chooser);

    const state = () => layout.tables[id] || defaults;

    function setState(change) {
      layout.tables = { ...layout.tables, [id]: { ...state(), ...change } };
      saveLayout();
      refresh();
    }

    function setColumns(columns) {
      setState({ columns });
    }

    function defaultDescending(key) {
      return ["entered", "qso_start", "changed_at", "version"].includes(key);
    }

    // First click sorts, the second reverses, the third goes back to the
    // order the QSOs were entered.
    function sortBy(key) {
      const now = state();
      if (now.sort !== key) setState({ sort: key, descending: defaultDescending(key) });
      else if (now.descending === defaultDescending(key)) setState({ descending: !now.descending });
      else setState({ sort: defaults.sort, descending: defaults.descending });
    }

    function describeSort() {
      const now = state();
      if (now.sort === "entered") return now.descending ? "newest entered first" : "oldest entered first";
      const shown = now.columns.map(column).find((c) => c.sort === now.sort);
      const name = shown ? shown.label : column(now.sort === "qso_start" ? "date" : now.sort).label;
      return `sorted by ${name}, ${now.descending ? "high to low" : "low to high"}`;
    }

    // -- Header: click to sort, drag to reorder --
    let dragged = false;

    function startColumnDrag(event, th, index) {
      if (event.button !== 0) return;
      const x0 = event.clientX;
      const y0 = event.clientY;
      let target = null;
      dragged = false;
      th.setPointerCapture(event.pointerId);
      const clear = () => {
        for (const cell of head.cells) cell.classList.remove("drop-before", "drop-after", "dragging");
      };
      const onMove = (e) => {
        if (!dragged && Math.hypot(e.clientX - x0, e.clientY - y0) < 6) return;
        if (!dragged) {
          // Moving a column, not selecting text on the page.
          window.getSelection().removeAllRanges();
          document.body.classList.add("moving");
        }
        dragged = true;
        clear();
        th.classList.add("dragging");
        target = null;
        for (const [i, cell] of [...head.cells].entries()) {
          const box = cell.getBoundingClientRect();
          if (e.clientX >= box.left && e.clientX <= box.right) {
            const after = e.clientX > box.left + box.width / 2;
            target = i + (after ? 1 : 0);
            if (i !== index) cell.classList.add(after ? "drop-after" : "drop-before");
          }
        }
      };
      const onUp = () => {
        th.removeEventListener("pointermove", onMove);
        th.removeEventListener("pointerup", onUp);
        th.removeEventListener("pointercancel", onUp);
        document.body.classList.remove("moving");
        clear();
        if (!dragged || target === null) return;
        const columns = [...state().columns];
        const [moved] = columns.splice(index, 1);
        columns.splice(target > index ? target - 1 : target, 0, moved);
        setColumns(columns);
      };
      th.addEventListener("pointermove", onMove);
      th.addEventListener("pointerup", onUp);
      th.addEventListener("pointercancel", onUp);
    }

    function renderHead() {
      const now = state();
      const cells = now.columns.map((key, index) => {
        const c = column(key);
        const th = document.createElement("th");
        th.dataset.column = key;
        th.className = "movable";
        th.title = c.sort ? "Click to sort. Drag to move." : "Drag to move.";
        const sorted = c.sort && c.sort === now.sort;
        if (sorted) th.setAttribute("aria-sort", now.descending ? "descending" : "ascending");
        if (c.sort) {
          const button = document.createElement("button");
          button.type = "button";
          button.className = "sort";
          button.textContent = c.label;
          if (sorted) {
            const arrow = document.createElement("span");
            arrow.className = "arrow";
            arrow.setAttribute("aria-hidden", "true");
            arrow.textContent = now.descending ? " ▼" : " ▲";
            button.append(arrow);
          }
          th.append(button);
        } else {
          th.textContent = c.label;
        }
        th.addEventListener("pointerdown", (event) => startColumnDrag(event, th, index));
        th.addEventListener("click", () => {
          if (dragged) { dragged = false; return; }
          if (c.sort) sortBy(c.sort);
        });
        return th;
      });
      head.replaceChildren(...cells);
    }

    // -- Chooser: show, hide, order, add any ADIF field --
    function chooserRow(key, shown, index, count) {
      const c = column(key);
      const li = document.createElement("li");
      const label = document.createElement("label");
      label.className = "inline";
      const box = document.createElement("input");
      box.type = "checkbox";
      box.checked = shown;
      box.dataset.key = key;
      box.dataset.act = "toggle";
      box.disabled = shown && count === 1; // a table keeps at least one column
      box.addEventListener("change", () => {
        const columns = state().columns;
        if (box.checked) {
          if (columns.length < MAX_COLUMNS) setColumns([...columns, key]);
        } else {
          setColumns(columns.filter((k) => k !== key));
        }
      });
      label.append(box, ` ${c.label}`);
      li.append(label);
      if (shown) {
        for (const [act, text, step] of [["up", "↑", -1], ["down", "↓", 1]]) {
          const b = document.createElement("button");
          b.type = "button";
          b.className = "small secondary";
          b.textContent = text;
          b.dataset.key = key;
          b.dataset.act = act;
          b.setAttribute("aria-label", `Move ${c.label} ${step < 0 ? "left" : "right"}`);
          b.disabled = step < 0 ? index === 0 : index === count - 1;
          b.addEventListener("click", () => {
            const columns = [...state().columns];
            columns.splice(index, 1);
            columns.splice(index + step, 0, key);
            setColumns(columns);
          });
          li.append(b);
        }
      }
      return li;
    }

    function renderChooser() {
      const focused = document.activeElement;
      const again = chooser.contains(focused) && focused.dataset.act
        ? `[data-key="${focused.dataset.key}"][data-act="${focused.dataset.act}"]` : null;
      const now = state();
      const shown = document.createElement("ol");
      now.columns.forEach((key, i) => shown.append(chooserRow(key, true, i, now.columns.length)));
      const lower = new Set(now.columns.map((k) => k.toLowerCase()));
      const more = document.createElement("ul");
      for (const key of suggested) {
        if (!lower.has(key.toLowerCase())) more.append(chooserRow(key, false, 0, 0));
      }
      const add = document.createElement("form");
      add.className = "row";
      const name = document.createElement("input");
      name.placeholder = "Any ADIF field, like GRIDSQUARE";
      name.maxLength = 64;
      name.spellcheck = false;
      name.dataset.key = "add";
      name.dataset.act = "name";
      name.setAttribute("aria-label", "ADIF field to add as a column");
      const addButton = document.createElement("button");
      addButton.type = "submit";
      addButton.className = "small";
      addButton.textContent = "Add";
      const problem = document.createElement("p");
      problem.className = "error";
      problem.setAttribute("role", "alert");
      add.append(name, addButton);
      add.addEventListener("submit", (event) => {
        event.preventDefault();
        const key = `field:${name.value.trim().toUpperCase()}`;
        if (!COLUMN_ID.test(key)) {
          problem.textContent = "A field name starts with a letter and has only letters, digits, and _.";
        } else if (lower.has(key.toLowerCase())) {
          problem.textContent = "That column is already shown.";
        } else if (now.columns.length >= MAX_COLUMNS) {
          problem.textContent = `A table shows at most ${MAX_COLUMNS} columns.`;
        } else {
          setColumns([...now.columns, key]);
        }
      });
      const reset = document.createElement("button");
      reset.type = "button";
      reset.className = "small secondary";
      reset.textContent = "Reset columns";
      reset.dataset.key = "reset";
      reset.dataset.act = "reset";
      reset.addEventListener("click", () => setState({ ...defaults }));
      const title = (text) => {
        const h = document.createElement("h3");
        h.textContent = text;
        return h;
      };
      const sortLine = document.createElement("p");
      sortLine.className = "hint";
      sortLine.textContent = `Now ${describeSort()}. Click a column heading to sort by it; click again to reverse, and a third time for the order entered.`;
      chooser.replaceChildren(title("Shown, left to right"), shown,
        ...(more.children.length ? [title("More columns"), more] : []),
        title("Add a field"), add, problem, sortLine, reset);
      if (again) {
        const el = $(again, chooser);
        if (el && !el.disabled) el.focus();
      }
    }

    function openChooser(open) {
      chooser.hidden = !open;
      chooserButton.setAttribute("aria-expanded", String(open));
      // Each glass box is drawn as its own layer, so lift the one holding
      // the open list above the boxes after it.
      const box = table.closest(".module");
      if (box) box.classList.toggle("raised", open);
      if (open) {
        renderChooser();
        const first = $("input, button", chooser);
        if (first) first.focus();
      }
    }

    chooserButton.addEventListener("click", () => openChooser(chooser.hidden));
    chooser.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        openChooser(false);
        chooserButton.focus();
      }
    });
    document.addEventListener("pointerdown", (event) => {
      if (!chooser.hidden && !chooser.contains(event.target) && event.target !== chooserButton) openChooser(false);
    });

    function refresh() {
      renderHead();
      if (!chooser.hidden) renderChooser();
      load();
    }

    // Show `rows`, one <tr> each; `decorate(tr, row)` adds classes and handlers.
    function render(rows, decorate) {
      const columns = state().columns.map(column);
      const body = table.tBodies[0];
      body.replaceChildren(...rows.map((row) => {
        const tr = document.createElement("tr");
        for (const c of columns) {
          const td = document.createElement("td");
          td.append(c.render(row));
          if (c.className) td.className = c.className;
          tr.append(td);
        }
        decorate(tr, row);
        return tr;
      }));
    }

    const handle = { state, refresh, render, describeSort };
    TABLES[id] = handle;
    renderHead();
    return handle;
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
    for (const table of Object.values(TABLES)) table.refresh();
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
    loadLayout(); // then shows the log, sorted the operator's way
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
    for (const name of ["CALL", "FREQ", "RST_SENT", "RST_RCVD", "NAME", "QTH", "STATE", "GRIDSQUARE", "COMMENT"]) form[name].value = "";
    form.QSO_DATE.value = date;
    form.TIME_ON.value = time;
    showCallZeros(form.CALL);
    form.CALL.focus();
  }

  // The Call box shows zeros slashed as they are typed. An input can only
  // hold plain text, so the box's own text is made invisible and a copy with
  // slashed zeros is drawn exactly over it; the value typed stays a plain 0.
  function showCallZeros(input) {
    const mirror = input.nextElementSibling;
    mirror.replaceChildren(callsign(input.value.toUpperCase()));
    mirror.scrollLeft = input.scrollLeft;
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
    add("QTH", form.QTH.value);
    add("STATE", form.STATE.value);
    add("GRIDSQUARE", form.GRIDSQUARE.value);
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

  // A callsign with its zeros drawn slashed, so N0OY never reads as NOOY. Only
  // the drawing changes: the text is still a plain 0, so copying, searching,
  // and everything stored or exported keep the callsign exactly as entered.
  function callsign(text) {
    const out = document.createDocumentFragment();
    for (const part of text.split(/(0)/)) {
      if (part === "0") {
        const zero = document.createElement("span");
        zero.className = "zero";
        zero.textContent = "0";
        out.append(zero);
      } else if (part) {
        out.append(part);
      }
    }
    return out;
  }

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

  const LOG_COLUMNS = {
    date: { label: "Date", sort: "qso_start", render: (q) => utcDate(q.qso_start) || field(q, "QSO_DATE") },
    time: { label: "Time", sort: "qso_start", render: (q) => utcTime(q.qso_start) || field(q, "TIME_ON") },
    call: { label: "Call", sort: "call", render: (q) => (q.call ? callsign(q.call) : "(no call)"), className: "call" },
    band: { label: "Band", sort: "band", render: (q) => q.band || "" },
    mode: { label: "Mode", sort: "mode", render: (q) => field(q, "SUBMODE") || q.mode || "" },
    operator: { label: "By", sort: "operator", render: (q) => q.operator },
    station: { label: "Station", sort: "station", render: (q) => q.station },
    version: { label: "Ver", sort: "version", render: (q) => String(q.version) },
    changed_at: { label: "Changed (UTC)", sort: "changed_at", render: (q) => q.changed_at.slice(0, 16).replace("T", " ") },
  };
  const FIELD_LABELS = {
    FREQ: "Freq (MHz)", RST_SENT: "RST sent", RST_RCVD: "RST rcvd", NAME: "Name", COMMENT: "Comment",
    GRIDSQUARE: "Grid", QTH: "QTH", STATE: "State", COUNTRY: "Country", TX_PWR: "Power (W)",
    CONTEST_ID: "Contest", SRX: "Rcvd #", STX: "Sent #", SUBMODE: "Submode", TIME_OFF: "Time off",
  };

  function logColumn(key) {
    if (LOG_COLUMNS[key]) return LOG_COLUMNS[key];
    const name = key.slice("field:".length).toUpperCase();
    return { label: FIELD_LABELS[name] || name, sort: `field:${name}`, render: (q) => field(q, name) };
  }

  let logTable = null;
  let searchTimer = null;

  async function loadLog() {
    const order = logTable.state();
    const params = new URLSearchParams({ limit: "100", sort: order.sort, newest_first: String(order.descending) });
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
    logTable.render(qsos, (row, qso) => {
      if (qso.deleted) row.className = "deleted";
      row.addEventListener("click", () => openDetail(qso.id));
    });
    $("#count").textContent = `${qsos.length === 100 ? "first 100" : qsos.length} shown, ${logTable.describeSort()}`;
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
    $("#detail-title").replaceChildren(qso.call ? callsign(qso.call) : "QSO", qso.deleted ? " (deleted)" : "");
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
    logTable = makeTable({
      id: "log",
      table: $("#log"),
      toolbar: $("#log-tools"),
      column: logColumn,
      suggested: [...Object.keys(LOG_COLUMNS), ...Object.keys(FIELD_LABELS).map((f) => `field:${f}`)],
      defaults: Object.freeze({
        columns: ["date", "time", "call", "band", "mode", "operator", "version"],
        sort: "entered",
        descending: true,
      }),
      load: () => (session && !$("#app").hidden ? loadLog() : null),
    });
    buildModuleHeads();
    applyLayout();
    fillSelect($("#entry [name=BAND]"), BANDS);
    fillSelect($("#entry [name=MODE]"), MODES);
    $("#entry [name=BAND]").value = "20m";

    $("#sign-in-form").addEventListener("submit", signIn);
    $("#sign-out").addEventListener("click", signOut);
    $("#arrange").addEventListener("click", () => arrange(!arranging()));
    $("#arrange-reset").addEventListener("click", () => {
      layout = { ...normalize(DEFAULT_LAYOUT), tables: layout.tables }; // columns have their own reset
      applyLayout();
      saveLayout();
    });
    $("#stored").addEventListener("transitionend", (event) => {
      if (event.propertyName === "opacity" && $("#stored").classList.contains("fading")) hideNotice();
    });
    $("#entry").addEventListener("submit", storeQso);
    const callInput = $("#entry [name=CALL]");
    for (const type of ["input", "scroll", "keyup", "select"]) {
      callInput.addEventListener(type, () => showCallZeros(callInput));
    }
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

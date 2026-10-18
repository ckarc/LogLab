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
  // ADIF 3.1.7 band and mode enumerations most used on the air.
  const BANDS = ["160m", "80m", "60m", "40m", "30m", "20m", "17m", "15m", "12m", "10m", "6m", "2m", "1.25m", "70cm"];
  const MODES = ["SSB", "CW", "FT8", "FT4", "FM", "AM", "RTTY", "PSK", "DIGITALVOICE", "JS8", "OLIVIA", "SSTV"];
  // FT4 is a submode of MFSK in ADIF; the entry form writes it correctly.
  const SUBMODE_OF = { FT4: "MFSK", JS8: "MFSK" };

  const MODULES = {
    entry: "New QSO", journal: "Not logged yet", log: "Log", detail: "QSO detail", map: "Field Day sections",
  };
  // The journal is never hidden: the operator must always see what is not
  // logged yet (Principle XV). The detail panel opens from the log.
  const HIDEABLE = new Set(["entry", "log", "map"]);
  const DEFAULT_LAYOUT = Object.freeze({
    version: 1,
    modules: Object.keys(MODULES).map((id) => ({ id, width: "full", side: "left", hidden: false })),
    tables: {},
    panels: {},
    detail: null,
    theme: "auto",
  });
  // Panels: parts of a module the operator places above, below, or beside
  // the module's main content, the way boxes are placed on the page. Each
  // sits in a `.panels` block next to its `.panel-main`.
  const PANELS = {
    "map.to-work": { title: "Sections to work", of: "the map", place: "bottom" },
  };
  const PLACES = ["top", "bottom", "left", "right"];
  // Auto follows the computer's light or dark setting. The choice is saved
  // with the operator's layout; this browser remembers the last one so the
  // page opens in it before the Engine answers.
  const THEMES = { auto: "Auto", light: "Light", dark: "Dark" };
  const THEME_KEY = "loglab.theme";
  const SAVE_AFTER_MS = 400;
  // One box on a page of its own: /?module=map shows only the section map,
  // full width, for a second screen or a bookmark. The journal still shows
  // when a QSO is waiting (Principle XV), and a QSO opened from the log
  // still opens its detail. The operator's saved layout is left alone.
  const POPPABLE = ["entry", "log", "map"];
  const SOLO = (() => {
    const id = new URLSearchParams(location.search).get("module");
    return POPPABLE.includes(id) ? id : null;
  })();
  const ALONE_WITH = new Set([SOLO, "journal", "detail"]);
  // The same checks the Engine makes on a saved table (Principle XII).
  const COLUMN_ID = /^(date|time|call|band|mode|operator|station|version|changed_at|field:[A-Za-z][A-Za-z0-9_]{0,63})$/;
  const SORT_KEY = /^(entered|qso_start|call|band|mode|operator|station|version|changed_at|field:[A-Za-z][A-Za-z0-9_]{0,63})$/;
  const MAX_COLUMNS = 40;
  // The ADIF fields the page knows by a plain name (adif-fields.js).
  const ADIF_FIELDS = new Map((window.LOGLAB_ADIF_FIELDS || []).map(([name, label, group]) => [name, { label, group }]));
  const FIELD_NAME = /^[A-Za-z][A-Za-z0-9_]{0,63}$/;
  const MAX_DETAIL_FIELDS = 80;
  // What a QSO's detail shows, even when empty, until the operator chooses.
  const DEFAULT_DETAIL = Object.freeze([
    "CALL", "QSO_DATE", "TIME_ON", "BAND", "MODE", "FREQ", "RST_SENT", "RST_RCVD",
    "NAME", "QTH", "STATE", "GRIDSQUARE", "COMMENT",
  ]);

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
        width: WIDTHS.includes(m.width) ? m.width : "full",
        side: sideFor(m.width, SIDES.includes(m.side) ? m.side : "left"),
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
    let detail = null;
    const chosen = saved && saved.detail && saved.detail.fields;
    if (Array.isArray(chosen)) {
      const names = new Set();
      const fields = chosen.filter((f) => {
        const ok = typeof f === "string" && FIELD_NAME.test(f) && !names.has(f.toUpperCase());
        if (ok) names.add(f.toUpperCase());
        return ok;
      }).map((f) => f.toUpperCase()).slice(0, MAX_DETAIL_FIELDS);
      detail = { fields };
    }
    const panels = {};
    for (const id of Object.keys(PANELS)) {
      const place = saved && saved.panels && saved.panels[id];
      if (PLACES.includes(place)) panels[id] = place;
    }
    const theme = saved && saved.theme in THEMES ? saved.theme : "auto";
    return { version: 1, modules, tables, panels, detail, theme };
  }

  function applyTheme(theme) {
    if (theme === "auto") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
    for (const b of document.querySelectorAll("[data-theme-choice]")) {
      b.setAttribute("aria-pressed", String(b.dataset.themeChoice === theme));
    }
    try { localStorage.setItem(THEME_KEY, theme); } catch { /* remembered only for this page */ }
  }

  function rememberedTheme() {
    try {
      const theme = localStorage.getItem(THEME_KEY);
      return theme in THEMES ? theme : "auto";
    } catch {
      return "auto";
    }
  }

  function chooseTheme(theme) {
    layout.theme = theme;
    applyTheme(theme);
    saveLayout();
  }

  const arranging = () => document.body.classList.contains("arranging");

  // Is this box on the page now? Boxes that show only when needed (Not
  // logged yet, QSO detail) take their place while the page is unlocked.
  function shown(m) {
    if (SOLO) return ALONE_WITH.has(m.id) && !moduleEl(m.id).hidden;
    return !m.hidden && (arranging() || !moduleEl(m.id).hidden);
  }

  // The page is six narrow tracks wide: a third takes two, a half three,
  // a full-width box all six. Boxes are placed in the operator's order, each
  // as high as its own tracks allow, so a short box on one side never leaves
  // a gap beside a tall one on the other. The grid's rows are one pixel high,
  // and each box spans as many as it is tall. Below THIRDS_FROM pixels wide,
  // thirds show as halves; on a phone, every box is full width (app.css).
  const GAP = 18;
  const THIRDS_FROM = 960;
  const WIDTHS = ["third", "half", "full"];
  const SIDES = ["left", "middle", "right"];

  // Only a third has a middle; anything else there moves to the left.
  const sideFor = (width, side) => (width !== "third" && side === "middle" ? "left" : side);

  // The tracks [from, to) a box takes, as `wide` or not.
  function tracksFor(width, side, wide) {
    const w = width === "third" && !wide ? "half" : width;
    if (w === "full") return [0, 6];
    if (w === "half") return side === "right" ? [3, 6] : [0, 3];
    return { left: [0, 2], middle: [2, 4], right: [4, 6] }[side] || [0, 2];
  }

  function pack() {
    const area = $("#workspace");
    const wide = area.clientWidth >= THIRDS_FROM;
    const heights = [0, 0, 0, 0, 0, 0];
    for (const m of layout.modules) {
      const el = moduleEl(m.id);
      if (!shown(m)) continue;
      const [from, to] = SOLO ? [0, 6] : tracksFor(m.width, m.side, wide);
      el.style.gridColumn = `${from + 1} / ${to + 1}`;
      const top = Math.max(...heights.slice(from, to));
      const tall = Math.max(1, Math.ceil(el.offsetHeight));
      el.style.gridRow = `${top + 1} / span ${tall}`;
      for (let t = from; t < to; t += 1) heights[t] = top + tall + GAP;
    }
  }

  // Boxes grow and shrink as the log loads, a QSO opens, or the window
  // changes; place them again each time, once per frame at most.
  let packing = 0;
  const repack = () => {
    if (!packing) packing = requestAnimationFrame(() => { packing = 0; pack(); });
  };
  const sizes = new ResizeObserver(repack);
  sizes.observe($("#workspace"));

  function applyLayout() {
    layout.modules.forEach((m, index) => {
      const el = moduleEl(m.id);
      el.style.order = String(index);
      el.classList.toggle("off", SOLO ? !ALONE_WITH.has(m.id) : m.hidden);
      el.dataset.width = m.width;
      el.dataset.place = m.width === "full" ? "full" : m.side;
      for (const b of el.querySelectorAll(".module-head [data-width]")) {
        b.setAttribute("aria-pressed", String(b.dataset.width === m.width));
      }
    });
    applyPanels();
    pack();
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
    layout.modules.splice(to, 0, { ...m, width, side: sideFor(width, width === "full" ? m.side : side) });
    applyLayout();
    return true;
  }

  async function loadLayout() {
    try {
      const saved = await api("GET", "/layout");
      layout = normalize(saved.layout || DEFAULT_LAYOUT);
    } catch {
      layout = { ...normalize(DEFAULT_LAYOUT), theme: rememberedTheme() }; // until the Engine answers
    }
    applyTheme(layout.theme);
    applyLayout();
    for (const table of Object.values(TABLES)) table.refresh();
  }

  // Locked is the normal state. Unlocked, every box shows a handle to move it.
  function arrange(on) {
    document.body.classList.toggle("arranging", on);
    $("#arrange-bar").hidden = !on;
    const toggle = $("#arrange");
    toggle.setAttribute("aria-pressed", String(on));
    toggle.textContent = on ? "🔒 Lock the layout" : "🔓 Unlock to move boxes";
    applyLayout();
  }

  // Where a box dropped at (x, y) goes. A third stays a third and goes to
  // the left, middle or right third the pointer is over. Any other box goes
  // full width near the centre line, or half width left or right of it.
  // Down the page: before the first box in those columns whose middle is
  // below the pointer, or else after the last one. Positions are measured
  // when the drag starts, so boxes moving aside never change the answer.
  const CENTRE_BAND = 0.2; // the middle fifth of the page counts as the centre line

  function dropTarget(x, y, others, area, current) {
    let width = "full";
    let side = "left";
    const single = getComputedStyle($("#workspace")).gridTemplateColumns.split(" ").length < 2;
    const wide = area.width >= THIRDS_FROM;
    const across = (x - area.left) / area.width;
    if (!single && current === "third" && wide) {
      width = "third";
      side = across < 1 / 3 ? "left" : across < 2 / 3 ? "middle" : "right";
    } else if (!single && across < 0.5 - CENTRE_BAND / 2) {
      width = "half";
    } else if (!single && across > 0.5 + CENTRE_BAND / 2) {
      width = "half";
      side = "right";
    }
    // The columns it will take, in pixels from the page's left edge.
    const [from, to] = single ? [0, 6] : tracksFor(width, side, wide);
    const track = (area.width - 5 * GAP) / 6;
    const left = area.left + from * (track + GAP);
    const right = area.left + to * (track + GAP) - GAP;
    const below = others.filter((o) => o.left < right && o.right > left);
    let index = layout.modules.length - 1;
    const next = below.find((o) => y < o.top + o.height / 2);
    if (next) index = next.index;
    else if (below.length) index = Math.max(...below.map((o) => o.index)) + 1;
    return { index, width, side };
  }

  const WHERE = {
    full: "Full width",
    "half:left": "Left half", "half:right": "Right half",
    "third:left": "Left third", "third:middle": "Middle third", "third:right": "Right third",
  };

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
        return { index, top: r.top, height: r.height, left: r.left, right: r.right };
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
      const t = dropTarget(e.clientX, e.clientY, others, area, layout.modules.find((m) => m.id === id).width);
      const key = `${t.index}:${t.width}:${t.side}`;
      if (key === last) return;
      last = key;
      ghostWhere.textContent = WHERE[t.width === "full" ? "full" : `${t.width}:${t.side}`];
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

  // From the keyboard: up and down move a box. Left and right step a third
  // between the three thirds, and any other box between left half, full
  // width and right half (L, C and R jump there). T, H and F make it a
  // third, a half, or full width.
  function keyMove(event, id) {
    const at = layout.modules.findIndex((m) => m.id === id);
    const m = layout.modules[at];
    const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;
    const sizeKeys = { t: "third", h: "half", f: "full" };
    let width = m.width;
    let side = m.side;
    let index = at;
    if (key === "ArrowUp") index = at - 1;
    else if (key === "ArrowDown") index = at + 1;
    else if (sizeKeys[key]) {
      width = sizeKeys[key];
    } else if (m.width === "third" && (key === "ArrowLeft" || key === "ArrowRight")) {
      const step = key === "ArrowLeft" ? -1 : 1;
      side = SIDES[Math.max(0, Math.min(2, SIDES.indexOf(side) + step))];
    } else if (key === "ArrowLeft" || key === "ArrowRight" || { l: 1, c: 1, r: 1 }[key]) {
      const spots = ["left", "centre", "right"];
      const spot = m.width === "full" ? "centre" : side === "right" ? "right" : "left";
      const go = key === "ArrowLeft" ? spots[Math.max(0, spots.indexOf(spot) - 1)]
        : key === "ArrowRight" ? spots[Math.min(2, spots.indexOf(spot) + 1)]
        : { l: "left", c: "centre", r: "right" }[key];
      width = go === "centre" ? "full" : "half";
      if (go !== "centre") side = go;
    } else {
      return;
    }
    event.preventDefault();
    if (place(id, index, width, side)) saveLayout();
    event.currentTarget.focus();
  }

  // -- Panels inside a module ---------------------------------------------

  const panelEl = (id) => document.querySelector(`.panel[data-panel="${id}"]`);
  const panelPlace = (id) => layout.panels[id] || PANELS[id].place;

  // Put each panel in its place: before or after the row that holds the
  // main content, or in that row to its left or right.
  function applyPanels() {
    for (const [id, panel] of Object.entries(PANELS)) {
      const el = panelEl(id);
      const place = panelPlace(id);
      if (!el || el.dataset.place === place) continue;
      const block = el.closest(".panels");
      const row = $(".panels-row", block);
      if (place === "top") block.insertBefore(el, row);
      else if (place === "bottom") block.append(el);
      else if (place === "left") row.insertBefore(el, $(".panel-main", row));
      else row.append(el);
      el.dataset.place = place;
      const handle = $(".panel-head .where", el);
      if (handle) handle.textContent = panelWhere(place, panel);
    }
  }

  const panelWhere = (place, panel) => ({
    top: `Above ${panel.of}`, bottom: `Below ${panel.of}`,
    left: `Left of ${panel.of}`, right: `Right of ${panel.of}`,
  })[place];

  function placePanel(id, place) {
    if (panelPlace(id) === place) return false;
    layout.panels = { ...layout.panels, [id]: place };
    applyPanels();
    return true;
  }

  // Where a panel dropped at (x, y) goes: the side of the module's panel
  // block the pointer is nearest, measured when the drag starts.
  function panelTarget(x, y, r) {
    const across = (x - (r.left + r.width / 2)) / (r.width / 2);
    const down = (y - (r.top + r.height / 2)) / (r.height / 2);
    if (Math.abs(across) > Math.abs(down)) return across < 0 ? "left" : "right";
    return down < 0 ? "top" : "bottom";
  }

  // Carried like a box: an outline where it will sit, and a card that
  // follows the pointer. The panel moves in the page as it is carried, which
  // ends any pointer capture, so the page itself follows the pointer.
  function startPanelDrag(event, id) {
    if (event.button !== 0) return;
    event.preventDefault();
    const handle = event.currentTarget;
    const el = panelEl(id);
    const panel = PANELS[id];
    const before = { ...layout.panels };
    const area = el.closest(".panels").getBoundingClientRect();
    const grab = handle.getBoundingClientRect();
    const dx = event.clientX - grab.left;
    const dy = event.clientY - grab.top;
    const ghost = document.createElement("div");
    ghost.className = "ghost";
    ghost.setAttribute("aria-hidden", "true");
    const ghostName = document.createElement("span");
    ghostName.textContent = `⠿ ${panel.title}`;
    const ghostWhere = document.createElement("span");
    ghostWhere.className = "ghost-where";
    ghostWhere.textContent = panelWhere(panelPlace(id), panel);
    ghost.append(ghostName, ghostWhere);
    ghost.style.width = `${Math.min(Math.max(grab.width, 260), 360)}px`;
    document.body.append(ghost);
    el.classList.add("placing");
    document.body.classList.add("moving");
    const follow = (e) => {
      ghost.style.transform = `translate(${e.clientX - dx}px, ${e.clientY - dy}px)`;
    };
    follow(event);
    const pointer = event.pointerId;
    const onMove = (e) => {
      if (e.pointerId !== pointer) return;
      follow(e);
      const place = panelTarget(e.clientX, e.clientY, area);
      ghostWhere.textContent = panelWhere(place, panel);
      placePanel(id, place);
    };
    const finish = (keep) => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      ghost.remove();
      el.classList.remove("placing");
      document.body.classList.remove("moving");
      if (keep) {
        saveLayout();
      } else {
        layout.panels = before;
        applyPanels();
      }
    };
    const onUp = (e) => { if (e.pointerId === pointer) finish(true); };
    const onCancel = (e) => { if (e.pointerId === pointer) finish(false); };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
  }

  // From the keyboard, an arrow key moves the panel to that side.
  function keyMovePanel(event, id) {
    const place = { ArrowUp: "top", ArrowDown: "bottom", ArrowLeft: "left", ArrowRight: "right" }[event.key];
    if (!place) return;
    event.preventDefault();
    if (placePanel(id, place)) saveLayout();
    event.currentTarget.focus();
  }

  function buildPanelHeads() {
    for (const [id, panel] of Object.entries(PANELS)) {
      const el = panelEl(id);
      if (!el) continue;
      const head = document.createElement("div");
      head.className = "panel-head";
      const handle = document.createElement("button");
      handle.type = "button";
      handle.className = "handle";
      handle.title = `Drag to put it above, below, or beside ${panel.of}.`;
      handle.setAttribute("aria-label",
        `Move ${panel.title}: drag it, or use the arrow keys to put it above, below, left or right of ${panel.of}`);
      const grip = document.createElement("span");
      grip.className = "grip";
      grip.setAttribute("aria-hidden", "true");
      grip.textContent = "⠿";
      const name = document.createElement("span");
      name.className = "module-title";
      name.textContent = panel.title;
      const where = document.createElement("span");
      where.className = "where hint";
      handle.append(grip, name, where);
      handle.addEventListener("pointerdown", (event) => startPanelDrag(event, id));
      handle.addEventListener("keydown", (event) => keyMovePanel(event, id));
      head.append(handle);
      el.prepend(head);
    }
  }

  function buildModuleHeads() {
    for (const [id, title] of Object.entries(MODULES)) {
      const head = document.createElement("div");
      head.className = "module-head";
      const handle = document.createElement("button");
      handle.type = "button";
      handle.className = "handle";
      handle.title = "Drag to move. Drop on the centre line for full width, left or right of it for half. A third goes to the third you drop it on.";
      handle.setAttribute("aria-label",
        `Move ${title}: drag it, or use the arrow keys (up and down to move, left and right to change side); T, H and F make it a third, a half, or full width`);
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
      // How wide the box is: a third, a half, or the full page.
      const widths = document.createElement("span");
      widths.className = "widths";
      widths.setAttribute("role", "group");
      widths.setAttribute("aria-label", `${title} width`);
      for (const [width, label, name] of [["third", "⅓", "One third"], ["half", "½", "Half"], ["full", "Full", "Full width"]]) {
        const size = document.createElement("button");
        size.type = "button";
        size.className = "small secondary";
        size.dataset.width = width;
        size.textContent = label;
        size.title = name;
        size.setAttribute("aria-label", `${name} width`);
        size.addEventListener("click", () => {
          const m = layout.modules.find((x) => x.id === id);
          changeLayout(id, { width, side: sideFor(width, m.side) });
        });
        widths.append(size);
      }
      head.append(widths);
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
      sizes.observe(box);
    }
  }

  // A box in a window of its own, to drag onto a second screen. Where the
  // browser can tell (Chrome, at http://127.0.0.1 or over HTTPS), and the
  // operator allows it, the window opens on the other screen by itself.
  function popOut(id) {
    const win = window.open(`/?module=${id}`, `loglab-${id}`, "popup,width=1200,height=820");
    if (!win || !("getScreenDetails" in window)) return;
    window.getScreenDetails().then((screens) => {
      const other = screens.screens.find((s) => s !== screens.currentScreen);
      if (!other) return;
      win.moveTo(other.availLeft, other.availTop);
      win.resizeTo(other.availWidth, other.availHeight);
    }).catch(() => { /* not allowed: the operator drags it over */ });
  }

  function buildPopOuts() {
    for (const id of POPPABLE) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "link popout";
      button.textContent = "⧉ Pop out";
      button.title = `Open ${MODULES[id]} in a window of its own, to put on another screen`;
      button.addEventListener("click", () => popOut(id));
      $("h2", moduleEl(id)).append(button);
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
    chooserButton.className = "link table-columns";
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
      name.placeholder = "Search fields, like Grid or POTA";
      name.setAttribute("list", "adif-field-list");
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
        const picked = fieldFromText(name.value);
        const key = `field:${picked || name.value.trim().toUpperCase()}`;
        if (!picked && name.value.trim() && FIELD_NAME.test(name.value.trim())) {
          problem.textContent = "More than one field matches: pick one from the list.";
        } else if (!COLUMN_ID.test(key)) {
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
    $("#menu-open").hidden = true;
    if ($("#menu-dialog").open) $("#menu-dialog").close();
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
    $("#menu-open").hidden = false;
    $("#arrange").hidden = Boolean(SOLO); // a page of one box has nothing to arrange
    $("#all-boxes").hidden = !SOLO;
    $("#who").textContent = `${session.operator} at ${session.station}`;
    $("#menu-who").textContent = `Signed in as ${session.operator} at ${session.station}.`;
    const mayLog = session.permissions.includes("log");
    for (const el of document.querySelectorAll("#entry button, #edit button[type=submit], #delete, #restore")) {
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
    for (const name of ["CALL", "FREQ", "RST_SENT", "RST_RCVD", "NAME", "QTH", "STATE", "GRIDSQUARE", "CLASS", "ARRL_SECT", "COMMENT"]) form[name].value = "";
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
    add("CLASS", form.CLASS.value.toUpperCase());
    add("ARRL_SECT", form.ARRL_SECT.value.toUpperCase());
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
        // Logged: it appears in the Log, which is confirmation enough. Only
        // problems get a notice.
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

  function hideNotice() {
    $("#stored").hidden = true;
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
  // Fields offered first in the Log's column chooser; any other is found by name.
  const SUGGESTED_FIELDS = [
    "FREQ", "RST_SENT", "RST_RCVD", "NAME", "COMMENT", "GRIDSQUARE", "QTH", "STATE", "COUNTRY", "TX_PWR",
    "CONTEST_ID", "SRX", "STX", "SUBMODE", "TIME_OFF", "CLASS", "ARRL_SECT", "LOTW_QSL_RCVD",
  ];
  // Fields shown as words rather than as their ADIF value.
  const FIELD_SHOWN = {
    // Logbook of the World: confirmed, sent and waiting, or not yet sent (lotw.js sends).
    LOTW_QSL_RCVD: {
      label: "LoTW",
      render: (q) => (field(q, "LOTW_QSL_RCVD").toUpperCase() === "Y" ? "Confirmed"
        : field(q, "LOTW_QSL_SENT").toUpperCase() === "Y" ? "Sent" : ""),
    },
  };

  const fieldLabel = (name) => (ADIF_FIELDS.get(name.toUpperCase()) || { label: name.toUpperCase() }).label;

  // What the operator typed in a field search: an ADIF name, or a plain name
  // ("Power (W)", "grid") that stands for one, or a field the list does not
  // know (APP_ and user-defined fields). Null when it only partly matches
  // known fields ("pota" could be POTA_REF or MY_POTA_REF), so they pick one.
  function fieldFromText(text) {
    const typed = text.trim();
    if (!typed) return null;
    const upper = typed.toUpperCase();
    if (ADIF_FIELDS.has(upper)) return upper;
    for (const [name, f] of ADIF_FIELDS) {
      if (f.label.toUpperCase() === upper || `${f.label} (${name})`.toUpperCase() === upper) return name;
    }
    for (const [name, f] of ADIF_FIELDS) {
      if (name.includes(upper) || f.label.toUpperCase().includes(upper)) return null;
    }
    return FIELD_NAME.test(typed) ? upper : null;
  }

  function logColumn(key) {
    if (LOG_COLUMNS[key]) return LOG_COLUMNS[key];
    const name = key.slice("field:".length).toUpperCase();
    const shown = FIELD_SHOWN[name];
    return { label: shown ? shown.label : fieldLabel(name), sort: `field:${name}`, render: shown ? shown.render : (q) => field(q, name) };
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
    // Other modules that show the log (map.js) follow it.
    document.dispatchEvent(new CustomEvent("loglab:log-loaded"));
  }

  // -- One QSO: fields, edit, delete, restore, history -------------------

  // The fields a QSO's detail always shows, in order: the operator's choice.
  const detailFields = () => (layout.detail ? layout.detail.fields : DEFAULT_DETAIL);

  function setDetailFields(fields) {
    layout.detail = fields === null ? null : { fields };
    saveLayout();
    if (current) showFields(current);
    if (!$("#detail-chooser").hidden) renderDetailChooser();
  }

  // One field of the open QSO: its plain name with its ADIF name beside it.
  // Which fields show is set by the Fields chooser, never QSO by QSO. A field
  // the operator always shows stays on screen when cleared. A field left
  // empty is not saved.
  function fieldRow(f, { pinned = false } = {}) {
    const row = document.createElement("tr");
    row.dataset.type = f.type || "";
    row.dataset.name = f.name;
    const nameCell = document.createElement("td");
    const label = document.createElement("span");
    label.className = "field-label";
    label.textContent = fieldLabel(f.name);
    const adif = document.createElement("span");
    adif.className = "field-adif";
    adif.textContent = f.name.toUpperCase();
    nameCell.append(label, adif);
    const value = document.createElement("input");
    value.value = f.value;
    value.setAttribute("aria-label", `${fieldLabel(f.name)} value`);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "link";
    remove.textContent = pinned ? "Clear" : "Remove";
    remove.addEventListener("click", () => {
      if (pinned) {
        value.value = "";
        value.focus();
      } else {
        row.remove();
      }
    });
    const valueCell = document.createElement("td");
    valueCell.append(value);
    const removeCell = document.createElement("td");
    removeCell.append(remove);
    row.append(nameCell, valueCell, removeCell);
    return row;
  }

  // The fields the operator always shows come first, in their order, empty or
  // not; then every other field the QSO has, so nothing it holds is hidden.
  function showFields(qso) {
    const typed = new Map([...$("#edit tbody").rows]
      .filter((r) => r.dataset.name && current && current.id === qso.id)
      .map((r) => [r.dataset.name.toUpperCase(), $("input", r).value]));
    const have = new Map(qso.fields.map((f) => [f.name.toUpperCase(), f]));
    const pinned = detailFields();
    const rows = pinned.map((name) => {
      const f = have.get(name) || { name, value: "", type: null };
      return fieldRow({ ...f, value: typed.has(name) ? typed.get(name) : f.value }, { pinned: true });
    });
    const shown = new Set(pinned);
    for (const f of qso.fields) {
      const name = f.name.toUpperCase();
      if (!shown.has(name)) rows.push(fieldRow({ ...f, value: typed.has(name) ? typed.get(name) : f.value }));
    }
    $("#edit tbody").replaceChildren(...rows);
  }

  // -- Which fields a QSO shows: the Fields chooser --

  function detailChooserRow(name, chosen, index, count) {
    const li = document.createElement("li");
    const label = document.createElement("label");
    label.className = "inline";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = chosen;
    box.dataset.key = name;
    box.dataset.act = "toggle";
    box.addEventListener("change", () => {
      const fields = detailFields();
      if (box.checked) {
        if (fields.length < MAX_DETAIL_FIELDS) setDetailFields([...fields, name]);
      } else {
        setDetailFields(fields.filter((f) => f !== name));
      }
    });
    const adif = document.createElement("span");
    adif.className = "field-adif";
    adif.textContent = name;
    label.append(box, ` ${fieldLabel(name)} `, adif);
    li.append(label);
    if (chosen) {
      for (const [act, text, step] of [["up", "↑", -1], ["down", "↓", 1]]) {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "small secondary";
        b.textContent = text;
        b.dataset.key = name;
        b.dataset.act = act;
        b.setAttribute("aria-label", `Move ${fieldLabel(name)} ${step < 0 ? "up" : "down"}`);
        b.disabled = step < 0 ? index === 0 : index === count - 1;
        b.addEventListener("click", () => {
          const fields = [...detailFields()];
          fields.splice(index, 1);
          fields.splice(index + step, 0, name);
          setDetailFields(fields);
        });
        li.append(b);
      }
    }
    return li;
  }

  function renderDetailChooser() {
    const chooser = $("#detail-chooser");
    const focused = document.activeElement;
    const again = chooser.contains(focused) && focused.dataset.act
      ? `[data-key="${focused.dataset.key}"][data-act="${focused.dataset.act}"]` : null;
    const search = $("#detail-search");
    const text = search.value.trim().toUpperCase();
    const matches = (name) => !text || name.includes(text) || fieldLabel(name).toUpperCase().includes(text);
    const fields = detailFields();
    const chosen = new Set(fields);
    const title = (words) => {
      const h = document.createElement("h3");
      h.textContent = words;
      return h;
    };
    const parts = [];
    const shown = document.createElement("ol");
    fields.forEach((name, i) => { if (matches(name)) shown.append(detailChooserRow(name, true, i, fields.length)); });
    if (shown.children.length || !text) parts.push(title("Always shown, top to bottom"), shown);
    if (!fields.length) parts.push(Object.assign(document.createElement("p"), { className: "hint", textContent: "None: a QSO shows only the fields it has." }));
    const groups = new Map();
    for (const [name, f] of ADIF_FIELDS) {
      if (chosen.has(name) || !matches(name)) continue;
      if (!groups.has(f.group)) groups.set(f.group, document.createElement("ul"));
      groups.get(f.group).append(detailChooserRow(name, false, 0, 0));
    }
    for (const [group, list] of groups) parts.push(title(group), list);
    // A field the list does not know (APP_ or user-defined) can be named exactly.
    const typed = search.value.trim();
    const found = groups.size || shown.children.length;
    if (typed && !found && FIELD_NAME.test(typed) && !ADIF_FIELDS.has(text) && !chosen.has(text)) {
      const add = document.createElement("button");
      add.type = "button";
      add.className = "small";
      add.textContent = `Always show ${text}`;
      add.dataset.key = "add";
      add.dataset.act = "add";
      add.addEventListener("click", () => {
        search.value = "";
        setDetailFields([...detailFields(), text]);
      });
      parts.push(title("Not a standard ADIF field"), add);
    } else if (text && !found) {
      parts.push(Object.assign(document.createElement("p"), { className: "hint", textContent: "No field matches." }));
    }
    const reset = document.createElement("button");
    reset.type = "button";
    reset.className = "small secondary";
    reset.textContent = "Reset fields";
    reset.dataset.key = "reset";
    reset.dataset.act = "reset";
    reset.addEventListener("click", () => setDetailFields(null));
    const end = document.createElement("div");
    end.className = "chooser-end";
    end.append(reset);
    $("#detail-chooser-list").replaceChildren(...parts, end);
    if (again) {
      const el = $(again, chooser);
      if (el && !el.disabled) el.focus();
    }
  }

  function openDetailChooser(open) {
    const chooser = $("#detail-chooser");
    chooser.hidden = !open;
    $("#detail-fields").setAttribute("aria-expanded", String(open));
    $("#detail").classList.toggle("raised", open);
    if (open) {
      $("#detail-search").value = "";
      renderDetailChooser();
      $("#detail-search").focus();
    }
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
    $("#edit tbody").replaceChildren();
    current = qso;
    showFields(qso);
    $("#edit-note").value = "";
    setError("#edit", "");
    $("#delete").hidden = qso.deleted;
    $("#restore").hidden = !qso.deleted;
    $("#edit button[type=submit]").hidden = qso.deleted;
    $("#detail-fields").hidden = qso.deleted;
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
      const value = $("input", row).value;
      if (!value.trim()) continue; // an empty field is not kept
      fields.push({ name: row.dataset.name, value, type: row.dataset.type || null });
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
    applyTheme(rememberedTheme());
    logTable = makeTable({
      id: "log",
      table: $("#log"),
      toolbar: $("#log-tools"),
      column: logColumn,
      suggested: [...Object.keys(LOG_COLUMNS), ...SUGGESTED_FIELDS.map((f) => `field:${f}`)],
      defaults: Object.freeze({
        columns: ["date", "time", "call", "band", "mode", "operator", "version", "field:LOTW_QSL_RCVD"],
        sort: "entered",
        descending: true,
      }),
      load: () => (session && !$("#app").hidden ? loadLog() : null),
    });
    buildModuleHeads();
    buildPanelHeads();
    if (SOLO) {
      document.body.classList.add("solo");
      document.title = `${MODULES[SOLO]} · LogLab`;
    } else {
      buildPopOuts();
    }
    applyLayout();
    fillSelect($("#entry [name=BAND]"), BANDS);
    fillSelect($("#entry [name=MODE]"), MODES);
    $("#entry [name=BAND]").value = "20m";

    // Other modules that change QSOs (lotw.js) ask for the log to be shown again.
    document.addEventListener("loglab:log-changed", () => { if (session && !$("#app").hidden) loadLog(); });
    $("#sign-in-form").addEventListener("submit", signIn);
    $("#sign-out").addEventListener("click", () => {
      $("#menu-dialog").close();
      signOut();
    });
    // The menu: one plain-text entry in the top bar opens everything else.
    const menu = $("#menu-dialog");
    $("#menu-open").addEventListener("click", () => menu.showModal());
    $("#menu-close").addEventListener("click", () => menu.close());
    menu.addEventListener("click", (event) => { if (event.target === menu) menu.close(); }); // the backdrop
    for (const b of document.querySelectorAll("[data-theme-choice]")) {
      b.addEventListener("click", () => chooseTheme(b.dataset.themeChoice));
    }
    // QRZ.com and LoTW keep their own dialogs; the menu just opens them.
    for (const b of document.querySelectorAll("[data-menu-opens]")) {
      b.addEventListener("click", () => {
        menu.close();
        $(`#${b.dataset.menuOpens}`).click();
      });
    }
    $("#arrange").addEventListener("click", () => {
      menu.close();
      arrange(!arranging());
    });
    $("#arrange-done").addEventListener("click", () => arrange(false));
    $("#arrange-reset").addEventListener("click", () => {
      // Columns and a QSO's fields have their own resets.
      // Panels go back to their places with the boxes.
      layout = { ...normalize(DEFAULT_LAYOUT), tables: layout.tables, detail: layout.detail, theme: layout.theme };
      applyLayout();
      saveLayout();
    });
    $("#entry").addEventListener("submit", storeQso);
    const callInput = $("#entry [name=CALL]");
    for (const type of ["input", "scroll", "keyup", "select"]) {
      callInput.addEventListener(type, () => showCallZeros(callInput));
    }
    $("#send-now").addEventListener("click", flushJournal);
    window.addEventListener("online", flushJournal);
    setInterval(flushJournal, RETRY_MS);
    // Another window of this browser (a popped-out box) changed the journal,
    // so a QSO may have just been logged there: show it here too.
    window.addEventListener("storage", (event) => {
      if (event.key !== JOURNAL_KEY) return;
      renderJournal();
      if (session && !$("#app").hidden) loadLog();
    });
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
    const fieldList = $("#adif-field-list");
    for (const [name, f] of ADIF_FIELDS) {
      const option = document.createElement("option");
      option.value = name;
      option.label = f.label;
      fieldList.append(option);
    }
    $("#detail-fields").addEventListener("click", () => openDetailChooser($("#detail-chooser").hidden));
    $("#detail-search").addEventListener("input", renderDetailChooser);
    $("#detail-chooser").addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        openDetailChooser(false);
        $("#detail-fields").focus();
      }
    });
    document.addEventListener("pointerdown", (event) => {
      if (!$("#detail-chooser").hidden && !$("#detail-chooser").contains(event.target) && event.target !== $("#detail-fields")) {
        openDetailChooser(false);
      }
    });
    $("#edit").addEventListener("submit", (event) => {
      event.preventDefault();
      if (!current) return;
      change("PUT", `/qsos/${current.id}`, { base_version: current.version, fields: editedFields(), note: note() });
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

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
    entry: "New QSO", profile: "Station profile", log: "Log",
    map: "Section map", world: "World map", cqzones: "CQ zone map", ituzones: "ITU zone map", stats: "Operation stats", onair: "On air",
  };
  // Neither the journal nor a QSO's detail is a box. The journal pops up over
  // the corner of the page whenever a QSO is not logged yet, and stays until
  // it is (Principle XV); a QSO's detail opens over the page from the pencil
  // at the end of its Log row.
  const HIDEABLE = new Set(["entry", "profile", "log", "map", "world", "cqzones", "ituzones", "stats", "onair"]);
  // Boxes each operator turns on for themselves, from the "Show" buttons:
  // they start out hidden unless an overlay's layout shows them.
  const START_HIDDEN = new Set(["stats", "onair"]);
  // Where a box added since the operator saved their layout first appears:
  // the station profile goes beside New QSO, the box it follows; the world
  // map and the zone maps take the section map's place, since an overlay
  // offers only the maps its rules count.
  const JOINS = {
    profile: { after: "entry", width: "full", side: "left" },
    world: { after: "map", like: "map", width: "full", side: "left" },
    cqzones: { after: "world", like: "map", width: "full", side: "left" },
    ituzones: { after: "cqzones", like: "map", width: "full", side: "left" },
  };
  // Which maps an overlay calls for, from what its exchange asks for: a
  // section (Field Day, Winter Field Day) brings the section map, a CQ zone
  // (CQ WW) the CQ zone map, an ITU zone (IARU HF) the ITU zone map. An
  // overlay that asks for none of them, and everyday logging, gets the world
  // map. Until overlay.js says, it is everyday.
  const MAPS = ["map", "world", "cqzones", "ituzones"];
  const MAPS_FOR = (overlay) => {
    const received = overlay?.exchange?.received || [];
    const maps = [];
    if (received.some((p) => p.sections)) maps.push("map");
    if (received.some((p) => p.field === "CQZ")) maps.push("cqzones");
    if (received.some((p) => p.field === "ITUZ")) maps.push("ituzones");
    return maps.length ? maps : ["world"];
  };
  const unoffered = new Set(["map", "cqzones", "ituzones"]);
  const WORLD_QSOS = 50; // how many of the latest QSOs the world map draws, until the operator chooses
  const MAX_WORLD_QSOS = 500;
  const DEFAULT_LAYOUT = Object.freeze({
    version: 1,
    modules: Object.keys(MODULES).map((id) => ({ id, width: "full", side: "left", scale: 100, hidden: START_HIDDEN.has(id), ...(JOINS[id] && { width: JOINS[id].width, side: JOINS[id].side }) })),
    tables: {},
    panels: {},
    detail: null,
    theme: "auto",
    operation: null,
    by_overlay: {},
    world_qsos: WORLD_QSOS,
    bio_open: false,
    stats_open: false,
    expanded: [],
    auto_expand: ["stats", "profile", "onair"],
    distance_unit: "mi",
    log_mine: true,
  });
  // Panels: parts of a module the operator places above, below, or beside
  // the module's main content, the way boxes are placed on the page. Each
  // sits in a `.panels` block next to its `.panel-main`.
  const PANELS = {
    "map.to-work": { title: "Sections to work", of: "the map", place: "bottom" },
    "cqzones.to-work": { title: "Zones to work", of: "the map", place: "bottom" },
    "ituzones.to-work": { title: "Zones to work", of: "the map", place: "bottom" },
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
  const POPPABLE = ["entry", "profile", "log", "map", "world", "cqzones", "ituzones", "stats", "onair"];
  const SOLO = (() => {
    const id = new URLSearchParams(location.search).get("module");
    return POPPABLE.includes(id) ? id : null;
  })();
  const ALONE_WITH = new Set([SOLO]);
  // Boxes whose growing part folds under a bar, and may be locked open.
  const EXPANDABLE = ["stats", "profile", "log", "onair"];
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
  let current = null; // the QSO open in the detail window
  let selected = null; // the QSO selected (highlighted) in the Log

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
  // profile.js (the station profile) also shows callsigns and a QSO table.
  // overlay.js (operation overlays) also reads and changes the layout, reads
  // the New QSO form, and gives the page its overlay's default layout.
  const LOOKUP_KEEP_MS = 60000;
  const lookups = new Map(); // call -> { at, answer: Promise }
  function lookupOnce(call) {
    const now = Date.now();
    for (const [key, held] of lookups) if (now - held.at > LOOKUP_KEEP_MS) lookups.delete(key);
    let held = lookups.get(call);
    if (!held) {
      held = { at: now, answer: api("GET", `/lookup?call=${encodeURIComponent(call)}`) };
      // A failed question is asked again next time, not remembered.
      held.answer.catch(() => lookups.delete(call));
      lookups.set(call, held);
    }
    return held.answer;
  }
  let overlayDefault = null; // the operator's overlay's starting layout, if it has one
  window.LogLab = Object.freeze({
    api,
    session: () => session,
    layout: () => layout,
    useLayout,
    entryFields: () => entryFields($("#entry")),
    callsign: (text) => callsign(text),
    // A table of QSOs shaped like the Log: its columns chosen, ordered, and sorted (profile.js).
    qsoTable: (options) => makeTable({
      ...options,
      column: logColumn,
      suggested: [...Object.keys(LOG_COLUMNS), ...SUGGESTED_FIELDS.map((f) => `field:${f}`)],
    }),
    openQso: (id) => openDetail(id),
    // Who a call is (profile.js and lookup.js both ask): one question per call
    // for a minute, so the profile box and the form fill share one answer.
    lookup: (call) => lookupOnce(call),
    forgetLookups: () => lookups.clear(), // the QRZ.com login changed, or a refresh was asked for
    setDefaultLayout: (saved) => { overlayDefault = saved; },
    // A box's growing part, folded under a bar that opens it over the page (stats.js, profile.js, onair.js).
    expander: (options) => expander(options),
    solo: () => SOLO, // the box this window was popped out for, or null on the workspace
    rowsHeight: (table, count) => rowsHeight(table, count),
  });

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

  // What the Engine is sent for a QSO: in an operation, the Engine adds its
  // club call, the operator and the contest's fields (overlay.js).
  function body(entry) {
    return entry.operation ? { id: entry.id, fields: entry.fields, operation: entry.operation }
      : { id: entry.id, fields: entry.fields };
  }

  // Send one journal entry. True when the Engine has logged it.
  async function send(entry) {
    sending.add(entry.id);
    try {
      const result = await api("POST", "/qsos", body(entry));
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
        scale: scaleOf(m.scale),
        hidden: HIDEABLE.has(m.id) && m.hidden === true,
      });
    }
    for (const id of Object.keys(MODULES)) {
      if (seen.has(id)) continue;
      const join = JOINS[id];
      const like = join && join.like && modules.find((m) => m.id === join.like);
      const hidden = START_HIDDEN.has(id);
      const added = like
        ? { id, width: like.width, side: like.side, scale: 100, hidden }
        : { id, width: join ? join.width : "full", side: join ? join.side : "left", scale: 100, hidden };
      const after = join && join.after ? modules.findIndex((m) => m.id === join.after) : -1;
      if (after >= 0) modules.splice(after + 1, 0, added);
      else modules.push(added);
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
    const op = saved && saved.operation;
    const operation = op && typeof op.id === "string" ? { id: op.id, gota: op.gota === true } : null;
    // Each overlay's own arrangement, kept while another overlay is in use (overlay.js).
    const by_overlay = {};
    for (const [id, kept] of Object.entries((saved && saved.by_overlay) || {})) {
      if (!/^[a-z][a-z0-9-]{0,31}$/.test(id) || !kept || !Array.isArray(kept.modules)) continue;
      const inner = normalize({ modules: kept.modules, tables: kept.tables });
      by_overlay[id] = { modules: inner.modules, tables: inner.tables };
    }
    const count = saved && Number(saved.world_qsos);
    const world_qsos = Number.isInteger(count) && count >= 1 && count <= MAX_WORLD_QSOS ? count : WORLD_QSOS;
    const bio_open = Boolean(saved && saved.bio_open === true);
    // Boxes locked open; the stats box's lock was once a field of its own.
    const expanded = [...new Set([
      ...(saved && Array.isArray(saved.expanded) ? saved.expanded : []),
      ...(saved && saved.stats_open === true ? ["stats"] : []),
    ])].filter((id) => EXPANDABLE.includes(id));
    const stats_open = false;
    // Boxes that open by themselves when the pointer rests in them; not the
    // Log, until the operator ticks it.
    const auto_expand = saved && Array.isArray(saved.auto_expand)
      ? [...new Set(saved.auto_expand)].filter((id) => EXPANDABLE.includes(id))
      : ["stats", "profile", "onair"];
    const distance_unit = saved && saved.distance_unit === "km" ? "km" : "mi";
    const log_mine = !(saved && saved.log_mine === false);
    return { version: 1, modules, tables, panels, detail, theme, operation, by_overlay, world_qsos, bio_open, stats_open, expanded, auto_expand, distance_unit, log_mine };
  }

  function applyTheme(theme) {
    if (theme === "auto") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
    for (const b of document.querySelectorAll("[data-theme-choice]")) {
      b.setAttribute("aria-pressed", String(b.dataset.themeChoice === theme));
    }
    try { localStorage.setItem(THEME_KEY, theme); } catch { /* remembered only for this page */ }
  }

  // The Menu's headings: one opens at a time, the one used last opens again
  // next time, and each heading shows what is set under it. A heading with
  // nothing to show hides itself; one holding an error opens so it is seen.
  const MENU_KEY = "loglab.menu";
  const THEME_NAMES = { auto: "Auto", light: "Light", dark: "Dark" };

  function tidyMenu(menu) {
    const sections = [...menu.querySelectorAll(".menu-section")];
    let saved = null;
    try { saved = localStorage.getItem(MENU_KEY); } catch { /* none remembered */ }
    for (const section of sections) {
      section.open = section.dataset.menuSection === saved;
      section.addEventListener("toggle", () => {
        if (!section.open) return;
        for (const other of sections) if (other !== section) other.open = false;
        try { localStorage.setItem(MENU_KEY, section.dataset.menuSection); } catch { /* not remembered */ }
      });
    }
    const refresh = () => {
      for (const section of sections) {
        const entries = [...section.querySelectorAll(".menu-body > *:not(.error)")];
        const empty = entries.length > 0 && entries.every((e) => e.hidden || isEmptyItems(e));
        if (section.hidden !== empty) section.hidden = empty;
        if ([...section.querySelectorAll(".error")].some((e) => e.textContent.trim())) section.open = true;
      }
      const theme = document.documentElement.dataset.theme || "auto";
      setText($("#menu-now-appearance"), THEME_NAMES[theme] || "");
      const choice = $("#op-choice");
      const picked = choice.selectedOptions[0];
      setText($("#menu-now-operation"), !picked || !choice.value ? "Everyday" : picked.textContent.replace(/ \(.*$/, ""));
    };
    new MutationObserver(refresh).observe(menu, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ["hidden"] });
    new MutationObserver(refresh).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    $("#op-choice").addEventListener("change", refresh);
    refresh();
  }

  // Writing only a change keeps the Menu's watcher from waking itself.
  function setText(element, text) {
    if (element.textContent !== text) element.textContent = text;
  }

  function isEmptyItems(element) {
    return element.classList.contains("menu-items") && [...element.children].every((c) => c.hidden);
  }

  function rememberedTheme() {
    try {
      const theme = localStorage.getItem(THEME_KEY);
      return theme in THEMES ? theme : "auto";
    } catch {
      return "auto";
    }
  }

  // Change part of the layout, show it, and save it (overlay.js).
  function useLayout(change) {
    layout = normalize({ ...layout, ...change });
    applyLayout();
    saveLayout();
    for (const table of Object.values(TABLES)) table.refresh();
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
    return !m.hidden && !unoffered.has(m.id) && !elsewhere.has(m.id) && (arranging() || !moduleEl(m.id).hidden);
  }

  // The operator's overlay chooses its map (overlay.js says which overlay is in use).
  document.addEventListener("loglab:overlay-shown", (event) => {
    const maps = MAPS_FOR(event.detail.overlay);
    for (const id of MAPS) {
      if (maps.includes(id)) unoffered.delete(id);
      else unoffered.add(id);
    }
    if (layout) applyLayout();
  });

  // The page is twelve narrow tracks wide: a quarter takes three, a third
  // four, a half six, two thirds eight, a full-width box all twelve. Boxes are placed in the
  // operator's order, each as high as its own tracks allow, so a short box
  // on one side never leaves a gap beside a tall one on the other. The
  // grid's rows are one pixel high, and each box spans as many as it is
  // tall. Below THIRDS_FROM pixels wide, quarters and thirds show as
  // halves, on the side they lean to, and two thirds as full width; on a
  // phone, every box is full width (app.css).
  const GAP = 18;
  const TRACKS = 12;
  const THIRDS_FROM = 960;
  const WIDTHS = ["quarter", "third", "half", "two-thirds", "full"];
  // What each width is called, in the order the width list offers them.
  const WIDTH_NAMES = [["quarter", "¼", "One quarter"], ["third", "⅓", "One third"], ["half", "½", "Half"],
    ["two-thirds", "⅔", "Two thirds"], ["full", "Full", "Full width"]];
  const SIDES = ["left", "middle-left", "middle", "middle-right", "right"];
  // The places across the page each width can sit, left to right.
  const SLOTS = {
    quarter: ["left", "middle-left", "middle-right", "right"],
    third: ["left", "middle", "right"],
    half: ["left", "right"],
    "two-thirds": ["left", "right"],
  };
  // How large a box draws what is inside it, in percent (the Engine's limits).
  const MIN_SCALE = 50;
  const MAX_SCALE = 200;
  const SCALE_STEP = 5;
  const scaleOf = (n) => (Number.isInteger(n) && n >= MIN_SCALE && n <= MAX_SCALE ? n : 100);

  // A side the width has; one it lacks moves to the nearest it has.
  function sideFor(width, side) {
    if (width === "full") return side === "right" ? "right" : "left";
    const slots = SLOTS[width];
    if (slots.includes(side)) return side;
    if (side === "middle") return width === "quarter" ? "middle-left" : "left";
    if (side === "middle-left") return width === "third" ? "middle" : "left";
    if (side === "middle-right") return width === "third" ? "middle" : "right";
    return "left";
  }

  // The width a box shows at, on a workspace `px` pixels wide.
  function shownWidth(width, px) {
    if ((width === "quarter" || width === "third") && px < THIRDS_FROM) return "half";
    if (width === "two-thirds" && px < THIRDS_FROM) return "full";
    return width;
  }

  // The tracks [from, to) a box takes on a workspace `px` pixels wide.
  function tracksFor(width, side, px) {
    const w = shownWidth(width, px);
    if (w === "full") return [0, TRACKS];
    // Two thirds start at the left edge or end at the right one.
    if (w === "two-thirds") return sideFor(w, side) === "right" ? [TRACKS / 3, TRACKS] : [0, (TRACKS * 2) / 3];
    const slots = SLOTS[w];
    const span = TRACKS / slots.length;
    const at = Math.max(0, slots.indexOf(sideFor(w, side)));
    return [at * span, (at + 1) * span];
  }

  function pack() {
    const area = $("#workspace");
    const px = area.clientWidth;
    const heights = new Array(TRACKS).fill(0);
    for (const m of layout.modules) {
      const el = moduleEl(m.id);
      if (!shown(m)) continue;
      const [from, to] = SOLO ? [0, TRACKS] : tracksFor(m.width, m.side, px);
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
      el.classList.toggle("off", SOLO ? !ALONE_WITH.has(m.id) : m.hidden || unoffered.has(m.id) || elsewhere.has(m.id));
      el.dataset.width = m.width;
      el.dataset.place = m.width === "full" ? "full" : m.side;
      const widthList = el.querySelector(".module-head select.widths");
      if (widthList) widthList.value = m.width;
      // The box keeps its width; what is inside it draws larger or smaller.
      el.style.setProperty("--box-scale", String(m.scale / 100));
      const slider = el.querySelector(".module-head .scale input");
      if (slider && document.activeElement !== slider) slider.value = String(m.scale);
      const shownScale = el.querySelector(".module-head .scale-reset");
      if (shownScale) shownScale.textContent = `${m.scale}%`;
      const auto = el.querySelector(".module-head .auto-expand input");
      if (auto) auto.checked = layout.auto_expand.includes(m.id);
    });
    applyPanels();
    pack();
    // While unlocked, the bar lists every box with a tick: untick to hide
    // one, tick a hidden one to bring it back (the Menu has the same list).
    const tray = $("#tray");
    const title = document.createElement("span");
    title.className = "tray-title";
    title.textContent = "Boxes shown:";
    tray.replaceChildren(title, ...boxChoices({ offeredOnly: true }));
    showBoxPicker();
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

  // How large box `id` draws its contents, in percent, kept in the limits.
  function setScale(id, percent) {
    const scale = Math.max(MIN_SCALE, Math.min(MAX_SCALE, Math.round(percent / SCALE_STEP) * SCALE_STEP));
    if (layout.modules.find((m) => m.id === id).scale === scale) return;
    changeLayout(id, { scale });
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
    document.dispatchEvent(new CustomEvent("loglab:layout-loaded"));
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

  // Where a box dropped at (x, y) goes. A quarter or a third keeps its
  // width and goes to the quarter or third the pointer is over. Any other
  // box goes full width near the centre line, or half width left or right
  // of it.
  // Down the page: before the first box in those columns whose middle is
  // below the pointer, or else after the last one. Positions are measured
  // when the drag starts, so boxes moving aside never change the answer.
  const CENTRE_BAND = 0.2; // the middle fifth of the page counts as the centre line

  function dropTarget(x, y, others, area, current) {
    let width = "full";
    let side = "left";
    const single = getComputedStyle($("#workspace")).gridTemplateColumns.split(" ").length < 2;
    const across = Math.max(0, Math.min(0.999, (x - area.left) / area.width));
    const narrow = shownWidth(current, area.width);
    if (!single && (narrow === "quarter" || narrow === "third")) {
      width = current;
      const slots = SLOTS[narrow];
      side = slots[Math.floor(across * slots.length)];
    } else if (!single && narrow === "two-thirds") {
      width = current;
      side = across < 0.5 ? "left" : "right";
    } else if (!single && across < 0.5 - CENTRE_BAND / 2) {
      width = "half";
    } else if (!single && across > 0.5 + CENTRE_BAND / 2) {
      width = "half";
      side = "right";
    }
    // The columns it will take, in pixels from the page's left edge.
    const [from, to] = single ? [0, TRACKS] : tracksFor(width, side, area.width);
    const track = (area.width - (TRACKS - 1) * GAP) / TRACKS;
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
    "quarter:left": "Left quarter", "quarter:middle-left": "Second quarter",
    "quarter:middle-right": "Third quarter", "quarter:right": "Right quarter",
    "two-thirds:left": "Left two thirds", "two-thirds:right": "Right two thirds",
  };

  // While something is carried near the top or bottom of the window, the
  // page scrolls that way, faster the closer the pointer gets to the edge,
  // so a box can go anywhere on a page taller than the screen. `onScroll`
  // runs after each step so the drop place follows the page.
  const EDGE = 64;
  const EDGE_SPEED = 24;
  function edgeScroll(onScroll) {
    let y = null;
    let frame = 0;
    const step = () => {
      frame = 0;
      if (y === null) return;
      const h = window.innerHeight;
      const zone = Math.min(EDGE, h / 4);
      const speed = y < zone ? -(zone - y) / zone : y > h - zone ? (y - (h - zone)) / zone : 0;
      if (!speed) return;
      const was = window.scrollY;
      window.scrollBy(0, Math.round(Math.max(-1, Math.min(1, speed)) * EDGE_SPEED) || Math.sign(speed));
      if (window.scrollY === was) return;
      onScroll();
      frame = requestAnimationFrame(step);
    };
    return {
      move(e) {
        y = e.clientY;
        if (!frame) frame = requestAnimationFrame(step);
      },
      stop() {
        y = null;
        if (frame) cancelAnimationFrame(frame);
        frame = 0;
      },
    };
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
    // Tops are kept in page coordinates, so they hold while the page scrolls.
    const rest = layout.modules.filter((m) => m.id !== id);
    const others = rest.map((m, index) => ({ m, index }))
      .filter(({ m }) => shown(m))
      .map(({ m, index }) => {
        const r = moduleEl(m.id).getBoundingClientRect();
        return { index, top: r.top + window.scrollY, height: r.height, left: r.left, right: r.right };
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
    let at = event;
    const aim = () => {
      const t = dropTarget(at.clientX, at.clientY + window.scrollY, others, area, layout.modules.find((m) => m.id === id).width);
      const key = `${t.index}:${t.width}:${t.side}`;
      if (key === last) return;
      last = key;
      const seen = shownWidth(t.width, area.width);
      ghostWhere.textContent = WHERE[seen === "full" ? "full" : `${seen}:${sideFor(seen, t.side)}`];
      // `index` counts the boxes without this one, which is where `place`
      // puts it once it has been lifted out.
      place(id, t.index, t.width, t.side);
    };
    const scroller = edgeScroll(aim);
    const onMove = (e) => {
      at = e;
      follow(e);
      aim();
      scroller.move(e);
    };
    const finish = (keep) => {
      scroller.stop();
      window.removeEventListener("scroll", aim);
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
    window.addEventListener("scroll", aim, { passive: true });
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", onUp);
    handle.addEventListener("pointercancel", onCancel);
  }

  // From the keyboard: up and down move a box. Left and right step a
  // quarter or a third across the page, and any other box between left
  // half, full width and right half (L, C and R jump there). Q, T, H, W
  // and F make it a quarter, a third, a half, two thirds, or full width. Minus and plus make
  // what is inside it smaller or larger, and 0 sets it back to 100%.
  function keyMove(event, id) {
    const at = layout.modules.findIndex((m) => m.id === id);
    const m = layout.modules[at];
    const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;
    const sizeKeys = { q: "quarter", t: "third", h: "half", w: "two-thirds", f: "full" };
    const scaleBy = { "-": -SCALE_STEP, "_": -SCALE_STEP, "+": SCALE_STEP, "=": SCALE_STEP }[key];
    if (scaleBy || key === "0") {
      event.preventDefault();
      setScale(id, key === "0" ? 100 : m.scale + scaleBy);
      return;
    }
    let width = m.width;
    let side = m.side;
    let index = at;
    if (key === "ArrowUp") index = at - 1;
    else if (key === "ArrowDown") index = at + 1;
    else if (sizeKeys[key]) {
      width = sizeKeys[key];
    } else if (["quarter", "third", "two-thirds"].includes(m.width) && (key === "ArrowLeft" || key === "ArrowRight")) {
      const slots = SLOTS[m.width];
      const step = key === "ArrowLeft" ? -1 : 1;
      side = slots[Math.max(0, Math.min(slots.length - 1, slots.indexOf(side) + step))];
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
    const block = el.closest(".panels").getBoundingClientRect();
    // In page coordinates, so it holds while the page scrolls.
    const area = { left: block.left, width: block.width, top: block.top + window.scrollY, height: block.height };
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
    let at = event;
    const aim = () => {
      const place = panelTarget(at.clientX, at.clientY + window.scrollY, area);
      ghostWhere.textContent = panelWhere(place, panel);
      placePanel(id, place);
    };
    const scroller = edgeScroll(aim);
    const onMove = (e) => {
      if (e.pointerId !== pointer) return;
      at = e;
      follow(e);
      aim();
      scroller.move(e);
    };
    const finish = (keep) => {
      scroller.stop();
      window.removeEventListener("scroll", aim);
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
    window.addEventListener("scroll", aim, { passive: true });
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
      handle.title = "Drag to move. Drop on the centre line for full width, left or right of it for half. A quarter or a third goes where you drop it.";
      handle.setAttribute("aria-label",
        `Move ${title}: drag it, or use the arrow keys (up and down to move, left and right to change side); Q, T, H, W and F make it a quarter, a third, a half, two thirds, or full width; minus and plus make its contents smaller or larger`);
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
      // How wide the box is, picked from a list: a quarter, a third, a
      // half, two thirds, or the full page.
      const widths = document.createElement("select");
      widths.className = "widths";
      widths.title = "How wide this box is";
      widths.setAttribute("aria-label", `${title} width`);
      for (const [width, label, name] of WIDTH_NAMES) {
        const option = document.createElement("option");
        option.value = width;
        option.textContent = label;
        option.title = name;
        widths.append(option);
      }
      widths.addEventListener("change", () => {
        const width = widths.value;
        const m = layout.modules.find((x) => x.id === id);
        changeLayout(id, { width, side: sideFor(width, m.side) });
      });
      head.append(widths);
      // How large the box draws what is inside it, for each operator's eyes.
      const scale = document.createElement("label");
      scale.className = "scale";
      scale.title = "Make what is inside this box smaller or larger";
      const scaleName = document.createElement("span");
      scaleName.textContent = "Size";
      const slider = document.createElement("input");
      slider.type = "range";
      slider.min = String(MIN_SCALE);
      slider.max = String(MAX_SCALE);
      slider.step = String(SCALE_STEP);
      slider.value = "100";
      slider.setAttribute("aria-label", `${title} contents size, in percent`);
      slider.addEventListener("input", () => setScale(id, Number(slider.value)));
      // The size it is now; pressing it goes back to 100%.
      const scaleReset = document.createElement("button");
      scaleReset.type = "button";
      scaleReset.className = "small secondary scale-reset";
      scaleReset.textContent = "100%";
      scaleReset.title = "Back to the normal size (100%)";
      scaleReset.setAttribute("aria-label", `${title} contents back to 100%`);
      scaleReset.addEventListener("click", () => setScale(id, 100));
      scale.append(scaleName, slider);
      head.append(scale, scaleReset);
      // A box that grows may open by itself when the pointer rests in it,
      // or only from its bar.
      if (EXPANDABLE.includes(id)) {
        const auto = document.createElement("label");
        auto.className = "auto-expand";
        auto.title = "Open the rest of this box when the pointer rests in it. Unticked, it opens only from its bar.";
        const tick = document.createElement("input");
        tick.type = "checkbox";
        tick.setAttribute("aria-label", `${title}: open by itself when the pointer rests in it`);
        tick.addEventListener("change", () => useLayout({
          auto_expand: tick.checked ? [...layout.auto_expand, id] : layout.auto_expand.filter((x) => x !== id),
        }));
        auto.append(tick, " Auto expand");
        head.append(auto);
      }
      if (HIDEABLE.has(id)) {
        const hide = document.createElement("button");
        hide.type = "button";
        hide.className = "small secondary";
        hide.dataset.act = "hide";
        hide.textContent = "Hide";
        hide.addEventListener("click", () => changeLayout(id, { hidden: true }));
        head.append(hide);
      }
      const box = moduleEl(id);
      box.prepend(head);
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

  // -- Which boxes show: a list in the Menu, beside Unlock ----------------

  // Every box the operator may hide, ticked when shown. The journal and a
  // QSO's detail come and go on their own, so they are not listed. A map the
  // overlay does not use, or a box open in its own window, says so.
  function buildBoxPicker() {
    const open = document.createElement("button");
    open.type = "button";
    open.id = "boxes-open";
    open.className = "link";
    open.setAttribute("aria-expanded", "false");
    open.setAttribute("aria-controls", "box-picker");
    open.textContent = "Choose which boxes show";
    const list = document.createElement("div");
    list.id = "box-picker";
    list.className = "box-picker";
    list.hidden = true;
    $("#arrange").after(open, list);
    open.addEventListener("click", () => {
      list.hidden = !list.hidden;
      open.setAttribute("aria-expanded", String(!list.hidden));
      if (!list.hidden) showBoxPicker();
    });
    if (SOLO) open.hidden = true;
  }

  function showBoxPicker() {
    const list = $("#box-picker");
    if (!list || list.hidden) return;
    list.replaceChildren(...boxChoices());
  }

  // The bar leaves out maps the overlay does not use; the Menu lists them, saying so.
  function boxChoices({ offeredOnly = false } = {}) {
    const rows = [];
    for (const m of layout.modules) {
      if (!HIDEABLE.has(m.id) || (offeredOnly && unoffered.has(m.id))) continue;
      const row = document.createElement("label");
      row.className = "inline";
      const tick = document.createElement("input");
      tick.type = "checkbox";
      tick.checked = !m.hidden;
      tick.addEventListener("change", () => changeLayout(m.id, { hidden: !tick.checked }));
      const why = unoffered.has(m.id) ? "not used for this kind of logging" : elsewhere.has(m.id) ? "open in its own window" : "";
      row.append(tick, ` ${MODULES[m.id]}`);
      if (why) {
        const note = document.createElement("span");
        note.className = "hint";
        note.textContent = `(${why})`;
        row.append(" ", note);
      }
      rows.push(row);
    }
    return rows;
  }

  // A box popped out into a window of its own is not shown here as well.
  // The window says it is open every second, and that it has closed when it
  // does; this page leaves the box out while it hears from it. Nothing is
  // saved: closing the window, or losing it, brings the box back here.
  const ELSEWHERE_MS = 3500;
  const elsewhere = new Map(); // box id -> when its window last said it was open
  const boxes = "BroadcastChannel" in window ? new BroadcastChannel("loglab-boxes") : null;

  function hearWindows() {
    if (!boxes) return;
    if (SOLO) {
      const say = () => boxes.postMessage({ open: SOLO });
      say();
      setInterval(say, 1000);
      window.addEventListener("pagehide", () => boxes.postMessage({ closed: SOLO }));
      boxes.addEventListener("message", (event) => { if (event.data.bringBack === SOLO) window.close(); });
      return;
    }
    const update = () => {
      const now = Date.now();
      let changed = false;
      for (const [id, heard] of elsewhere) {
        if (now - heard > ELSEWHERE_MS) { elsewhere.delete(id); changed = true; }
      }
      return changed;
    };
    boxes.addEventListener("message", (event) => {
      const { open, closed } = event.data || {};
      const was = new Set(elsewhere.keys());
      if (POPPABLE.includes(open)) elsewhere.set(open, Date.now());
      if (POPPABLE.includes(closed)) elsewhere.delete(closed);
      if (update() || was.size !== elsewhere.size || [...was].some((id) => !elsewhere.has(id))) showElsewhere();
    });
    setInterval(() => { if (update()) showElsewhere(); }, 1000);
  }

  // Say which boxes are in other windows, with a way to bring each back.
  function showElsewhere() {
    const line = $("#elsewhere");
    line.replaceChildren();
    for (const id of elsewhere.keys()) {
      const item = document.createElement("span");
      item.textContent = `${MODULES[id]} is open in its own window.`;
      const back = document.createElement("button");
      back.type = "button";
      back.className = "link";
      back.textContent = "Bring it back";
      back.addEventListener("click", () => {
        boxes.postMessage({ bringBack: id });
        elsewhere.delete(id);
        showElsewhere();
      });
      item.append(" ", back);
      line.append(item);
    }
    line.hidden = elsewhere.size === 0;
    applyLayout();
    // A box coming back shows what changed while it was away.
    document.dispatchEvent(new CustomEvent("loglab:log-loaded"));
  }

  function buildPopOuts() {
    for (const id of POPPABLE) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "link popout";
      // Just the icon; the tooltip and the label say what it does.
      button.textContent = "⧉";
      button.title = `Pop out: open ${MODULES[id]} in a window of its own, to put on another screen`;
      button.setAttribute("aria-label", `Pop out ${MODULES[id]}`);
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
  function makeTable({ id, table, toolbar, column, suggested, defaults, load, edit = null, entered = ["newest entered first", "oldest entered first"] }) {
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
      if (now.sort === "entered") return now.descending ? entered[0] : entered[1];
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
        for (const [i, cell] of [...head.cells].filter((c) => c.dataset.column).entries()) {
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
      // With `edit`, each row ends in a pencil that opens it (the Log).
      if (edit) {
        const th = document.createElement("th");
        th.className = "edit-col";
        th.setAttribute("aria-label", "Edit");
        cells.push(th);
      }
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
        // A pretend QSO (demo mode) always says so, whatever columns are shown (Principle XX).
        if (row.fields && field(row, "APP_LOGLAB_PRETEND").trim().toUpperCase() === "Y" && tr.firstChild) {
          tr.classList.add("pretend");
          tr.title = "Pretend QSO from demo mode, not a real contact";
          tr.firstChild.append(" ", Object.assign(document.createElement("span"), { className: "pretend-tag", textContent: "pretend" }));
        }
        if (edit) {
          const td = document.createElement("td");
          td.className = "edit-col";
          const pencil = document.createElement("button");
          pencil.type = "button";
          pencil.className = "link edit-row";
          pencil.textContent = "✎";
          pencil.title = "Open this QSO to change, delete or see its history";
          pencil.setAttribute("aria-label", "Edit this QSO");
          pencil.addEventListener("click", (event) => {
            event.stopPropagation(); // the pencil opens the QSO; the row click only selects it
            edit(row);
          });
          td.append(pencil);
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
    $("#menu-open").hidden = Boolean(SOLO); // a popped-out box is only the box: no top bar, no Menu
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
    selectQso(null);
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

  // Empty New QSO for the next contact, keeping the band and mode. Esc
  // (cleared) does the same as a store but logs nothing.
  function resetEntry({ cleared = false } = {}) {
    const form = $("#entry");
    const { date, time } = utcNow();
    for (const name of ["CALL", "FREQ", "RST_SENT", "RST_RCVD", "NAME", "QTH", "STATE", "GRIDSQUARE", "CLASS", "ARRL_SECT", "COMMENT"]) form[name].value = "";
    form.QSO_DATE.value = date;
    form.TIME_ON.value = time;
    showCallZeros(form.CALL);
    form.CALL.focus();
    document.dispatchEvent(new CustomEvent("loglab:entry-reset", { detail: { cleared } }));
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
      // A box the operator's overlay leaves out is not sent (overlay.js).
      if (form[name] && form[name].closest("label").hidden) return;
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
    add("SIG_INFO", form.SIG_INFO.value.toUpperCase());
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
      operation: layout.operation,
    };
    // Into the journal before it goes anywhere (Principle XV).
    const journaled = writeJournal([...readJournal(), entry]);
    try {
      const result = journaled ? await send(entry) : await api("POST", "/qsos", body(entry));
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
    // overlay.js offers to show only one operation's QSOs, like Field Day 2027.
    if ($("#log-operation").value) params.set("operation", $("#log-operation").value);
    // In an operation the Log shows the operator's own QSOs or the whole club's.
    const inOperation = Boolean($("#log-operation").value);
    $("#log-whose").hidden = !inOperation;
    $("#log-club").setAttribute("aria-checked", String(!layout.log_mine));
    for (const side of document.querySelectorAll("#log-whose [data-whose]")) {
      side.classList.toggle("on", (side.dataset.whose === "mine") === layout.log_mine);
    }
    if (inOperation && layout.log_mine) params.set("mine", "true");
    let qsos;
    try {
      qsos = await api("GET", `/qsos?${params}`);
    } catch (e) {
      $("#count").textContent = e.message;
      return;
    }
    logTable.render(qsos, (row, qso) => {
      if (qso.deleted) row.className = "deleted";
      row.dataset.qso = qso.id;
      row.classList.toggle("selected", Boolean(selected && selected.id === qso.id));
      // A click selects the row (the station profile follows it); a second click lets it go.
      row.addEventListener("click", () => selectQso(selected && selected.id === qso.id ? null : qso));
    });
    $("#count").textContent = `${qsos.length === 100 ? "first 100" : qsos.length} shown, ${logTable.describeSort()}`;
    // Other modules that show the log (map.js) follow it.
    document.dispatchEvent(new CustomEvent("loglab:log-loaded"));
  }

  // -- Boxes that grow ----------------------------------------------------

  // A box whose content grows (the stats' chart and tables, the station
  // profile's biography and QSOs, the Log, On air) keeps a compact part in
  // view, with a bar under it. Resting the pointer anywhere in the box
  // opens the rest over the page, on top of the boxes below, so nothing on
  // the page moves; a pointer only passing over (moving briskly, or there
  // for less than DWELL_MS) opens nothing. Pointing at the bar opens it at
  // once. Both only in a box the operator lets open by itself
  // (`auto_expand`, not the Log at first); any box opens from its bar. It fades back once the pointer has been out of the box for
  // GRACE_MS, so a slip over the edge doesn't close it. A tap or Enter
  // opens it too, and Esc or a tap elsewhere closes it (and it stays shut
  // until the pointer leaves the box). The lock in the bar keeps it open in
  // the box, taking its room on the page; which boxes the operator locked
  // open is kept with their layout (`expanded`), on any workstation.
  //
  // `part` is what folds: `over` it for a part that folds out of sight, or
  // under one that stays `compact()` pixels tall, scrolling there unless it
  // should `clip`; `label()` names what the bar opens.
  const FADE_MS = 450; // the fade out (app.css)
  const DWELL_MS = 500; // resting this long in a box opens it
  const GRACE_MS = 300; // out of the box this long before it closes
  const BRISK = 0.5; // px per ms: faster than this is passing over, and the dwell starts again
  const expanders = new Map();

  function expander({ id, box, part, label, compact = () => 0, over = false, clip = false }) {
    // A part that folds out of sight has its bar over it; one that stays
    // partly in view, under it.
    const outer = document.createElement("div");
    outer.className = "expander";
    const pane = document.createElement("div");
    pane.className = "expander-pane";
    const bar = document.createElement("div");
    bar.className = "expander-bar";
    const opener = document.createElement("button");
    opener.type = "button";
    opener.className = "expander-open";
    const lock = document.createElement("button");
    lock.type = "button";
    lock.className = "expander-lock";
    bar.append(opener, lock);
    if (part.parentNode) part.replaceWith(outer);
    if (over) pane.append(bar, part);
    else pane.append(part, bar);
    outer.append(pane);
    let peek = false;
    let fading = 0;
    let dwell = 0; // waiting for the pointer to rest in the box
    let grace = 0; // waiting before closing, after the pointer left
    let last = null; // the pointer's last place and time, for its speed
    let hush = false; // closed by hand: stays shut until the pointer leaves the box
    // A box popped out into its own window has the room: it shows everything.
    const locked = () => SOLO === id || layout.expanded.includes(id);

    function show() {
      const pinned = locked();
      const open = pinned || peek;
      const low = over ? 0 : compact() || 0;
      const more = over ? part.textContent.trim() !== "" || part.querySelector("img, table, svg") !== null
        : low > 0 && part.scrollHeight > low + 2;
      outer.classList.toggle("pinned", pinned);
      outer.classList.toggle("empty", over && !open); // folded out of sight
      bar.hidden = SOLO === id || (!more && !open); // popped out, it is simply all there
      part.style.maxHeight = pinned ? "" : peek ? "70vh" : low ? `${low}px` : "";
      part.style.overflowY = clip && !open ? "hidden" : ""; // folded, the rest waits for the bar rather than scrolling
      opener.textContent = `${open ? "▾" : "▸"} ${label()}`;
      opener.setAttribute("aria-expanded", String(open));
      lock.hidden = !open || SOLO === id;
      lock.textContent = pinned ? "🔒" : "🔓";
      lock.setAttribute("aria-pressed", String(pinned));
      lock.title = pinned ? "Locked open: click to let it fold away again" : "Lock it open, taking its room in the box";
      lock.setAttribute("aria-label", lock.title);
    }

    function setPeek(on) {
      if (on === peek && !fading) return;
      clearTimeout(fading);
      fading = 0;
      if (on && !locked()) {
        if (!peek) outer.style.height = `${outer.offsetHeight}px`; // hold its place, so nothing below moves
        peek = true;
        outer.classList.remove("leaving");
        outer.classList.add("peek");
        box.classList.add("raised"); // above the boxes below it
        show();
        return;
      }
      if (!peek) return;
      outer.classList.add("leaving"); // fade out, then fold back into place
      fading = setTimeout(() => {
        fading = 0;
        peek = false;
        outer.classList.remove("peek", "leaving");
        outer.style.height = "";
        box.classList.remove("raised");
        show();
      }, FADE_MS);
    }

    // Only for a box with more to show, and not while boxes are being
    // arranged or dragged.
    const may = () => !hush && !bar.hidden && !locked() && layout.auto_expand.includes(id)
      && !document.body.classList.contains("arranging") && !document.body.classList.contains("moving");
    const settle = () => {
      clearTimeout(dwell);
      dwell = setTimeout(() => { dwell = 0; if (may()) setPeek(true); }, DWELL_MS);
    };
    opener.addEventListener("pointerenter", (e) => { if (e.pointerType === "mouse" && may()) setPeek(true); });
    opener.addEventListener("click", () => {
      if (locked()) return;
      const on = !peek || Boolean(fading);
      hush = !on;
      setPeek(on);
    });
    box.addEventListener("pointerenter", (e) => {
      if (e.pointerType !== "mouse") return;
      clearTimeout(grace);
      grace = 0;
      last = { x: e.clientX, y: e.clientY, t: e.timeStamp };
      if (peek && fading) setPeek(true); // back before it finished fading
      else if (!peek) settle();
    });
    box.addEventListener("pointermove", (e) => {
      if (e.pointerType !== "mouse" || peek) return;
      const fast = last && Math.hypot(e.clientX - last.x, e.clientY - last.y) > BRISK * Math.max(e.timeStamp - last.t, 1);
      last = { x: e.clientX, y: e.clientY, t: e.timeStamp };
      if (fast || !dwell) settle();
    });
    box.addEventListener("pointerleave", (e) => {
      if (e.pointerType !== "mouse") return;
      clearTimeout(dwell);
      dwell = 0;
      last = null;
      hush = false;
      clearTimeout(grace);
      grace = peek ? setTimeout(() => { grace = 0; setPeek(false); }, GRACE_MS) : 0;
    });
    box.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && peek) {
        hush = true;
        setPeek(false);
        opener.focus();
      }
    });
    document.addEventListener("pointerdown", (e) => { if (peek && !box.contains(e.target)) setPeek(false); });
    lock.addEventListener("click", () => {
      const pin = !locked();
      clearTimeout(fading);
      fading = 0;
      peek = false;
      outer.classList.remove("peek", "leaving");
      outer.style.height = "";
      box.classList.remove("raised");
      useLayout({ expanded: pin ? [...layout.expanded, id] : layout.expanded.filter((x) => x !== id) });
      show();
    });

    // What is inside changes (new rows, a profile looked up), and so does
    // its size: the bar and its label follow.
    const sizes = new ResizeObserver(() => show());
    const watch = () => {
      sizes.disconnect();
      sizes.observe(part);
      for (const child of part.children) sizes.observe(child);
    };
    new MutationObserver(() => { watch(); show(); }).observe(part, { childList: true, subtree: true, characterData: true });
    watch();
    const item = { outer, show };
    expanders.set(id, item);
    show();
    return item;
  }
  document.addEventListener("loglab:layout-loaded", () => { for (const e of expanders.values()) e.show(); });

  // How tall a table is with its heading and its first `count` rows, so it
  // can show that many and scroll the rest under a heading that stays put.
  // Measured from the rows, so it holds at any box size and with tags like
  // "pretend" in a row; 0 while the table is out of sight.
  function rowsHeight(table, count) {
    const rows = [...table.tBodies[0].rows].slice(0, count);
    if (!rows.length || !table.offsetHeight) return 0;
    return (table.tHead ? table.tHead.offsetHeight : 0) + rows.reduce((sum, row) => sum + row.offsetHeight, 0) + 2;
  }

  // The Log shows ten rows; the bar opens the rest.
  $("#log-wrap").classList.add("rows-wrap");
  expander({
    id: "log",
    box: moduleEl("log"),
    part: $("#log-wrap"),
    label: () => `All ${$("#log tbody").rows.length} shown`,
    compact: () => rowsHeight($("#log"), 10),
  });

  // -- One QSO: fields, edit, delete, restore, history -------------------

  // The fields a QSO's detail always shows, in order: the operator's choice.
  const detailFields = () => (layout.detail ? layout.detail.fields : DEFAULT_DETAIL);

  function setDetailFields(fields) {
    layout.detail = fields === null ? null : { fields };
    saveLayout();
    if (current) showFields(current);
    if (!$("#detail-chooser").hidden) renderDetailChooser();
  }

  // One field of the open QSO, laid out like a New QSO box: its plain name
  // with its ADIF name beside it, over its value. Which fields show is set by
  // the Fields chooser, never QSO by QSO. A field the operator always shows
  // stays on screen when cleared. A field left empty is not saved.
  const WIDE_FIELDS = new Set(["COMMENT", "NOTES", "ADDRESS", "QSLMSG", "NAME", "QTH"]);
  function fieldBox(f, { pinned = false } = {}) {
    const box = document.createElement("div");
    box.className = "field";
    box.dataset.type = f.type || "";
    box.dataset.name = f.name;
    if (WIDE_FIELDS.has(f.name.toUpperCase()) || f.value.length > 24) box.classList.add("wide");
    const head = document.createElement("div");
    head.className = "field-head";
    const label = document.createElement("span");
    label.className = "field-label";
    label.textContent = fieldLabel(f.name);
    const adif = document.createElement("span");
    adif.className = "field-adif";
    adif.textContent = f.name.toUpperCase();
    const value = document.createElement("input");
    value.value = f.value;
    value.setAttribute("aria-label", fieldLabel(f.name));
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "link field-remove";
    remove.textContent = "×";
    remove.title = pinned ? "Clear" : "Remove this field";
    remove.setAttribute("aria-label", `${pinned ? "Clear" : "Remove"} ${fieldLabel(f.name)}`);
    remove.tabIndex = -1;
    remove.addEventListener("click", () => {
      if (pinned) {
        value.value = "";
        value.focus();
      } else {
        box.remove();
      }
    });
    head.append(label, adif, remove);
    box.append(head, value);
    return box;
  }

  const fieldBoxes = () => [...$("#edit-fields").querySelectorAll(".field")];

  // The fields the operator always shows come first, in their order, empty or
  // not; then every other field the QSO has, so nothing it holds is hidden.
  function showFields(qso) {
    const typed = new Map(fieldBoxes()
      .filter((b) => current && current.id === qso.id)
      .map((b) => [b.dataset.name.toUpperCase(), $("input", b).value]));
    const have = new Map(qso.fields.map((f) => [f.name.toUpperCase(), f]));
    const pinned = detailFields();
    const boxes = pinned.map((name) => {
      const f = have.get(name) || { name, value: "", type: null };
      return fieldBox({ ...f, value: typed.has(name) ? typed.get(name) : f.value }, { pinned: true });
    });
    const shown = new Set(pinned);
    const more = qso.fields.filter((f) => !shown.has(f.name.toUpperCase()));
    if (more.length) {
      const heading = document.createElement("h4");
      heading.className = "field-group";
      heading.textContent = "More fields this QSO has";
      boxes.push(heading);
      for (const f of more) {
        const name = f.name.toUpperCase();
        boxes.push(fieldBox({ ...f, value: typed.has(name) ? typed.get(name) : f.value }));
      }
    }
    $("#edit-fields").replaceChildren(...boxes);
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
    const dialog = $("#detail");
    if (!dialog.open) dialog.showModal();
    $("#detail-title").replaceChildren(qso.call ? callsign(qso.call) : "QSO", qso.deleted ? " (deleted)" : "");
    $("#detail-meta").textContent =
      `ID ${qso.id} · version ${qso.version} · last changed ${qso.changed_at.slice(0, 16).replace("T", " ")} UTC ` +
      `by ${qso.operator} at ${qso.station}`;
    $("#edit-fields").replaceChildren();
    current = qso;
    showFields(qso);
    $("#edit-note").value = "";
    setError("#edit", "");
    $("#delete").hidden = qso.deleted;
    $("#restore").hidden = !qso.deleted;
    $("#edit button[type=submit]").hidden = qso.deleted;
    $("#detail-fields").hidden = qso.deleted;
    renderHistory(history);
    if (selected && selected.id === qso.id) selectQso(qso); // its call may have just changed
  }

  function closeDetail() {
    current = null;
    if ($("#detail").open) $("#detail").close();
  }

  // The selected Log row is highlighted, and the station profile shows its
  // call while New QSO's Call box is empty (profile.js).
  function selectQso(qso) {
    selected = qso;
    for (const row of document.querySelectorAll("#log tbody tr")) {
      row.classList.toggle("selected", Boolean(qso && row.dataset.qso === String(qso.id)));
    }
    const call = qso && !qso.deleted ? qso.call || "" : "";
    document.dispatchEvent(new CustomEvent("loglab:qso-selected", { detail: { call } }));
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
    for (const box of fieldBoxes()) {
      const value = $("input", box).value;
      if (!value.trim()) continue; // an empty field is not kept
      fields.push({ name: box.dataset.name, value, type: box.dataset.type || null });
    }
    return fields;
  }

  // A change shows the QSO again as it now stands, or, with `close`, closes
  // the window once the Engine has it.
  async function change(method, path, body, { close = false } = {}) {
    setError("#edit", "");
    try {
      const result = await api(method, path, body);
      await loadLog();
      if (close) closeDetail();
      else await openDetail(result.qso.id);
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
      edit: (qso) => { selectQso(qso); openDetail(qso.id); },
    });
    buildModuleHeads();
    buildPanelHeads();
    if (SOLO) {
      document.body.classList.add("solo");
      document.title = `${MODULES[SOLO]} · LabLog`;
    } else {
      buildPopOuts();
    }
    hearWindows();
    buildBoxPicker();
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
    $("#menu-open").addEventListener("click", () => { if (!SOLO) menu.showModal(); });
    $("#menu-close").addEventListener("click", () => menu.close());
    menu.addEventListener("click", (event) => { if (event.target === menu) menu.close(); }); // the backdrop
    tidyMenu(menu);
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
      // An overlay's own layout is its starting point (overlay.js).
      const base = normalize(overlayDefault || DEFAULT_LAYOUT);
      layout = { ...base, tables: layout.tables, detail: layout.detail, theme: layout.theme, operation: layout.operation, by_overlay: layout.by_overlay, world_qsos: layout.world_qsos, bio_open: layout.bio_open, stats_open: layout.stats_open, expanded: layout.expanded, auto_expand: layout.auto_expand, distance_unit: layout.distance_unit, log_mine: layout.log_mine };
      applyLayout();
      saveLayout();
    });
    $("#entry").addEventListener("submit", storeQso);
    // Esc anywhere in New QSO throws the half-typed contact away and starts
    // over. Nothing was journaled yet, so nothing is logged. An open band or
    // mode list takes its own Esc first.
    $("#entry").addEventListener("keydown", (event) => {
      if (event.key !== "Escape" || event.defaultPrevented || event.isComposing) return;
      event.preventDefault();
      setError("#entry", "");
      resetEntry({ cleared: true });
    });
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
    // A slide between My QSOs and Club QSOs; either name moves it there.
    const showMine = (mine) => {
      layout.log_mine = mine;
      saveLayout();
      loadLog();
    };
    $("#log-club").addEventListener("click", () => showMine(!layout.log_mine));
    for (const side of document.querySelectorAll("#log-whose [data-whose]")) {
      side.addEventListener("click", () => showMine(side.dataset.whose === "mine"));
    }
    $("#close-detail").addEventListener("click", closeDetail);
    $("#detail").addEventListener("close", () => { current = null; }); // Esc closes it too
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
        event.preventDefault(); // close the field list, not the QSO window under it
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
        change("POST", `/qsos/${current.id}/delete`, { note: note() }, { close: true }); // done with it
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

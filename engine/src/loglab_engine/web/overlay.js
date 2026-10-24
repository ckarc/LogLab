// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Operation overlays. An overlay (a contest template the Engine serves,
// Principle III) says which boxes New QSO asks for, how the page starts out,
// and, for a contest, what counts as a dupe. An operation is one use of an
// overlay, such as Field Day under the club call: every operator signs in as
// themselves (Principle XXI), picks the operation from the Menu, and the
// Engine logs each QSO as the operation's station call, noting the operator.
// Everyday logging is no operation at all: the operator's own call, and no
// dupe check. The dupe check is the Engine's (Principles I and II), so every
// position gets the same answer from the same log. A dupe is a warning,
// never a refusal.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const EVERYDAY = "everyday";
  const CHECK_AFTER_MS = 250;
  const form = $("#entry");
  const note = $("#dupe-note");

  let overlays = []; // every overlay, Everyday first
  let operations = []; // the operations open to log in
  let everyOperation = []; // ended ones too, for the Log's choice of QSOs
  let overlay = null; // the overlay in use
  let operation = null; // the operation in use, or null for everyday logging
  let timeTyped = false;
  let checkTimer = null;
  let asked = 0;

  const byId = (list, id) => list.find((x) => x.id === id) || null;
  const everyday = () => byId(overlays, EVERYDAY);
  const isAdmin = () => (LogLab.session()?.permissions || []).includes("admin");
  // Who may change an operation, ended or not, until it is archived.
  const mayChange = (op) => !op.archived_at && (isAdmin() || (op.station_call === LogLab.session()?.operator && !op.gota_call));

  // -- Loading -------------------------------------------------------------

  async function load() {
    try {
      [overlays, operations, everyOperation] = await Promise.all([
        LogLab.api("GET", "/overlays"),
        LogLab.api("GET", "/operations"),
        LogLab.api("GET", "/operations?include_ended=true"),
      ]);
    } catch (e) {
      $("#op-error").textContent = `Operations could not be loaded: ${e.message}`;
      return;
    }
    $("#op-error").textContent = "";
    const chosen = LogLab.layout().operation;
    operation = chosen ? byId(operations, chosen.id) : null;
    if (chosen && !operation) {
      // Ended or gone: back to everyday logging, and say so in the menu.
      $("#op-error").textContent = "The operation you were logging in has ended. You are logging under your own call.";
      LogLab.useLayout({ operation: null });
    }
    overlay = (operation && byId(overlays, operation.overlay)) || everyday();
    show();
  }

  // -- The page in this overlay ---------------------------------------------

  function show() {
    if (!overlay) return;
    // The boxes are put in the overlay's order on the page itself, not only
    // drawn in it, so Tab moves through them in that order too.
    const fields = overlay.entry.fields;
    const grid = $(".grid", form);
    const place = (label) => {
      const input = label.querySelector("input[name], select[name]");
      const at = input ? fields.indexOf(input.name) : -1;
      return at < 0 ? fields.length : at;
    };
    const labels = [...grid.children].filter((el) => el.tagName === "LABEL");
    labels.sort((a, b) => place(a) - place(b)); // stable: hidden boxes keep their order
    for (const label of labels) {
      label.hidden = place(label) === fields.length;
      grid.append(label);
    }
    storeAfter = overlay.entry.store_after && fields.includes(overlay.entry.store_after) ? overlay.entry.store_after : null;
    for (const part of overlay.exchange?.received || []) {
      const input = form[part.field];
      if (input) {
        input.placeholder = part.hint.replace(/^Like /, "e.g. ");
        input.title = `${part.label}: ${part.hint}`;
      }
    }
    LogLab.setDefaultLayout(overlay.layout ? arrangement(overlay) : null);
    // The page shows the map this overlay calls for (app.js, map.js).
    document.dispatchEvent(new CustomEvent("loglab:overlay-shown", { detail: { overlay } }));
    showLoggingAs();
    renderMenu();
    renderLogChoice();
    clearNote();
    check();
  }

  // Tab moves through New QSO's boxes in the overlay's order. The Store QSO
  // button stays where it is drawn; when the overlay puts it after a box, Tab
  // reaches it there, and goes on from it to the next box.
  let storeAfter = null;
  form.addEventListener("keydown", (event) => {
    if (event.key !== "Tab" || !storeAfter || event.altKey || event.ctrlKey || event.metaKey) return;
    const store = form.querySelector("button[type=submit]");
    const boxes = [...form.querySelectorAll(".grid > label:not([hidden])")]
      .map((label) => label.querySelector("input[name], select[name]"))
      .filter(Boolean);
    const at = boxes.findIndex((input) => input.name === storeAfter);
    if (at < 0) return;
    const order = [...boxes.slice(0, at + 1), store, ...boxes.slice(at + 1)];
    if (at === boxes.length - 1) return; // already last: the page's own order
    const here = order.indexOf(event.target);
    if (here < 0) return;
    // Past the last box Tab goes round to the first, not back to Store QSO.
    const next = order[here + (event.shiftKey ? -1 : 1)] || (event.shiftKey ? null : order[0]);
    if (!next) return; // back out of the first box: the page's own order
    event.preventDefault();
    next.focus();
  });

  // The overlay's starting layout, as the page's layout keeps one.
  function arrangement(o) {
    const modules = o.layout.modules.map((m) => ({ id: m.id, width: m.width, side: m.side, hidden: m.hidden }));
    return { modules, tables: { log: { columns: [...o.layout.log_columns], sort: "entered", descending: true } } };
  }

  // The operation and the call QSOs are made as, beside New QSO's title.
  function showLoggingAs() {
    const name = $("#overlay-name");
    const gota = Boolean(LogLab.layout().operation?.gota && operation?.gota_call);
    name.hidden = !operation;
    if (operation) {
      const when = shortHours(operation);
      name.textContent = `${operation.name}${operation.practice ? " (practice)" : ""} · ${gota ? `GOTA ${callText(operation.gota_call)}` : callText(operation.station_call)}${when ? ` · ${when}` : ""}`;
    }
    tick();
  }

  // A countdown beside the label: to the start until the operation begins,
  // then to its end. The last hour shows in red.
  const countdown = $("#op-countdown");
  function tick() {
    const now = Date.now();
    const start = operation?.starts_at ? Date.parse(operation.starts_at) : null;
    const end = operation?.ends_at ? Date.parse(operation.ends_at) : null;
    let text = "";
    let left = null;
    if (start !== null && now < start) {
      left = start - now;
      text = `starts in ${span(left)}`;
    } else if (end !== null && now <= end) {
      left = end - now;
      text = `ends in ${span(left)}`;
    } else if (end !== null) {
      text = "ended";
    }
    countdown.hidden = !text;
    countdown.textContent = text;
    countdown.classList.toggle("soon", left !== null && left < 3600e3 && text.startsWith("ends"));
  }
  setInterval(tick, 1000);

  // "2d 04:10:33", or "04:10:33" under a day.
  function span(ms) {
    const s = Math.floor(ms / 1000);
    const two = (n) => String(n).padStart(2, "0");
    const clock = `${two(Math.floor(s / 3600) % 24)}:${two(Math.floor(s / 60) % 60)}:${two(s % 60)}`;
    const days = Math.floor(s / 86400);
    return days ? `${days}d ${clock}` : clock;
  }

  // An operation's hours, short enough for the label: "Jan 30 1600 – Jan 31 2159".
  const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  function shortHours(op) {
    const at = (iso) => {
      const d = new Date(iso);
      const hhmm = `${String(d.getUTCHours()).padStart(2, "0")}${String(d.getUTCMinutes()).padStart(2, "0")}`;
      return `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()} ${hhmm}`;
    };
    if (op.starts_at && op.ends_at) return `${at(op.starts_at)} – ${at(op.ends_at)}`;
    if (op.starts_at) return `from ${at(op.starts_at)}`;
    if (op.ends_at) return `until ${at(op.ends_at)}`;
    return "";
  }

  // Zeros in callsigns are written slashed, as everywhere else on the page.
  const callText = (text) => text.replaceAll("0", "Ø");

  // Moving to another overlay keeps how the page was arranged in this one,
  // and brings back how it was arranged in the other (or its starting layout).
  function useOperation(next, gota) {
    const now = LogLab.layout();
    const from = overlay ? overlay.id : EVERYDAY;
    const nextOverlay = (next && byId(overlays, next.overlay)) || everyday();
    const change = { operation: next ? { id: next.id, gota: Boolean(gota && next.gota_call) } : null };
    if (nextOverlay.id !== from) {
      const kept = { ...now.by_overlay, [from]: { modules: now.modules, tables: now.tables } };
      const back = kept[nextOverlay.id] || (nextOverlay.layout ? arrangement(nextOverlay) : null);
      delete kept[nextOverlay.id];
      change.by_overlay = kept;
      if (back) {
        change.modules = back.modules;
        change.tables = { ...now.tables, ...back.tables };
      }
    }
    LogLab.useLayout(change);
    operation = next;
    overlay = nextOverlay;
    show();
    document.dispatchEvent(new CustomEvent("loglab:log-changed")); // the map counts this operation's QSOs
  }

  // -- The Menu ------------------------------------------------------------

  function renderMenu() {
    const select = $("#op-choice");
    select.replaceChildren();
    const own = document.createElement("option");
    own.value = "";
    own.textContent = `Everyday logging as ${LogLab.session()?.operator || "yourself"}`;
    select.append(own);
    for (const op of operations) {
      const option = document.createElement("option");
      option.value = op.id;
      option.textContent = `${op.name} (${op.overlay_name}, as ${op.station_call})`;
      select.append(option);
    }
    select.value = operation ? operation.id : "";
    const gotaRow = $("#op-gota-row");
    gotaRow.hidden = !operation?.gota_call;
    $("#op-gota").checked = Boolean(LogLab.layout().operation?.gota);
    $("#op-gota-call").textContent = operation?.gota_call ? `(logs as ${operation.gota_call})` : "";
    $("#op-current").textContent = operation ? describe(operation) : "Everyday logging: QSOs are made as your own call, with no dupe check.";
    $("#op-edit").hidden = !operation || !mayChange(operation);
  }

  function describe(op) {
    const sent = Object.values(op.sent).join(" ");
    return `${op.overlay_name}: QSOs are made as ${op.station_call}${op.gota_call ? ` (GOTA ${op.gota_call})` : ""}` +
      `${sent ? `, sending ${sent}` : ""}, with you noted as the operator on each one.${hours(op) ? ` ${hours(op)}` : ""}`;
  }

  // An operation's hours, in UTC, as a sentence.
  const utc = (iso) => iso.slice(0, 16).replace("T", " ");
  function hours(op) {
    if (op.starts_at && op.ends_at) return `Runs ${utc(op.starts_at)} to ${utc(op.ends_at)} UTC.`;
    if (op.starts_at) return `Starts ${utc(op.starts_at)} UTC.`;
    if (op.ends_at) return `Ends ${utc(op.ends_at)} UTC.`;
    return "";
  }

  // -- The Log: one operation's QSOs, and its tally --------------------------

  const logChoice = $("#log-operation");
  let followed; // the operation the Log last switched to by itself

  // In an operation the Log shows only its QSOs, like the map; logging under
  // your own call, the whole log. Another operation, or all QSOs, can still
  // be picked by hand until the operator switches again.
  function renderLogChoice() {
    const before = logChoice.value;
    const active = operation ? operation.id : "";
    const wanted = active !== followed ? active : before;
    followed = active;
    logChoice.replaceChildren(new Option("All QSOs", ""));
    for (const op of everyOperation) logChoice.append(new Option(`${op.name} only`, op.id));
    logChoice.value = everyOperation.some((op) => op.id === wanted) ? wanted : "";
    logChoice.hidden = everyOperation.length === 0;
    if (logChoice.value !== before) document.dispatchEvent(new CustomEvent("loglab:log-changed"));
  }

  async function showTally() {
    const box = $("#op-tally");
    const id = logChoice.value;
    if (!id) {
      box.hidden = true;
      return;
    }
    let summary;
    try {
      summary = await LogLab.api("GET", `/operations/${id}/summary`);
    } catch {
      box.hidden = true;
      return;
    }
    if (logChoice.value !== id) return; // changed while asking
    box.replaceChildren();
    const head = document.createElement("p");
    const parts = [`${summary.name}: ${summary.qsos} QSO${summary.qsos === 1 ? "" : "s"}, ${summary.counted} counting`];
    if (summary.outside) parts.push(`${summary.outside} outside its hours`);
    if (summary.not_counted) parts.push(`${summary.not_counted} on a band or mode that doesn't count`);
    head.textContent = `${parts.join(", ")}.`;
    const op = byId(everyOperation, id);
    if (op?.archived_at) {
      head.append(" ", Object.assign(document.createElement("span"), {
        className: "hint", textContent: `Archived ${utc(op.archived_at)} UTC: it and its QSOs can't be changed.`,
      }));
    } else if (op && mayChange(op)) {
      const change = Object.assign(document.createElement("button"), {
        type: "button", className: "link", textContent: op.ended ? "Change or archive" : "Change",
      });
      change.addEventListener("click", () => openSetup(op));
      head.append(" ", change);
    }
    box.append(head);
    // The band and mode table and the count by operator are in the Stats box.
    box.hidden = false;
  }

  // -- Setting up an operation ----------------------------------------------

  const dialog = $("#op-dialog");
  const setup = $("#op-form");
  let editing = null;

  function openSetup(op) {
    editing = op;
    $("#op-title").textContent = op ? `Change ${op.name}${op.ended ? " (ended)" : ""}` : "Set up an operation";
    setup.querySelector(".error").textContent = "";
    const select = setup.overlay;
    select.replaceChildren();
    for (const o of overlays) {
      const option = document.createElement("option");
      option.value = o.id;
      option.textContent = o.name;
      select.append(option);
    }
    select.value = op ? op.overlay : (byId(overlays, "arrl-fd") ? "arrl-fd" : EVERYDAY);
    setup.name.value = op ? op.name : "";
    setup.station_call.value = op ? op.station_call : (isAdmin() ? "" : LogLab.session()?.operator || "");
    setup.gota_call.value = op?.gota_call || "";
    setup.practice.checked = Boolean(op?.practice);
    setup.practice.disabled = Boolean(op); // set when the operation is made
    $("#op-end").hidden = !op || op.ended;
    $("#op-archive").hidden = !op?.ended;
    for (const end of ["starts", "ends"]) {
      const at = op?.[`${end}_at`] || "";
      setup[`${end}_day`].value = at.slice(0, 10);
      setup[`${end}_time`].value = at.slice(11, 16).replace(":", "");
    }
    renderSent(op ? op.sent : {});
    dialog.showModal();
  }

  function renderSent(values) {
    const chosen = byId(overlays, setup.overlay.value);
    $("#op-summary").textContent = chosen ? chosen.summary : "";
    $("#op-period").textContent = chosen?.rules ? `When it runs: ${chosen.rules.period}` : "";
    $("#op-gota-field").hidden = !chosen?.gota;
    const box = $("#op-sent");
    box.replaceChildren();
    for (const part of chosen?.exchange?.sent || []) {
      const label = document.createElement("label");
      const input = document.createElement("input");
      input.name = `sent.${part.key}`;
      input.required = !part.optional;
      input.maxLength = 40;
      input.spellcheck = false;
      input.autocapitalize = "characters";
      input.placeholder = part.hint;
      input.value = values[part.key] || "";
      label.append(`${part.label} `, input);
      // A park reference shows the park's name from LabLog's own park list.
      if (chosen.program && part.key === "park") {
        const name = document.createElement("span");
        name.className = "park-name hint";
        name.setAttribute("aria-live", "polite");
        label.append(name);
        input.addEventListener("input", () => window.LogLabParks.nameInto(input.value, name));
        window.LogLabParks.nameInto(input.value, name);
      }
      box.append(label);
    }
  }

  async function saveSetup(event, ended = false) {
    event?.preventDefault();
    const error = setup.querySelector(".error");
    error.textContent = "";
    const chosen = byId(overlays, setup.overlay.value);
    const sent = {};
    for (const part of chosen?.exchange?.sent || []) sent[part.key] = setup[`sent.${part.key}`].value.trim().toUpperCase();
    let starts_at, ends_at;
    try {
      starts_at = moment("starts", "start");
      ends_at = moment("ends", "end");
    } catch (e) {
      error.textContent = e.message;
      return;
    }
    const body = {
      overlay: setup.overlay.value,
      name: setup.name.value.trim(),
      station_call: setup.station_call.value.trim().toUpperCase(),
      gota_call: chosen?.gota && setup.gota_call.value.trim() ? setup.gota_call.value.trim().toUpperCase() : null,
      sent,
      ended: ended || Boolean(editing?.ended),
      starts_at,
      ends_at,
      practice: editing ? Boolean(editing.practice) : setup.practice.checked,
    };
    try {
      const saved = editing ? await LogLab.api("PUT", `/operations/${editing.id}`, body) : await LogLab.api("POST", "/operations", body);
      dialog.close();
      [operations, everyOperation] = await Promise.all([
        LogLab.api("GET", "/operations"),
        LogLab.api("GET", "/operations?include_ended=true"),
      ]);
      const current = !editing || editing.id === operation?.id;
      if (!current) refreshLog(); // changed from the Log: whatever you log as stays
      else if (saved.ended) useOperation(null, false);
      else useOperation(byId(operations, saved.id), LogLab.layout().operation?.gota);
    } catch (e) {
      error.textContent = e.message;
    }
  }

  // The Log shows the change at once; the operation logged in stays as it was.
  function refreshLog() {
    renderLogChoice();
    showTally();
  }

  // Archiving locks an ended operation and its QSOs for good, so they always
  // match the log that was sent in.
  async function archive() {
    const error = setup.querySelector(".error");
    error.textContent = "";
    if (!editing || !confirm(`Archive ${editing.name}? Its settings and QSOs are locked for good and can't be changed or unlocked. Do this once its log has been submitted, and after any LoTW or QRZ upload.`)) return;
    try {
      await LogLab.api("POST", `/operations/${editing.id}/archive`);
      dialog.close();
      everyOperation = await LogLab.api("GET", "/operations?include_ended=true");
      refreshLog();
    } catch (e) {
      error.textContent = e.message;
    }
  }

  // A start or end typed as a UTC date and HHMM time (Principle VIII), or null
  // when both are left empty: no limit.
  function moment(end, word) {
    const day = setup[`${end}_day`].value;
    const time = setup[`${end}_time`].value.trim().replace(":", "");
    if (!day && !time) return null;
    if (!day || !/^([01][0-9]|2[0-3])[0-5][0-9]$/.test(time)) {
      throw new Error(`Give the ${word} as a date and a 24-hour UTC time, like 1800.`);
    }
    return `${day}T${time.slice(0, 2)}:${time.slice(2)}:00Z`;
  }

  // -- The dupe check --------------------------------------------------------

  function clearNote() {
    note.hidden = true;
    note.classList.remove("warn");
    note.replaceChildren();
  }

  function say(text, { warn = false, problems = [] } = {}) {
    note.replaceChildren(text);
    for (const p of problems) {
      const line = document.createElement("span");
      line.className = "problem";
      line.textContent = p;
      note.append(line);
    }
    note.classList.toggle("warn", warn || problems.length > 0);
    note.hidden = !text && problems.length === 0;
  }

  function check() {
    clearTimeout(checkTimer);
    if (!overlay || !overlay.dupes) return clearNote();
    checkTimer = setTimeout(runCheck, CHECK_AFTER_MS);
  }

  async function runCheck() {
    const fields = LogLab.entryFields();
    if (!fields.some((f) => f.name === "CALL")) return clearNote();
    const ask = ++asked;
    const body = { fields };
    const chosen = LogLab.layout().operation;
    if (operation && chosen) body.operation = { id: operation.id, gota: Boolean(chosen.gota) };
    let result;
    try {
      result = await LogLab.api("POST", `/overlays/${overlay.id}/check`, body);
    } catch {
      return clearNote(); // no Engine, no check: logging still works (Principle V)
    }
    if (ask !== asked) return; // the operator has typed more since
    // Only what needs attention is shown: a dupe, a QSO that won't count, a
    // bad exchange, and which bands and modes a station was already worked on.
    const problems = result.problems.filter((p) => !/is missing\.$/.test(p.message)).map((p) => p.message);
    const call = callText(fields.find((f) => f.name === "CALL").value);
    if (result.status === "dupe") {
      const first = result.dupes[0];
      const when = first.qso_start ? ` at ${first.qso_start.slice(11, 16)} UTC` : "";
      say(`DUPE: ${call} was already worked on ${result.band} ${result.mode_group}${when}, by ${first.operator}. You can still store it.`,
        { warn: true, problems });
    } else if (result.status === "not_counted") {
      say(result.message, { warn: true, problems });
    } else if (result.status === "new" && result.worked.length) {
      const before = [...new Set(result.worked.filter((w) => w.band && w.mode_group).map((w) => `${w.band} ${w.mode_group}`))];
      say(before.length ? `${call} was already worked on ${before.join(", ")}.` : "", { problems });
    } else {
      say("", { problems });
    }
  }

  // -- Wiring ----------------------------------------------------------------

  document.addEventListener("loglab:layout-loaded", load);
  document.addEventListener("loglab:overlays-changed", load); // the overlay editor saved one
  document.addEventListener("loglab:log-loaded", showTally);
  logChoice.addEventListener("change", () => document.dispatchEvent(new CustomEvent("loglab:log-changed")));
  document.addEventListener("loglab:entry-reset", () => {
    timeTyped = false;
    clearNote();
  });
  for (const name of ["CALL", "BAND", "MODE", "CLASS", "ARRL_SECT", "SIG_INFO"]) {
    for (const type of ["input", "change"]) form[name].addEventListener(type, check);
  }
  for (const name of ["QSO_DATE", "TIME_ON"]) form[name].addEventListener("input", () => { timeTyped = true; });
  // In a live overlay the QSO is stamped with the moment it is stored, unless
  // the operator typed a time (logging a QSO made a few minutes ago).
  form.addEventListener("submit", () => {
    if (!overlay?.entry.live || timeTyped) return;
    const now = new Date().toISOString();
    form.QSO_DATE.value = now.slice(0, 10);
    form.TIME_ON.value = now.slice(11, 16).replace(":", "");
  }, { capture: true });

  // Another position may have set up or ended an operation since.
  $("#menu-open").addEventListener("click", () => { if (LogLab.session()) load(); });
  $("#op-choice").addEventListener("change", (event) => {
    useOperation(byId(operations, event.target.value), false);
  });
  $("#op-gota").addEventListener("change", (event) => useOperation(operation, event.target.checked));
  // Another part of the page (demo mode) asks this browser to log in an operation.
  document.addEventListener("loglab:use-operation", async (event) => {
    await load();
    const next = byId(operations, event.detail.id);
    if (next) useOperation(next, false);
  });
  $("#op-new").addEventListener("click", () => openSetup(null));
  $("#op-edit").addEventListener("click", () => openSetup(operation));
  setup.overlay.addEventListener("change", () => renderSent({}));
  setup.addEventListener("submit", (event) => saveSetup(event));
  $("#op-end").addEventListener("click", (event) => {
    if (editing && confirm(`End ${editing.name}? Its QSOs stay in the log; no new QSOs can be logged in it.`)) saveSetup(event, true);
  });
  $("#op-archive").addEventListener("click", archive);
  $("#op-cancel").addEventListener("click", () => dialog.close());
})();

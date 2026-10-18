// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Operation overlays. An overlay (a contest template the Engine serves,
// Principle III) says which boxes New QSO asks for, how the page starts out,
// and, for a contest, what counts as a dupe. An operation is one use of an
// overlay, such as Field Day under the club call: every operator signs in as
// themselves (Principle XIII), picks the operation from the Menu, and the
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
    const fields = overlay.entry.fields;
    for (const label of form.querySelectorAll(".grid > label")) {
      const input = label.querySelector("input[name], select[name]");
      const at = input ? fields.indexOf(input.name) : -1;
      label.hidden = at < 0;
      label.style.order = at < 0 ? "" : String(at);
    }
    for (const part of overlay.exchange?.received || []) {
      const input = form[part.field];
      if (input) {
        input.placeholder = part.hint.replace(/^Like /, "e.g. ");
        input.title = `${part.label}: ${part.hint}`;
      }
    }
    LogLab.setDefaultLayout(overlay.layout ? arrangement(overlay) : null);
    showLoggingAs();
    renderMenu();
    renderLogChoice();
    clearNote();
    check();
  }

  // The overlay's starting layout, as the page's layout keeps one.
  function arrangement(o) {
    const modules = o.layout.modules.map((m) => ({ id: m.id, width: m.width, side: m.side, hidden: m.hidden }));
    return { modules, tables: { log: { columns: [...o.layout.log_columns], sort: "entered", descending: true } } };
  }

  // Which call QSOs are being made as, always in sight on New QSO: the
  // operation's call, or the operator's own while an operation is running,
  // so nobody forgets they switched.
  function showLoggingAs() {
    const banner = $("#logging-as");
    const own = LogLab.session()?.operator || "";
    const gota = Boolean(LogLab.layout().operation?.gota && operation?.gota_call);
    banner.classList.toggle("in-operation", Boolean(operation));
    if (operation) {
      const call = gota ? operation.gota_call : operation.station_call;
      $("span", banner).textContent = `Logging as ${callText(call)}${gota ? " (GOTA)" : ""} for ${operation.name}. You are noted as the operator, ${callText(own)}.`;
    } else if (operations.length) {
      const running = operations.map((op) => op.name).join(", ");
      $("span", banner).textContent = `Logging as ${callText(own)}, your own call. Not in ${running}.`;
    }
    banner.hidden = !operation && !operations.length;
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
    $("#op-edit").hidden = !operation || !(isAdmin() || operation.station_call === LogLab.session()?.operator);
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
    box.append(head);
    if (summary.by_band_mode.length) box.append(tallyTable(summary.by_band_mode));
    if (summary.by_operator.length) {
      const by = document.createElement("p");
      by.className = "hint";
      by.textContent = `By operator: ${summary.by_operator.map((t) => `${callText(t.operator)} ${t.qsos}`).join(" · ")}`;
      box.append(by);
    }
    box.hidden = false;
  }

  // Bands down the side, mode groups across, as the contest groups them.
  function tallyTable(rows) {
    const groups = ["CW", "Phone", "Digital"].filter((g) => rows.some((r) => r.mode_group === g));
    const bands = [...new Set(rows.map((r) => r.band || "—"))];
    const table = document.createElement("table");
    table.className = "tally";
    const cell = (tag, text) => { const c = document.createElement(tag); c.textContent = text; return c; };
    const header = document.createElement("tr");
    header.append(cell("th", "Band"), ...groups.map((g) => cell("th", g)), cell("th", "Total"));
    table.append(header);
    for (const band of bands) {
      const tr = document.createElement("tr");
      const mine = rows.filter((r) => (r.band || "—") === band);
      tr.append(cell("td", band), ...groups.map((g) => cell("td", String(mine.find((r) => r.mode_group === g)?.qsos || ""))),
        cell("td", String(mine.reduce((n, r) => n + r.qsos, 0))));
      table.append(tr);
    }
    return table;
  }

  // -- Setting up an operation ----------------------------------------------

  const dialog = $("#op-dialog");
  const setup = $("#op-form");
  let editing = null;

  function openSetup(op) {
    editing = op;
    $("#op-title").textContent = op ? `Change ${op.name}` : "Set up an operation";
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
    $("#op-end").hidden = !op;
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
      input.required = true;
      input.maxLength = 40;
      input.spellcheck = false;
      input.autocapitalize = "characters";
      input.placeholder = part.hint;
      input.value = values[part.key] || "";
      label.append(`${part.label} `, input);
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
      ended,
      starts_at,
      ends_at,
    };
    try {
      const saved = editing ? await LogLab.api("PUT", `/operations/${editing.id}`, body) : await LogLab.api("POST", "/operations", body);
      dialog.close();
      [operations, everyOperation] = await Promise.all([
        LogLab.api("GET", "/operations"),
        LogLab.api("GET", "/operations?include_ended=true"),
      ]);
      if (ended) useOperation(null, false);
      else useOperation(byId(operations, saved.id), LogLab.layout().operation?.gota);
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
    if (!overlay || !overlay.contest_id) return clearNote();
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
  $("#logging-as button").addEventListener("click", () => $("#menu-open").click());
  document.addEventListener("loglab:entry-reset", () => {
    timeTyped = false;
    clearNote();
  });
  for (const name of ["CALL", "BAND", "MODE", "CLASS", "ARRL_SECT"]) {
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
  $("#op-new").addEventListener("click", () => openSetup(null));
  $("#op-edit").addEventListener("click", () => openSetup(operation));
  setup.overlay.addEventListener("change", () => renderSent({}));
  setup.addEventListener("submit", (event) => saveSetup(event));
  $("#op-end").addEventListener("click", (event) => {
    if (editing && confirm(`End ${editing.name}? Its QSOs stay in the log; no new QSOs can be logged in it.`)) saveSetup(event, true);
  });
  $("#op-cancel").addEventListener("click", () => dialog.close());
})();

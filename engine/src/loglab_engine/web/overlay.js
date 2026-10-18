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
  let operations = []; // the operations not ended
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
      [overlays, operations] = await Promise.all([LogLab.api("GET", "/overlays"), LogLab.api("GET", "/operations")]);
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
    const gota = Boolean(LogLab.layout().operation?.gota && operation?.gota_call);
    const name = $("#overlay-name");
    name.hidden = !operation;
    if (operation) {
      name.textContent = `${operation.name} · ${gota ? `GOTA ${callText(operation.gota_call)}` : callText(operation.station_call)}`;
    }
    renderMenu();
    clearNote();
    check();
  }

  // The overlay's starting layout, as the page's layout keeps one.
  function arrangement(o) {
    const modules = o.layout.modules.map((m) => ({ id: m.id, width: m.width, side: m.side, hidden: m.hidden }));
    return { modules, tables: { log: { columns: [...o.layout.log_columns], sort: "entered", descending: true } } };
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
      `${sent ? `, sending ${sent}` : ""}, with you noted as the operator on each one.`;
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
    renderSent(op ? op.sent : {});
    dialog.showModal();
  }

  function renderSent(values) {
    const chosen = byId(overlays, setup.overlay.value);
    $("#op-summary").textContent = chosen ? chosen.summary : "";
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
    const body = {
      overlay: setup.overlay.value,
      name: setup.name.value.trim(),
      station_call: setup.station_call.value.trim().toUpperCase(),
      gota_call: chosen?.gota && setup.gota_call.value.trim() ? setup.gota_call.value.trim().toUpperCase() : null,
      sent,
      ended,
    };
    try {
      const saved = editing ? await LogLab.api("PUT", `/operations/${editing.id}`, body) : await LogLab.api("POST", "/operations", body);
      dialog.close();
      operations = await LogLab.api("GET", "/operations");
      if (ended) useOperation(null, false);
      else useOperation(byId(operations, saved.id), LogLab.layout().operation?.gota);
    } catch (e) {
      error.textContent = e.message;
    }
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

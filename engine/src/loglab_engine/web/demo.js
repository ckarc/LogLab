// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Demo mode, in the Menu under Admin: an administrator turns it on and off.
// While it is on, the Engine has pretend operators (Q1PRE, Q2PRE...) on the
// air at pretend stations with pretend radios, logging made-up QSOs with Q
// calls in the practice operation Demo Field Day, so every box has something
// to show. Everything it makes says it is pretend (Principle XX), and a
// practice operation never uploads. Turning it on also switches this browser
// to that operation; "Watch it here" does the same later.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const row = $("#demo-row");
  const box = $("#demo-on");
  const status = $("#demo-status");
  const watch = $("#demo-watch");
  const EVERY_MS = 30000;
  const LOG_MS = 20000;
  const IDLE = status.textContent;
  let state = null;

  const isAdmin = () => (LogLab.session()?.permissions || []).includes("admin");
  const problem = (text) => { $("#demo-error").textContent = text; };

  function render() {
    box.checked = Boolean(state?.on);
    const here = state?.on && LogLab.layout().operation?.id === state.operation;
    watch.hidden = !state?.on || here;
    if (!state?.on) {
      status.textContent = state?.problem ? `${state.problem} ${IDLE}` : IDLE;
      return;
    }
    const since = state.started_at.slice(11, 16).replace(":", "");
    status.textContent = `On since ${since}Z in ${state.operation_name}, a practice operation: `
      + `${state.stations.length} pretend stations, ${state.qsos} pretend QSOs so far. `
      + "Turn it off here; it also stops if the Engine restarts or you sign out.";
  }

  async function refresh() {
    row.hidden = !isAdmin();
    if (!LogLab.session()) return;
    try {
      state = await LogLab.api("GET", "/demo");
      problem("");
    } catch (e) {
      problem(`Demo mode could not be checked: ${e.message}`);
    }
    render();
  }

  box.addEventListener("change", async () => {
    box.disabled = true;
    try {
      state = await LogLab.api("PUT", "/demo", { on: box.checked });
      problem("");
      if (state.on) document.dispatchEvent(new CustomEvent("loglab:use-operation", { detail: { id: state.operation } }));
    } catch (e) {
      problem(`Demo mode could not be turned ${box.checked ? "on" : "off"}: ${e.message}`);
    }
    box.disabled = false;
    render();
  });
  watch.addEventListener("click", () => {
    if (state?.on) document.dispatchEvent(new CustomEvent("loglab:use-operation", { detail: { id: state.operation } }));
    watch.hidden = true;
  });
  document.addEventListener("loglab:layout-loaded", refresh);
  document.addEventListener("loglab:log-changed", render); // this browser may have switched operation
  // The log and maps show new QSOs when this browser logs one; the pretend
  // operators log elsewhere, so while watching them, look again every so often.
  setInterval(() => {
    if (state?.on && LogLab.session() && LogLab.layout().operation?.id === state.operation) {
      document.dispatchEvent(new CustomEvent("loglab:log-changed"));
    }
  }, LOG_MS);
  $("#menu-open").addEventListener("click", () => { if (LogLab.session()) refresh(); });
  setInterval(() => { if (LogLab.session()) refresh(); }, EVERY_MS); // anyone may watch it
})();

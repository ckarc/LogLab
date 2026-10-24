// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Station Agents: an administrator approves an Agent that asks to join as a
// station, and sees the stations that have one. A new Agent shows a short
// code on its own computer; the same code shows here, and approving makes the
// station's key, which the Agent collects by itself (Principle XXI). Nobody
// copies a key by hand. Only administrators see any of this.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const dialog = $("#agents-dialog");
  const opener = $("#agents-open");
  const waitingLabel = $("#agents-waiting");
  const WATCH_MS = 10000; // how often an administrator's page looks for Agents asking to join
  const OPEN_MS = 3000; // and how often while this dialog is open

  const isAdmin = () => (LogLab.session()?.permissions || []).includes("admin");
  const problem = (text) => { $(".error", dialog).textContent = text; };

  function button(text, onClick, secondary = true) {
    const b = Object.assign(document.createElement("button"), { type: "button", textContent: text });
    b.className = secondary ? "secondary small" : "small";
    b.addEventListener("click", onClick);
    return b;
  }

  function span(className, text) {
    return Object.assign(document.createElement("span"), { className, textContent: text });
  }

  async function act(method, path, confirmText) {
    if (confirmText && !confirm(confirmText)) return;
    problem("");
    try {
      await LogLab.api(method, path);
    } catch (e) {
      problem(e.message);
    }
    refresh();
  }

  function waitingRow(p) {
    const item = document.createElement("li");
    const asked = new Date(p.asked_at).toISOString().slice(11, 16).replace(":", "");
    item.append(
      span("agent-station", p.station),
      span("agent-code", p.code),
      span("hint", `${p.agent}, asked ${asked}Z${p.replaces ? "; replaces this station's Agent" : ""}`),
    );
    const actions = span("agent-actions", "");
    actions.append(
      button("Approve", () => act("POST", `/agent/pairings/${p.id}/approve`), false),
      button("Refuse", () => act("POST", `/agent/pairings/${p.id}/refuse`)),
    );
    item.append(actions);
    return item;
  }

  function keyedRow(station) {
    const item = document.createElement("li");
    item.append(span("agent-station done", station));
    const actions = span("agent-actions", "");
    actions.append(button("Disconnect", () => act(
      "DELETE",
      `/agent/keys/${encodeURIComponent(station)}`,
      `Disconnect the Agent at ${station}? It stops reporting its radio until it asks to join again and you approve it.`,
    )));
    item.append(actions);
    return item;
  }

  function showWaiting(count) {
    waitingLabel.hidden = !count;
    waitingLabel.textContent = count === 1 ? "1 Agent asking to join" : `${count} Agents asking to join`;
  }

  async function refresh() {
    if (!isAdmin()) {
      showWaiting(0);
      return;
    }
    try {
      const waiting = await LogLab.api("GET", "/agent/pairings");
      showWaiting(waiting.length);
      if (!dialog.open) return;
      const { stations } = await LogLab.api("GET", "/agent/keys");
      $("#agents-waiting-list").replaceChildren(...waiting.map(waitingRow));
      $("#agents-none").hidden = waiting.length > 0;
      $("#agents-keyed").replaceChildren(...stations.map(keyedRow));
      $("#agents-no-keys").hidden = stations.length > 0;
    } catch (e) {
      if (dialog.open) problem(e.message);
    }
  }

  function open() {
    $("#menu-dialog").close();
    problem("");
    dialog.showModal();
    refresh();
  }

  opener.addEventListener("click", open);
  waitingLabel.addEventListener("click", open);
  $("#agents-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("click", (event) => { if (event.target === dialog) dialog.close(); });
  document.addEventListener("loglab:layout-loaded", () => {
    opener.hidden = !isAdmin();
    refresh();
  });
  setInterval(() => { if (!dialog.open) refresh(); }, WATCH_MS);
  setInterval(() => { if (dialog.open) refresh(); }, OPEN_MS);
})();

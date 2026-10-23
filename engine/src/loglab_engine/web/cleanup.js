// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Data clean-up: an administrator clears the QRZ.com lookups LabLog kept for
// calls that never made it into the log, once they are old enough. Only that
// cache goes; the log is never touched, and a call in any QSO, deleted ones
// too, keeps its lookup. A cleared call is simply looked up again if typed.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const dialog = $("#cleanup-dialog");
  const opener = $("#cleanup-open");
  const age = $("#cleanup-days");
  const count = $("#cleanup-count");
  const clear = $("#cleanup-clear");

  const isAdmin = () => (LogLab.session()?.permissions || []).includes("admin");
  const problem = (text) => { $(".error", dialog).textContent = text; };

  function size(bytes) {
    if (bytes >= 1048576) return `${(bytes / 1048576).toFixed(1)} MB`;
    if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
    return `${bytes} bytes`;
  }

  let found = 0;

  async function load() {
    problem("");
    count.classList.remove("done");
    try {
      const state = await LogLab.api("GET", `/callbook/cleanup?days=${age.value}`);
      found = state.calls;
      if (!found) {
        count.textContent = `Nothing to clear: every saved lookup older than ${state.days} days is of a logged call.`;
      } else {
        const media = state.photos_and_bios ? `, with ${state.photos_and_bios} photos and bios (${size(state.bytes)})` : "";
        count.textContent = `${found.toLocaleString()} saved lookups of calls never logged are older than ${state.days} days${media}.`;
      }
      clear.disabled = !found;
    } catch (e) {
      problem(e.message);
    }
  }

  opener.addEventListener("click", () => {
    $("#menu-dialog").close();
    load();
    dialog.showModal();
  });
  age.addEventListener("change", load);
  clear.addEventListener("click", async () => {
    if (!found || !confirm(`Clear ${found.toLocaleString()} saved lookups? Logged calls and the log itself are not touched.`)) return;
    problem("");
    try {
      const done = await LogLab.api("POST", "/callbook/cleanup", { days: Number(age.value) });
      found = 0;
      clear.disabled = true;
      count.textContent = `Cleared ${done.removed.toLocaleString()}.`;
      count.classList.add("done");
    } catch (e) {
      problem(e.message);
    }
  });
  $("#cleanup-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("click", (event) => { if (event.target === dialog) dialog.close(); });
  document.addEventListener("loglab:layout-loaded", () => { opener.hidden = !isAdmin(); });
})();

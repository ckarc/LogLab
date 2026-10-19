// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The US and Canada call lists: how current LogLab's own copies of the FCC's
// and ISED's amateur lists are (Principle VI: each list's source and date are
// shown), with the credit Canada's licence asks for. The Engine keeps them
// current by itself whenever it has internet; an admin can ask it to check now.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const dialog = $("#licenses-dialog");
  const list = $("#licenses-list");
  const check = $("#licenses-check");
  const NAMES = { fcc: "United States (FCC)", ised: "Canada (ISED)" };

  function day(iso) {
    return new Date(iso).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" });
  }

  function line(source) {
    const item = document.createElement("li");
    const name = NAMES[source.source] || source.country;
    if (!source.imported) {
      item.textContent = `${name}: not loaded yet.`;
      return item;
    }
    let text = `${name}: ${source.calls.toLocaleString()} calls, from the list of ${day(source.published || source.imported)}`;
    if (source.changes_imported) text += `, with changes to ${day(source.changes_published || source.changes_imported)}`;
    item.textContent = `${text}.`;
    item.classList.add("done");
    return item;
  }

  function problem(text) { $(".error", dialog).textContent = text; }

  async function show() {
    problem("");
    try {
      const state = await window.LogLab.api("GET", "/licenses");
      list.replaceChildren(...state.sources.map(line));
      $("#licenses-problem").textContent = state.problem ? `Last check: ${state.problem}` : "";
      const credits = $("#licenses-credits");
      credits.replaceChildren(...state.sources.map((s) => {
        const p = document.createElement("p");
        p.className = "hint";
        p.textContent = s.credit;
        return p;
      }));
    } catch (e) {
      problem(e.message);
    }
  }

  $("#licenses-open").addEventListener("click", () => {
    $("#menu-dialog").close();
    const session = window.LogLab.session();
    check.hidden = !session || !session.permissions.includes("admin");
    show();
    dialog.showModal();
  });
  check.addEventListener("click", async () => {
    problem("");
    try {
      await window.LogLab.api("POST", "/licenses/refresh");
      $("#licenses-problem").textContent = "Checking now. Loading a new FCC list takes a few minutes; lookups keep working meanwhile.";
    } catch (e) {
      problem(e.message);
    }
  });
  $("#licenses-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("click", (event) => { if (event.target === dialog) dialog.close(); });
})();

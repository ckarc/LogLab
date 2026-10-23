// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The POTA park list: how current LogLab's own copy of the Parks on the Air
// park list is (Principle VI: its source and date are shown), and a search to
// find a park in it with no internet (Principle V). The Engine keeps it
// current by itself; an admin can ask it to check now.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const dialog = $("#parks-dialog");
  const search = $("#parks-search");
  const found = $("#parks-found");
  const check = $("#parks-check");
  let asked = 0; // only the latest search's answer is shown

  function day(iso) {
    return new Date(iso).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" });
  }

  function problem(text) { $(".error", dialog).textContent = text; }

  function line(park) {
    const item = document.createElement("li");
    const ref = document.createElement("strong");
    ref.textContent = park.reference;
    const name = document.createElement("span");
    name.textContent = ` ${park.name}`;
    item.append(ref, name);
    const where = [park.locations.join(", "), park.grid].filter(Boolean).join(" · ");
    if (where || !park.active) {
      const more = document.createElement("span");
      more.className = "hint";
      more.textContent = ` ${where}${park.active ? "" : " (no longer active)"}`;
      item.append(more);
    }
    return item;
  }

  async function show() {
    problem("");
    try {
      const state = await window.LogLab.api("GET", "/park-list");
      $("#parks-state").textContent = state.imported
        ? `${state.parks.toLocaleString()} parks, from the list of ${day(state.published || state.imported)}.`
        : "Not loaded yet. LabLog loads it the next time it is on the shop line.";
      $("#parks-problem").textContent = state.problem ? `Last check: ${state.problem}` : "";
      $("#parks-credit").textContent = state.credit;
    } catch (e) {
      problem(e.message);
    }
  }

  async function find() {
    const text = search.value.trim();
    const mine = ++asked;
    if (!text) { found.replaceChildren(); return; }
    try {
      const answer = await window.LogLab.api("GET", `/parks?q=${encodeURIComponent(text)}&limit=10`);
      if (mine !== asked) return;
      if (!answer.parks.length) {
        const none = document.createElement("li");
        none.className = "hint";
        none.textContent = "No park in the list matches.";
        found.replaceChildren(none);
        return;
      }
      found.replaceChildren(...answer.parks.map(line));
    } catch (e) {
      if (mine === asked) problem(e.message);
    }
  }

  $("#parks-open").addEventListener("click", () => {
    $("#menu-dialog").close();
    const session = window.LogLab.session();
    check.hidden = !session || !session.permissions.includes("admin");
    show();
    dialog.showModal();
    search.focus();
  });
  let typing = null;
  search.addEventListener("input", () => {
    clearTimeout(typing);
    typing = setTimeout(find, 200);
  });
  check.addEventListener("click", async () => {
    problem("");
    try {
      await window.LogLab.api("POST", "/park-list/refresh");
      $("#parks-problem").textContent = "Checking now. Lookups keep working meanwhile.";
    } catch (e) {
      problem(e.message);
    }
  });
  $("#parks-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("click", (event) => { if (event.target === dialog) dialog.close(); });
})();

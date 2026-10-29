// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Lab Stock: each radio model's everyday settings, which the lab's radios are
// put back to between users. A Station Agent sends a radio's settings in as a
// proposal; it changes nothing until an administrator approves it here,
// after seeing exactly what it changes against the Lab Stock now. Every
// proposal and decision is kept, and going back to an earlier Lab Stock is a
// new proposal that needs its own approval. Only administrators see any of
// this; the Engine enforces it too.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LabLog = window.LabLog;
  const dialog = $("#lab-stock-dialog");
  const opener = $("#lab-stock-open");
  const list = $("#lab-stock-models");

  const isAdmin = () => (LabLog.session()?.permissions || []).includes("admin");
  const problem = (text) => { $(".error", dialog).textContent = text; };

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function button(text, onClick, secondary = true) {
    const b = Object.assign(document.createElement("button"), { type: "button", textContent: text });
    b.className = secondary ? "secondary small" : "small";
    b.addEventListener("click", onClick);
    return b;
  }

  const utc = (when) => `${new Date(when).toISOString().slice(0, 16).replace("T", " ")}Z`;
  const value = (s) => (s ? (s.shown ? `${s.shown} (${s.bytes})` : s.bytes) : "not set");

  function from(p) {
    const where = p.again_of
      ? `Lab Stock #${p.again_of} proposed again`
      : `read from radio ${p.serial} at ${p.station}`;
    return `${where}${p.proposed_by ? ` by ${p.proposed_by}` : ""}, ${utc(p.proposed_at)}`;
  }

  // What approving `p` would change against `now` (null: no Lab Stock yet).
  function differences(p, now) {
    const before = new Map((now?.settings || []).map((s) => [s.id, s]));
    const after = new Map(p.settings.map((s) => [s.id, s]));
    const rows = [];
    for (const s of p.settings) {
      const old = before.get(s.id);
      if (!old || old.bytes !== s.bytes) rows.push({ label: s.label, was: old, becomes: s });
    }
    for (const s of now?.settings || []) {
      if (!after.has(s.id)) rows.push({ label: s.label, was: s, becomes: null });
    }
    return rows;
  }

  function settingsTable(rows, head) {
    const table = el("table", "lab-stock-table");
    const top = el("tr");
    for (const text of head) top.append(el("th", "", text));
    table.append(top);
    for (const cells of rows) {
      const tr = el("tr");
      for (const text of cells) tr.append(el("td", "", text));
      table.append(tr);
    }
    return table;
  }

  function allSettings(p, summary) {
    const details = el("details", "lab-stock-all");
    details.append(el("summary", "", summary));
    details.append(settingsTable(p.settings.map((s) => [s.label, value(s)]), ["Setting", "Value"]));
    return details;
  }

  async function act(method, path, body, confirmText) {
    if (confirmText && !confirm(confirmText)) return;
    problem("");
    try {
      await LabLog.api(method, path, body);
    } catch (e) {
      problem(e.message);
    }
    refresh();
  }

  function proposalCard(p, now) {
    const card = el("li", "lab-stock-proposal");
    card.append(el("div", "lab-stock-title", `Proposal #${p.id}`));
    card.append(el("div", "hint", from(p)));
    if (p.note) card.append(el("div", "lab-stock-note", p.note));
    const changes = differences(p, now);
    if (!now) {
      card.append(el("p", "", `The first Lab Stock for this model: ${p.settings.length} settings.`));
      card.append(allSettings(p, "Show every setting"));
    } else if (changes.length === 0) {
      card.append(el("p", "", `The same as Lab Stock #${now.id}; approving it changes nothing.`));
    } else {
      card.append(el("p", "", `Changes ${changes.length} of the Lab Stock's settings:`));
      card.append(settingsTable(
        changes.map((c) => [c.label, value(c.was), c.becomes ? value(c.becomes) : "left out"]),
        ["Setting", `Lab Stock #${now.id}`, `Proposal #${p.id}`],
      ));
    }
    const actions = el("div", "lab-stock-actions");
    const what = now ? `${changes.length} change${changes.length === 1 ? "" : "s"}` : `${p.settings.length} settings`;
    actions.append(
      button("Approve", () => act(
        "POST",
        `/lab-stock/${p.id}/approve`,
        { against: now ? now.id : null },
        `Make proposal #${p.id} the ${p.maker} ${p.model} Lab Stock (${what})? ` +
          "Every station's Agent puts it on a radio only when the operator there asks and says yes.",
      ), false),
      button("Refuse", () => act(
        "POST",
        `/lab-stock/${p.id}/refuse`,
        undefined,
        `Refuse proposal #${p.id}? It is kept, and can never become the Lab Stock.`,
      )),
    );
    card.append(actions);
    return card;
  }

  function historyRow(p) {
    const item = el("li");
    const decided = p.decision
      ? `${p.decision.approved ? "approved" : "refused"} by ${p.decision.by}, ${utc(p.decision.at)}`
      : "";
    item.append(el("span", "lab-stock-title", `#${p.id}`), el("span", "hint", `${from(p)}; ${decided}`));
    if (p.status === "approved") {
      item.append(button("Propose again", () => act(
        "POST",
        `/lab-stock/${p.id}/again`,
        undefined,
        `Propose Lab Stock #${p.id} again? It becomes a new proposal, which still needs approving.`,
      )));
    }
    return item;
  }

  function modelSection(name, proposals) {
    const section = el("section", "lab-stock-model");
    section.append(el("h3", "", name));
    const now = proposals.find((p) => p.current) || null;
    if (now) {
      section.append(el("p", "", `Lab Stock #${now.id}: approved by ${now.decision.by}, ${utc(now.decision.at)}.`));
      section.append(el("p", "hint", from(now)));
      section.append(allSettings(now, `Show its ${now.settings.length} settings`));
    } else {
      section.append(el("p", "hint", "No Lab Stock approved yet."));
    }
    const waiting = proposals.filter((p) => p.status === "proposed");
    if (waiting.length) {
      section.append(el("h4", "", "Waiting for approval"));
      const ul = el("ul", "lab-stock-list");
      ul.append(...waiting.map((p) => proposalCard(p, now)));
      section.append(ul);
    }
    const earlier = proposals.filter((p) => p.status !== "proposed" && !p.current);
    if (earlier.length) {
      const details = el("details", "lab-stock-history");
      details.append(el("summary", "", `Earlier (${earlier.length})`));
      const ul = el("ul", "lab-stock-list");
      ul.append(...earlier.map(historyRow));
      details.append(ul);
      section.append(details);
    }
    return section;
  }

  async function refresh() {
    if (!dialog.open) return;
    try {
      const proposals = await LabLog.api("GET", "/lab-stock");
      const models = new Map();
      for (const p of proposals) {
        const name = `${p.maker} ${p.model}`;
        if (!models.has(name)) models.set(name, []);
        models.get(name).push(p);
      }
      list.replaceChildren(...[...models].sort(([a], [b]) => a.localeCompare(b))
        .map(([name, found]) => modelSection(name, found)));
      $("#lab-stock-none").hidden = models.size > 0;
    } catch (e) {
      problem(e.message);
    }
  }

  function open() {
    $("#menu-dialog").close();
    problem("");
    dialog.showModal();
    refresh();
  }

  opener.addEventListener("click", open);
  $("#lab-stock-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("click", (event) => { if (event.target === dialog) dialog.close(); });
  document.addEventListener("loglab:layout-loaded", () => { opener.hidden = !isAdmin(); });
})();

// SPDX-FileCopyrightText: 2026 LabLog contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Lab profiles: each radio model's default setup for each program (VARA,
// FT8, JS8, fldigi, SSTV), which an enrolled radio gets as that program
// starts. An administrator updates one here from a station's radio: LabLog
// asks that station's Agent to read the radio (reading changes nothing), shows
// the profile now beside what the radio has, with the settings changed on the
// radio (since its Agent started or last put a profile on) ticked, and one press saves the ticked settings, as the radio has
// them, as the new default. Every version is kept; going back to an earlier
// one saves it again. Only administrators see any of this; the Engine
// enforces it too.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LabLog = window.LabLog;
  const dialog = $("#lab-profiles-dialog");
  const opener = $("#lab-profiles-open");
  const stationPick = $("#lab-profile-station");
  const programPick = $("#lab-profile-program");
  const readButton = $("#lab-profile-read");
  const status = $("#lab-profile-status");
  const reading = $("#lab-profile-reading");
  const saved = $("#lab-profile-saved");

  // What each program's profile goes on for, as the Agent tells programs apart.
  const STARTS = { VARA: "VarAC or VARA", FT8: "WSJT-X", JS8: "JS8Call", fldigi: "fldigi", SSTV: "MMSSTV" };
  const WAIT_MS = 90000;
  const LOOK_MS = 2000;

  const isAdmin = () => (LabLog.session()?.permissions || []).includes("admin");
  const problem = (text) => { $(".error", dialog).textContent = text; };
  const utc = (when) => `${new Date(when).toISOString().slice(0, 16).replace("T", " ")}Z`;
  const value = (s) => (s ? s.shown || s.bytes : "not in it");
  let asking = 0;

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

  function table(head) {
    const t = el("table", "lab-stock-table");
    const top = el("tr");
    for (const text of head) top.append(el("th", "", text));
    t.append(top);
    return t;
  }

  // One row of the reading: tick, setting, the profile now, the radio now.
  function row(s, before, ticked, changed) {
    const tr = el("tr");
    const box = Object.assign(document.createElement("input"), { type: "checkbox", checked: ticked });
    box.dataset.id = s.id;
    box.setAttribute("aria-label", `Put ${s.label} in the profile`);
    const tick = el("td");
    tick.append(box);
    const name = el("td", "", s.label);
    if (changed) name.append(" ", el("span", "lab-profile-changed", "changed on the radio"));
    const differs = !before || before.bytes !== s.bytes;
    tr.append(tick, name, el("td", "", value(before)), el("td", differs ? "lab-profile-differs" : "", value(s)));
    return tr;
  }

  function showReading(read) {
    reading.replaceChildren();
    const before = new Map(read.profile.map((s) => [s.id, s]));
    const changed = new Set(read.changed);
    const main = read.settings.filter((s) => before.has(s.id) || changed.has(s.id));
    const rest = read.settings.filter((s) => !before.has(s.id) && !changed.has(s.id));
    const name = `${read.model} ${read.program}`;
    reading.append(el("p", "", `${read.maker} ${read.model} (serial ${read.serial}) at ${read.station}, read ${utc(read.read_at)}. ` +
      `The ${read.program} profile now: ${read.profile_from}.`));
    reading.append(el("p", "hint", changed.size
      ? `Ticked: the profile's settings, and the ${changed.size} changed on this radio since its Agent started or last put a profile on. Untick what shouldn't be in the default.`
      : "Ticked: the profile's settings. Nothing was changed on this radio since its Agent started or last put a profile on; tick any other setting below to add it."));
    const head = ["", "Setting", "Profile now", "This radio"];
    const t = table(head);
    for (const s of main) t.append(row(s, before.get(s.id), true, changed.has(s.id)));
    reading.append(t);
    if (rest.length) {
      const more = el("details", "lab-stock-all");
      more.append(el("summary", "", `Add another of this radio's settings (${rest.length} more)`));
      const t2 = table(head);
      for (const s of rest) t2.append(row(s, null, false, false));
      more.append(t2);
      reading.append(more);
    }
    const left = read.profile.filter((s) => !read.settings.some((r) => r.id === s.id));
    if (left.length) {
      reading.append(el("p", "hint", `Not read from this radio, so left out: ${left.map((s) => s.label).join(", ")}.`));
    }
    const actions = el("div", "lab-stock-actions");
    actions.append(button(`Update the default ${read.program} profile`, () => save(read, name), false));
    reading.append(actions);
  }

  async function save(read, name) {
    const ids = [...reading.querySelectorAll("input[type=checkbox]:checked")].map((box) => box.dataset.id);
    if (!ids.length) {
      problem("Tick at least one setting for the profile.");
      return;
    }
    const sure = `Make these ${ids.length} settings, as the radio at ${read.station} has them, the ${name} default profile? ` +
      `Every enrolled ${read.model} gets it as ${STARTS[read.program] || read.program} starts, from the next time its Agent hears from LabLog.`;
    if (!confirm(sure)) return;
    problem("");
    try {
      const made = await LabLog.api("POST", "/lab-profiles", { read: read.id, settings: ids, against: read.against });
      reading.replaceChildren();
      status.textContent = `Saved: the ${name} default profile is version #${made.id} now.`;
    } catch (e) {
      problem(e.message);
    }
    refresh();
  }

  async function ask() {
    problem("");
    reading.replaceChildren();
    const station = stationPick.value;
    const program = programPick.value;
    if (!station) {
      problem("Pick the station whose radio is set up the way the profile should be.");
      return;
    }
    const mine = ++asking;
    readButton.disabled = true;
    try {
      let read = await LabLog.api("POST", "/profile-reads", { station, program });
      status.textContent = `Asked the Agent at ${station} to read its radio...`;
      const until = Date.now() + WAIT_MS;
      while (read.state === "waiting" && Date.now() < until && mine === asking && dialog.open) {
        await new Promise((done) => setTimeout(done, LOOK_MS));
        read = await LabLog.api("GET", `/profile-reads/${read.id}`);
      }
      if (mine !== asking) return;
      if (read.state === "read") {
        status.textContent = "";
        showReading(read);
      } else if (read.state === "failed") {
        status.textContent = "";
        problem(`The Agent at ${station} couldn't read its radio: ${read.problem}`);
      } else {
        status.textContent = "";
        problem(`The Agent at ${station} hasn't answered. It reads the radio only while it runs with --engine connected to this LabLog, with the radio found.`);
      }
    } catch (e) {
      status.textContent = "";
      problem(e.message);
    } finally {
      if (mine === asking) readButton.disabled = false;
    }
  }

  function versionLine(p) {
    const from = p.again_of ? `version #${p.again_of} again` : `read from radio ${p.serial} at ${p.station}`;
    return `#${p.id}: ${from}, saved by ${p.saved_by}, ${utc(p.saved_at)}`;
  }

  function profileSection(name, versions) {
    const section = el("section", "lab-stock-model");
    section.append(el("h3", "", name));
    const now = versions.find((p) => p.current);
    section.append(el("p", "", `Version ${versionLine(now)}.`));
    const details = el("details", "lab-stock-all");
    details.append(el("summary", "", `Show its ${now.settings.length} settings`));
    const t = table(["Setting", "Value"]);
    for (const s of now.settings) {
      const tr = el("tr");
      tr.append(el("td", "", s.label), el("td", "", value(s)));
      t.append(tr);
    }
    details.append(t);
    section.append(details);
    const earlier = versions.filter((p) => !p.current);
    if (earlier.length) {
      const history = el("details", "lab-stock-history");
      history.append(el("summary", "", `Earlier (${earlier.length})`));
      const ul = el("ul", "lab-stock-list");
      for (const p of earlier) {
        const item = el("li");
        item.append(el("span", "hint", versionLine(p)), button("Use this one again", async () => {
          if (!confirm(`Make version #${p.id} the ${name} default profile again?`)) return;
          problem("");
          try {
            await LabLog.api("POST", `/lab-profiles/${p.id}/again`, { against: now.id });
          } catch (e) {
            problem(e.message);
          }
          refresh();
        }));
        ul.append(item);
      }
      history.append(ul);
      section.append(history);
    }
    return section;
  }

  async function refresh() {
    if (!dialog.open) return;
    try {
      const versions = await LabLog.api("GET", "/lab-profiles");
      const groups = new Map();
      for (const p of versions) {
        const name = `${p.maker} ${p.model} ${p.program}`;
        if (!groups.has(name)) groups.set(name, []);
        groups.get(name).push(p);
      }
      saved.replaceChildren(...[...groups].sort(([a], [b]) => a.localeCompare(b))
        .map(([name, found]) => profileSection(name, found)));
      $("#lab-profiles-none").hidden = groups.size > 0;
    } catch (e) {
      problem(e.message);
    }
  }

  async function stations() {
    try {
      const keys = await LabLog.api("GET", "/agent/keys");
      const was = stationPick.value;
      stationPick.replaceChildren(...keys.stations.map((name) => Object.assign(document.createElement("option"), { value: name, textContent: name })));
      if (keys.stations.includes(was)) stationPick.value = was;
      $("#lab-profile-no-stations").hidden = keys.stations.length > 0;
    } catch (e) {
      problem(e.message);
    }
  }

  function open() {
    $("#menu-dialog").close();
    problem("");
    status.textContent = "";
    reading.replaceChildren();
    dialog.showModal();
    stations();
    refresh();
  }

  opener.addEventListener("click", open);
  readButton.addEventListener("click", ask);
  $("#lab-profiles-close").addEventListener("click", () => dialog.close());
  dialog.addEventListener("click", (event) => { if (event.target === dialog) dialog.close(); });
  dialog.addEventListener("close", () => { asking += 1; readButton.disabled = false; });
  document.addEventListener("loglab:layout-loaded", () => { opener.hidden = !isAdmin(); });
})();

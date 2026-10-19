// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The On air box: every workstation in the operation, with the band and mode
// its New QSO box is set to. While this browser is in an operation it tells
// the Engine what it is set to, every so often and whenever the band or mode
// changes, whether or not the box is shown, so the others can see it. The
// Engine keeps the list and says who is active and who shares a band and
// mode (Principles I and II). The band and mode are as the operator set
// them, not read from a radio (Principle X).
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const box = $("#onair");
  const EVERY_MS = 30000;
  let reportedTo = null; // the operation this browser last said it was in
  let ended = null; // an operation the Engine said has ended; stop reporting to it

  const shown = () => !box.classList.contains("off") && !$("#app").hidden;

  function ago(iso) {
    if (!iso) return "none yet";
    const minutes = Math.max(0, Math.round((Date.now() - Date.parse(iso)) / 60000));
    if (minutes < 1) return "just now";
    if (minutes < 60) return `${minutes} min ago`;
    return `${iso.slice(11, 13)}${iso.slice(14, 16)}Z`;
  }

  function cell(content, className = "") {
    const td = document.createElement("td");
    if (className) td.className = className;
    if (typeof content === "string") td.textContent = content;
    else td.append(content);
    return td;
  }

  function render(list) {
    $("#onair-scope").textContent = list.name;
    $("#onair-wrap").hidden = !list.stations.length;
    const rows = list.stations.map((s) => {
      const row = document.createElement("tr");
      if (s.clash) row.className = "clash";
      if (s.you) row.classList.add("you");
      const dot = document.createElement("span");
      dot.className = `onair-dot ${s.active ? "active" : "idle"}`;
      dot.title = s.active ? `Active: logged in the last ${list.active_minutes} minutes` : "Idle: no QSO lately";
      const who = document.createDocumentFragment();
      who.append(LogLab.callsign(s.operator));
      if (s.you) who.append(Object.assign(document.createElement("span"), { className: "hint", textContent: " (you)" }));
      const mode = document.createDocumentFragment();
      mode.append(s.mode || "—");
      if (s.clash) {
        mode.append(Object.assign(document.createElement("span"), {
          className: "onair-clash", textContent: " ⚠ same band and mode",
          title: `Another station is on ${s.band} ${s.mode_group} as ${s.call}. Field Day allows one transmitter per band and mode.`,
        }));
      }
      row.append(cell(dot), cell(who), cell(s.station), cell(LogLab.callsign(s.call)), cell(s.band || "—"), cell(mode), cell(ago(s.last_qso)));
      return row;
    });
    $("#onair-list tbody").replaceChildren(...rows);
    const active = list.stations.filter((s) => s.active).length;
    $("#onair-note").textContent = list.stations.length
      ? `${list.stations.length} on the air, ${active} active in the last ${list.active_minutes} minutes.`
      : "No workstation is on the air yet.";
  }

  function entry() {
    const band = $("#entry [name=BAND]").value || null;
    const mode = $("#entry [name=MODE]").value || null;
    return { band, mode, gota: Boolean(LogLab.layout().operation?.gota) };
  }

  async function report() {
    const session = LogLab.session();
    const op = session && LogLab.layout().operation;
    $("#onair-none").hidden = Boolean(op);
    if (reportedTo && reportedTo !== op?.id) {
      const left = reportedTo;
      reportedTo = null;
      LogLab.api("DELETE", `/operations/${left}/on-air`).catch(() => {}); // it drops off on its own anyway
    }
    if (!op) {
      $("#onair-wrap").hidden = true;
      $("#onair-scope").textContent = "";
      $("#onair-note").textContent = "";
      return;
    }
    try {
      let list;
      if (ended !== op.id && session.permissions.includes("log")) {
        try {
          list = await LogLab.api("PUT", `/operations/${op.id}/on-air`, entry());
          reportedTo = op.id;
        } catch (e) {
          if (!/ended/.test(e.message)) throw e;
          ended = op.id;
        }
      }
      if (!shown()) return;
      list ??= await LogLab.api("GET", `/operations/${op.id}/on-air`);
      render(list);
      $("#onair-error").textContent = "";
    } catch (e) {
      $("#onair-error").textContent = `Who is on the air could not be updated: ${e.message}`;
    }
  }

  document.addEventListener("loglab:layout-loaded", report);
  document.addEventListener("loglab:log-loaded", report); // a QSO was stored, or the operation changed
  for (const name of ["BAND", "MODE"]) $(`#entry [name=${name}]`).addEventListener("change", report);
  let wasOff = box.classList.contains("off");
  new MutationObserver(() => {
    const off = box.classList.contains("off");
    if (wasOff && !off) report();
    wasOff = off;
  }).observe(box, { attributes: true, attributeFilter: ["class"] });
  setInterval(report, EVERY_MS);
})();

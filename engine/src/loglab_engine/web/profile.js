// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The station profile: a box that shows who the call being typed in New QSO
// is, and every QSO already in the log with that call. It follows the Call
// box as the operator types, pausing for a moment so it does not ask about
// every letter. Who the call is comes from the Engine's callbook (Principle I:
// the Engine talks to QRZ.com, never the page); with no QRZ.com login or no
// internet the box still shows the log's QSOs with the call (Principle V).
// When QRZ.com has a photo or a biography for the call, the Engine fetches it
// once and keeps it, so it shows again with no internet; a biography arrives
// as plain words only, and is shown as text (Principle XII).
// The box can be popped out to a window of its own: the windows of one
// browser tell each other the call typed, with nothing sent over the network.
"use strict";

(() => {
  const CALL = /^[A-Z0-9]+(\/[A-Z0-9]+)*$/;
  // A call worth asking about: a digit with a letter after it, like K9K or N0OY.
  const LOOKS_COMPLETE = /[0-9][A-Z]/;
  const PAUSE_MS = 600;
  const LIMIT = 100;
  const FACTS = [["qth", "QTH"], ["state", "State"], ["country", "Country"], ["grid", "Grid"]];
  const QUIET = {
    not_found: "QRZ.com doesn't know this call.",
    not_set_up: "Set up QRZ.com in the Menu to see names and places here.",
  };
  const $ = (selector, root = document) => root.querySelector(selector);
  const form = $("#entry");
  let shown = ""; // the call the box is about
  let asked = 0; // each question gets a number, so a late answer to an old one is dropped
  let pauseTimer = null;

  let channel = null;
  try { channel = new BroadcastChannel("loglab-call"); } catch { /* one window only */ }

  const table = window.LogLab.qsoTable({
    id: "profile",
    table: $("#profile-log"),
    toolbar: $("#profile-tools"),
    defaults: Object.freeze({
      columns: ["date", "time", "band", "mode", "field:FREQ", "field:RST_SENT", "field:RST_RCVD", "operator"],
      sort: "qso_start",
      descending: true,
    }),
    load: () => loadWorked(),
  });

  function clear() {
    shown = "";
    asked += 1;
    $("#profile-body").hidden = true;
    $("#profile-empty").hidden = false;
    showPhoto(null);
  }

  function showPhoto(url) {
    const photo = $("#profile-photo");
    photo.hidden = !url;
    if (url) photo.src = url;
    else photo.removeAttribute("src");
  }

  // The photo is read with the operator's sign-in, so it comes as data the
  // page shows itself, never as a link to QRZ.com (Principles I and V).
  async function loadPhoto(call, question, refresh) {
    const session = window.LogLab.session();
    if (!session) return;
    const query = new URLSearchParams({ call });
    if (refresh) query.set("refresh", "true");
    try {
      const response = await fetch(`/api/v1/lookup/photo?${query}`, {
        headers: { Authorization: `Bearer ${session.token}` },
      });
      if (!response.ok) return;
      const picture = await response.blob();
      if (!/^image\/(jpeg|png|gif|webp)$/.test(picture.type)) return;
      const url = await new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(reader.result);
        reader.onerror = () => reject(reader.error);
        reader.readAsDataURL(picture);
      });
      if (question === asked) showPhoto(url);
    } catch {
      /* no photo this time */
    }
  }

  async function loadBio(call, question, refresh) {
    let bio;
    try {
      bio = await window.LogLab.api("GET", `/lookup/bio?call=${encodeURIComponent(call)}${refresh ? "&refresh=true" : ""}`);
    } catch {
      return;
    }
    if (question !== asked || !bio.text) return;
    $("#profile-bio").replaceChildren(...bio.text.split("\n").map((line) => {
      const p = document.createElement("p");
      p.textContent = line;
      return p;
    }));
    $("#profile-bio-part").hidden = false;
  }

  function fact(label, value) {
    const row = document.createElement("div");
    const dt = document.createElement("dt");
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.textContent = value;
    row.append(dt, dd);
    return row;
  }

  function showFound(found) {
    $("#profile-name").textContent = found.name || "";
    $("#profile-facts").replaceChildren(...FACTS.filter(([key]) => found[key]).map(([key, label]) => fact(label, found[key])));
    const source = $("#profile-source");
    if (found.name || FACTS.some(([key]) => found[key])) {
      const when = found.fetched_at ? `, looked up ${found.fetched_at.slice(0, 10)}` : "";
      source.textContent = `From QRZ.com${when}.${found.status === "found" ? "" : ` ${found.message}`}`;
    } else {
      source.textContent = QUIET[found.status] || found.message;
    }
  }

  async function lookUp(call, question, refresh = false) {
    let found;
    try {
      found = await window.LogLab.api("GET", `/lookup?call=${encodeURIComponent(call)}${refresh ? "&refresh=true" : ""}`);
    } catch {
      found = { status: "unreachable", message: "The Engine can't be reached right now." };
    }
    if (question !== asked) return;
    showFound(found);
    $("#profile-refresh").hidden = found.status === "not_set_up" || !found.fetched_at;
    if (found.photo) loadPhoto(call, question, refresh);
    else showPhoto(null);
    if (found.bio) loadBio(call, question, refresh);
    else $("#profile-bio-part").hidden = true;
  }

  async function loadWorked() {
    const call = shown;
    if (!call || !window.LogLab.session()) return;
    const order = table.state();
    const params = new URLSearchParams({
      call, limit: String(LIMIT), sort: order.sort, newest_first: String(order.descending),
    });
    let qsos;
    try {
      qsos = await window.LogLab.api("GET", `/qsos?${params}`);
    } catch (e) {
      if (call === shown) $("#profile-worked").textContent = e.message;
      return;
    }
    if (call !== shown) return;
    table.render(qsos, (row, qso) => row.addEventListener("click", () => window.LogLab.openQso(qso.id)));
    const worked = $("#profile-worked");
    worked.classList.toggle("first", qsos.length === 0);
    worked.textContent = qsos.length === 0 ? "Not worked before"
      : `Worked before: ${qsos.length === LIMIT ? `the latest ${LIMIT}` : qsos.length} QSO${qsos.length === 1 ? "" : "s"}, ${table.describeSort()}`;
    $(".table-wrap", $("#profile-body")).hidden = qsos.length === 0;
  }

  function show(call) {
    if (call === shown) return;
    if (!CALL.test(call) || call.length < 3 || !LOOKS_COMPLETE.test(call)) { clear(); return; }
    shown = call;
    asked += 1;
    $("#profile-call").replaceChildren(window.LogLab.callsign(call));
    $("#profile-name").textContent = "";
    $("#profile-facts").replaceChildren();
    $("#profile-source").textContent = "";
    $("#profile-worked").textContent = "";
    $("#profile-refresh").hidden = true;
    $("#profile-bio-part").hidden = true;
    $("#profile-bio").replaceChildren();
    showPhoto(null);
    $("#profile-empty").hidden = true;
    $("#profile-body").hidden = false;
    lookUp(call, asked);
    loadWorked();
  }

  const typed = () => form.CALL.value.trim().toUpperCase();

  function tell(message) {
    if (channel) channel.postMessage(message);
  }

  form.CALL.addEventListener("input", () => {
    clearTimeout(pauseTimer);
    const call = typed();
    if (!call) {
      clear();
      tell({ call: "" });
      return;
    }
    pauseTimer = setTimeout(() => { show(call); tell({ call }); }, PAUSE_MS);
  });
  form.CALL.addEventListener("change", () => {
    clearTimeout(pauseTimer);
    show(typed());
    tell({ call: typed() });
  });

  $("#profile-refresh").addEventListener("click", () => {
    if (!shown) return;
    asked += 1;
    $("#profile-source").textContent = "Asking QRZ.com…";
    lookUp(shown, asked, true);
  });

  // After a QSO is stored the box keeps the station just worked, now with
  // the new QSO in its list.
  document.addEventListener("loglab:log-loaded", () => loadWorked());
  document.addEventListener("loglab:entry-reset", () => tell({ stored: true }));
  if (channel) {
    channel.addEventListener("message", (event) => {
      const message = event.data || {};
      if (message.stored) loadWorked();
      else if (typeof message.call === "string") show(message.call.trim().toUpperCase());
    });
  }
})();

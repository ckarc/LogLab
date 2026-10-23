// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The station profile: a box that shows who the call being typed in New QSO
// is, and every QSO already in the log with that call. It follows the Call
// box as the operator types, pausing for a moment so it does not ask about
// every letter. While the Call box is empty it shows the call of the QSO
// selected in the Log, and with neither it is empty. Who the call is comes from the Engine's callbook (Principle I:
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
  const PAUSE_MS = 1000; // a call is looked up once typing stops this long, or on Tab
  const LIMIT = 100;
  const FACTS = [["qth", "QTH"], ["state", "State"], ["country", "Country"], ["grid", "Grid"]];
  const QUIET = {
    not_found: "QRZ.com doesn't know this call.",
    not_set_up: "Set up QRZ.com in the Menu to see names and places here.",
  };
  const $ = (selector, root = document) => root.querySelector(selector);
  const form = $("#entry");
  let shown = ""; // the call the box is about
  let selected = ""; // the call of the QSO selected in the Log, if any
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

  // -- How far and which way: from this station to the one shown ----------
  // The other end is the centre of a grid square, and this end is FieldLab's
  // GPS fix or else a grid square too, so the distance and heading are worked
  // out, not measured, and the box says so. With either end unknown, nothing is shown.
  const KM_PER_MI = 1.609344;
  const HOME_KEEP_MS = 5 * 60 * 1000;
  let home = null; // { at, place: Promise<{lat, lon, source} | null> }

  function gridPlace(text) {
    const grid = String(text || "").trim().toUpperCase();
    if (!/^[A-R]{2}(?:[0-9]{2}(?:[A-X]{2}(?:[0-9]{2})?)?)?$/.test(grid)) return null;
    let lon = -180 + (grid.charCodeAt(0) - 65) * 20;
    let lat = -90 + (grid.charCodeAt(1) - 65) * 10;
    let width = 20;
    let height = 10;
    const steps = [[10, 48], [24, 65], [10, 48]];
    for (let at = 2, i = 0; at < grid.length; at += 2, i += 1) {
      const [parts, first] = steps[i];
      width /= parts;
      height /= parts;
      lon += (grid.charCodeAt(at) - first) * width;
      lat += (grid.charCodeAt(at + 1) - first) * height;
    }
    return { lat: lat + height / 2, lon: lon + width / 2 };
  }

  // Where this station is: the same place the world map draws from.
  function homePlace() {
    if (home && Date.now() - home.at < HOME_KEEP_MS) return home.place;
    const params = new URLSearchParams({ limit: "1" });
    const op = window.LogLab.layout().operation;
    if (op) params.set("operation", op.id);
    const place = window.LogLab.api("GET", `/paths?${params}`).then((r) => r.home || null, () => null);
    home = { at: Date.now(), place };
    return place;
  }

  // Great-circle distance in km and the short-path heading in degrees from `a` to `b`.
  function pathBetween(a, b) {
    const rad = Math.PI / 180;
    const [p1, p2] = [a.lat * rad, b.lat * rad];
    const dl = (b.lon - a.lon) * rad;
    const h = Math.sin((p2 - p1) / 2) ** 2 + Math.cos(p1) * Math.cos(p2) * Math.sin(dl / 2) ** 2;
    const km = 2 * 6371.0088 * Math.asin(Math.min(1, Math.sqrt(h)));
    const y = Math.sin(dl) * Math.cos(p2);
    const x = Math.cos(p1) * Math.sin(p2) - Math.sin(p1) * Math.cos(p2) * Math.cos(dl);
    const heading = (Math.atan2(y, x) / rad + 360) % 360;
    return { km, heading };
  }

  let path = null; // the distance and heading shown, in km and degrees

  function showPath() {
    $("#profile-path").hidden = !path;
    if (!path) return;
    const unit = window.LogLab.layout().distance_unit === "km" ? "km" : "mi";
    const amount = unit === "km" ? path.km : path.km / KM_PER_MI;
    $("#profile-distance").textContent = `≈ ${Math.round(amount).toLocaleString()} ${unit}`;
    $("#profile-heading").textContent = `heading ${String(Math.round(path.heading) % 360).padStart(3, "0")}°`;
    const note = $("#profile-path .profile-path-note");
    note.textContent = path.gps ? "from GPS to their grid square" : "from grid squares";
    note.title = path.gps
      ? "Worked out from this station's GPS position to the centre of their grid square, not measured"
      : "Worked out from the two stations' grid squares, not measured";
  }

  async function findPath(grid, question) {
    path = null;
    showPath();
    const there = gridPlace(grid);
    if (!there) return;
    const here = await homePlace();
    if (question !== asked || !here) return;
    path = { ...pathBetween(here, there), gps: here.source === "gps" };
    showPath();
  }

  $("#profile-distance").addEventListener("click", () => {
    const unit = window.LogLab.layout().distance_unit === "km" ? "mi" : "km";
    window.LogLab.useLayout({ distance_unit: unit });
    showPath();
  });
  document.addEventListener("loglab:layout-loaded", () => { home = null; showPath(); });

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
      found = refresh ? await window.LogLab.api("GET", `/lookup?call=${encodeURIComponent(call)}&refresh=true`)
        : await window.LogLab.lookup(call);
    } catch {
      found = { status: "unreachable", message: "The Engine can't be reached right now." };
    }
    if (question !== asked) return;
    showFound(found);
    findPath(found.grid, question);
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
    path = null;
    showPath();
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

  // Show what the box should be about right now: the Call box, else the
  // selected QSO, else nothing.
  function follow() {
    clearTimeout(pauseTimer);
    const call = typed() || selected;
    show(call);
    tell({ call });
  }

  form.CALL.addEventListener("input", () => {
    clearTimeout(pauseTimer);
    const call = typed();
    if (!call) {
      follow();
      return;
    }
    pauseTimer = setTimeout(() => { show(call); tell({ call }); }, PAUSE_MS);
  });
  form.CALL.addEventListener("change", () => {
    clearTimeout(pauseTimer);
    show(typed());
    tell({ call: typed() });
  });

  // The biography and the QSOs worked before fold away under the name and
  // photo, behind a bar that says how many and opens them over the page
  // (app.js, expander).
  window.LogLab.expander({
    id: "profile",
    box: $("#profile"),
    part: $("#profile-more"),
    over: true,
    label: () => {
      const worked = $("#profile-worked").textContent.split(",")[0] || "Worked before";
      return $("#profile-bio-part").hidden ? worked : `${worked} · biography`;
    },
  });

  // A click anywhere in the biography (or Enter on it) opens it large in a
  // window of its own, to read it easily; Esc, Close or a click outside
  // closes it.
  const bioDialog = $("#bio-dialog");
  const readBio = () => {
    const name = $("#profile-name").textContent.trim();
    $("#bio-title").textContent = [$("#profile-call").textContent.trim(), name].filter(Boolean).join(" · ");
    $("#bio-dialog-text").replaceChildren(...[...$("#profile-bio").childNodes].map((node) => node.cloneNode(true)));
    bioDialog.showModal();
    bioDialog.scrollTop = 0;
  };
  $("#profile-bio").addEventListener("click", readBio);
  $("#profile-bio").addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      readBio();
    }
  });
  $("#bio-close").addEventListener("click", () => bioDialog.close());
  bioDialog.addEventListener("click", (event) => { if (event.target === bioDialog) bioDialog.close(); }); // the backdrop

  $("#profile-refresh").addEventListener("click", () => {
    if (!shown) return;
    asked += 1;
    $("#profile-source").textContent = "Asking QRZ.com…";
    window.LogLab.forgetLookups();
    lookUp(shown, asked, true);
  });

  // A stored or cleared form empties the Call box, so the box goes back to
  // the selected QSO or empties. Its list refreshes when the Log reloads.
  document.addEventListener("loglab:log-loaded", () => loadWorked());
  document.addEventListener("loglab:entry-reset", (event) => {
    follow();
    if (!event.detail?.cleared) tell({ stored: true });
  });
  document.addEventListener("loglab:qso-selected", (event) => {
    selected = String(event.detail?.call || "").trim().toUpperCase();
    if (!typed()) follow();
  });
  if (channel) {
    channel.addEventListener("message", (event) => {
      const message = event.data || {};
      if (message.stored) loadWorked();
      else if (typeof message.call === "string") show(message.call.trim().toUpperCase());
    });
  }
})();

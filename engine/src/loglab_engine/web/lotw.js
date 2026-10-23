// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Logbook of the World. The LoTW link in the Log box says how many QSOs wait
// to be sent and opens a dialog where the operator saves their callsign
// certificate, station location and LoTW website login, and fetches
// confirmations. Nothing goes to LoTW live: the operator prepares a numbered
// batch of waiting QSOs, checks it, and sends or cancels it (Principle XIII).
// The Engine does all of it (Principle I): it signs with the operator's
// certificate, talks to LoTW, keeps each batch, and records what was sent and
// confirmed on each QSO. Logging never waits for LoTW (Principle V).
"use strict";

(() => {
  const US = new Set([291, 6, 110]);
  const CANADA = 1;
  const $ = (selector, root = document) => root.querySelector(selector);
  const api = (...args) => window.LogLab.api(...args);
  const dialog = $("#lotw-dialog");
  const link = $("#lotw-open");
  const certForm = $("#lotw-cert-form");
  const locationForm = $("#lotw-location-form");
  const loginForm = $("#lotw-login-form");
  let places = null;
  let setup = null;

  function problem(form, text) { $(".error", form).textContent = text; }

  function plural(n, word) { return `${n} ${word}${n === 1 ? "" : "s"}`; }

  function showLink(state) {
    const ready = state && state.certificate && state.location;
    link.textContent = ready && state.batch ? `LoTW · batch ${state.batch.id} to check`
      : ready && state.waiting ? `LoTW · ${state.waiting} to send` : "LoTW";
  }

  const day = (iso) => (iso ? iso.slice(0, 10) : "");
  const clock = (iso) => (iso ? iso.slice(11, 16) : "");
  const stamp = (iso) => (iso ? `${day(iso)} ${clock(iso)} UTC` : "");

  function cell(text) {
    const td = document.createElement("td");
    td.textContent = text || "";
    return td;
  }

  function qsoRows(qsos) {
    return qsos.map((q) => {
      const tr = document.createElement("tr");
      tr.append(cell(day(q.qso_start)), cell(clock(q.qso_start)), cell(q.call), cell(q.band), cell(q.mode));
      return tr;
    });
  }

  function qsoTable(qsos) {
    const table = document.createElement("table");
    table.className = "lotw-qsos";
    const head = document.createElement("tr");
    for (const h of ["Date (UTC)", "Time", "Call", "Band", "Mode", "Sent"]) {
      const th = document.createElement("th");
      th.textContent = h;
      head.append(th);
    }
    const body = document.createElement("tbody");
    for (const [i, tr] of qsoRows(qsos).entries()) {
      tr.append(cell(qsos[i].sent ? "Yes" : "No"));
      body.append(tr);
    }
    table.append(head, body);
    return table;
  }

  // The batch waiting for review: its QSOs, and Send and Cancel.
  async function showBatch(batch) {
    const box = $("#lotw-batch");
    box.hidden = !batch;
    $("#lotw-prepare").hidden = Boolean(batch);
    if (!batch) return;
    const full = batch.qsos ? batch : await api("GET", `/lotw/batches/${batch.id}`);
    $("#lotw-batch-title").textContent = `Batch ${full.id}: ${plural(full.count, "QSO")} waiting for your review`;
    $("tbody", box).replaceChildren(...qsoRows(full.qsos));
    $("#lotw-send").textContent = `Send batch ${full.id} to LoTW`;
    $("#lotw-send").dataset.batch = full.id;
    $("#lotw-cancel").dataset.batch = full.id;
  }

  const STATES = { ready: "waiting for review", sent: "sent", cancelled: "cancelled" };

  async function showHistory() {
    const list = await api("GET", "/lotw/batches");
    const done = list.filter((b) => b.state !== "ready");
    $("#lotw-history-part").hidden = done.length === 0;
    $("#lotw-history").replaceChildren(...done.map((b) => {
      const row = document.createElement("details");
      row.className = "lotw-batch-row";
      const summary = document.createElement("summary");
      const when = b.state === "sent" ? `sent ${stamp(b.finished_at)}` : `${STATES[b.state]} ${stamp(b.finished_at)}`;
      summary.textContent = `Batch ${b.id} · ${when} · ${b.state === "sent" ? `${b.sent} of ${plural(b.count, "QSO")}` : plural(b.count, "QSO")}`;
      const said = document.createElement("span");
      said.className = "hint";
      said.textContent = b.answer || "";
      summary.append(said);
      row.append(summary);
      row.addEventListener("toggle", async () => {
        if (!row.open || row.dataset.loaded) return;
        row.dataset.loaded = "1";
        try {
          row.append(qsoTable((await api("GET", `/lotw/batches/${b.id}`)).qsos));
        } catch (e) {
          row.append(document.createTextNode(e.message));
        }
      });
      return row;
    }));
  }

  async function refreshLink() {
    try {
      showLink(await api("GET", "/lotw"));
    } catch {
      // The Engine is out of reach or signed out; the link keeps its last text.
    }
  }

  function option(value, text) {
    const o = document.createElement("option");
    o.value = value;
    o.textContent = text;
    return o;
  }

  function fillCounties(state, chosen) {
    const select = locationForm.us_county;
    const found = places.us_states.find((s) => s.code === state);
    select.replaceChildren(option("", "(none)"), ...(found ? found.counties.map((c) => option(c, c)) : []));
    select.value = chosen || "";
  }

  // When a state or province lies in one ITU zone and one CQ zone, fill them in.
  function suggestZones(place) {
    if (place && place.zones.length === 1) {
      [locationForm.ituz.value, locationForm.cqz.value] = place.zones[0].map(String);
    }
  }

  function showSetup(state) {
    setup = state;
    showLink(state);
    const cert = state.certificate;
    const note = $("#lotw-state");
    note.textContent = state.message;
    note.classList.toggle("warn", Boolean(state.problem));
    note.classList.toggle("done", !state.problem && Boolean(cert && state.location));
    $("#lotw-actions").hidden = !(cert && state.location);
    $("#lotw-prepare").textContent = state.waiting ? `Prepare a batch of ${plural(Math.min(state.waiting, 1000), "QSO")}` : "Prepare a batch";
    $("#lotw-prepare").disabled = !state.waiting;
    showBatch(cert && state.location ? state.batch : null).catch((e) => { $("#lotw-result").textContent = e.message; });
    $("#lotw-fetch").disabled = !state.username;
    $("#lotw-fetch").title = state.username ? "" : "Save your LoTW website login first";
    $("#lotw-remove").hidden = !(cert || state.username);
    $("#lotw-cert").textContent = cert
      ? `${cert.call}, ${cert.entity}. Signs QSOs from ${cert.qso_first}${cert.qso_last ? ` to ${cert.qso_last}` : ""}. Expires ${cert.expires.slice(0, 10)}.`
      : "No certificate saved yet.";
    $("#lotw-cert-part").open = !cert;
    $("#lotw-location-part").hidden = !cert;
    $("#lotw-location-part").open = Boolean(cert) && !state.location;
    $("#lotw-login-part").open = Boolean(cert && state.location) && !state.username;
    if (cert) fillLocation(cert.dxcc, state.location);
    if (state.username) loginForm.username.value = state.username;
  }

  function fillLocation(dxcc, location) {
    const us = US.has(dxcc);
    const ca = dxcc === CANADA;
    for (const el of dialog.querySelectorAll(".us-only")) el.hidden = !us;
    for (const el of dialog.querySelectorAll(".ca-only")) el.hidden = !ca;
    const only = { 6: "AK", 110: "HI" }[dxcc]; // Alaska and Hawaii are entities of their own
    const states = places.us_states.filter((s) => us && (only ? s.code === only : !["AK", "HI"].includes(s.code)));
    locationForm.us_state.replaceChildren(option("", "Choose…"), ...states.map((s) => option(s.code, s.name)));
    locationForm.ca_province.replaceChildren(option("", "Choose…"), ...places.ca_provinces.map((p) => option(p.code, p.name)));
    const loc = location || {};
    locationForm.grid.value = loc.grid || "";
    locationForm.us_state.value = loc.us_state || "";
    fillCounties(loc.us_state, loc.us_county);
    locationForm.ca_province.value = loc.ca_province || "";
    locationForm.ituz.value = loc.ituz || "";
    locationForm.cqz.value = loc.cqz || "";
  }

  async function open() {
    for (const form of [certForm, locationForm, loginForm]) problem(form, "");
    certForm.reset();
    loginForm.password.value = "";
    $("#lotw-result").textContent = "";
    $("#lotw-skipped").replaceChildren();
    dialog.showModal();
    try {
      if (!places) places = await api("GET", "/lotw/places");
      showSetup(await api("GET", "/lotw"));
      await showHistory();
    } catch (e) {
      $("#lotw-state").textContent = e.message;
    }
  }

  function readFile(file) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result).split(",", 2)[1] || "");
      reader.onerror = () => reject(new Error("The file could not be read."));
      reader.readAsDataURL(file);
    });
  }

  async function save(form, path, body) {
    problem(form, "");
    try {
      showSetup(await api("PUT", path, await body()));
      return true;
    } catch (e) {
      problem(form, e.message);
      return false;
    }
  }

  certForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    const file = certForm.file.files[0];
    if (!file) return;
    if (await save(certForm, "/lotw/certificate", async () => ({
      p12_base64: await readFile(file), password: certForm.password.value,
    }))) certForm.reset();
  });

  locationForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    const us = !locationForm.us_state.closest("label").hidden;
    const ca = !locationForm.ca_province.closest("label").hidden;
    await save(locationForm, "/lotw/location", async () => ({
      grid: locationForm.grid.value.trim(),
      ituz: Number(locationForm.ituz.value),
      cqz: Number(locationForm.cqz.value),
      us_state: us ? locationForm.us_state.value || null : null,
      us_county: us ? locationForm.us_county.value || null : null,
      ca_province: ca ? locationForm.ca_province.value || null : null,
    }));
  });

  locationForm.us_state.addEventListener("change", () => {
    const code = locationForm.us_state.value;
    fillCounties(code, "");
    suggestZones(places.us_states.find((s) => s.code === code));
  });
  locationForm.ca_province.addEventListener("change", () => {
    suggestZones(places.ca_provinces.find((p) => p.code === locationForm.ca_province.value));
  });

  loginForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (await save(loginForm, "/lotw/login", async () => ({
      username: loginForm.username.value.trim(), password: loginForm.password.value,
    }))) loginForm.password.value = "";
  });

  function showSkipped(skipped, list) {
    for (const s of skipped) {
      const item = document.createElement("li");
      const call = document.createElement("b");
      call.textContent = s.call || "(no call)";
      item.append(call, ` ${s.reason}`);
      list.append(item);
    }
  }

  async function act(button, path, show) {
    const result = $("#lotw-result");
    const list = $("#lotw-skipped");
    button.disabled = true;
    result.textContent = "Working…";
    result.classList.remove("error");
    list.replaceChildren();
    try {
      const done = await api("POST", path);
      result.textContent = done.message || "";
      result.classList.toggle("error", Boolean(done.status) && !["done", "nothing"].includes(done.status));
      if (done.skipped) showSkipped(done.skipped, list);
      if (show) show(done);
      document.dispatchEvent(new CustomEvent("loglab:log-changed"));
      showSetup(await api("GET", "/lotw"));
      await showHistory();
    } catch (e) {
      result.textContent = e.message;
      result.classList.add("error");
    } finally {
      button.disabled = false;
    }
  }

  $("#lotw-prepare").addEventListener("click", (event) => act(event.target, "/lotw/batches"));
  $("#lotw-send").addEventListener("click", (event) => {
    const id = event.target.dataset.batch;
    if (!window.confirm(`Send batch ${id} to LoTW now? LoTW keeps what it receives.`)) return;
    act(event.target, `/lotw/batches/${id}/send`);
  });
  $("#lotw-cancel").addEventListener("click", (event) => act(event.target, `/lotw/batches/${event.target.dataset.batch}/cancel`, () => {
    $("#lotw-result").textContent = "Batch cancelled. Nothing was sent; its QSOs are waiting again.";
  }));
  $("#lotw-fetch").addEventListener("click", (event) => act(event.target, "/lotw/download"));

  $("#lotw-remove").addEventListener("click", async () => {
    if (!window.confirm("Remove your LoTW certificate, location and login from LabLog? Your QSOs keep what they say about LoTW.")) return;
    try {
      await api("DELETE", "/lotw");
      showSetup(await api("GET", "/lotw"));
      certForm.reset();
      loginForm.reset();
    } catch (e) {
      $("#lotw-state").textContent = e.message;
    }
  });

  link.addEventListener("click", open);
  $("#lotw-close").addEventListener("click", () => dialog.close());
  // The log loads after signing in and after each QSO is stored: a good moment to recount.
  document.addEventListener("loglab:log-loaded", refreshLink);
})();

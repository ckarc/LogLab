// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Logbook of the World. The LoTW link in the Log box says how many QSOs wait
// to be sent and opens a dialog where the operator saves their callsign
// certificate, station location and LoTW website login, sends waiting QSOs,
// and fetches confirmations. The Engine does all of it (Principle I): it signs
// with the operator's certificate, talks to LoTW, and records what was sent
// and confirmed on each QSO. Logging never waits for LoTW (Principle V).
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
    link.textContent = state && state.certificate && state.location && state.waiting
      ? `LoTW · ${state.waiting} to send` : "LoTW";
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
    $("#lotw-actions").hidden = !(cert && state.location);
    $("#lotw-send").textContent = state.waiting ? `Send ${plural(state.waiting, "QSO")} to LoTW` : "Send to LoTW";
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

  async function act(button, path, show) {
    const result = $("#lotw-result");
    const list = $("#lotw-skipped");
    button.disabled = true;
    result.textContent = "Working…";
    list.replaceChildren();
    try {
      const done = await api("POST", path);
      result.textContent = done.message;
      result.classList.toggle("error", !["done", "nothing"].includes(done.status));
      show(done, list);
      document.dispatchEvent(new CustomEvent("loglab:log-changed"));
      showSetup(await api("GET", "/lotw"));
    } catch (e) {
      result.textContent = e.message;
      result.classList.add("error");
    } finally {
      button.disabled = false;
    }
  }

  $("#lotw-send").addEventListener("click", (event) => act(event.target, "/lotw/upload", (done, list) => {
    for (const s of done.skipped) {
      const item = document.createElement("li");
      const call = document.createElement("b");
      call.textContent = s.call || "(no call)";
      item.append(call, ` ${s.reason}`);
      list.append(item);
    }
  }));
  $("#lotw-fetch").addEventListener("click", (event) => act(event.target, "/lotw/download", () => {}));

  $("#lotw-remove").addEventListener("click", async () => {
    if (!window.confirm("Remove your LoTW certificate, location and login from LogLab? Your QSOs keep what they say about LoTW.")) return;
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

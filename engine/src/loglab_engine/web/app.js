// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// LogLab's default web client. It uses only the Engine's published API
// (Principle I) and loads nothing from the internet (Principle V). The Engine
// is the authority: after every change this page shows what the Engine
// returned, not what it sent (Principle II). Every time is UTC (Principle VIII).
"use strict";

(() => {
  const SESSION_KEY = "loglab.session";
  // ADIF 3.1.7 band and mode enumerations most used on the air.
  const BANDS = ["160m", "80m", "60m", "40m", "30m", "20m", "17m", "15m", "12m", "10m", "6m", "2m", "1.25m", "70cm"];
  const MODES = ["SSB", "CW", "FT8", "FT4", "FM", "AM", "RTTY", "PSK", "DIGITALVOICE", "JS8", "OLIVIA", "SSTV"];
  // FT4 is a submode of MFSK in ADIF; the entry form writes it correctly.
  const SUBMODE_OF = { FT4: "MFSK", JS8: "MFSK" };

  const $ = (selector, root = document) => root.querySelector(selector);
  let session = null;
  let current = null; // the QSO open in the detail panel

  // -- API ---------------------------------------------------------------

  class ApiError extends Error {
    constructor(status, message) {
      super(message);
      this.status = status;
    }
  }

  async function api(method, path, body) {
    const headers = { Accept: "application/json" };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (session) headers.Authorization = `Bearer ${session.token}`;
    let response;
    try {
      response = await fetch(`/api/v1${path}`, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch {
      throw new ApiError(0, "The Engine cannot be reached. Nothing was changed.");
    }
    if (response.status === 401 && session) {
      forget();
      showSignIn("Your session has ended. Sign in again.");
      throw new ApiError(401, "Signed out");
    }
    if (response.status === 204) return null;
    const data = await response.json().catch(() => null);
    if (!response.ok) throw new ApiError(response.status, describe(data) || response.statusText);
    return data;
  }

  function describe(data) {
    if (!data || data.detail === undefined) return "";
    if (typeof data.detail === "string") return data.detail;
    if (Array.isArray(data.detail)) {
      return data.detail.map((d) => `${(d.loc || []).slice(1).join(".")}: ${d.msg}`).join("; ");
    }
    return JSON.stringify(data.detail);
  }

  // -- Session -----------------------------------------------------------

  function remember(value) {
    session = value;
    try { localStorage.setItem(SESSION_KEY, JSON.stringify(value)); } catch { /* private mode */ }
  }

  function forget() {
    session = null;
    try { localStorage.removeItem(SESSION_KEY); } catch { /* private mode */ }
  }

  function restore() {
    try {
      const saved = JSON.parse(localStorage.getItem(SESSION_KEY) || "null");
      if (saved && typeof saved.token === "string") session = saved;
    } catch { session = null; }
  }

  function showSignIn(message = "") {
    $("#app").hidden = true;
    $("#who").hidden = true;
    $("#sign-out").hidden = true;
    $("#sign-in").hidden = false;
    $("#sign-in-form .error").textContent = message;
    const station = $("#sign-in-form [name=station]");
    if (!station.value) station.value = localStorage.getItem("loglab.station") || "";
    $("#sign-in-form [name=callsign]").focus();
  }

  function showApp() {
    $("#sign-in").hidden = true;
    $("#app").hidden = false;
    $("#who").hidden = false;
    $("#sign-out").hidden = false;
    $("#who").textContent = `${session.operator} at ${session.station}`;
    const mayLog = session.permissions.includes("log");
    for (const el of document.querySelectorAll("#entry button, #edit button[type=submit], #delete, #restore, #add-field")) {
      el.disabled = !mayLog;
    }
    if (!mayLog) setError("#entry", "You can view the log but not change it (no log permission).");
    resetEntry();
    loadLog();
  }

  async function signIn(event) {
    event.preventDefault();
    const form = event.target;
    const error = $(".error", form);
    error.textContent = "";
    const body = {
      callsign: form.callsign.value.trim().toUpperCase(),
      secret: form.secret.value,
      station: form.station.value.trim(),
    };
    try {
      remember(await api("POST", "/sessions", body));
      try { localStorage.setItem("loglab.station", body.station); } catch { /* private mode */ }
      form.secret.value = "";
      showApp();
    } catch (e) {
      error.textContent = e.message;
      form.secret.select();
    }
  }

  async function signOut() {
    try { await api("DELETE", "/sessions/current"); } catch { /* already ended */ }
    forget();
    closeDetail();
    showSignIn("Signed out.");
  }

  // -- Entering a QSO ----------------------------------------------------

  function utcNow() {
    const now = new Date();
    return {
      date: now.toISOString().slice(0, 10),
      time: now.toISOString().slice(11, 16).replace(":", ""),
    };
  }

  function fillSelect(select, values) {
    for (const value of values) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = value;
      select.append(option);
    }
  }

  function resetEntry() {
    const form = $("#entry");
    const { date, time } = utcNow();
    for (const name of ["CALL", "FREQ", "RST_SENT", "RST_RCVD", "NAME", "COMMENT"]) form[name].value = "";
    form.QSO_DATE.value = date;
    form.TIME_ON.value = time;
    form.CALL.focus();
  }

  function entryFields(form) {
    const fields = [];
    const add = (name, value) => {
      const text = value.trim();
      if (text) fields.push({ name, value: text, type: null });
    };
    add("CALL", form.CALL.value.toUpperCase());
    add("QSO_DATE", form.QSO_DATE.value.replaceAll("-", ""));
    add("TIME_ON", form.TIME_ON.value.replace(":", ""));
    add("BAND", form.BAND.value.toLowerCase());
    const mode = form.MODE.value;
    if (SUBMODE_OF[mode]) {
      add("MODE", SUBMODE_OF[mode]);
      add("SUBMODE", mode);
    } else {
      add("MODE", mode);
    }
    add("FREQ", form.FREQ.value);
    add("RST_SENT", form.RST_SENT.value);
    add("RST_RCVD", form.RST_RCVD.value);
    add("NAME", form.NAME.value);
    add("COMMENT", form.COMMENT.value);
    return fields;
  }

  async function storeQso(event) {
    event.preventDefault();
    const form = event.target;
    setError("#entry", "");
    if (form.FREQ.value.trim() && !/^\d+(\.\d+)?$/.test(form.FREQ.value.trim())) {
      setError("#entry", "Frequency is in MHz, like 14.205");
      return;
    }
    const button = $("button[type=submit]", form);
    button.disabled = true;
    try {
      const result = await api("POST", "/qsos", { fields: entryFields(form) });
      const qso = result.qso;
      const notice = $("#stored");
      notice.textContent = `${qso.call || "QSO"} stored by ${qso.operator} at ${qso.station}. ${result.message}`;
      notice.hidden = false;
      resetEntry();
      await loadLog();
    } catch (e) {
      setError("#entry", e.message);
    } finally {
      button.disabled = !session || !session.permissions.includes("log");
    }
  }

  function setError(selector, message) {
    $(`${selector} .error`).textContent = message;
  }

  // -- The log -----------------------------------------------------------

  function field(qso, name) {
    const found = qso.fields.find((f) => f.name.toUpperCase() === name);
    return found ? found.value : "";
  }

  function utcDate(text) {
    return text ? text.slice(0, 10) : "";
  }

  function utcTime(text) {
    return text ? text.slice(11, 16) : "";
  }

  function cell(row, text, className) {
    const td = document.createElement("td");
    td.textContent = text;
    if (className) td.className = className;
    row.append(td);
  }

  let searchTimer = null;

  async function loadLog() {
    const params = new URLSearchParams({ limit: "100" });
    const call = $("#search").value.trim();
    if (call) params.set("call", call);
    if ($("#show-deleted").checked) params.set("include_deleted", "true");
    let qsos;
    try {
      qsos = await api("GET", `/qsos?${params}`);
    } catch (e) {
      $("#count").textContent = e.message;
      return;
    }
    const body = $("#log tbody");
    body.replaceChildren();
    for (const qso of qsos) {
      const row = document.createElement("tr");
      if (qso.deleted) row.className = "deleted";
      cell(row, utcDate(qso.qso_start) || field(qso, "QSO_DATE"));
      cell(row, utcTime(qso.qso_start) || field(qso, "TIME_ON"));
      cell(row, qso.call || "(no call)", "call");
      cell(row, qso.band || "");
      cell(row, field(qso, "SUBMODE") || qso.mode || "");
      cell(row, qso.operator);
      cell(row, String(qso.version));
      row.addEventListener("click", () => openDetail(qso.id));
      body.append(row);
    }
    $("#count").textContent = qsos.length === 100 ? "latest 100 shown" : `${qsos.length} shown`;
  }

  // -- One QSO: fields, edit, delete, restore, history -------------------

  function fieldRow(f) {
    const row = document.createElement("tr");
    const name = document.createElement("input");
    name.value = f.name;
    name.maxLength = 64;
    name.dataset.type = f.type || "";
    name.setAttribute("aria-label", "Field name");
    const value = document.createElement("input");
    value.value = f.value;
    value.setAttribute("aria-label", "Value");
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "link";
    remove.textContent = "Remove";
    remove.addEventListener("click", () => row.remove());
    for (const el of [name, value, remove]) {
      const td = document.createElement("td");
      td.append(el);
      row.append(td);
    }
    return row;
  }

  async function openDetail(id) {
    let qso;
    let history;
    try {
      [qso, history] = await Promise.all([api("GET", `/qsos/${id}`), api("GET", `/qsos/${id}/history`)]);
    } catch (e) {
      $("#count").textContent = e.message;
      return;
    }
    current = qso;
    $("#detail").hidden = false;
    $("#detail-title").textContent = `${qso.call || "QSO"}${qso.deleted ? " (deleted)" : ""}`;
    $("#detail-meta").textContent =
      `ID ${qso.id} · version ${qso.version} · last changed ${qso.changed_at.slice(0, 16).replace("T", " ")} UTC ` +
      `by ${qso.operator} at ${qso.station}`;
    const rows = $("#edit tbody");
    rows.replaceChildren(...qso.fields.map(fieldRow));
    $("#edit-note").value = "";
    setError("#edit", "");
    $("#delete").hidden = qso.deleted;
    $("#restore").hidden = !qso.deleted;
    $("#edit button[type=submit]").hidden = qso.deleted;
    $("#add-field").hidden = qso.deleted;
    renderHistory(history);
    $("#detail").scrollIntoView({ behavior: "smooth", block: "start" });
  }

  function closeDetail() {
    current = null;
    $("#detail").hidden = true;
  }

  function show(f) {
    return `<${f.name}${f.type ? `:${f.type}` : ""}> '${f.value}'`;
  }

  function renderHistory(versions) {
    const list = $("#history");
    list.replaceChildren();
    let previous = null;
    for (const v of versions) {
      const item = document.createElement("li");
      const what = document.createElement("div");
      const when = v.changed_at.slice(0, 19).replace("T", " ");
      const label = document.createElement("span");
      label.className = "what";
      label.textContent = `v${v.version} ${v.action}`;
      what.append(label, ` by ${v.operator} at ${v.station}, ${when} UTC`);
      if (v.import_file) what.append(` (import ${v.import_id}: ${v.import_file})`);
      if (v.note) what.append(` — “${v.note}”`);
      item.append(what);
      const changes = document.createElement("ul");
      if (previous === null) {
        const li = document.createElement("li");
        li.textContent = `${v.fields.length} fields as first entered`;
        changes.append(li);
      } else {
        const before = previous.fields.map(show);
        const after = v.fields.map(show);
        const removed = before.filter((x) => !after.includes(x));
        const added = after.filter((x) => !before.includes(x));
        for (const x of removed) {
          const li = document.createElement("li");
          li.className = "removed";
          li.textContent = `− ${x}`;
          changes.append(li);
        }
        for (const x of added) {
          const li = document.createElement("li");
          li.className = "added";
          li.textContent = `+ ${x}`;
          changes.append(li);
        }
        if (!removed.length && !added.length) {
          const li = document.createElement("li");
          li.textContent = { delete: "marked deleted; fields kept", restore: "brought back; fields kept" }[v.action] || "fields unchanged";
          changes.append(li);
        }
      }
      item.append(changes);
      list.append(item);
      previous = v;
    }
  }

  function editedFields() {
    const fields = [];
    for (const row of $("#edit tbody").rows) {
      const [name, value] = row.querySelectorAll("input");
      if (!name.value.trim()) continue;
      fields.push({ name: name.value.trim(), value: value.value, type: name.dataset.type || null });
    }
    return fields;
  }

  async function change(method, path, body) {
    setError("#edit", "");
    try {
      const result = await api(method, path, body);
      await loadLog();
      await openDetail(result.qso.id);
    } catch (e) {
      setError("#edit", e.status === 409 ? `${e.message} The latest version is shown now.` : e.message);
      if (e.status === 409 && current) await openDetail(current.id);
    }
  }

  function note() {
    const text = $("#edit-note").value.trim();
    return text || null;
  }

  // -- Start -------------------------------------------------------------

  async function start() {
    fillSelect($("#entry [name=BAND]"), BANDS);
    fillSelect($("#entry [name=MODE]"), MODES);
    $("#entry [name=BAND]").value = "20m";

    $("#sign-in-form").addEventListener("submit", signIn);
    $("#sign-out").addEventListener("click", signOut);
    $("#entry").addEventListener("submit", storeQso);
    $("#now").addEventListener("click", () => {
      const { date, time } = utcNow();
      $("#entry [name=QSO_DATE]").value = date;
      $("#entry [name=TIME_ON]").value = time;
    });
    $("#search").addEventListener("input", () => {
      clearTimeout(searchTimer);
      searchTimer = setTimeout(loadLog, 250);
    });
    $("#show-deleted").addEventListener("change", loadLog);
    $("#close-detail").addEventListener("click", closeDetail);
    $("#add-field").addEventListener("click", () => {
      $("#edit tbody").append(fieldRow({ name: "", value: "", type: null }));
    });
    $("#edit").addEventListener("submit", (event) => {
      event.preventDefault();
      if (!current) return;
      change("PUT", `/qsos/${current.id}`, {
        base_version: current.version,
        fields: editedFields(),
        note: note(),
      });
    });
    $("#delete").addEventListener("click", () => {
      if (current && confirm("Delete this QSO? It is kept in the history and can be restored.")) {
        change("POST", `/qsos/${current.id}/delete`, { note: note() });
      }
    });
    $("#restore").addEventListener("click", () => {
      if (current) change("POST", `/qsos/${current.id}/restore`, { note: note() });
    });

    try {
      const info = await api("GET", "/status");
      $("#version").textContent = `${info.name} ${info.version}`;
      $("#source").href = info.source;
    } catch { /* shown when the first real request fails */ }

    restore();
    if (!session) {
      showSignIn();
      return;
    }
    try {
      const me = await api("GET", "/sessions/current");
      session = { ...session, permissions: me.permissions };
      showApp();
    } catch {
      if (!$("#sign-in").hidden) return;
      showSignIn();
    }
  }

  start();
})();

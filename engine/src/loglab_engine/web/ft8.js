// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The FT8 box: what this station's Agent hears on FT8, live. A waterfall
// scrolls as the audio arrives, and every 15 seconds the stations decoded in
// the slot just ended join the list. Click one and New QSO fills in with its
// call, grid, the report to send, band, frequency and mode.
//
// Every decode is labelled with how it was obtained (Principle XX): today
// each one is received, straight from the signal. Decodes from the Agent's
// pretend FT8 band are made-up stations: they are marked pretend, and a QSO
// filled in from one carries APP_LOGLAB_PRETEND, so the Engine only lets it
// into a practice operation. This box only listens: nothing here transmits.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const box = $("#ft8");
  const form = $("#entry");
  const canvas = $("#ft8-waterfall");
  const POLL_MS = 1000;
  const IDLE_MS = 3000;
  const SHOWN_ROWS = 120; // one minute of waterfall
  const SHOWN_SLOTS = 4; // one minute of decodes
  const COLUMNS = {
    time: { label: "UTC", sort: "qso_start", render: (d) => d.time },
    "field:DB": { label: "dB", sort: "field:DB", render: (d) => signed(d.snr_db), className: "num" },
    "field:DT": { label: "DT", sort: "field:DT", render: (d) => d.dt_s.toFixed(1), className: "num" },
    "field:AUDIO": { label: "Freq (Hz)", sort: "field:AUDIO", render: (d) => String(d.base_hz), className: "num" },
    "field:MESSAGE": { label: "Message", sort: "field:MESSAGE", render: (d) => message(d.message), className: "ft8-message" },
    call: { label: "From", sort: "call", render: (d) => (d.de ? LogLab.callsign(d.de) : ""), className: "call" },
    "field:GRIDSQUARE": { label: "Grid", sort: "field:GRIDSQUARE", render: (d) => d.grid || "" },
    "field:TO": { label: "To", sort: "field:TO", render: (d) => (d.to ? LogLab.callsign(d.to) : d.cq !== null ? "CQ" : ""), className: "call" },
    "field:HEARD": { label: "Heard", sort: "field:HEARD", render: heardBadge },
    "field:WORKED": { label: "Worked", sort: "field:WORKED", render: (d) => (d.worked ? "✓" : "") },
  };
  const VALUE = {
    qso_start: (d) => d.slot,
    "field:DB": (d) => d.snr_db,
    "field:DT": (d) => d.dt_s,
    "field:AUDIO": (d) => d.base_hz,
    "field:MESSAGE": (d) => d.message,
    call: (d) => d.de || "",
    "field:GRIDSQUARE": (d) => d.grid || "",
    "field:TO": (d) => d.to || "",
    "field:HEARD": (d) => d.kind,
    "field:WORKED": (d) => Number(d.worked),
  };
  const table = LogLab.table({
    id: "ft8",
    table: $("#ft8-list"),
    toolbar: $("#ft8-tools"),
    column: (key) => COLUMNS[key] || { label: key.replace(/^field:/, ""), sort: key, render: () => "" },
    suggested: Object.keys(COLUMNS),
    defaults: Object.freeze({
      columns: ["time", "field:DB", "field:DT", "field:AUDIO", "field:MESSAGE", "field:GRIDSQUARE", "field:HEARD", "field:WORKED"],
      sort: "entered",
      descending: true,
    }),
    entered: ["newest heard first", "oldest heard first"],
    load: () => showList(),
  });

  let seq = 0;
  let asking = false; // the operation (or null) the last answer's worked marks are for
  let state = null; // the latest answer: listening or not, source, dial
  let rows = []; // waterfall rows, oldest first
  let slots = []; // decoded slots, oldest first
  let marker = null; // the audio frequency of the decode under the pointer

  const signed = (n) => (n >= 0 ? `+${String(n).padStart(2, "0")}` : `-${String(-n).padStart(2, "0")}`);
  const mhz = (hz) => (hz / 1e6).toFixed(6).replace(/0+$/, "").replace(/\.$/, "");

  function message(text) {
    // Calls in the message get slashed zeros too, so N0OY never reads NOOY.
    const out = document.createDocumentFragment();
    text.split(" ").forEach((word, i) => {
      if (i) out.append(" ");
      const call = /\d/.test(word) && /[A-Z]/.test(word) && word.length > 3 && !/^[A-R]{2}\d{2}$/.test(word);
      out.append(call ? LogLab.callsign(word) : word);
    });
    return out;
  }

  function heardBadge(d) {
    const badge = document.createElement("span");
    badge.className = `ft8-kind ${d.kind}${d.pretend ? " pretend" : ""}`;
    badge.textContent = d.pretend ? `${d.kind}, pretend` : d.kind;
    badge.title = d.pretend
      ? "Decoded from the pretend FT8 band: a made-up station, not a real one"
      : d.kind === "received" ? "Decoded from the signal itself" : `Not all of it was received: ${d.kind}`;
    return badge;
  }

  // -- Waterfall ----------------------------------------------------------

  // Dark blue static, through cyan and yellow, to white for the strongest.
  const PALETTE = (() => {
    const stops = [[0, [8, 12, 40]], [70, [20, 50, 140]], [130, [0, 170, 210]], [190, [250, 220, 60]], [255, [255, 255, 255]]];
    const out = new Uint8ClampedArray(256 * 3);
    for (let v = 0; v < 256; v++) {
      const hi = stops.findIndex(([at]) => at >= v);
      const [a, ca] = stops[Math.max(hi - 1, 0)];
      const [b, cb] = stops[hi];
      const t = b === a ? 0 : (v - a) / (b - a);
      for (let k = 0; k < 3; k++) out[v * 3 + k] = ca[k] + (cb[k] - ca[k]) * t;
    }
    return out;
  })();

  // The rows are painted one pixel each, then stretched to the canvas's
  // real size, so the time labels and markers stay sharp.
  const painted = document.createElement("canvas");

  function drawWaterfall() {
    const columns = state?.columns || 480;
    painted.width = columns;
    painted.height = SHOWN_ROWS;
    const paint = painted.getContext("2d");
    const image = paint.createImageData(columns, SHOWN_ROWS);
    image.data.fill(255);
    const shown = rows.slice(-SHOWN_ROWS);
    // Newest at the top, scrolling down.
    shown.forEach((row, n) => {
      const y = shown.length - 1 - n;
      for (let x = 0; x < columns; x++) {
        const level = parseInt(row.levels.substr(x * 2, 2), 16) || 0;
        const at = (y * columns + x) * 4;
        image.data[at] = PALETTE[level * 3];
        image.data[at + 1] = PALETTE[level * 3 + 1];
        image.data[at + 2] = PALETTE[level * 3 + 2];
      }
    });
    for (let y = shown.length; y < SHOWN_ROWS; y++) {
      for (let x = 0; x < columns; x++) {
        const at = (y * columns + x) * 4;
        image.data[at] = PALETTE[0]; image.data[at + 1] = PALETTE[1]; image.data[at + 2] = PALETTE[2];
      }
    }
    paint.putImageData(image, 0, 0);
    const ratio = window.devicePixelRatio || 1;
    const width = Math.max(Math.round(canvas.clientWidth * ratio), 1);
    const height = Math.max(Math.round(canvas.clientHeight * ratio), 1);
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
    const ctx = canvas.getContext("2d");
    ctx.imageSmoothingEnabled = false;
    ctx.drawImage(painted, 0, 0, width, height);
    const rowHeight = height / SHOWN_ROWS;
    // A faint line and the UTC time where each slot begins.
    ctx.font = `${Math.round(11 * ratio)}px system-ui, sans-serif`;
    shown.forEach((row, n) => {
      if (row.row !== 0) return;
      const y = (shown.length - n) * rowHeight;
      ctx.fillStyle = "rgba(255,255,255,0.35)";
      ctx.fillRect(0, Math.round(y), width, Math.max(1, ratio));
      ctx.fillStyle = "rgba(255,255,255,0.85)";
      ctx.fillText(slotTime(row.slot), 6 * ratio, Math.max(y - 4 * ratio, 12 * ratio));
    });
    if (marker !== null) {
      const hzWidth = columns * (state?.column_hz || 6.25);
      ctx.fillStyle = "rgba(255,80,110,0.9)";
      for (const hz of [marker, marker + 50]) ctx.fillRect(Math.round((hz / hzWidth) * width), 0, Math.max(1, ratio), height);
    }
  }

  function drawScale() {
    const scale = $("#ft8-scale");
    const width = (state?.columns || 480) * (state?.column_hz || 6.25);
    scale.replaceChildren(...[500, 1000, 1500, 2000, 2500].map((hz) => {
      const tick = document.createElement("span");
      tick.style.left = `${(hz / width) * 100}%`;
      tick.textContent = hz;
      return tick;
    }));
  }

  const slotTime = (slot) => new Date(slot * 15000).toISOString().slice(11, 19);

  // -- Decodes ------------------------------------------------------------

  function decodes() {
    const order = table.state();
    const all = [];
    slots.slice(-SHOWN_SLOTS).forEach((s, i) => {
      for (const [j, d] of s.decodes.entries()) all.push({ ...d, slot: s.slot, time: slotTime(s.slot), order: i * 1000 + j });
    });
    const value = VALUE[order.sort];
    const sorted = value
      ? all.sort((a, b) => {
        const x = value(a); const y = value(b);
        return (x < y ? -1 : x > y ? 1 : a.order - b.order) * (order.descending ? -1 : 1);
      })
      : all.sort((a, b) => (order.descending ? b.order - a.order : a.order - b.order));
    return sorted;
  }

  function myCalls() {
    const calls = new Set();
    const session = LogLab.session();
    if (session?.operator) calls.add(session.operator.toUpperCase());
    const chosen = LogLab.layout()?.operation;
    if (chosen?.station_call) calls.add(chosen.station_call.toUpperCase());
    return calls;
  }

  function showList() {
    if (!state?.listening) return;
    const mine = myCalls();
    const shown = decodes();
    table.render(shown, (tr, d) => {
      tr.classList.toggle("ft8-cq", d.cq !== null);
      tr.classList.toggle("ft8-mine", Boolean(d.to && mine.has(d.to)));
      tr.classList.toggle("ft8-worked", d.worked);
      if (d.de) {
        tr.tabIndex = 0;
        tr.title = `Fill in New QSO with ${d.de}`;
        tr.addEventListener("click", () => fill(d));
        tr.addEventListener("keydown", (e) => { if (e.key === "Enter") fill(d); });
      }
      tr.addEventListener("pointerenter", () => { marker = d.base_hz; drawWaterfall(); });
      tr.addEventListener("pointerleave", () => { marker = null; drawWaterfall(); });
    });
    const count = slots.length ? slots[slots.length - 1].decodes.length : 0;
    $("#ft8-count").textContent = `${count} heard in the last slot · ${table.describeSort()}`;
  }

  // -- Filling in New QSO -------------------------------------------------

  function put(name, value) {
    const input = form[name];
    if (!input || value === null || value === undefined) return;
    if (input.tagName === "SELECT" && ![...input.options].some((o) => o.value === value)) {
      input.append(Object.assign(document.createElement("option"), { value, textContent: value }));
    }
    input.value = value;
    // The radio stops filling a box once it changes, the lookup, the dupe
    // check and On air hear of it.
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }

  // The latest grid each station gave, from everything kept.
  function gridOf(call) {
    for (let i = slots.length - 1; i >= 0; i--) {
      const found = slots[i].decodes.find((d) => d.de === call && d.grid);
      if (found) return found.grid;
    }
    return null;
  }

  // The latest report a station gave one of our calls, if it gave one.
  function reportFrom(call) {
    const mine = myCalls();
    for (let i = slots.length - 1; i >= 0; i--) {
      const found = slots[i].decodes.find((d) => d.de === call && d.report && mine.has(d.to));
      if (found) return found.report;
    }
    return null;
  }

  function fill(d) {
    put("CALL", d.de);
    put("MODE", "FT8");
    if (state.band) put("BAND", state.band);
    put("FREQ", mhz(d.freq_hz));
    put("RST_SENT", signed(d.snr_db));
    const report = reportFrom(d.de);
    if (report) put("RST_RCVD", report);
    const grid = gridOf(d.de);
    if (grid) put("GRIDSQUARE", grid);
    markPretend(d.pretend);
    form.CALL.focus();
  }

  function markPretend(pretend) {
    form.APP_LOGLAB_PRETEND.value = pretend ? "Y" : "";
    $("#entry-pretend").hidden = !pretend;
  }

  // Typing another call by hand means a different contact.
  form.CALL.addEventListener("input", (e) => { if (e.isTrusted) markPretend(false); });
  document.addEventListener("loglab:entry-reset", () => {
    markPretend(false);
    seq = 0; // a QSO was stored: fetch everything again for the worked marks
  });

  // -- Listening ----------------------------------------------------------

  function show() {
    const listening = Boolean(state?.listening);
    $("#ft8-none").hidden = listening;
    $("#ft8-body").hidden = !listening;
    $("#ft8-pretend").hidden = !(listening && state.source === "pretend");
    $("#ft8-scope").textContent = listening && state.dial_hz
      ? `${mhz(state.dial_hz)} MHz${state.band ? ` · ${state.band}` : ""}` : "";
  }

  async function poll() {
    const session = LogLab.session();
    if (!session?.token || !session.permissions?.includes("view") || box.offsetParent === null) return IDLE_MS;
    const operation = LogLab.layout()?.operation?.id || null;
    if (operation !== asking) seq = 0;
    const params = new URLSearchParams({ after: String(seq) });
    if (operation) params.set("operation", operation);
    let answer;
    try {
      answer = await LogLab.api("GET", `/ft8?${params}`);
      $("#ft8-error").textContent = "";
    } catch (e) {
      $("#ft8-error").textContent = e.message;
      return IDLE_MS;
    }
    const fresh = seq === 0;
    asking = operation;
    const before = state;
    state = answer;
    if (!answer.listening) {
      rows = []; slots = []; seq = 0;
      show();
      return IDLE_MS;
    }
    if (fresh || before?.dial_hz !== answer.dial_hz || answer.seq < seq) {
      rows = []; slots = [];
      drawScale();
    }
    seq = answer.seq;
    rows = rows.concat(answer.rows).slice(-SHOWN_ROWS);
    if (answer.slots.length) slots = slots.concat(answer.slots).slice(-SHOWN_SLOTS * 2);
    show();
    if (answer.rows.length || fresh) drawWaterfall();
    if (answer.slots.length || fresh) showList();
    return POLL_MS;
  }

  async function keepListening() {
    for (;;) {
      const wait = await poll().catch(() => IDLE_MS);
      await new Promise((resolve) => { setTimeout(resolve, wait); });
    }
  }

  new ResizeObserver(() => drawWaterfall()).observe(canvas);
  drawScale();
  drawWaterfall();
  keepListening();
})();

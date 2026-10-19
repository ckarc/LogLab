// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The FT8 waterfall and FT8 boxes: what this station's Agent hears on FT8,
// live. Each is a box of its own, shown, sized and moved on its own. The
// waterfall scrolls as the audio arrives, and every 15 seconds the stations
// decoded in the slot just ended join the FT8 box's list. Click one and New
// QSO fills in with its call, grid, the report to send, band, frequency and
// mode. Pointing at a station marks it on the waterfall.
//
// Every decode is labelled with how it was obtained (Principle XX): today
// each one is received, straight from the signal. Decodes from the Agent's
// pretend FT8 band are made-up stations: they are marked pretend, and a QSO
// filled in from one carries APP_LOGLAB_PRETEND, so the Engine only lets it
// into a practice operation. These boxes only listen: nothing here transmits.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const boxes = [$("#waterfall"), $("#ft8")];
  const form = $("#entry");
  const canvas = $("#ft8-waterfall");
  const spectrum = $("#ft8-spectrum");
  const POLL_MS = 1000;
  const IDLE_MS = 3000;
  const SHOWN_SECONDS = 60; // one minute of waterfall
  const SPECTRUM_SECONDS = 1; // the spectrum line is the average of this past second
  const PALETTE_KEY = "loglab.ft8.palette";
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

  // Colour scales, from static to the strongest signal, each a smooth blend
  // between a few colours. The operator picks one; this browser remembers it.
  const PALETTES = {
    ocean: { label: "Ocean", stops: [[0, [4, 8, 28]], [50, [10, 30, 100]], [105, [20, 90, 200]], [150, [0, 190, 220]], [200, [250, 225, 70]], [255, [255, 255, 255]]] },
    rainbow: { label: "Rainbow", stops: [[0, [0, 0, 10]], [45, [10, 10, 120]], [95, [0, 110, 220]], [140, [0, 200, 120]], [185, [240, 230, 30]], [225, [255, 120, 0]], [255, [255, 20, 20]]] },
    ember: { label: "Ember", stops: [[0, [6, 2, 10]], [60, [60, 10, 90]], [120, [170, 20, 70]], [180, [250, 110, 20]], [230, [255, 210, 60]], [255, [255, 255, 220]]] },
    grey: { label: "Grey", stops: [[0, [6, 6, 8]], [255, [255, 255, 255]]] },
  };
  const blend = (stops) => {
    const out = new Uint8ClampedArray(256 * 3);
    for (let v = 0; v < 256; v++) {
      const hi = stops.findIndex(([at]) => at >= v);
      const [a, ca] = stops[Math.max(hi - 1, 0)];
      const [b, cb] = stops[hi];
      // Eased, so one colour melts into the next with no visible band.
      const t = b === a ? 0 : (v - a) / (b - a);
      const eased = t * t * (3 - 2 * t);
      for (let k = 0; k < 3; k++) out[v * 3 + k] = ca[k] + (cb[k] - ca[k]) * eased;
    }
    return out;
  };
  const remembered = (() => {
    try { return localStorage.getItem(PALETTE_KEY); } catch { return null; }
  })();
  let paletteName = remembered in PALETTES ? remembered : "ocean";
  let PALETTE = blend(PALETTES[paletteName].stops);

  const choosePalette = $("#ft8-palette");
  choosePalette.replaceChildren(...Object.entries(PALETTES).map(([name, p]) => Object.assign(document.createElement("option"), { value: name, textContent: p.label })));
  choosePalette.value = paletteName;
  choosePalette.addEventListener("change", () => {
    paletteName = choosePalette.value in PALETTES ? choosePalette.value : "ocean";
    PALETTE = blend(PALETTES[paletteName].stops);
    try { localStorage.setItem(PALETTE_KEY, paletteName); } catch { /* remembered for this visit only */ }
    drawWaterfall();
    drawSpectrum();
  });

  const columnsOf = () => state?.columns || 960;
  const hzOf = () => columnsOf() * (state?.column_hz || 3.125);
  const shownRows = () => SHOWN_SECONDS * (state?.rows_per_second || 4);
  const levelsOf = (row) => {
    const columns = columnsOf();
    const out = new Uint8Array(columns);
    for (let x = 0; x < columns; x++) out[x] = parseInt(row.levels.substr(x * 2, 2), 16) || 0;
    return out;
  };

  // Each row is painted one pixel high, then stretched smoothly to the
  // canvas's real size; the time labels and markers are drawn at full size
  // over it, so they stay sharp.
  const painted = document.createElement("canvas");
  let decoded = []; // the shown rows' levels, oldest first, kept to paint again

  function fitted(target) {
    const ratio = window.devicePixelRatio || 1;
    const width = Math.max(Math.round(target.clientWidth * ratio), 1);
    const height = Math.max(Math.round(target.clientHeight * ratio), 1);
    if (target.width !== width) target.width = width;
    if (target.height !== height) target.height = height;
    return { ratio, width, height };
  }

  function drawMarker(ctx, width, height, ratio) {
    if (marker === null) return;
    const hz = hzOf();
    // An FT8 signal is 50 Hz wide: shade it and edge it.
    const left = (marker / hz) * width;
    const right = ((marker + 50) / hz) * width;
    ctx.fillStyle = "rgba(255,80,110,0.16)";
    ctx.fillRect(left, 0, right - left, height);
    ctx.fillStyle = "rgba(255,80,110,0.9)";
    for (const x of [left, right]) ctx.fillRect(Math.round(x), 0, Math.max(1, ratio), height);
  }

  function drawWaterfall() {
    const columns = columnsOf();
    const shown = rows.slice(-shownRows());
    const height = shownRows();
    painted.width = columns;
    painted.height = height;
    const paint = painted.getContext("2d");
    const image = paint.createImageData(columns, height);
    // Newest at the top, scrolling down; below the oldest row, static colour.
    for (let y = 0; y < height; y++) {
      const n = shown.length - 1 - y;
      const levels = n >= 0 ? decoded[n] : null;
      for (let x = 0; x < columns; x++) {
        const level = levels ? levels[x] : 0;
        const at = (y * columns + x) * 4;
        image.data[at] = PALETTE[level * 3];
        image.data[at + 1] = PALETTE[level * 3 + 1];
        image.data[at + 2] = PALETTE[level * 3 + 2];
        image.data[at + 3] = 255;
      }
    }
    paint.putImageData(image, 0, 0);
    const { ratio, width, height: tall } = fitted(canvas);
    const ctx = canvas.getContext("2d");
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(painted, 0, 0, width, tall);
    const rowHeight = tall / height;
    // A faint line and the UTC time where each slot begins.
    ctx.font = `${Math.round(11 * ratio)}px system-ui, sans-serif`;
    shown.forEach((row, n) => {
      if (row.row !== 0) return;
      const y = (shown.length - n) * rowHeight;
      ctx.fillStyle = "rgba(255,255,255,0.3)";
      ctx.fillRect(0, Math.round(y), width, Math.max(1, ratio));
      ctx.fillStyle = "rgba(255,255,255,0.85)";
      ctx.fillText(slotTime(row.slot), 6 * ratio, Math.max(y - 4 * ratio, 12 * ratio));
    });
    drawMarker(ctx, width, tall, ratio);
  }

  // The spectrum line: how strong the audio is across the band this past
  // second, averaged over its rows and a few neighbouring columns so it
  // reads as a steady line, not a scribble.
  function drawSpectrum() {
    const { ratio, width, height } = fitted(spectrum);
    const ctx = spectrum.getContext("2d");
    ctx.clearRect(0, 0, width, height);
    // Faint level lines.
    ctx.fillStyle = "rgba(255,255,255,0.07)";
    for (const f of [0.25, 0.5, 0.75]) ctx.fillRect(0, Math.round(height * f), width, Math.max(1, ratio));
    const recent = decoded.slice(-SPECTRUM_SECONDS * (state?.rows_per_second || 4));
    if (recent.length) {
      const columns = columnsOf();
      const average = new Float32Array(columns);
      for (const levels of recent) for (let x = 0; x < columns; x++) average[x] += levels[x] / recent.length;
      const SPREAD = 2;
      const point = (x) => {
        let sum = 0; let count = 0;
        for (let k = Math.max(x - SPREAD, 0); k <= Math.min(x + SPREAD, columns - 1); k++) { sum += average[k]; count++; }
        return height - 3 * ratio - (sum / count / 255) * (height - 6 * ratio);
      };
      const at = (x) => (x / (columns - 1)) * width;
      const top = PALETTE.slice(230 * 3, 230 * 3 + 3);
      const mid = PALETTE.slice(140 * 3, 140 * 3 + 3);
      ctx.beginPath();
      ctx.moveTo(0, height);
      for (let x = 0; x < columns; x++) ctx.lineTo(at(x), point(x));
      ctx.lineTo(width, height);
      ctx.closePath();
      const fill = ctx.createLinearGradient(0, 0, 0, height);
      fill.addColorStop(0, `rgba(${mid[0]},${mid[1]},${mid[2]},0.55)`);
      fill.addColorStop(1, `rgba(${mid[0]},${mid[1]},${mid[2]},0.04)`);
      ctx.fillStyle = fill;
      ctx.fill();
      ctx.beginPath();
      for (let x = 0; x < columns; x++) (x ? ctx.lineTo : ctx.moveTo).call(ctx, at(x), point(x));
      ctx.strokeStyle = `rgb(${top[0]},${top[1]},${top[2]})`;
      ctx.lineWidth = 1.25 * ratio;
      ctx.lineJoin = "round";
      ctx.stroke();
    }
    drawMarker(ctx, width, height, ratio);
  }

  function drawScale() {
    const scale = $("#ft8-scale");
    const width = hzOf();
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
      tr.addEventListener("pointerenter", () => { marker = d.base_hz; drawWaterfall(); drawSpectrum(); });
      tr.addEventListener("pointerleave", () => { marker = null; drawWaterfall(); drawSpectrum(); });
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

  // What both boxes show alike: whether anything is listening, and to what.
  const each = (selector, set) => boxes.forEach((b) => b.querySelectorAll(selector).forEach(set));
  const problem = (text) => each(".ft8-error", (p) => { p.textContent = text; });

  function show() {
    const listening = Boolean(state?.listening);
    const scope = listening && state.dial_hz ? `${mhz(state.dial_hz)} MHz${state.band ? ` · ${state.band}` : ""}` : "";
    each(".ft8-none", (p) => { p.hidden = listening; });
    each(".ft8-body", (div) => { div.hidden = !listening; });
    each(".ft8-pretend", (span) => { span.hidden = !(listening && state.source === "pretend"); });
    each(".ft8-scope", (span) => { span.textContent = scope; });
  }

  async function poll() {
    const session = LogLab.session();
    if (!session?.token || !session.permissions?.includes("view") || boxes.every((b) => b.offsetParent === null)) return IDLE_MS;
    const operation = LogLab.layout()?.operation?.id || null;
    if (operation !== asking) seq = 0;
    const params = new URLSearchParams({ after: String(seq) });
    if (operation) params.set("operation", operation);
    let answer;
    try {
      answer = await LogLab.api("GET", `/ft8?${params}`);
      problem("");
    } catch (e) {
      problem(e.message);
      return IDLE_MS;
    }
    const fresh = seq === 0;
    asking = operation;
    const before = state;
    state = answer;
    if (!answer.listening) {
      rows = []; decoded = []; slots = []; seq = 0;
      show();
      return IDLE_MS;
    }
    if (fresh || before?.dial_hz !== answer.dial_hz || answer.seq < seq) {
      rows = []; decoded = []; slots = [];
      drawScale();
    }
    seq = answer.seq;
    rows = rows.concat(answer.rows).slice(-shownRows());
    decoded = decoded.concat(answer.rows.map(levelsOf)).slice(-rows.length);
    if (answer.slots.length) slots = slots.concat(answer.slots).slice(-SHOWN_SLOTS * 2);
    show();
    if (answer.rows.length || fresh) { drawWaterfall(); drawSpectrum(); }
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
  new ResizeObserver(() => drawSpectrum()).observe(spectrum);
  drawScale();
  drawWaterfall();
  drawSpectrum();
  keepListening();
})();

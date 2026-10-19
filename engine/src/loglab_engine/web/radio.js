// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The radio at this station, live, as its Station Agent reports it. The Agent
// owns the radio and tells the Engine every change; the Engine streams it to
// the browsers signed in at this station (Principles IX and I). This page
// only listens: nothing here can tune the radio or key it.
//
// New QSO follows the radio: its frequency, band, and mode fill in as the
// radio changes (Principle X). A data mode is left as it is, since the radio
// only says "data" and the operator says FT8 or PSK31. Typing in one of those
// boxes stops it following the radio until the QSO is stored. A small label
// on New QSO and a line in the On air box show the radio and whether it is
// transmitting.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const form = $("#entry");
  const label = $("#radio-label");
  const line = $("#onair-radio");
  const fromRadio = $("#freq-from-radio");
  const RETRY_MS = [1000, 2000, 5000, 10000];
  const QUIET_MS = 20000; // the Engine says something at least every 10 s
  let state = null; // the latest word about this station's radio
  let typed = new Set(); // boxes the operator changed by hand since the last QSO
  let streaming = null; // the session token the stream is open for
  let failures = 0;

  const mhz = (hz) => (hz / 1e6).toFixed(6).replace(/0+$/, "").replace(/\.$/, "");

  function ensureOption(select, value) {
    if (![...select.options].some((o) => o.value === value)) {
      select.append(Object.assign(document.createElement("option"), { value, textContent: value }));
    }
  }

  function set(name, value) {
    const input = form[name];
    if (!input || typed.has(name) || input.value === value) return;
    if (input.tagName === "SELECT") ensureOption(input, value);
    input.value = value;
    input.dispatchEvent(new Event("change")); // the dupe check and On air hear of it
  }

  function follow() {
    const radio = state?.radio;
    fromRadio.hidden = !radio || typed.has("FREQ");
    if (!radio) return;
    set("FREQ", mhz(radio.frequency_hz));
    if (radio.band) set("BAND", radio.band);
    if (radio.adif_mode) set("MODE", radio.adif_mode);
  }

  function describe(radio) {
    const parts = [`${radio.mhz.replace(/(\.\d{3})(\d{3})$/, "$1.$2")} MHz`];
    if (radio.mode) parts.push(radio.mode);
    return parts.join(" ");
  }

  function txBadge(radio) {
    const badge = document.createElement("span");
    badge.className = `radio-tx ${radio.transmitting ? "on" : ""}`;
    badge.textContent = radio.transmitting ? "TX" : "RX";
    badge.title = radio.transmitting ? "The radio says it is transmitting" : "The radio says it is receiving";
    return badge;
  }

  function show() {
    const radio = state?.radio;
    label.hidden = !state?.heard;
    if (radio) {
      label.replaceChildren(`${radio.model} · ${describe(radio)} `);
      if (radio.transmitting !== null) label.append(txBadge(radio));
      label.title = `${radio.maker} ${radio.model}, through ${radio.path === "pretend" ? "the pretend radio" : radio.path}`;
      label.classList.remove("problem");
    } else if (state?.heard) {
      label.textContent = "Radio not answering";
      label.title = state.problem || "";
      label.classList.add("problem");
    }

    line.classList.toggle("problem", Boolean(state?.heard && !radio));
    if (!state) {
      line.textContent = "";
    } else if (!state.heard) {
      line.textContent = `No Station Agent is reporting a radio at ${state.station}.`;
    } else if (!radio) {
      line.textContent = `The Station Agent at ${state.station} can't read its radio: ${state.problem || "no reason given"}.`;
    } else {
      line.replaceChildren(
        Object.assign(document.createElement("strong"), { textContent: `${radio.maker} ${radio.model}` }),
        ` at ${state.station}: ${describe(radio)}${radio.band ? `, ${radio.band}` : ", out of band"} `,
      );
      if (radio.transmitting !== null) line.append(txBadge(radio));
    }
  }

  function heard(next) {
    const before = state?.radio;
    state = next;
    show();
    follow();
    // The On air list shows the radio's band and mode once it changes.
    if (before?.band !== next.radio?.band || before?.mode !== next.radio?.mode) {
      document.dispatchEvent(new CustomEvent("loglab:radio-changed", { detail: next }));
    }
  }

  async function listen(token) {
    const response = await fetch("/api/v1/rig/stream", {
      headers: { Authorization: `Bearer ${token}`, Accept: "application/x-ndjson" },
      cache: "no-store",
    });
    if (!response.ok || !response.body) throw new Error(`HTTP ${response.status}`);
    failures = 0;
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffered = "";
    let quiet = null;
    const wake = () => {
      clearTimeout(quiet);
      quiet = setTimeout(() => reader.cancel(), QUIET_MS); // the connection went silent
    };
    wake();
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done || LogLab.session()?.token !== token) break;
        wake();
        buffered += decoder.decode(value, { stream: true });
        let newline;
        while ((newline = buffered.indexOf("\n")) >= 0) {
          const text = buffered.slice(0, newline);
          buffered = buffered.slice(newline + 1);
          if (text.length > 4096) continue;
          try { heard(JSON.parse(text)); } catch { /* a line cut short: the next one says it all */ }
        }
        if (buffered.length > 8192) buffered = "";
      }
    } finally {
      clearTimeout(quiet);
      reader.cancel().catch(() => {});
    }
  }

  // Keep one stream open while someone is signed in, and open it again
  // whenever it ends: the Engine ends each one after a while on purpose.
  async function keepListening() {
    for (;;) {
      const session = LogLab.session();
      if (!session?.token || !session.permissions?.includes("view")) {
        streaming = null;
        if (state) { state = null; show(); fromRadio.hidden = true; }
        await new Promise((resolve) => { setTimeout(resolve, 1000); });
        continue;
      }
      streaming = session.token;
      try {
        await listen(streaming);
      } catch {
        failures += 1;
        if (state?.heard) {
          state = { ...state, heard: false, radio: null };
          show();
          fromRadio.hidden = true;
        }
      }
      await new Promise((resolve) => { setTimeout(resolve, failures ? RETRY_MS[Math.min(failures, RETRY_MS.length) - 1] : 200); });
    }
  }

  // The operator typing in a box means they want what they typed.
  for (const name of ["FREQ", "BAND", "MODE"]) {
    form[name].addEventListener("input", () => {
      typed.add(name);
      if (name === "FREQ") fromRadio.hidden = true;
    });
  }
  // A stored QSO starts the next one following the radio again.
  document.addEventListener("loglab:entry-reset", () => {
    typed = new Set();
    follow();
  });
  keepListening();
})();

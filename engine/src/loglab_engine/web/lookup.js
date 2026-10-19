// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// QRZ.com: the operator's logbook (sent with its API key) and the callsign
// lookup on the New QSO form. Both are optional; logging never waits on them.
// The lookup: When the operator leaves the Call
// box, this asks the Engine who the call is (Principle I: the Engine talks to
// QRZ.com, never the page) and fills in the name, QTH, state and grid boxes
// the operator left empty. It never overwrites what the operator typed. With
// no QRZ.com login or no internet the Engine says so and nothing is filled in:
// logging works exactly the same (Principle V).
"use strict";

(() => {
  const FILLS = { name: "NAME", qth: "QTH", state: "STATE", grid: "GRIDSQUARE" };
  const CALL = /^[A-Z0-9]+(\/[A-Z0-9]+)*$/;
  const FADE_AFTER_MS = 4000; // a status note fades; a refused login stays
  const $ = (selector, root = document) => root.querySelector(selector);
  const form = $("#entry");
  const note = $("#lookup-note");
  let asked = "";
  let fadeTimer = null;

  function say(text, { warn = false } = {}) {
    clearTimeout(fadeTimer);
    note.textContent = text;
    note.classList.toggle("warn", warn);
    note.classList.remove("fading");
    if (text && !warn) fadeTimer = setTimeout(() => note.classList.add("fading"), FADE_AFTER_MS);
  }

  // Clear what an earlier lookup filled in, unless the operator has changed it since.
  function unfill() {
    for (const name of Object.values(FILLS)) {
      const input = form[name];
      if (input.dataset.filled !== undefined && input.value === input.dataset.filled) input.value = "";
      delete input.dataset.filled;
    }
  }

  async function lookUp() {
    const call = form.CALL.value.trim().toUpperCase();
    if (call === asked) return;
    asked = call;
    unfill();
    say("");
    if (!CALL.test(call)) return;
    let found;
    try {
      found = await window.LogLab.api("GET", `/lookup?call=${encodeURIComponent(call)}`);
    } catch {
      return; // the Engine is out of reach: the journal handles logging, nothing to fill
    }
    // The operator may have moved on, or stored the QSO, while QRZ.com answered.
    if (form.CALL.value.trim().toUpperCase() !== call) return;
    if (found.status === "found") {
      for (const [key, name] of Object.entries(FILLS)) {
        const input = form[name];
        if (found[key] && !input.value.trim()) {
          input.value = found[key];
          input.dataset.filled = found[key];
        }
      }
    }
    if (found.status === "not_set_up") return; // set up from the QRZ.com link, never nagged
    say(found.message, { warn: found.status === "login_failed" });
  }

  form.CALL.addEventListener("change", lookUp);
  form.CALL.addEventListener("input", () => {
    if (form.CALL.value.trim() === "") { asked = ""; unfill(); say(""); }
  });
  form.addEventListener("submit", () => { asked = ""; say(""); });
  note.addEventListener("transitionend", () => {
    if (note.classList.contains("fading")) say("");
  });

  // -- Setting up the operator's own QRZ.com login -------------------------

  const dialog = $("#qrz-dialog");
  const setup = $("#qrz-form");
  const remove = $("#qrz-remove");

  function problem(text) { $(".error", setup).textContent = text; }

  // ``checked``: QRZ.com accepted the login just now, so it gets a green tick.
  function showState(login, checked = false) {
    $("#qrz-state").textContent = login.message;
    $("#qrz-state").classList.toggle("done", checked);
    remove.hidden = login.username === null;
    if (login.username) setup.username.value = login.username;
  }

  async function open() {
    problem("");
    setup.password.value = "";
    try {
      showState(await window.LogLab.api("GET", "/callbook/qrz"));
    } catch (e) {
      problem(e.message);
    }
    dialog.showModal();
    book.api_key.focus();
  }

  setup.addEventListener("submit", async (event) => {
    event.preventDefault();
    problem("");
    try {
      const saved = await window.LogLab.api("PUT", "/callbook/qrz", {
        username: setup.username.value.trim(),
        password: setup.password.value,
      });
      setup.password.value = "";
      showState(saved, saved.status === null);
      if (saved.status === null) {
        asked = ""; // look the current call up with the new login
        if (form.CALL.value.trim()) lookUp();
      } else if (saved.status === "login_failed") {
        problem(saved.message);
      }
    } catch (e) {
      problem(e.message);
    }
  });

  remove.addEventListener("click", async () => {
    try {
      await window.LogLab.api("DELETE", "/callbook/qrz");
      setup.username.value = "";
      setup.password.value = "";
      showState(await window.LogLab.api("GET", "/callbook/qrz"));
    } catch (e) {
      problem(e.message);
    }
  });

  // -- Sending QSOs to the operator's QRZ logbook --------------------------

  const book = $("#qrz-book-form");
  const stop = $("#qrz-book-remove");

  function bookProblem(text) { $(".error", book).textContent = text; }

  // A green tick when QSOs are going to the logbook with nothing wrong.
  function showBook(state) {
    const parts = [state.message];
    const fine = state.book_call !== null && !state.problem && !state.message.includes("refused")
      && !state.message.includes("can't be reached");
    $("#qrz-book-state").classList.toggle("done", fine);
    if (state.problem) parts.push(state.problem);
    if (state.refused) parts.push(`${state.refused} refused by QRZ.com${state.last_refusal ? ` (latest: ${state.last_refusal})` : ""}.`);
    $("#qrz-book-state").textContent = parts.join(" ");
    stop.hidden = state.book_call === null;
    if (state.book_call) book.book_call.value = state.book_call;
  }

  async function loadBook() {
    bookProblem("");
    book.api_key.value = "";
    try {
      showBook(await window.LogLab.api("GET", "/logbook/qrz"));
    } catch (e) {
      bookProblem(e.message);
    }
  }

  book.addEventListener("submit", async (event) => {
    event.preventDefault();
    bookProblem("");
    const body = { api_key: book.api_key.value.trim() };
    const call = book.book_call.value.trim().toUpperCase();
    if (call) body.book_call = call;
    try {
      const saved = await window.LogLab.api("PUT", "/logbook/qrz", body);
      book.api_key.value = "";
      showBook(saved);
      if (saved.message.includes("refused")) bookProblem("QRZ.com refused this API key.");
    } catch (e) {
      bookProblem(e.message);
    }
  });

  stop.addEventListener("click", async () => {
    try {
      await window.LogLab.api("DELETE", "/logbook/qrz");
      book.book_call.value = "";
      await loadBook();
    } catch (e) {
      bookProblem(e.message);
    }
  });

  $("#qrz-open").addEventListener("click", () => { loadBook(); open(); });
  $("#qrz-close").addEventListener("click", () => dialog.close());
})();

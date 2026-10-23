// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The Operation stats box: how the whole group is doing in an operation like
// Field Day. The Engine works out every number from the log (Principle I),
// so every workstation shows the same ones (Principle II); this page only
// draws them. Which stats show is set by the operation's overlay. Other
// workstations' QSOs arrive by asking the Engine again every so often, over
// the local network, while the box is on screen (Principle V).
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const box = $("#stats");
  const body = $("#stats-body");
  const EVERY_MS = 20000;
  const GROUPS = ["CW", "Digital", "Phone"];
  let asking = null;

  // The hourly chart and the tables fold away under the tiles. Pointing at
  // the folded line opens them over the page, on top of the boxes below, so
  // nothing moves; they fade away again once the pointer leaves the box. A
  // tap or Enter opens them too, for tablets and the keyboard. The lock in
  // the opened part keeps them open in the box itself, taking their room on
  // the page; whether the operator locked them is kept with their layout
  // (stats_open), on any workstation. The same elements are kept across
  // refreshes, so a refresh never opens or closes them.
  const more = el("div", { className: "stat-more" });
  const opener = el("button", { type: "button", className: "stat-more-head" });
  const lock = el("button", { type: "button", className: "stat-lock" });
  const inside = el("div", { className: "stat-more-body" });
  more.append(el("div", { className: "stat-more-row" }, opener, lock), inside);
  let peek = false;
  const locked = () => LogLab.layout().stats_open === true;

  function showMore() {
    const pinned = locked();
    const open = pinned || peek;
    more.classList.toggle("pinned", pinned);
    more.classList.toggle("peek", peek && !pinned);
    box.classList.toggle("raised", peek && !pinned); // above the boxes below it
    opener.textContent = `${open ? "▾" : "▸"} Each hour, bands and modes, operators`;
    opener.setAttribute("aria-expanded", String(open));
    lock.hidden = !open;
    lock.textContent = pinned ? "🔒" : "🔓";
    lock.setAttribute("aria-pressed", String(pinned));
    lock.title = pinned ? "Locked open: click to let it fold away again" : "Lock it open, taking its room in the box";
    lock.setAttribute("aria-label", lock.title);
  }
  const setPeek = (on) => {
    if (peek === on) return;
    peek = on;
    showMore();
  };

  opener.addEventListener("pointerenter", (e) => { if (e.pointerType === "mouse") setPeek(true); });
  opener.addEventListener("click", () => { if (!locked()) setPeek(!peek); });
  box.addEventListener("pointerleave", (e) => { if (e.pointerType === "mouse") setPeek(false); });
  box.addEventListener("keydown", (e) => { if (e.key === "Escape" && peek) { setPeek(false); opener.focus(); } });
  document.addEventListener("pointerdown", (e) => { if (peek && !box.contains(e.target)) setPeek(false); });
  lock.addEventListener("click", () => {
    const pin = !locked();
    peek = false;
    LogLab.useLayout({ stats_open: pin });
    showMore();
  });
  document.addEventListener("loglab:layout-loaded", () => { peek = false; showMore(); });
  showMore();

  const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
  const hhmm = (iso) => `${iso.slice(11, 13)}${iso.slice(14, 16)}Z`;
  const shown = () => !box.classList.contains("off") && !$("#app").hidden && document.visibilityState === "visible";

  function el(name, props = {}, ...children) {
    const node = Object.assign(document.createElement(name), props);
    node.append(...children);
    return node;
  }

  // One number with a small label under it.
  function tile(label, value, hint = "") {
    const t = el("div", { className: "stat-tile" }, el("span", { className: "stat-value", textContent: String(value) }), el("span", { className: "stat-label", textContent: label }));
    if (hint) t.title = hint;
    return t;
  }

  function tiles(s) {
    const row = el("div", { className: "stat-tiles" });
    for (const id of s.show) {
      if (id === "rate") {
        row.append(
          tile("QSOs", s.qsos, `${s.counted} counting`),
          tile("Last hour", s.last_hour, "QSOs made in the last 60 minutes"),
          tile("Rate /hr", s.rate, `At the pace of the last 10 minutes (${plural(s.last_10, "QSO")})`),
        );
        if (s.best_hour) row.append(tile("Best hour", s.best_hour.qsos, `The hour starting ${hhmm(s.best_hour.start)}`));
      } else if (id === "score" && s.score !== null) {
        const points = Object.entries(s.points).map(([g, n]) => `${g} ${n}`).join(", ");
        row.append(tile("Score", s.score, `QSO points (${points}), before the power multiplier and bonus points`));
      } else if (id === "dupes") {
        row.append(tile("Dupes", s.dupes, "Repeat contacts on the same band and mode; they score nothing"));
      } else if (id === "sections") {
        row.append(tile("Sections", `${s.sections_worked}/${s.sections_total}`, "ARRL and RAC sections worked"));
      }
    }
    return row.children.length ? row : null;
  }

  // QSOs in each UTC hour, as bars; the hour now is drawn stronger.
  function hourly(s) {
    if (!s.hourly.length) return el("p", { className: "hint", textContent: "No hours yet: the chart starts when the operation does." });
    const top = Math.max(1, ...s.hourly.map((x) => x.qsos));
    const every = Math.ceil(s.hourly.length / 24); // hour labels, at most 24 of them
    const nowHour = s.now.slice(0, 13);
    const chart = el("div", { className: "stat-hours", role: "img", ariaLabel: "QSOs in each hour" });
    s.hourly.forEach((x, i) => {
      const column = el("div", { className: `stat-hour${x.start.slice(0, 13) === nowHour ? " now" : ""}`, title: `${hhmm(x.start)}: ${plural(x.qsos, "QSO")}` });
      const bar = el("span", { className: "bar" });
      bar.style.height = `${(x.qsos / top) * 100}%`;
      column.append(
        el("span", { className: "count", textContent: x.qsos && s.hourly.length <= 36 ? String(x.qsos) : "" }),
        el("span", { className: "track" }, bar),
        el("span", { className: "hour", textContent: i % every === 0 ? x.start.slice(11, 13) : "" }),
      );
      chart.append(column);
    });
    return el("div", { className: "stat-wide" }, el("h3", { textContent: "QSOs each hour (UTC)" }), chart);
  }

  // Bands down, mode groups across: what the Field Day summary sheet asks for.
  function bandMode(s) {
    if (!s.by_band_mode.length) return null;
    const modes = [...new Set(s.by_band_mode.map((b) => b.mode))].sort((a, b) => (GROUPS.indexOf(a) + 1 || 99) - (GROUPS.indexOf(b) + 1 || 99) || a.localeCompare(b));
    const bands = [...new Set(s.by_band_mode.map((b) => b.band))];
    const cell = new Map(s.by_band_mode.map((b) => [`${b.band} ${b.mode}`, b.qsos]));
    const table = el("table", { className: "stat-table" });
    table.append(el("thead", {}, el("tr", {}, el("th", { textContent: "Band" }), ...modes.map((m) => el("th", { textContent: m })), el("th", { textContent: "Total" }))));
    const rows = bands.map((band) => {
      const counts = modes.map((m) => cell.get(`${band} ${m}`) || 0);
      return el("tr", {}, el("td", { textContent: band }), ...counts.map((n) => el("td", { textContent: n ? String(n) : "·" })), el("td", { textContent: String(counts.reduce((a, b) => a + b, 0)) }));
    });
    const totals = modes.map((m) => s.by_mode[m] || 0);
    rows.push(el("tr", { className: "total" }, el("td", { textContent: "Total" }), ...totals.map((n) => el("td", { textContent: String(n) })), el("td", { textContent: String(totals.reduce((a, b) => a + b, 0)) })));
    table.append(el("tbody", {}, ...rows));
    return el("div", {}, el("h3", { textContent: "By band and mode" }), el("div", { className: "table-wrap" }, table));
  }

  function operators(s) {
    if (!s.operators.length) return null;
    const scored = s.score !== null;
    const table = el("table", { className: "stat-table" });
    table.append(el("thead", {}, el("tr", {},
      el("th", { textContent: "Operator" }), el("th", { textContent: "QSOs" }), el("th", { textContent: "Last hour" }),
      ...(scored ? [el("th", { textContent: "Points" })] : []))));
    table.append(el("tbody", {}, ...s.operators.map((o) => el("tr", {},
      el("td", {}, LogLab.callsign(o.operator)), el("td", { textContent: String(o.qsos) }), el("td", { textContent: String(o.last_hour) }),
      ...(scored ? [el("td", { textContent: String(o.points) })] : [])))));
    return el("div", {}, el("h3", { textContent: "By operator" }), el("div", { className: "table-wrap" }, table));
  }

  function render(s) {
    $("#stats-scope").textContent = `${s.name} · updated ${hhmm(s.now)}`;
    const parts = [tiles(s)];
    const panels = el("div", { className: "stat-panels" });
    for (const id of s.show) {
      const part = id === "hourly" ? hourly(s) : id === "band_mode" ? bandMode(s) : id === "operators" ? operators(s) : null;
      if (part) panels.append(part);
    }
    if (panels.children.length) {
      inside.replaceChildren(panels);
      parts.push(more);
    }
    const notes = [];
    if (s.outside) notes.push(`${plural(s.outside, "QSO")} outside the operation's hours`);
    if (s.not_counted) notes.push(`${plural(s.not_counted, "QSO")} on a band or mode that doesn't count`);
    if (notes.length) parts.push(el("p", { className: "hint", textContent: `Not counted: ${notes.join(", ")}.` }));
    body.replaceChildren(...parts.filter(Boolean));
  }

  async function refresh() {
    const op = LogLab.session() && LogLab.layout().operation;
    $("#stats-none").hidden = Boolean(op);
    body.hidden = !op;
    if (!op) {
      $("#stats-scope").textContent = "";
      $("#stats-error").textContent = "";
      return;
    }
    if (!shown() || asking) return;
    asking = LogLab.api("GET", `/operations/${op.id}/stats`);
    try {
      const s = await asking;
      if (LogLab.layout().operation?.id !== op.id) return; // moved to another operation while asking
      render(s);
      $("#stats-error").textContent = "";
    } catch (e) {
      $("#stats-error").textContent = `The stats could not be updated: ${e.message}`;
    } finally {
      asking = null;
    }
  }

  document.addEventListener("loglab:layout-loaded", refresh);
  document.addEventListener("loglab:log-loaded", refresh); // a QSO was stored here, or the operation changed
  document.addEventListener("visibilitychange", refresh);
  box.addEventListener("loglab:refresh", refresh);
  // A box shown again from its "Show" button catches up at once.
  let wasOff = box.classList.contains("off");
  new MutationObserver(() => {
    const off = box.classList.contains("off");
    if (wasOff && !off) refresh();
    wasOff = off;
  }).observe(box, { attributes: true, attributeFilter: ["class"] });
  setInterval(refresh, EVERY_MS);
})();

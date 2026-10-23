// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The Operation stats box: how the whole group is doing in an operation like
// Field Day. The Engine works out every number from the log (Principle I),
// so every workstation shows the same ones (Principle II); this page only
// draws them. Which stats show is set by the operation's overlay. With no
// operation picked (everyday logging) it shows the signed-in operator's own
// stats instead: today, this week and year, bands and modes, states and
// countries, the last two as the log names them, never guessed. Other
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

  // The hourly chart and the tables fold away under the tiles, behind a bar
  // that opens them over the page (app.js, expander). The same elements are
  // kept across refreshes, so a refresh never opens or closes them.
  const inside = el("div", { className: "stat-more-body" });
  let moreLabel = "Each hour, bands and modes, operators";
  const more = LogLab.expander({ id: "stats", box, part: inside, over: true, label: () => moreLabel });

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

  // Bands from the longest wavelength down (160m before 20m before 2m before
  // 70cm); anything else, like SAT, after them.
  const UNIT = { m: 1, cm: 0.01, mm: 0.001 };
  function metres(band) {
    const m = /^(\d+(?:\.\d+)?)(mm|cm|m)$/i.exec(band);
    return m ? Number(m[1]) * UNIT[m[2].toLowerCase()] : -1;
  }
  const byBand = (a, b) => metres(b) - metres(a) || a.localeCompare(b);

  // Bands down, mode groups across: what the Field Day summary sheet asks for.
  function bandMode(s) {
    if (!s.by_band_mode.length) return null;
    const modes = [...new Set(s.by_band_mode.map((b) => b.mode))].sort((a, b) => (GROUPS.indexOf(a) + 1 || 99) - (GROUPS.indexOf(b) + 1 || 99) || a.localeCompare(b));
    const bands = [...new Set(s.by_band_mode.map((b) => b.band))].sort(byBand);
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
    moreLabel = "Each hour, bands and modes, operators";
    $("#stats-scope").textContent = `${s.name} · updated ${hhmm(s.now)}`;
    const parts = [tiles(s)];
    const panels = el("div", { className: "stat-panels" });
    for (const id of s.show) {
      const part = id === "hourly" ? hourly(s) : id === "band_mode" ? bandMode(s) : id === "operators" ? operators(s) : null;
      if (part) panels.append(part);
    }
    if (panels.children.length) {
      inside.replaceChildren(panels);
      parts.push(more.outer);
    }
    const notes = [];
    if (s.outside) notes.push(`${plural(s.outside, "QSO")} outside the operation's hours`);
    if (s.not_counted) notes.push(`${plural(s.not_counted, "QSO")} on a band or mode that doesn't count`);
    if (notes.length) parts.push(el("p", { className: "hint", textContent: `Not counted: ${notes.join(", ")}.` }));
    body.replaceChildren(...parts.filter(Boolean));
  }

  // -- Everyday: the operator's own stats ---------------------------------

  // A list of short names, like state codes, as small chips.
  function chips(names, className = "") {
    return el("div", { className: `stat-chips ${className}`.trim() }, ...names.map((n) => el("span", { className: "stat-chip", textContent: n })));
  }

  function states(p) {
    const needed = p.states_needed;
    const part = el("div", {}, el("h3", { textContent: `States worked: ${p.states.length} of ${p.states_total}` }));
    if (p.states.length) part.append(chips(p.states));
    if (p.states.length && needed.length && needed.length <= 25) part.append(el("p", { className: "hint", textContent: `Still needed: ${needed.join(" ")}` }));
    if (!p.states.length) part.append(el("p", { className: "hint", textContent: "None yet: a QSO counts here once its State box names a US state." }));
    return part;
  }

  function countries(p) {
    const part = el("div", {}, el("h3", { textContent: `Countries worked: ${p.countries.length}` }));
    if (p.countries.length) {
      const table = el("table", { className: "stat-table" });
      table.append(el("thead", {}, el("tr", {}, el("th", { textContent: "Country" }), el("th", { textContent: "QSOs" }))));
      table.append(el("tbody", {}, ...p.countries.map((c) => el("tr", { title: c.dxcc === null ? "Not on the DXCC list: counted as the log names it" : `DXCC entity ${c.dxcc}` },
        el("td", { textContent: c.name }), el("td", { textContent: String(c.qsos) })))));
      part.append(el("div", { className: "table-wrap stat-scroll" }, table));
    }
    if (p.no_country) {
      part.append(el("p", { className: "hint", textContent: `${plural(p.no_country, "QSO")} name no country, so they are not counted here. LabLog doesn't guess a country from the call.` }));
    }
    return part;
  }

  function renderPersonal(p) {
    moreLabel = "Bands and modes, states, countries";
    $("#stats-scope").textContent = `Everyday · ${p.operator} · updated ${hhmm(p.now)}`;
    const row = el("div", { className: "stat-tiles" },
      tile("Today", p.today, "Your QSOs since 0000Z today"),
      tile("This week", p.week, "Your QSOs since 0000Z Monday"),
      tile("This year", p.year, "Your QSOs since 0000Z on 1 January"),
      tile("All time", p.total, p.last_qso ? `Every QSO you made; the latest at ${p.last_qso.slice(0, 10)} ${hhmm(p.last_qso)}` : "Every QSO you made"),
      tile("States", `${p.states.length}/${p.states_total}`, "US states worked, from each QSO's State as logged"),
      tile("Countries", p.countries.length, "Countries worked, from each QSO's DXCC entity or Country as logged"),
    );
    const parts = [row];
    if (!p.total) parts.push(el("p", { className: "hint", textContent: "Your stats start with your first QSO." }));
    const panels = el("div", { className: "stat-panels" });
    const table = bandMode({ by_band_mode: p.by_band_mode, by_mode: p.by_band_mode.reduce((m, b) => ({ ...m, [b.mode]: (m[b.mode] || 0) + b.qsos }), {}) });
    if (table) panels.append(table);
    if (p.total) panels.append(states(p), countries(p));
    if (panels.children.length) {
      inside.replaceChildren(panels);
      parts.push(more.outer);
    }
    body.replaceChildren(...parts);
  }

  async function refresh() {
    const signedIn = Boolean(LogLab.session());
    const op = signedIn && LogLab.layout().operation;
    $("#stats-none").hidden = signedIn;
    body.hidden = !signedIn;
    if (!signedIn) {
      $("#stats-scope").textContent = "";
      $("#stats-error").textContent = "";
      return;
    }
    if (!shown() || asking) return;
    asking = LogLab.api("GET", op ? `/operations/${op.id}/stats` : "/stats/personal");
    try {
      const s = await asking;
      if ((LogLab.layout().operation?.id || null) !== (op ? op.id : null)) return; // moved to another operation while asking
      if (op) render(s);
      else renderPersonal(s);
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

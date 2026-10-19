// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The world map: a path from the station to each of the latest QSOs, along
// the great circle, the way the signal went. The Engine says where each end
// was (Principle I); this page only draws it. The map's shapes ship with
// LogLab, so it draws with no internet connection (Principle V). The map is
// centred on the station, so paths seldom run off its edge; the countries are
// drawn three times side by side so the edge never cuts one off.
"use strict";

(() => {
  const SVG = "http://www.w3.org/2000/svg";
  const NORTH = 84; // degrees of latitude shown: nothing is logged further north or south
  const SOUTH = 60;
  const MAX_ZOOM = 8;
  const STEPS = 48; // points along each path
  const $ = (selector, root = document) => root.querySelector(selector);
  const box = $("#world");
  const svg = $("#world-svg");
  const info = $("#world-info");
  const qsos = $("#world-qsos");
  let world = null; // the bundled countries, in degrees
  let full = null; // the whole map's view box
  let view = null; // the part of it shown now
  let centre = null; // the longitude the map is centred on
  let latest = null; // the Engine's last answer
  let loading = null;

  const el = (name, attrs = {}) => {
    const node = document.createElementNS(SVG, name);
    for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, String(value));
    return node;
  };

  const callText = (text) => (text || "").replaceAll("0", "Ø");
  const radians = (deg) => (deg * Math.PI) / 180;
  const degrees = (rad) => (rad * 180) / Math.PI;

  // Points along the great circle from `a` to `b`, as [longitude, latitude],
  // with each longitude within half a turn of the one before, so the path
  // runs on across the map's edge instead of jumping back.
  function greatCircle(a, b) {
    const toXyz = (p) => {
      const lat = radians(p.lat);
      const lon = radians(p.lon);
      return [Math.cos(lat) * Math.cos(lon), Math.cos(lat) * Math.sin(lon), Math.sin(lat)];
    };
    const p = toXyz(a);
    const q = toXyz(b);
    const dot = Math.min(1, Math.max(-1, p[0] * q[0] + p[1] * q[1] + p[2] * q[2]));
    const angle = Math.acos(dot);
    const points = [];
    let last = null;
    for (let i = 0; i <= STEPS; i += 1) {
      const t = i / STEPS;
      let v;
      if (angle < 1e-9) v = p;
      else {
        const s = Math.sin(angle);
        const ka = Math.sin((1 - t) * angle) / s;
        const kb = Math.sin(t * angle) / s;
        v = [ka * p[0] + kb * q[0], ka * p[1] + kb * q[1], ka * p[2] + kb * q[2]];
      }
      let lon = degrees(Math.atan2(v[1], v[0]));
      const lat = degrees(Math.atan2(v[2], Math.hypot(v[0], v[1])));
      if (last !== null) lon += 360 * Math.round((last - lon) / 360);
      last = lon;
      points.push([lon, lat]);
    }
    return points;
  }

  // A longitude as a distance east of the map's centre, from −180 to 180.
  const east = (lon) => ((((lon - centre) % 360) + 540) % 360) - 180;

  async function loadWorld() {
    const response = await fetch("/static/maps/world.json");
    if (!response.ok) throw new Error("the map's shapes could not be loaded");
    world = await response.json();
    full = { x: -180, y: -NORTH, w: 360, h: NORTH + SOUTH };
  }

  // Draw the countries, centred on `lon`, once for each side of the edge.
  function drawCountries(lon) {
    centre = lon;
    view = { ...full };
    const defs = el("defs");
    const land = el("g", { id: "world-land" });
    for (const shape of world.shapes) {
      const path = el("path", { d: shape.d });
      const title = el("title");
      title.textContent = shape.name;
      path.append(title);
      land.append(path);
    }
    defs.append(land);
    const countries = el("g", { class: "world-countries" });
    for (const turn of [-360, 0, 360]) {
      countries.append(el("use", { href: "#world-land", transform: `translate(${turn - centre} 0)` }));
    }
    svg.replaceChildren(defs, countries, el("g", { class: "world-paths" }), el("g", { class: "world-ends" }));
    showView();
  }

  function draw(result) {
    const home = result.home;
    const lon = home ? Math.round(home.lon) : 0;
    if (!world) return;
    if (centre !== lon || !svg.querySelector(".world-countries")) drawCountries(lon);
    const paths = $(".world-paths", svg);
    const ends = $(".world-ends", svg);
    paths.replaceChildren();
    ends.replaceChildren();
    const count = result.paths.length;
    // Oldest first, so the newest paths are drawn on top; older ones fade.
    [...result.paths].reverse().forEach((p, index) => {
      const points = greatCircle(p.station, p.other);
      const shift = east(points[0][0]) - points[0][0];
      const d = points.map(([x, y], i) => `${i ? "L" : "M"}${(x + shift).toFixed(2)} ${(-y).toFixed(2)}`).join("");
      const age = count > 1 ? index / (count - 1) : 1;
      const fade = (0.35 + 0.65 * age).toFixed(2);
      const group = el("g", { class: "world-path", "data-id": p.id, tabindex: 0, role: "button" });
      group.style.setProperty("--age", fade); // through the style object: the page allows no inline style attributes
      const title = el("title");
      title.textContent = describe(p);
      group.append(title, el("path", { class: "hit", d }), el("path", { class: "line", d }));
      paths.append(group);
      const [x, y] = points[points.length - 1];
      const end = el("circle", { cx: (x + shift).toFixed(2), cy: (-y).toFixed(2), r: 0.9, class: "world-end" });
      end.style.setProperty("--age", fade);
      ends.append(end);
    });
    if (home) {
      ends.append(el("circle", { cx: east(home.lon).toFixed(2), cy: (-home.lat).toFixed(2), r: 1.4, class: "world-home" }));
    }
  }

  function describe(p) {
    const when = p.qso_start ? `${p.qso_start.slice(0, 10)} ${p.qso_start.slice(11, 16)} UTC` : "";
    const how = [p.band, p.mode].filter(Boolean).join(" ");
    return [callText(p.call), how, when, `${p.km.toLocaleString()} km`].filter(Boolean).join(" · ");
  }

  function showSummary(result) {
    const asked = result.paths.length + result.without_place + result.without_home;
    $("#world-count").textContent = asked ? `${result.paths.length} of ${asked} drawn` : "No QSOs yet";
    const note = $("#world-note");
    // Said once, quietly, only when nothing can be drawn because the station's place is unknown.
    note.hidden = Boolean(result.home) || result.without_home === 0;
    note.textContent = "Your station's grid isn't known yet. Save your LoTW station location, or look up your own call, and paths will be drawn from there.";
  }

  async function refresh() {
    if (!window.LogLab || $("#app").hidden || box.classList.contains("off")) return;
    const layout = window.LogLab.layout();
    if (document.activeElement !== qsos) qsos.value = String(layout.world_qsos);
    try {
      if (!world) {
        loading = loading || loadWorld();
        await loading;
      }
      // In an operation, only its QSOs; logging under your own call, the whole log.
      const params = new URLSearchParams({ limit: String(layout.world_qsos) });
      if (layout.operation) params.set("operation", layout.operation.id);
      latest = await window.LogLab.api("GET", `/paths?${params}`);
      draw(latest);
      showSummary(latest);
      $("#world-error").textContent = "";
    } catch (e) {
      loading = null;
      $("#world-error").textContent = `The world map could not be updated: ${e.message}`;
    }
  }

  // How many of the latest QSOs to draw: the operator's own choice, kept with
  // their layout so it follows them to any workstation.
  qsos.addEventListener("change", () => {
    const n = Number(qsos.value);
    if (!Number.isInteger(n) || n < 1 || n > Number(qsos.max)) {
      qsos.value = String(window.LogLab.layout().world_qsos);
      return;
    }
    window.LogLab.useLayout({ world_qsos: n });
    refresh();
  });

  // -- Zoom and pan ------------------------------------------------------

  function showView() {
    svg.setAttribute("viewBox", `${view.x} ${view.y} ${view.w} ${view.h}`);
    svg.classList.toggle("zoomed", view.w < full.w / 1.5);
    $("#world-fit").disabled = view.w >= full.w;
  }

  function clampView() {
    view.w = Math.min(full.w, Math.max(full.w / MAX_ZOOM, view.w));
    view.h = view.w * (full.h / full.w);
    view.x = Math.min(full.x + full.w - view.w, Math.max(full.x, view.x));
    view.y = Math.min(full.y + full.h - view.h, Math.max(full.y, view.y));
  }

  function zoom(factor, px, py) {
    if (!view) return;
    const w = view.w / factor;
    const h = view.h / factor;
    const at = px === undefined ? { x: view.x + view.w / 2, y: view.y + view.h / 2 } : { x: px, y: py };
    view = px === undefined
      ? { x: at.x - w / 2, y: at.y - h / 2, w, h }
      : { x: at.x - (at.x - view.x) / factor, y: at.y - (at.y - view.y) / factor, w, h };
    clampView();
    showView();
  }

  function mapPoint(event) {
    const r = svg.getBoundingClientRect();
    return {
      x: view.x + ((event.clientX - r.left) / r.width) * view.w,
      y: view.y + ((event.clientY - r.top) / r.height) * view.h,
    };
  }

  svg.addEventListener("wheel", (event) => {
    // Only with Ctrl (or a trackpad pinch, which sends it), so the page still scrolls.
    if (!view || !event.ctrlKey) return;
    event.preventDefault();
    const p = mapPoint(event);
    zoom(event.deltaY < 0 ? 1.25 : 0.8, p.x, p.y);
  }, { passive: false });

  let drag = null;
  svg.addEventListener("pointerdown", (event) => {
    if (!view || event.button !== 0 || view.w >= full.w) return;
    drag = { id: event.pointerId, x: event.clientX, y: event.clientY, view: { ...view }, moved: false };
  });
  svg.addEventListener("pointermove", (event) => {
    if (!drag || drag.id !== event.pointerId) return;
    const r = svg.getBoundingClientRect();
    const dx = event.clientX - drag.x;
    const dy = event.clientY - drag.y;
    if (!drag.moved && Math.hypot(dx, dy) < 4) return;
    if (!drag.moved) svg.setPointerCapture(event.pointerId);
    drag.moved = true;
    view = { ...drag.view, x: drag.view.x - (dx / r.width) * view.w, y: drag.view.y - (dy / r.height) * view.h };
    clampView();
    showView();
  });
  const endDrag = () => { setTimeout(() => { drag = null; }, 0); };
  svg.addEventListener("pointerup", endDrag);
  svg.addEventListener("pointercancel", endDrag);

  // Tap or click a path to read its QSO under the map.
  function pick(target) {
    const group = target && target.closest && target.closest(".world-path");
    for (const g of svg.querySelectorAll(".world-path.picked")) g.classList.remove("picked");
    const p = group && latest && latest.paths.find((x) => x.id === group.dataset.id);
    if (!p) { info.textContent = ""; return; }
    group.classList.add("picked");
    group.parentNode.append(group); // on top
    info.textContent = describe(p);
  }
  svg.addEventListener("click", (event) => { if (!(drag && drag.moved)) pick(event.target); });
  svg.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") { event.preventDefault(); pick(event.target); }
  });
  svg.addEventListener("dblclick", (event) => {
    const p = mapPoint(event);
    zoom(2, p.x, p.y);
  });

  $("#world-in").addEventListener("click", () => zoom(1.6));
  $("#world-out").addEventListener("click", () => zoom(1 / 1.6));
  $("#world-fit").addEventListener("click", () => { if (view) { view = { ...full }; showView(); } });

  // The log page says when the log has been loaded or changed; the map follows.
  document.addEventListener("loglab:log-loaded", refresh);
  // Shown again when the overlay changes to one that calls for it.
  document.addEventListener("loglab:overlay-shown", () => setTimeout(refresh, 0));
  box.addEventListener("loglab:refresh", refresh);
})();

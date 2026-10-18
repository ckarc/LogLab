// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The section map: every ARRL and RAC section, coloured in once the log has a
// QSO with it, the way ARRL Field Day counts them. The Engine says which
// sections are worked (Principle I); this page only draws what it says
// (Principle II). The map's shapes ship with LogLab, so it draws with no
// internet connection (Principle V). It is one of the page's modules: the
// operator moves, widens or hides it like any other box.
"use strict";

(() => {
  const SVG = "http://www.w3.org/2000/svg";
  const MAX_ZOOM = 8;
  const TINY = 16; // shapes narrower than this (map units) show their label only when zoomed in
  const $ = (selector, root = document) => root.querySelector(selector);
  const box = $("#map");
  const svg = $("#map-svg");
  const info = $("#map-info");
  let shapes = null; // [{ id, sections, d, label }], from the bundled map file
  let full = null; // the whole map's view box
  let view = null; // the part of it shown now
  let sections = new Map(); // code -> { name, qsos, ... } as the Engine last said
  let loading = null;

  const el = (name, attrs = {}) => {
    const node = document.createElementNS(SVG, name);
    for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, String(value));
    return node;
  };

  const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

  // How wide a shape is, from its path: "M x y L x y ... Z", x first.
  function width(d) {
    const xs = (d.match(/-?\d+(\.\d+)?/g) || []).filter((_, i) => i % 2 === 0).map(Number);
    return xs.length ? Math.max(...xs) - Math.min(...xs) : 0;
  }

  async function loadShapes() {
    const response = await fetch("/static/maps/arrl-sections.json");
    if (!response.ok) throw new Error("the map's shapes could not be loaded");
    const map = await response.json();
    full = { x: 0, y: 0, w: map.width, h: map.height };
    view = { ...full };
    shapes = map.shapes;
    const insets = el("g", { class: "map-insets" });
    for (const inset of map.insets) {
      insets.append(el("rect", { x: inset.x, y: inset.y, width: inset.width, height: inset.height, rx: 6 }));
      const title = el("text", { x: inset.x + 6, y: inset.y + 12 });
      title.textContent = inset.title;
      insets.append(title);
    }
    const areas = el("g", { class: "map-areas" });
    const labels = el("g", { class: "map-labels" });
    for (const shape of shapes) {
      const path = el("path", { d: shape.d, "data-id": shape.id, tabindex: 0, role: "button" });
      path.append(el("title"));
      areas.append(path);
      const label = el("text", { x: shape.label[0], y: shape.label[1], "data-id": shape.id });
      label.textContent = shape.id;
      labels.append(label);
    }
    svg.replaceChildren($("defs", svg), insets, areas, labels);
    for (const shape of shapes) {
      if (width(shape.d) < TINY) $(`text[data-id="${shape.id}"]`, labels).classList.add("tiny");
    }
    showView();
  }

  function describe(shape) {
    const parts = shape.sections.map((code) => {
      const s = sections.get(code);
      return s ? `${s.name} (${code}): ${s.qsos ? plural(s.qsos, "QSO") : "not worked"}` : code;
    });
    return parts.join("\n");
  }

  function paint() {
    if (!shapes) return;
    for (const shape of shapes) {
      const worked = shape.sections.filter((code) => (sections.get(code) || {}).qsos > 0).length;
      const path = $(`path[data-id="${shape.id}"]`, svg);
      path.classList.toggle("worked", worked === shape.sections.length);
      path.classList.toggle("partly", worked > 0 && worked < shape.sections.length);
      $("title", path).textContent = describe(shape);
      const label = $(`text[data-id="${shape.id}"]`, svg);
      label.textContent = shape.sections.length > 1 ? `${shape.id} ${worked}/${shape.sections.length}` : shape.id;
    }
  }

  function showSummary(result) {
    const total = result.sections.length;
    $("#map-count").textContent = `${result.worked} of ${total} sections worked`;
    const missing = result.sections.filter((s) => s.qsos === 0);
    const list = $("#map-missing");
    list.replaceChildren();
    for (const s of missing) {
      const item = document.createElement("li");
      item.textContent = s.code;
      item.title = s.name;
      list.append(item);
    }
    $("#map-missing-head").textContent = missing.length
      ? `Still to work (${missing.length})`
      : "Every section worked!";
  }

  async function refresh() {
    if (!window.LogLab || $("#app").hidden) return;
    try {
      if (!shapes) {
        loading = loading || loadShapes();
        await loading;
      }
      // In an operation, only its QSOs count; logging under your own call, the whole log.
      const op = window.LogLab.layout().operation;
      const result = await window.LogLab.api("GET", op ? `/sections?operation=${op.id}` : "/sections");
      $("#map-scope").textContent = op ? "with a QSO in this operation" : "with a QSO in the log";
      sections = new Map(result.sections.map((s) => [s.code, s]));
      paint();
      showSummary(result);
      $("#map-error").textContent = "";
    } catch (e) {
      loading = null;
      $("#map-error").textContent = `The map could not be updated: ${e.message}`;
    }
  }

  // -- Zoom and pan ------------------------------------------------------

  function showView() {
    svg.setAttribute("viewBox", `${view.x} ${view.y} ${view.w} ${view.h}`);
    svg.classList.toggle("zoomed", view.w < full.w / 1.8);
    $("#map-fit").disabled = view.w >= full.w;
  }

  function clampView() {
    view.w = Math.min(full.w, Math.max(full.w / MAX_ZOOM, view.w));
    view.h = view.w * (full.h / full.w);
    view.x = Math.min(full.w - view.w, Math.max(0, view.x));
    view.y = Math.min(full.h - view.h, Math.max(0, view.y));
  }

  // Where the + button zooms: the section picked last; or, from the whole
  // map, Kansas, the middle of the lower 48; or else where the view is now.
  function focus() {
    const picked = svg.querySelector("path.picked");
    const id = picked ? picked.dataset.id : view.w >= full.w ? "KS" : null;
    const shape = id && shapes.find((s) => s.id === id);
    return shape ? { x: shape.label[0], y: shape.label[1] } : { x: view.x + view.w / 2, y: view.y + view.h / 2 };
  }

  // Zoom by `factor` (above 1 zooms in) keeping the map point at (px, py)
  // where it is on screen; with no point, centre on what the operator picked.
  function zoom(factor, px, py) {
    if (!full) return;
    const w = view.w / factor;
    const h = view.h / factor;
    if (px === undefined) {
      const at = factor > 1 ? focus() : { x: view.x + view.w / 2, y: view.y + view.h / 2 };
      view = { x: at.x - w / 2, y: at.y - h / 2, w, h };
    } else {
      view = { x: px - (px - view.x) / factor, y: py - (py - view.y) / factor, w, h };
    }
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
    if (!full || !event.ctrlKey) return;
    event.preventDefault();
    const p = mapPoint(event);
    zoom(event.deltaY < 0 ? 1.25 : 0.8, p.x, p.y);
  }, { passive: false });

  let drag = null;
  svg.addEventListener("pointerdown", (event) => {
    if (!full || event.button !== 0 || view.w >= full.w) return;
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

  // Tap or click a section to read its name and count under the map.
  function pick(target) {
    const id = target && target.dataset && target.dataset.id;
    const shape = shapes && shapes.find((s) => s.id === id);
    for (const p of svg.querySelectorAll("path.picked")) p.classList.remove("picked");
    if (!shape) { info.textContent = ""; return; }
    $(`path[data-id="${id}"]`, svg).classList.add("picked");
    info.textContent = describe(shape).replaceAll("\n", " · ");
  }
  svg.addEventListener("click", (event) => { if (!(drag && drag.moved)) pick(event.target); });
  svg.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") { event.preventDefault(); pick(event.target); }
  });
  svg.addEventListener("dblclick", (event) => {
    const p = mapPoint(event);
    zoom(2, p.x, p.y);
  });

  $("#map-in").addEventListener("click", () => zoom(1.6));
  $("#map-out").addEventListener("click", () => zoom(1 / 1.6));
  $("#map-fit").addEventListener("click", () => { if (full) { view = { ...full }; showView(); } });

  // The log page says when the log has been loaded or changed; the map follows.
  document.addEventListener("loglab:log-loaded", refresh);
  box.addEventListener("loglab:refresh", refresh);
})();

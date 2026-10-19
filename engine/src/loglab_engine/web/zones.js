// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The zone maps: every CQ zone (CQ WW) or every ITU zone (IARU HF), coloured
// in once the log has a QSO with it. The Engine says which zones are worked
// (Principle I); this page only draws what it says (Principle II). The
// zones' shapes and the countries under them ship with LogLab, so the maps
// draw with no internet connection (Principle V). Each map is one of the
// page's modules, and its list of zones still to work is a panel in it.
"use strict";

(() => {
  const SVG = "http://www.w3.org/2000/svg";
  const MAX_ZOOM = 8;
  const $ = (selector, root = document) => root.querySelector(selector);

  const el = (name, attrs = {}) => {
    const node = document.createElementNS(SVG, name);
    for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, String(value));
    return node;
  };
  const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

  // The countries are drawn faintly over the zones, so the zones' edges can
  // be read against the land. Both maps share one copy.
  let countries = null;
  const loadCountries = async () => {
    const response = await fetch("/static/maps/world.json");
    if (!response.ok) throw new Error("the map's countries could not be loaded");
    return (await response.json()).shapes;
  };

  // One zone map. `kind` is "cq" or "itu"; `north` and `south` are the
  // degrees of latitude it shows (the ITU zones reach Antarctica).
  function zoneMap({ id, kind, label, north, south }) {
    const box = $(`#${id}`);
    const svg = $(`#${id}-svg`);
    const info = $(`#${id}-info`);
    const full = { x: -180, y: -north, w: 360, h: north + south };
    let shapes = null; // [{ zone, name, label, d }], from the bundled map file
    let view = { ...full };
    let zones = new Map(); // zone -> QSOs, as the Engine last said
    let loading = null;

    async function load() {
      const [response, land] = await Promise.all([
        fetch(`/static/maps/${kind}-zones.json`),
        countries || (countries = loadCountries()),
      ]);
      if (!response.ok) throw new Error("the map's zones could not be loaded");
      shapes = (await response.json()).shapes;
      const defs = el("defs");
      const outline = el("g", { id: `${id}-land` });
      for (const shape of await land) outline.append(el("path", { d: shape.d }));
      defs.append(outline);
      const areas = el("g", { class: "zone-areas" });
      const labels = el("g", { class: "zone-labels" });
      const over = el("g", { class: "zone-countries" });
      // Zones that cross the 180th meridian run past it; each is drawn again
      // a turn east and west, so whatever runs off one edge shows at the other.
      for (const turn of [-360, 0, 360]) {
        for (const shape of shapes) {
          const path = el("path", { d: shape.d, "data-zone": shape.zone, transform: `translate(${turn} 0)` });
          if (turn === 0) { path.setAttribute("tabindex", 0); path.setAttribute("role", "button"); }
          path.append(el("title"));
          areas.append(path);
          // A zone along the map's top or bottom edge has its number just inside it.
          const y = Math.min(south - 3, Math.max(3 - north, shape.label[1]));
          const text = el("text", { x: shape.label[0] + turn, y, "data-zone": shape.zone });
          text.textContent = shape.zone;
          labels.append(text);
        }
        over.append(el("use", { href: `#${id}-land`, transform: `translate(${turn} 0)` }));
      }
      svg.replaceChildren(defs, areas, over, labels);
      showView();
    }

    const nameOf = (zone) => (shapes.find((s) => s.zone === zone) || {}).name || "";
    const describe = (zone) => {
      const n = zones.get(zone) || 0;
      return `${label} ${zone} · ${nameOf(zone)} · ${n ? plural(n, "QSO") : "not worked"}`;
    };

    function paint() {
      if (!shapes) return;
      for (const path of svg.querySelectorAll(".zone-areas path")) {
        const zone = Number(path.dataset.zone);
        path.classList.toggle("worked", (zones.get(zone) || 0) > 0);
        $("title", path).textContent = describe(zone);
      }
    }

    function showSummary(result) {
      const total = result.zones.length;
      $(`#${id}-count`).textContent = `${result.worked} of ${total} zones worked`;
      const missing = result.zones.filter((z) => z.qsos === 0);
      const list = $(`#${id}-missing`);
      list.replaceChildren();
      for (const z of missing) {
        const item = document.createElement("li");
        item.textContent = z.zone;
        item.title = nameOf(z.zone);
        list.append(item);
      }
      $(`#${id}-missing-head`).textContent = missing.length
        ? `Still to work (${missing.length})`
        : "Every zone worked!";
      // Values that name no zone, like an IARU HQ station's society: logged, not on the map.
      const other = $(`#${id}-other`);
      other.hidden = !result.unrecognized.length;
      other.textContent = `Also logged, not a zone: ${result.unrecognized.map((u) => `${u.value} (${u.qsos})`).join(", ")}`;
    }

    async function refresh() {
      if (!window.LogLab || $("#app").hidden || box.classList.contains("off")) return;
      try {
        if (!shapes) {
          loading = loading || load();
          await loading;
        }
        // In an operation, only its QSOs count; logging under your own call, the whole log.
        const op = window.LogLab.layout().operation;
        const params = new URLSearchParams({ kind });
        if (op) params.set("operation", op.id);
        const result = await window.LogLab.api("GET", `/zones?${params}`);
        $(`#${id}-scope`).textContent = op ? "with a QSO in this operation" : "with a QSO in the log";
        zones = new Map(result.zones.map((z) => [z.zone, z.qsos]));
        paint();
        showSummary(result);
        $(`#${id}-error`).textContent = "";
      } catch (e) {
        loading = null;
        countries = null;
        $(`#${id}-error`).textContent = `The map could not be updated: ${e.message}`;
      }
    }

    // -- Zoom and pan ----------------------------------------------------

    function showView() {
      svg.setAttribute("viewBox", `${view.x} ${view.y} ${view.w} ${view.h}`);
      svg.classList.toggle("zoomed", view.w < full.w / 1.5);
      $(`#${id}-fit`).disabled = view.w >= full.w;
    }

    function clampView() {
      view.w = Math.min(full.w, Math.max(full.w / MAX_ZOOM, view.w));
      view.h = view.w * (full.h / full.w);
      view.x = Math.min(full.x + full.w - view.w, Math.max(full.x, view.x));
      view.y = Math.min(full.y + full.h - view.h, Math.max(full.y, view.y));
    }

    function zoom(factor, px, py) {
      const w = view.w / factor;
      const h = view.h / factor;
      view = px === undefined
        ? { x: view.x + view.w / 2 - w / 2, y: view.y + view.h / 2 - h / 2, w, h }
        : { x: px - (px - view.x) / factor, y: py - (py - view.y) / factor, w, h };
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
      if (!event.ctrlKey) return;
      event.preventDefault();
      const p = mapPoint(event);
      zoom(event.deltaY < 0 ? 1.25 : 0.8, p.x, p.y);
    }, { passive: false });

    let drag = null;
    svg.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || view.w >= full.w) return;
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

    // Tap or click a zone to read its number, name and count under the map.
    function pick(target) {
      const zone = target && target.dataset && Number(target.dataset.zone);
      for (const p of svg.querySelectorAll("path.picked")) p.classList.remove("picked");
      if (!zone || !shapes) { info.textContent = ""; return; }
      for (const p of svg.querySelectorAll(`.zone-areas path[data-zone="${zone}"]`)) p.classList.add("picked");
      info.textContent = describe(zone);
    }
    svg.addEventListener("click", (event) => { if (!(drag && drag.moved)) pick(event.target); });
    svg.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") { event.preventDefault(); pick(event.target); }
    });
    svg.addEventListener("dblclick", (event) => {
      const p = mapPoint(event);
      zoom(2, p.x, p.y);
    });

    $(`#${id}-in`).addEventListener("click", () => zoom(1.6));
    $(`#${id}-out`).addEventListener("click", () => zoom(1 / 1.6));
    $(`#${id}-fit`).addEventListener("click", () => { view = { ...full }; showView(); });

    // The log page says when the log has been loaded or changed; the map follows.
    document.addEventListener("loglab:log-loaded", refresh);
    // Shown again when the overlay changes to one that calls for it.
    document.addEventListener("loglab:overlay-shown", () => setTimeout(refresh, 0));
    box.addEventListener("loglab:refresh", refresh);
  }

  zoneMap({ id: "cqzones", kind: "cq", label: "CQ zone", north: 84, south: 60 });
  zoneMap({ id: "ituzones", kind: "itu", label: "ITU zone", north: 85, south: 85 });
})();

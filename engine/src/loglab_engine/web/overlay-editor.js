// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// The overlay editor, for administrators. An overlay is data (Principle III):
// this page fills in plain forms and sends the whole definition to the Engine,
// which parses it into the same strict model as the overlays it ships
// (Principle XII) and lets only a signed-in administrator save it (Principle
// XIII). The built-in overlays can't be changed, only copied. Every save is a
// new version, and a save over someone else's newer one is refused.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  // ADIF 3.1.7 bands, lowest frequency first.
  const BANDS = [
    "2190m", "630m", "560m", "160m", "80m", "60m", "40m", "30m", "20m", "17m", "15m", "12m", "10m", "8m",
    "6m", "5m", "4m", "2m", "1.25m", "70cm", "33cm", "23cm", "13cm", "9cm", "6cm", "3cm", "1.25cm",
    "6mm", "4mm", "2.5mm", "2mm", "1mm", "submm",
  ];
  const MODULE_NAMES = { entry: "New QSO", journal: "Not logged yet", log: "Log", detail: "QSO detail", map: "Field Day sections" };
  const WIDTH_NAMES = { third: "⅓", half: "½", full: "full" };
  const ABOUT = ["builtin", "version", "retired", "saved_by", "saved_at"];
  const ID = /^[a-z][a-z0-9-]{0,31}$/;
  const KEY = /^[a-z][a-z0-9_]{0,31}$/;
  const COLUMN = /^[a-z_]+(:[A-Z][A-Z0-9_]*)?$/;

  const isAdmin = () => (LogLab.session()?.permissions || []).includes("admin");
  const plain = (o) => JSON.parse(JSON.stringify(o));
  const words = (text) => text.split(/[\s,]+/).map((w) => w.trim().toUpperCase()).filter(Boolean);
  const slug = (text, max = 32) => text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^[^a-z]+|-+$/g, "").slice(0, max).replace(/-+$/, "");
  const keyFrom = (text) => slug(text).replaceAll("-", "_") || "part";

  // The fields the New QSO box has, with the labels it shows them under.
  function entryChoices() {
    return [...document.querySelectorAll("#entry .grid > label")].flatMap((label) => {
      const input = label.querySelector("input[name], select[name]");
      if (!input) return [];
      const text = [...label.childNodes].filter((n) => n.nodeType === Node.TEXT_NODE).map((n) => n.textContent).join("").trim();
      return [{ name: input.name, label: text || input.name }];
    });
  }

  // -- The dialog -----------------------------------------------------------

  const dialog = document.createElement("dialog");
  dialog.className = "card qrz-dialog ov-dialog";
  dialog.setAttribute("aria-labelledby", "ov-title");
  dialog.innerHTML = `
    <div class="menu-head">
      <h2 id="ov-title">Overlays</h2>
      <button type="button" class="secondary small" data-ov="close">Close</button>
    </div>
    <div id="ov-list-view">
      <p class="hint">An overlay sets up LogLab for one kind of operating: which boxes New QSO asks for, how the page starts out, and, for a contest, its bands, modes, exchange and dupe check. The built-in ones can't be changed, but you can copy one and change the copy.</p>
      <div class="actions ov-top">
        <button type="button" class="small" data-ov="new">New overlay</button>
        <label class="inline"><input type="checkbox" id="ov-retired"> Show retired ones</label>
      </div>
      <ul id="ov-list" class="ov-list"></ul>
      <p class="error" role="alert"></p>
    </div>
    <form id="ov-form" autocomplete="off" hidden novalidate>
      <fieldset>
        <legend>About</legend>
        <label>Name <input name="o_name" required maxlength="60" placeholder="e.g. Club Field Day"></label>
        <label>Short name <input name="o_id" required maxlength="32" spellcheck="false"><span class="hint">Lower-case letters, digits and dashes. It can't be changed after the first save.</span></label>
        <label>Summary <textarea name="summary" required maxlength="400" rows="2" placeholder="What operators see when they pick it"></textarea></label>
        <label class="inline"><input type="checkbox" name="contest"> This is a contest</label>
      </fieldset>

      <div id="ov-contest">
        <fieldset>
          <legend>Contest</legend>
          <label>Contest ID <input name="contest_id" maxlength="40" spellcheck="false" placeholder="e.g. ARRL-FD"><span class="hint">The ADIF CONTEST_ID written on each QSO; QSOs are dupe-checked against others with the same one.</span></label>
          <label>Rules title <input name="rules_title" maxlength="400" placeholder="e.g. 2027 ARRL Field Day Rules"></label>
          <label>Rules link <input name="rules_url" type="url" maxlength="300" placeholder="https://…"></label>
          <label>Rules revised <input name="rules_revised" type="date"></label>
          <label>When it runs <input name="rules_period" maxlength="400" placeholder="e.g. Fourth full weekend of June, 1800 UTC Saturday to 2059 UTC Sunday"></label>
          <label class="inline"><input type="checkbox" name="gota"> It has a GOTA station, with a call of its own</label>
        </fieldset>
        <fieldset>
          <legend>Bands</legend>
          <div class="segmented ov-band-way" role="radiogroup" aria-label="Which bands count">
            <label class="inline"><input type="radio" name="band_way" value="allowed"> Only the ticked bands count</label>
            <label class="inline"><input type="radio" name="band_way" value="excluded"> Every band counts except the ticked ones</label>
          </div>
          <div id="ov-bands" class="ov-bands"></div>
          <label class="inline"><input type="checkbox" name="satellite"> Satellite QSOs count as a band of their own</label>
        </fieldset>
        <fieldset>
          <legend>Modes</legend>
          <label>CW modes <input name="modes_cw" spellcheck="false" placeholder="e.g. CW"></label>
          <label>Phone modes <input name="modes_phone" spellcheck="false" placeholder="e.g. SSB, AM, FM, DIGITALVOICE"></label>
          <label>Modes that don't count <input name="modes_excluded" spellcheck="false" placeholder="e.g. FT8, FT4"></label>
          <p class="hint">ADIF mode names, separated by commas. Every other mode counts as Digital.</p>
        </fieldset>
        <fieldset>
          <legend>Dupe check</legend>
          <label class="inline"><input type="checkbox" name="dupes"> Warn when a station has already been worked</label>
          <div id="ov-dupes">
            <label class="inline"><input type="checkbox" name="per_band"> A station may be worked again on another band</label>
            <label class="inline"><input type="checkbox" name="per_mode"> A station may be worked again in another mode (CW, Phone, Digital)</label>
            <label>The rule, as the contest states it <textarea name="dupe_rule" maxlength="400" rows="2"></textarea></label>
          </div>
        </fieldset>
        <fieldset>
          <legend>Exchange you send</legend>
          <p class="hint">Set once for each operation, like your class and section.</p>
          <div id="ov-sent" class="ov-parts"></div>
          <button type="button" class="secondary small" data-ov="add-sent">Add a part</button>
          <h4>Written on each QSO</h4>
          <div id="ov-sent-fields" class="ov-rows"></div>
          <button type="button" class="secondary small" data-ov="add-sent-field">Add a field</button>
          <p class="hint">Put a part's short name in braces, like {class} {section}.</p>
        </fieldset>
        <fieldset>
          <legend>Exchange you receive</legend>
          <p class="hint">Typed for each QSO, and checked before it is stored.</p>
          <div id="ov-received" class="ov-parts"></div>
          <button type="button" class="secondary small" data-ov="add-received">Add a part</button>
        </fieldset>
      </div>

      <fieldset>
        <legend>New QSO</legend>
        <p class="hint">Tick the boxes New QSO asks for, and put them in order.</p>
        <ol id="ov-fields" class="ov-fields"></ol>
        <label class="inline"><input type="checkbox" name="live"> Stamp each QSO with the time it is stored, unless a time was typed</label>
      </fieldset>

      <fieldset>
        <legend>Starting layout</legend>
        <p id="ov-layout" class="hint"></p>
        <div class="actions">
          <button type="button" class="secondary small" data-ov="layout-mine">Use my layout as it is now</button>
          <button type="button" class="secondary small" data-ov="layout-none">No starting layout</button>
        </div>
      </fieldset>

      <div class="actions">
        <button type="submit">Save</button>
        <button type="button" class="secondary" data-ov="back">Cancel</button>
      </div>
      <p class="error" role="alert"></p>
    </form>`;
  document.body.append(dialog);

  const form = $("#ov-form", dialog);
  const listView = $("#ov-list-view", dialog);
  let overlays = [];
  let draft = null; // the overlay being edited
  let base = 0; // the version it was opened at; 0 when new
  let isNew = true;
  let idTyped = false;

  // -- The list --------------------------------------------------------------

  async function openList() {
    form.hidden = true;
    listView.hidden = false;
    $(".error", listView).textContent = "";
    try {
      // Retired ones too, so a new overlay never takes a retired one's short name.
      overlays = await LogLab.api("GET", "/overlays?include_retired=true");
    } catch (e) {
      $(".error", listView).textContent = e.message;
      return;
    }
    const list = $("#ov-list", dialog);
    list.replaceChildren();
    for (const o of overlays.filter((x) => !x.retired || $("#ov-retired", dialog).checked)) {
      const item = document.createElement("li");
      const about = document.createElement("div");
      const name = document.createElement("strong");
      name.textContent = o.name;
      const tag = document.createElement("span");
      tag.className = "hint";
      tag.textContent = o.builtin ? "Built in" : `${o.retired ? "Retired · " : ""}Saved by ${o.saved_by.replaceAll("0", "Ø")}`;
      const summary = document.createElement("span");
      summary.className = "hint ov-summary";
      summary.textContent = o.summary;
      about.append(name, " ", tag, summary);
      const buttons = document.createElement("div");
      buttons.className = "ov-buttons";
      if (!o.builtin) buttons.append(button(o.retired ? "Bring back" : "Change", () => openForm(o, false)));
      buttons.append(button("Copy", () => openForm(o, true)));
      if (!o.builtin && !o.retired) buttons.append(button("Retire", () => retire(o)));
      item.append(about, buttons);
      list.append(item);
    }
  }

  function button(text, onClick, className = "secondary small") {
    const b = document.createElement("button");
    b.type = "button";
    b.className = className;
    b.textContent = text;
    b.addEventListener("click", onClick);
    return b;
  }

  async function retire(o) {
    if (!confirm(`Retire ${o.name}? It disappears from the list of overlays. QSOs logged in it stay in the log, and you can bring it back later.`)) return;
    try {
      await LogLab.api("DELETE", `/overlays/${o.id}?base_version=${o.version}`);
      changed();
      await openList();
    } catch (e) {
      $(".error", listView).textContent = e.message;
    }
  }

  // Overlay.js shows the new list in the Menu and in Set up an operation.
  const changed = () => document.dispatchEvent(new CustomEvent("loglab:overlays-changed"));

  // -- The form --------------------------------------------------------------

  function uniqueId(text) {
    const root = slug(text, 28) || "overlay";
    const taken = new Set(overlays.map((o) => o.id));
    if (!taken.has(root)) return root;
    for (let n = 2; ; n += 1) if (!taken.has(`${root}-${n}`)) return `${root}-${n}`;
  }

  function openForm(o, copy) {
    draft = o ? plain(o) : {
      format: 1, id: "", name: "", summary: "",
      entry: { fields: ["CALL", "BAND", "MODE", "FREQ", "QSO_DATE", "TIME_ON", "RST_SENT", "RST_RCVD", "COMMENT"], live: false },
      layout: null, gota: false,
    };
    for (const key of ABOUT) delete draft[key];
    isNew = copy || !o;
    base = isNew ? 0 : o.version;
    if (copy) {
      draft.name = `${o.name} (copy)`.slice(0, 60);
      draft.id = uniqueId(draft.name);
    }
    idTyped = !isNew;
    $("#ov-title", dialog).textContent = !o ? "New overlay" : copy ? `Copy of ${o.name}` : `Change ${o.name}`;
    listView.hidden = true;
    form.hidden = false;
    $(".error", form).textContent = "";
    fill();
    form.o_name.focus();
  }

  // Contest parts a new contest starts with: the usual mode groups, and a
  // dupe check once per band and mode.
  function contestDefaults() {
    draft.contest_id ??= "";
    draft.rules ??= { title: "", url: "", revised: "", period: "" };
    draft.bands ??= { excluded: [], satellite: false };
    draft.modes ??= { cw: ["CW"], phone: ["SSB", "AM", "FM", "DIGITALVOICE"], excluded: [] };
    draft.dupes ??= { per: ["band", "mode"], rule: "Each station counts once per band and mode." };
    draft.exchange ??= { sent: [], sent_fields: {}, received: [] };
  }

  function fill() {
    form.o_name.value = draft.name;
    form.o_id.value = draft.id;
    form.o_id.disabled = !isNew;
    form.summary.value = draft.summary;
    const contest = Boolean(draft.contest_id !== undefined && draft.contest_id !== null);
    form.contest.checked = contest;
    if (contest) contestDefaults();
    fillContest();
    fillFields();
    form.live.checked = Boolean(draft.entry.live);
    showLayout();
  }

  function fillContest() {
    const box = $("#ov-contest", dialog);
    box.hidden = !form.contest.checked;
    if (!form.contest.checked) return;
    form.contest_id.value = draft.contest_id || "";
    form.rules_title.value = draft.rules.title;
    form.rules_url.value = draft.rules.url;
    form.rules_revised.value = draft.rules.revised;
    form.rules_period.value = draft.rules.period;
    form.gota.checked = Boolean(draft.gota);
    const way = draft.bands.allowed ? "allowed" : "excluded";
    form.band_way.value = way;
    const ticked = new Set(draft.bands.allowed || draft.bands.excluded || []);
    const bands = $("#ov-bands", dialog);
    bands.replaceChildren();
    for (const band of [...BANDS, ...[...ticked].filter((b) => !BANDS.includes(b))]) {
      const label = document.createElement("label");
      label.className = "inline";
      const input = document.createElement("input");
      input.type = "checkbox";
      input.value = band;
      input.checked = ticked.has(band);
      label.append(input, band);
      bands.append(label);
    }
    form.satellite.checked = Boolean(draft.bands.satellite);
    form.modes_cw.value = draft.modes.cw.join(", ");
    form.modes_phone.value = draft.modes.phone.join(", ");
    form.modes_excluded.value = (draft.modes.excluded || []).join(", ");
    form.dupes.checked = Boolean(draft.dupes);
    const dupes = draft.dupes || { per: ["band", "mode"], rule: "" };
    form.per_band.checked = dupes.per.includes("band");
    form.per_mode.checked = dupes.per.includes("mode");
    form.dupe_rule.value = dupes.rule;
    $("#ov-dupes", dialog).hidden = !form.dupes.checked;
    fillParts($("#ov-sent", dialog), draft.exchange.sent, false);
    fillParts($("#ov-received", dialog), draft.exchange.received, true);
    const rows = $("#ov-sent-fields", dialog);
    rows.replaceChildren();
    for (const [name, value] of Object.entries(draft.exchange.sent_fields)) addSentField(name, value);
  }

  // One part of an exchange: what it is called, how it is checked.
  function fillParts(box, parts, received) {
    box.replaceChildren();
    for (const part of parts) addPart(box, part, received);
  }

  function addPart(box, part, received) {
    const row = document.createElement("div");
    row.className = "ov-part";
    row.innerHTML = `
      <label>Label <input data-p="label" maxlength="40" required></label>
      <label>Short name <input data-p="key" maxlength="32" spellcheck="false" placeholder="from the label"></label>
      ${received ? '<label>Logged in <input data-p="field" list="adif-field-list" maxlength="64" spellcheck="false" placeholder="ADIF field, e.g. CLASS"></label>' : ""}
      <label>Hint <input data-p="hint" maxlength="200"></label>
      <label>Checked as <select data-p="check"><option value="sections">an ARRL or RAC section</option><option value="pattern">matching a pattern</option></select></label>
      <label data-p-pattern>Pattern <input data-p="pattern" maxlength="200" spellcheck="false" placeholder="e.g. ^[1-9][0-9]?[A-F]$"></label>
      <label>Also accepted <input data-p="also" spellcheck="false" placeholder="e.g. DX"></label>`;
    const get = (p) => row.querySelector(`[data-p="${p}"]`);
    get("label").value = part.label || "";
    get("key").value = part.key || "";
    if (received) get("field").value = part.field || "";
    get("hint").value = part.hint || "";
    get("check").value = part.sections ? "sections" : "pattern";
    get("pattern").value = part.pattern || "";
    get("also").value = (part.also || []).join(", ");
    const showPattern = () => { row.querySelector("[data-p-pattern]").hidden = get("check").value !== "pattern"; };
    get("check").addEventListener("change", showPattern);
    showPattern();
    row.append(button("Remove", () => row.remove(), "link ov-remove"));
    box.append(row);
  }

  function readParts(box, received) {
    return [...box.querySelectorAll(".ov-part")].map((row) => {
      const get = (p) => row.querySelector(`[data-p="${p}"]`).value.trim();
      const sections = get("check") === "sections";
      const part = {
        key: get("key") || keyFrom(get("label")),
        label: get("label"),
        hint: get("hint"),
        pattern: sections ? null : get("pattern") || null,
        sections,
        also: words(get("also")),
      };
      if (received) part.field = get("field").toUpperCase() || null;
      return part;
    });
  }

  function addSentField(name = "", value = "") {
    const row = document.createElement("div");
    row.className = "ov-row";
    const field = document.createElement("input");
    field.setAttribute("list", "adif-field-list");
    field.placeholder = "ADIF field, e.g. STX_STRING";
    field.spellcheck = false;
    field.value = name;
    field.dataset.p = "name";
    const text = document.createElement("input");
    text.placeholder = "{class} {section}";
    text.spellcheck = false;
    text.value = value;
    text.dataset.p = "value";
    row.append(field, text, button("Remove", () => row.remove(), "link ov-remove"));
    $("#ov-sent-fields", dialog).append(row);
  }

  // New QSO's boxes: the ticked ones in the overlay's order, then the rest.
  function fillFields() {
    const choices = entryChoices();
    const order = [...draft.entry.fields, ...choices.map((c) => c.name).filter((n) => !draft.entry.fields.includes(n))];
    const list = $("#ov-fields", dialog);
    list.replaceChildren();
    for (const name of order) {
      const choice = choices.find((c) => c.name === name) || { name, label: name };
      const item = document.createElement("li");
      item.dataset.field = name;
      const label = document.createElement("label");
      label.className = "inline";
      const input = document.createElement("input");
      input.type = "checkbox";
      input.checked = draft.entry.fields.includes(name);
      input.disabled = name === "CALL"; // New QSO always asks for the call
      label.append(input, choice.label);
      const up = button("↑", () => item.previousElementSibling?.before(item), "secondary small");
      up.setAttribute("aria-label", `Move ${choice.label} up`);
      const down = button("↓", () => item.nextElementSibling?.after(item), "secondary small");
      down.setAttribute("aria-label", `Move ${choice.label} down`);
      item.append(label, up, down);
      list.append(item);
    }
  }

  function showLayout() {
    const text = $("#ov-layout", dialog);
    if (!draft.layout) {
      text.textContent = "None: operators keep the layout they have.";
      return;
    }
    const shown = draft.layout.modules.filter((m) => !m.hidden).map((m) => `${MODULE_NAMES[m.id] || m.id} (${WIDTH_NAMES[m.width]})`);
    text.textContent = `Boxes: ${shown.join(", ")}. The Log shows ${draft.layout.log_columns.length} columns.`;
  }

  function useMyLayout() {
    const now = LogLab.layout();
    const columns = (now.tables?.log?.columns || draft.layout?.log_columns || ["time", "call", "band", "mode", "operator"]).filter((c) => COLUMN.test(c));
    draft.layout = {
      modules: now.modules.map((m) => ({ id: m.id, width: m.width, side: m.side, hidden: Boolean(m.hidden) })),
      log_columns: columns,
    };
    showLayout();
  }

  // -- Saving ----------------------------------------------------------------

  // The form as an overlay definition. Problems the page can see are listed
  // here; the Engine checks the rest.
  function read() {
    const problems = [];
    const definition = {
      format: 1,
      id: form.o_id.value.trim(),
      name: form.o_name.value.trim(),
      summary: form.summary.value.trim(),
      entry: {
        fields: [...$("#ov-fields", dialog).children].filter((li) => li.querySelector("input").checked).map((li) => li.dataset.field),
        live: form.live.checked,
      },
      layout: draft.layout,
      gota: false,
    };
    if (!definition.name) problems.push("Give it a name.");
    else if (overlays.some((o) => o.id !== definition.id && !o.retired && o.name.toLowerCase() === definition.name.toLowerCase())) {
      problems.push(`Another overlay is already called ${definition.name}; give this one its own name.`);
    }
    if (!ID.test(definition.id)) problems.push("The short name needs lower-case letters, digits and dashes, starting with a letter.");
    if (!definition.summary) problems.push("Give it a summary.");
    if (form.contest.checked) {
      const cw = words(form.modes_cw.value);
      const phone = words(form.modes_phone.value);
      const ticked = [...$("#ov-bands", dialog).querySelectorAll("input:checked")].map((i) => i.value);
      const bands = { satellite: form.satellite.checked };
      if (form.band_way.value === "allowed") bands.allowed = ticked;
      else bands.excluded = ticked;
      Object.assign(definition, {
        contest_id: form.contest_id.value.trim().toUpperCase(),
        gota: form.gota.checked,
        rules: {
          title: form.rules_title.value.trim(),
          url: form.rules_url.value.trim(),
          revised: form.rules_revised.value,
          period: form.rules_period.value.trim(),
        },
        bands,
        modes: { cw, phone, excluded: words(form.modes_excluded.value) },
        dupes: form.dupes.checked
          ? { per: [form.per_band.checked && "band", form.per_mode.checked && "mode"].filter(Boolean), rule: form.dupe_rule.value.trim() }
          : null,
        exchange: {
          sent: readParts($("#ov-sent", dialog), false),
          received: readParts($("#ov-received", dialog), true),
          sent_fields: Object.fromEntries([...$("#ov-sent-fields", dialog).children]
            .map((row) => [row.querySelector('[data-p="name"]').value.trim().toUpperCase(), row.querySelector('[data-p="value"]').value.trim()])
            .filter(([name]) => name)),
        },
      });
      if (!definition.contest_id) problems.push("Give the contest ID, like ARRL-FD.");
      const r = definition.rules;
      if (!r.title || !r.period) problems.push("Give the rules' title and when the contest runs.");
      if (!/^https:\/\/\S+$/.test(r.url)) problems.push("The rules link must start with https://.");
      if (!r.revised) problems.push("Give the date the rules were revised.");
      if (form.band_way.value === "allowed" && !ticked.length) problems.push("Tick the bands that count.");
      if (!cw.length || !phone.length) problems.push("List at least one CW mode and one Phone mode.");
      if (definition.dupes && !definition.dupes.rule) problems.push("Write the dupe rule as the contest states it.");
      for (const part of [...definition.exchange.sent, ...definition.exchange.received]) {
        if (!part.label) problems.push("Every part of the exchange needs a label.");
        else if (!KEY.test(part.key)) problems.push(`${part.label}: the short name needs lower-case letters, digits and _.`);
        else if (!part.sections && !part.pattern) problems.push(`${part.label}: give a pattern, or check it as a section.`);
      }
      for (const part of definition.exchange.received) {
        if (part.label && !part.field) problems.push(`${part.label}: say which field it is logged in.`);
        else if (part.field && !definition.entry.fields.includes(part.field)) {
          problems.push(`${part.label}: tick its box (${part.field}) under New QSO, so operators can type it.`);
        }
      }
    }
    return { definition, problems };
  }

  async function save(event) {
    event.preventDefault();
    const error = $(".error", form);
    const { definition, problems } = read();
    if (problems.length) {
      error.textContent = [...new Set(problems)].join(" ");
      return;
    }
    error.textContent = "";
    try {
      await LogLab.api("PUT", `/overlays/${definition.id}`, { definition, base_version: base });
    } catch (e) {
      error.textContent = e.message;
      return;
    }
    changed();
    $("#ov-title", dialog).textContent = "Overlays";
    await openList();
  }

  // -- Wiring ----------------------------------------------------------------

  const opener = $("#ov-open");
  document.addEventListener("loglab:layout-loaded", () => { opener.hidden = !isAdmin(); });
  opener.addEventListener("click", () => {
    $("#menu-dialog").close();
    $("#ov-title", dialog).textContent = "Overlays";
    dialog.showModal();
    openList();
  });
  form.o_name.addEventListener("input", () => { if (!idTyped) form.o_id.value = uniqueId(form.o_name.value); });
  form.o_id.addEventListener("input", () => { idTyped = true; });
  form.contest.addEventListener("change", () => {
    if (form.contest.checked) {
      contestDefaults();
      if (!form.live.checked) form.live.checked = true;
    }
    fillContest();
  });
  form.dupes.addEventListener("change", () => { $("#ov-dupes", dialog).hidden = !form.dupes.checked; });
  form.addEventListener("submit", save);
  $("#ov-retired", dialog).addEventListener("change", openList);
  dialog.addEventListener("click", (event) => {
    const action = event.target.closest("[data-ov]")?.dataset.ov;
    if (action === "close") dialog.close();
    else if (action === "new") openForm(null, false);
    else if (action === "back") { $("#ov-title", dialog).textContent = "Overlays"; openList(); }
    else if (action === "add-sent") addPart($("#ov-sent", dialog), { sections: false }, false);
    else if (action === "add-received") addPart($("#ov-received", dialog), { sections: false }, true);
    else if (action === "add-sent-field") addSentField();
    else if (action === "layout-mine") useMyLayout();
    else if (action === "layout-none") { draft.layout = null; showLayout(); }
  });
})();

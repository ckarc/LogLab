// SPDX-FileCopyrightText: 2026 LogLab contributors
// SPDX-License-Identifier: AGPL-3.0-only

// Import an ADIF file: the QSOs of another program's log (WSJT-X's
// wsjtx_log.adi, for one) come into LabLog. The Engine leaves alone every QSO
// it already has, so the same program's log can be imported again later and
// only its new QSOs come in (Principle XV). Nothing here decides what counts
// as the same QSO; the Engine does, for every workstation alike.
"use strict";

(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const LogLab = window.LogLab;
  const dialog = $("#import-dialog");
  const form = $("#import-form");
  const result = $("#import-result");
  const warnings = $("#import-warnings");
  const problem = (text) => { $(".error", form).textContent = text; };

  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

  function readFile(file) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result).split(",", 2)[1] || "");
      reader.onerror = () => reject(new Error("The file could not be read."));
      reader.readAsDataURL(file);
    });
  }

  // The operation the operator is logging in now, as the Menu names it.
  function current() {
    const chosen = LogLab.layout()?.operation;
    if (!chosen) return null;
    const option = $("#op-choice").selectedOptions[0];
    const name = option && option.value === chosen.id ? option.textContent.replace(/ \(.*$/, "") : "This operation";
    return { ...chosen, name };
  }

  function open() {
    $("#menu-dialog").close();
    form.reset();
    problem("");
    result.hidden = true;
    warnings.replaceChildren();
    const op = current();
    $("#import-where").hidden = !op;
    $("#import-op-name").textContent = op ? op.name : "";
    dialog.showModal();
  }

  function describe(out) {
    if (out.already_imported !== null) {
      return `${out.file_name} was imported before, exactly as it is now (import ${out.already_imported}). Nothing was added.`;
    }
    const parts = [out.added ? `Added ${plural(out.added, "new QSO", "new QSOs")} from ${out.file_name}.` : `No new QSOs in ${out.file_name}.`];
    if (out.already_logged) parts.push(`${plural(out.already_logged, "QSO was", "QSOs were")} already in the log.`);
    if (out.already_deleted) parts.push(`${plural(out.already_deleted, "QSO was", "QSOs were")} deleted before and stay deleted.`);
    if (out.repeated) parts.push(`${plural(out.repeated, "QSO was", "QSOs were")} in the file twice and came in once.`);
    if (out.outside_hours) parts.push(`${plural(out.outside_hours, "QSO was", "QSOs were")} outside the operation's hours and left out.`);
    if (out.no_time) parts.push(`${plural(out.no_time, "QSO has", "QSOs have")} no date or time, so they can't go in an operation; import them with No operation.`);
    return parts.join(" ");
  }

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const file = form.file.files[0];
    if (!file) return;
    problem("");
    result.hidden = true;
    warnings.replaceChildren();
    const button = $("button[type=submit]", form);
    button.disabled = true;
    try {
      const body = { file_name: file.name.slice(0, 255) || "import.adi", data_base64: await readFile(file) };
      const op = current();
      if (op && form.where.value === "operation") body.operation = { id: op.id, gota: Boolean(op.gota) };
      const out = await LogLab.api("POST", "/imports", body);
      result.textContent = describe(out);
      result.hidden = false;
      for (const w of out.warnings) warnings.append(Object.assign(document.createElement("li"), { textContent: w }));
      if (out.added) document.dispatchEvent(new CustomEvent("loglab:log-changed"));
    } catch (e) {
      problem(e.message);
    } finally {
      button.disabled = false;
    }
  });

  $("#import-open").addEventListener("click", open);
  $("#import-close").addEventListener("click", () => dialog.close());
})();

// fork: the Xbox fork's additions to the app's window (see FORK.md), kept out of app.js and the app's
// dictionaries so the original app's updates merge without conflicts:
// - its own words in Arabic (looked up before the app's: the app's dictionaries stay as they are);
// - Debugging: the measurement log's switch (fork-settings.json; off by default);
// - the update banner: updates of the original app are only announced in this copy (installing one
//   would replace this build and its Xbox support), with a link to the new version's page.
(() => {
  const WORDS = {
    ar: {
      "Measurement log (lines.log)": "سجل القياس (lines.log)",
      "Records what happens to each line read — first seen, sent to the translator, translated (and from where), shown, hidden, sent to and drawn on a TV — and why a line was lost. For measurements only: it changes nothing the app reads or shows. Off by default.":
        "يسجّل ما يجري لكل سطر مقروء — أول ظهور، والإرسال إلى المترجم، والترجمة (ومصدرها)، والعرض، والإخفاء، والإرسال إلى التلفاز ورسمه عليه — وسبب فقد أي سطر. للقياس فقط: لا يغيّر شيئًا مما يقرؤه التطبيق أو يعرضه. مطفأ افتراضيًا.",
      "Log size (MB; the file and one older copy are kept)": "حجم السجل (ميغابايت؛ يُحتفظ بالملف ونسخة أقدم واحدة)",
      "Open the log's folder": "افتح مجلد السجل",
      "The fork's settings could not be saved: {e}": "تعذّر حفظ إعدادات النسخة: {e}",
      "Version {version} of the original app is out. This copy (Xbox) doesn't install it: its changes come merged into this copy's own builds.":
        "صدر الإصدار {version} من التطبيق الأصلي. هذه النسخة (Xbox) لا تثبّته: تصل تغييراته مدمجة في إصدارات هذه النسخة.",
      "Release page": "صفحة الإصدار",
    },
  };

  // The fork's words first, then the app's (window.t is the app's lookup: ui/i18n.js).
  const appT = window.t;
  window.t = (en, vars) => {
    const own = (WORDS[I18N.lang] || {})[en];
    if (own === undefined) return appT(en, vars);
    let s = own;
    if (vars) for (const [k, v] of Object.entries(vars)) s = s.split(`{${k}}`).join(String(v));
    return s;
  };

  const invoke = (...a) => window.__TAURI__.core.invoke(...a);
  const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
  let settings = null;
  const load = async () => {
    try {
      settings = await invoke("fork_settings_get");
    } catch (e) {
      settings = null;
    }
    return settings;
  };

  // ---- Debugging: the measurement log's switch, added under the app's own (it redraws its form: added again)
  async function addLogSwitch(form) {
    if (form.querySelector("#fork-log")) return;
    const s = await load();
    if (!s || form.querySelector("#fork-log")) return;
    const box = document.createElement("div");
    box.id = "fork-log-box";
    box.innerHTML = `
      <label class="check" style="color:var(--text);font-weight:600"><input type="checkbox" id="fork-log" ${s.line_log ? "checked" : ""}/> ${esc(t("Measurement log (lines.log)"))}</label>
      <div class="muted small" style="margin:-6px 0 0">${esc(t("Records what happens to each line read — first seen, sent to the translator, translated (and from where), shown, hidden, sent to and drawn on a TV — and why a line was lost. For measurements only: it changes nothing the app reads or shows. Off by default."))}</div>
      <div class="field"><label>${esc(t("Log size (MB; the file and one older copy are kept)"))}</label><div class="inline">
        <input type="number" id="fork-log-mb" min="1" max="64" step="1" value="${Number(s.line_log_mb) || 1}" style="width:6em" /></div></div>
      <button class="btn" id="fork-log-open">${esc(t("Open the log's folder"))}</button>`;
    form.appendChild(box);
    const save = async (change) => {
      try {
        settings = await invoke("fork_settings_set", { value: { ...settings, ...change } });
      } catch (e) {
        alert(t("The fork's settings could not be saved: {e}", { e }));
      }
      const cb = box.querySelector("#fork-log");
      if (cb && settings) cb.checked = !!settings.line_log;
      const mb = box.querySelector("#fork-log-mb");
      if (mb && settings) mb.value = settings.line_log_mb;
    };
    box.querySelector("#fork-log").addEventListener("change", (e) => save({ line_log: e.target.checked }));
    box.querySelector("#fork-log-mb").addEventListener("change", (e) => save({ line_log_mb: Math.min(Math.max(Math.round(Number(e.target.value) || 1), 1), 64) }));
    box.querySelector("#fork-log-open").addEventListener("click", () => invoke("fork_open_log_folder").catch(() => {}));
  }
  const form = document.getElementById("debug-form");
  if (form) {
    new MutationObserver(() => addLogSwitch(form)).observe(form, { childList: true });
    addLogSwitch(form);
  }

  // ---- the update banner: an original version is only announced
  const bar = document.getElementById("update-bar");
  const announce = async () => {
    if (!bar || bar.classList.contains("hidden")) return;
    const s = settings || (await load());
    if (!s || s.install_upstream_updates) return;
    let version = "";
    try {
      version = ((await invoke("update_status")) || {}).version || "";
    } catch (e) {}
    const text = document.getElementById("update-text");
    const now = document.getElementById("update-now");
    if (text) text.textContent = t("Version {version} of the original app is out. This copy (Xbox) doesn't install it: its changes come merged into this copy's own builds.", { version });
    if (now) {
      now.textContent = t("Release page");
      now.disabled = false;
    }
  };
  if (bar) new MutationObserver(announce).observe(bar, { attributes: true, attributeFilter: ["class"] });
  // (before the app's own handler, which would try to install it)
  document.addEventListener(
    "click",
    (e) => {
      if (!e.target.closest || !e.target.closest("#update-now")) return;
      if (!settings || settings.install_upstream_updates) return;
      e.stopImmediatePropagation();
      e.preventDefault();
      invoke("fork_open_update_page").catch(() => {});
    },
    true,
  );
})();

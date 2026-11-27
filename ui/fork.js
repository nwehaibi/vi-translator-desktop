// fork: the Xbox fork's additions to the app's window (see FORK.md), kept out of app.js and the app's
// dictionaries so the original app's updates merge without conflicts:
// - its own words in Arabic (looked up before the app's: the app's dictionaries stay as they are);
// - Show on → "Measurement (this copy)": the measurement log's switch (fork-settings.json; off by
//   default), a section of its own after the app's Debugging. Not inside it: the original shows
//   Debugging in development builds only (app.js: dev_build), and this copy's builds are release ones;
// - the update banner: updates of the original app are only announced in this copy (installing one
//   would replace this build and its Xbox support), with a link to the new version's page.
(() => {
  const WORDS = {
    ar: {
      "Measurement (this copy)": "القياس (هذه النسخة)",
      "This copy's (Xbox) tools to measure how the app reads, translates and shows each line, and what that costs the PC. They change nothing the app reads or shows.":
        "أدوات هذه النسخة (Xbox) لقياس قراءة التطبيق لكل سطر وترجمته وعرضه، وما يكلّفه ذلك الجهاز. لا تغيّر شيئًا مما يقرؤه التطبيق أو يعرضه.",
      "The measurement tools could not be loaded: {e}": "تعذّر تحميل أدوات القياس: {e}",
      "Measurement log (a file per session)": "سجل القياس (ملف لكل جلسة)",
      "Records what happens to each line read — first seen, sent to the translator, translated (and from where), shown, hidden, sent to and drawn on a TV — and why a line was lost. For measurements only: it changes nothing the app reads or shows. Off by default.":
        "يسجّل ما يجري لكل سطر مقروء — أول ظهور، والإرسال إلى المترجم، والترجمة (ومصدرها)، والعرض، والإخفاء، والإرسال إلى التلفاز ورسمه عليه — وسبب فقد أي سطر. للقياس فقط: لا يغيّر شيئًا مما يقرؤه التطبيق أو يعرضه. مطفأ افتراضيًا.",
      "Size of a log part (MB; a session goes on in a new part when one is full: nothing is deleted)": "حجم جزء السجل (ميغابايت؛ تكمل الجلسة في جزء جديد عند امتلائه: لا يُحذف شيء)",
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

  // ---- Show on → "Measurement (this copy)": a section of the fork's own, right after the app's Debugging
  // (in every build: Debugging is the original's and shows in development builds only), drawn again in the
  // window's language once the app has set it (it does after starting; a new language reloads the page)
  function measureSection() {
    let sec = document.getElementById("fork-measure");
    if (sec) return sec;
    sec = document.createElement("div");
    sec.id = "fork-measure";
    const debug = document.getElementById("debug-section");
    if (debug && debug.parentElement) debug.after(sec);
    else (document.getElementById("tab-show") || document.body).appendChild(sec);
    return sec;
  }
  let drawn = 0;
  async function drawMeasure() {
    const turn = ++drawn;
    const sec = measureSection();
    let s = settings,
      failed = null;
    if (!s) {
      try {
        s = settings = await invoke("fork_settings_get");
      } catch (e) {
        failed = e;
      }
    }
    if (turn !== drawn) return; // (drawn again meanwhile)
    const head = `<h3>${esc(t("Measurement (this copy)"))}</h3>
      <p class="muted small">${esc(t("This copy's (Xbox) tools to measure how the app reads, translates and shows each line, and what that costs the PC. They change nothing the app reads or shows."))}</p>`;
    if (!s) {
      sec.innerHTML = `${head}<div class="callout">${esc(t("The measurement tools could not be loaded: {e}", { e: failed }))}</div>`;
      return;
    }
    sec.innerHTML = `${head}
      <div class="form" id="fork-measure-form">
        <label class="check" style="color:var(--text);font-weight:600"><input type="checkbox" id="fork-log" ${s.line_log ? "checked" : ""}/> ${esc(t("Measurement log (a file per session)"))}</label>
        <div class="muted small" style="margin:-6px 0 0">${esc(t("Records what happens to each line read — first seen, sent to the translator, translated (and from where), shown, hidden, sent to and drawn on a TV — and why a line was lost. For measurements only: it changes nothing the app reads or shows. Off by default."))}</div>
        <div class="field"><label>${esc(t("Size of a log part (MB; a session goes on in a new part when one is full: nothing is deleted)"))}</label><div class="inline">
          <input type="number" id="fork-log-mb" min="16" max="64" step="1" value="${Number(s.line_log_mb) || 16}" style="width:6em" /></div></div>
        <button class="btn" id="fork-log-open">${esc(t("Open the log's folder"))}</button>
      </div>`;
    const save = async (change) => {
      try {
        settings = await invoke("fork_settings_set", { value: { ...settings, ...change } });
      } catch (e) {
        alert(t("The fork's settings could not be saved: {e}", { e }));
      }
      const cb = sec.querySelector("#fork-log");
      if (cb && settings) cb.checked = !!settings.line_log;
      const mb = sec.querySelector("#fork-log-mb");
      if (mb && settings) mb.value = settings.line_log_mb;
    };
    sec.querySelector("#fork-log").addEventListener("change", (e) => save({ line_log: e.target.checked }));
    sec.querySelector("#fork-log-mb").addEventListener("change", (e) => save({ line_log_mb: Math.min(Math.max(Math.round(Number(e.target.value) || 16), 16), 64) }));
    sec.querySelector("#fork-log-open").addEventListener("click", () => invoke("fork_open_log_folder").catch(() => {}));
  }
  drawMeasure();
  new MutationObserver(() => drawMeasure()).observe(document.documentElement, { attributes: true, attributeFilter: ["lang"] });

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

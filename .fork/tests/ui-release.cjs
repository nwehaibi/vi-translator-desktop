// fork: the measurement tools are in the app's window in a release build too (see FORK.md).
//
// The original shows its Debugging section in development builds only (app.js: dev_build), and this
// copy's builds are release ones: the measurement log's switch, once put inside Debugging, could not be
// seen in them. This test opens the real window (ui/index.html and every script of it) in Chromium, the
// app's commands answered by a stand-in for the app, with dev_build false (a release build) and true (a
// development build), in English and in Arabic, and checks:
// - the app started (it asked dev_build), and left Debugging hidden in a release build, shown in a
//   development one (the original's behaviour, unchanged);
// - the fork's section "Measurement (this copy)" is not inside Debugging, is there once, and can be seen
//   on the Show on tab, with the log's switch, its size and its folder button, in both builds;
// - the switch saves the fork's setting; the section is in the window's language;
// - with an update of the original out: its banner only announces it (the release page), and "Update
//   now" doesn't install it (Phase 1.1).
//
//   node .fork/tests/ui-release.cjs <fixture.json>
// fixture.json: `cargo run -q -p vi-translator-core --example fork_window_fixture` (the app's default
// settings and languages, as the app sends them). Needs the playwright package (NODE_PATH) and Chromium.
const path = require("path");
const fs = require("fs");
const { chromium } = require("playwright");

const UI = path.resolve(__dirname, "../../ui/index.html");
const fixture = JSON.parse(fs.readFileSync(process.argv[2] || "", "utf8"));

let failures = 0;
function check(ok, what) {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failures++;
}

async function run(browser, { dev, lang, update = null }) {
  const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
  const errors = [];
  // (each error with where it came from: the fork's script, or the rest of the window with this stand-in)
  page.on("pageerror", (e) => errors.push(`${e.message} @ ${(e.stack || "").split("\n").slice(1, 2).join("").trim()}`));
  const config = { ...fixture.config, ui_lang: lang };
  await page.addInitScript(
    ({ dev, config, langs, update }) => {
      window.__calls = [];
      let fork = { line_log: false, line_log_mb: 1, install_upstream_updates: false };
      const answers = {
        get_config: config,
        dev_build: dev,
        app_version: "1.4.4",
        has_offline: true,
        update_status: update,
        fork_open_update_page: null,
        install_update: null,
        list_langs: langs,
        list_fonts: [],
        list_windows: [],
        list_screens: [],
        // (as the app answers them: an object of lists, the Xbox player's window among the sources)
        list_sources: { windows: [], devices: [], obs: { ok: false, error: "OBS is not running" }, screens: [], xbox: "Xbox — VI Translator", ps5: "ps5:" },
        page_options: { pages: [], options: [] },
        list_tvs: [],
        find_tvs: [],
        get_screen: { layout: null, reason: [] },
        frame_saver_status: { on: false, count: 0, folder: null },
        perf_status: null,
        sound_inputs: [],
        sound_status: "",
        vcam_status: "",
        fork_settings_get: () => fork,
        fork_settings_set: (a) => (fork = { ...a.value }),
        fork_open_log_folder: "C:/data",
      };
      const reply = (cmd, args) => {
        window.__calls.push([cmd, args || null]);
        const a = answers[cmd];
        return typeof a === "function" ? a(args) : a === undefined ? null : a;
      };
      const win = { setFullscreen: async () => {}, isFullscreen: async () => false, listen: async () => () => {}, onResized: async () => () => {} };
      window.__TAURI__ = {
        core: { invoke: async (cmd, args) => reply(cmd, args) },
        event: { listen: async () => () => {}, emit: async () => {} },
        window: { getCurrentWindow: () => win },
        webviewWindow: { getCurrentWebviewWindow: () => win },
      };
    },
    { dev, config, langs: fixture.langs, update },
  );
  await page.goto("file://" + UI);
  const label = `${dev ? "development" : "release"} build, ${lang}${update ? ", an update of the original out" : ""}`;
  // the app started: it asked whether this is a development build (and has answered it by now)
  const started = await page
    .waitForFunction(() => window.__calls.some((c) => c[0] === "dev_build"), null, { timeout: 20000 })
    .then(() => true)
    .catch(() => false);
  check(started, `${label}: the app started (asked dev_build)`);
  await page.waitForTimeout(500);
  const debugHidden = await page.evaluate(() => document.getElementById("debug-section")?.hidden);
  check(debugHidden === !dev, `${label}: the original's Debugging is ${dev ? "shown" : "hidden"}, as the original does`);
  const where = await page.evaluate(() => {
    const sec = document.getElementById("fork-measure");
    const debug = document.getElementById("debug-section");
    return { sections: document.querySelectorAll("#fork-measure").length, switches: document.querySelectorAll("#fork-log").length, inDebug: !!(sec && debug && debug.contains(sec)) };
  });
  check(where.sections === 1 && where.switches === 1, `${label}: the measurement section and its switch are there once`);
  check(!where.inDebug, `${label}: the measurement section is not inside Debugging`);
  // on the Show on tab (the app's own tab button)
  await page.click('.tabs button[data-tab="show"]').catch(() => {});
  for (const id of ["#fork-measure h3", "#fork-log", "#fork-log-mb", "#fork-log-open"]) {
    check(await page.isVisible(id), `${label}: ${id} can be seen on the Show on tab`);
  }
  const title = await page.evaluate(() => document.querySelector("#fork-measure h3")?.textContent || "");
  check(lang === "ar" ? title.includes("القياس") : title.includes("Measurement"), `${label}: the section is in the window's language (${title.trim()})`);
  // the switch saves the fork's setting
  await page.click("#fork-log", { timeout: 3000 }).catch(() => {});
  const saved = await page
    .waitForFunction(() => window.__calls.some((c) => c[0] === "fork_settings_set" && c[1] && c[1].value && c[1].value.line_log === true), null, { timeout: 5000 })
    .then(() => true)
    .catch(() => false);
  check(saved, `${label}: the switch saves the measurement log's setting`);
  // an update of the original: announced only
  if (update) {
    const bar = await page.waitForFunction(() => !document.getElementById("update-bar").classList.contains("hidden") && /1\.4\.5/.test(document.getElementById("update-text").textContent) && document.getElementById("update-now").textContent !== "Update now", null, { timeout: 5000 }).then(() => true).catch(() => false);
    const text = await page.evaluate(() => document.getElementById("update-text").textContent);
    check(bar, `${label}: the original's update is announced (${text.trim()})`);
    await page.click("#update-now", { timeout: 3000 }).catch(() => {});
    await page.waitForTimeout(300);
    const calls = await page.evaluate(() => window.__calls.map((c) => c[0]));
    check(calls.includes("fork_open_update_page") && !calls.includes("install_update"), `${label}: "Update now" opens the release page and installs nothing`);
  }
  const forkErrors = errors.filter((e) => e.includes("fork.js"));
  check(forkErrors.length === 0, `${label}: no error from the fork's script${forkErrors.length ? ": " + forkErrors.join(" | ") : ""}`);
  for (const e of errors.filter((e) => !e.includes("fork.js"))) console.log(`     (the window, with this stand-in for the app: ${e})`);
  await page.close();
}

(async () => {
  const browser = await chromium.launch();
  try {
    await run(browser, { dev: false, lang: "en" });
    await run(browser, { dev: true, lang: "en" });
    await run(browser, { dev: false, lang: "ar" });
    // (an update of the original is out)
    const update = { version: "1.4.5", notes: "", page: "https://github.com/vi-translator/vi-translator-desktop/releases/tag/v1.4.5", app_bundle: false };
    await run(browser, { dev: false, lang: "ar", update });
  } finally {
    await browser.close();
  }
  console.log(failures ? `${failures} failed` : "all passed");
  process.exit(failures ? 1 : 0);
})().catch((e) => {
  console.error(e);
  process.exit(2);
});

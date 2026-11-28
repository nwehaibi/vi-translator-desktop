//! fork: what the app's window gets from the app when it starts, as the app sends it — the default
//! settings (get_config) and the target languages (list_langs) — for the test of the window in a release
//! build (.fork/tests/ui-release.cjs; see FORK.md). Prints them as JSON.

fn main() {
    let v = serde_json::json!({
        "config": vi_translator_core::config::Config::default(),
        "langs": vi_translator_core::langs::LANGS.to_vec(),
    });
    println!("{v}");
}

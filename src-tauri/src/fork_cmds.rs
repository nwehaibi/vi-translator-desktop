//! fork: the Xbox fork's commands for the app's window (see FORK.md): its switches (fork-settings.json in
//! the data folder), the measurement log's folder, and the page of an original update it doesn't install.

use vi_translator_core::fork::{linelog, settings};

/// Why "Update now" does nothing in this copy (the window says it its own way: ui/fork.js).
pub const NOTIFY_ONLY: &str = "This copy (Xbox) doesn't install updates of the original app: they come merged into its own builds.";

/// Apply the fork's switches (at start and after a change): the measurement log on or off.
pub fn apply(data_dir: &std::path::Path, s: &settings::ForkSettings) {
    linelog::configure_file(s.line_log.then(|| data_dir.join(linelog::DIR)), s.line_log_mb);
}

/// Updates of the original app are only announced, unless the fork's switch says to install them
/// (installing one would replace this build, its Xbox support with it).
pub fn installs_upstream_updates() -> bool {
    settings::get().install_upstream_updates
}

/// The fork's switches.
#[tauri::command]
pub fn fork_settings_get(webview: tauri::Webview) -> Result<settings::ForkSettings, String> {
    crate::main_only(&webview)?;
    Ok(settings::get())
}

/// Change the fork's switches (saved in fork-settings.json, applied at once).
#[tauri::command]
pub fn fork_settings_set(webview: tauri::Webview, value: settings::ForkSettings) -> Result<settings::ForkSettings, String> {
    crate::main_only(&webview)?;
    let s = settings::save(value).map_err(|e| format!("{e:#}"))?;
    apply(&crate::data_dir(), &s);
    Ok(s)
}

/// Open the folder the measurement log's sessions are written to (`lines` in the app's data folder). Its path.
#[tauri::command]
pub fn fork_open_log_folder(webview: tauri::Webview) -> Result<String, String> {
    crate::main_only(&webview)?;
    let dir = crate::data_dir().join(linelog::DIR);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    crate::debug_frames::open_folder(&dir).map_err(|e| e.to_string())?;
    Ok(dir.display().to_string())
}

/// Open the page of the original app's new version (the update banner, while updates are only announced).
#[tauri::command]
pub fn fork_open_update_page(webview: tauri::Webview, state: tauri::State<crate::AppState>) -> Result<(), String> {
    crate::main_only(&webview)?;
    let page = state.update.lock().unwrap().as_ref().map(|u| u.page.clone()).ok_or("no update available")?;
    // (only GitHub's release pages: the address comes from GitHub's answer)
    if !page.starts_with("https://github.com/") {
        return Err("not a release page".into());
    }
    crate::open_url(&page)
}

//! fork: the Xbox fork's commands for the app's window (see FORK.md): its switches (fork-settings.json in
//! the data folder), the measurement log's folder, and the page of an original update it doesn't install.

use vi_translator_core::fork::{linelog, settings};

/// Why "Update now" does nothing in this copy (the window says it its own way: ui/fork.js).
pub const NOTIFY_ONLY: &str = "This copy (Xbox) doesn't install updates of the original app: they come merged into its own builds.";

/// Apply the fork's switches (at start and after a change): the measurement log on or off.
pub fn apply(data_dir: &std::path::Path, s: &settings::ForkSettings) {
    linelog::configure_file(s.line_log.then(|| data_dir.join(linelog::DIR)), s.line_log_mb);
}

/// The process's TLS crypto provider, installed at the start of the program (rustls' own remedy, its
/// `CryptoProvider` documentation): aws-lc-rs, the provider reqwest already uses for every HTTPS request
/// here. rustls is built with both of its providers in this app — aws-lc-rs through reqwest, ring through
/// webrtc's DTLS — so it can't pick one by itself, and whatever asks it for the process's provider
/// panics: webrtc's DTLS does when the app's own Xbox player connects to the console (1.5.0's own bug:
/// "peer connecting", then a panic in the thread `xbox-rt`, and no picture). Nothing is turned off: the
/// same certificate checks, by the provider reqwest would have chosen anyway. Left alone if one is
/// installed already.
pub fn install_tls_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        // (another thread installing one at the same moment: theirs stays, and is as good)
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    /// What webrtc's DTLS asks rustls for as the app's Xbox player connects (dtls::conn::DTLSConn::new):
    /// a certificate verifier, built with the process's crypto provider. A `RootCertStore` with no root
    /// is refused (an error); what matters is that it doesn't panic.
    fn dtls_verifier() -> Result<Result<(), String>, ()> {
        std::panic::catch_unwind(|| {
            rustls::client::WebPkiServerVerifier::builder(Arc::new(rustls::RootCertStore::empty())).build().map(|_| ()).map_err(|e| e.to_string())
        })
        .map_err(|_| ())
    }

    #[test]
    fn fork_the_process_has_one_tls_provider_and_the_xbox_players_dtls_gets_it() {
        // (nothing else in the app installs one: before it is, both of rustls' providers are built in and
        // it can't choose — the panic the Xbox player hit)
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            assert!(dtls_verifier().is_err(), "rustls chose a provider by itself: the two aren't both built in any more");
        }
        super::install_tls_provider();
        // what webrtc's DTLS asks for no longer panics (no roots: refused, as it should be)
        assert!(dtls_verifier().is_ok_and(|r| r.is_err()));
        // the provider is aws-lc-rs, the one reqwest uses for HTTPS here: the same cipher suites
        let names = |p: &rustls::crypto::CryptoProvider| p.cipher_suites.iter().map(|s| format!("{:?}", s.suite())).collect::<Vec<_>>();
        let installed = rustls::crypto::CryptoProvider::get_default().expect("installed");
        assert_eq!(names(installed), names(&rustls::crypto::aws_lc_rs::default_provider()));
        // and again: it's left as it is
        super::install_tls_provider();
        assert!(Arc::ptr_eq(installed, rustls::crypto::CryptoProvider::get_default().unwrap()));
    }
}

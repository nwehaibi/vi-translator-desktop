//! The fork's switches, in their own file (`fork-settings.json` in the app's data folder), not in the
//! app's settings: the original app's settings file and its code stay as they are, and a switch can be
//! turned off again (or the file deleted) to get the original behaviour back.
//!
//! Every switch is off by default. A missing or unreadable file means the defaults; an unreadable one
//! is never written over until a switch is changed from the app.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// The file's name in the data folder.
pub const FILE: &str = "fork-settings.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ForkSettings {
    /// The per-line measurement log (a file per session in `lines/` in the data folder): off by default.
    pub line_log: bool,
    /// The size of a part of a session's file, in MB (a full part goes on in the next one; nothing is
    /// deleted): 16 to 64.
    pub line_log_mb: u32,
    /// Install updates of the original app from the update banner. Off: the banner only tells that a
    /// new original version is out (installing it would replace this build and its Xbox support).
    pub install_upstream_updates: bool,
}

impl Default for ForkSettings {
    fn default() -> Self {
        Self { line_log: false, line_log_mb: 16, install_upstream_updates: false }
    }
}

impl ForkSettings {
    /// Kept within what the app uses (a hand-edited file can hold anything).
    pub fn sanitize(&mut self) {
        // (16 at the least: a session's file was 1 MB before parts, an older settings file keeps that)
        self.line_log_mb = self.line_log_mb.clamp(16, 64);
    }
}

/// The settings in use and the folder they're kept in (None before `load`: the defaults).
static CURRENT: RwLock<Option<(PathBuf, ForkSettings)>> = RwLock::new(None);

/// The settings file in a data folder.
pub fn file(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// Read the settings from `data_dir` (the defaults when there are none), and keep them there from now on.
pub fn load(data_dir: &Path) -> ForkSettings {
    let mut s: ForkSettings = std::fs::read(file(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    s.sanitize();
    *CURRENT.write().unwrap() = Some((data_dir.to_path_buf(), s.clone()));
    s
}

/// The settings in use (the defaults before `load`).
pub fn get() -> ForkSettings {
    CURRENT.read().unwrap().as_ref().map(|(_, s)| s.clone()).unwrap_or_default()
}

/// Change the settings and save them (whole or not at all) where `load` read them.
pub fn save(mut s: ForkSettings) -> anyhow::Result<ForkSettings> {
    s.sanitize();
    let mut cur = CURRENT.write().unwrap();
    let dir = cur.as_ref().map(|(d, _)| d.clone()).ok_or_else(|| anyhow::anyhow!("the fork's settings were never loaded"))?;
    crate::config::write_atomic(&file(&dir), serde_json::to_string_pretty(&s)?.as_bytes())?;
    *cur = Some((dir, s.clone()));
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_settings_are_off_by_default_and_kept_in_their_own_file() {
        let dir = std::env::temp_dir().join(format!("vit-fork-settings-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        // nothing there: everything off
        let s = load(&dir);
        assert_eq!(s, ForkSettings::default());
        assert!(!s.line_log && !s.install_upstream_updates);
        assert!(!file(&dir).exists(), "loading writes nothing");
        // a switch turned on is saved and read back
        let saved = save(ForkSettings { line_log: true, line_log_mb: 500, ..s }).unwrap();
        assert_eq!(saved.line_log_mb, 64, "kept within range");
        assert_eq!(load(&dir), saved);
        assert_eq!(save(ForkSettings { line_log_mb: 1, ..saved.clone() }).unwrap().line_log_mb, 16, "16 MB a part at the least");
        // a broken file reads as the defaults
        std::fs::write(file(&dir), b"{not json").unwrap();
        assert_eq!(load(&dir), ForkSettings::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}

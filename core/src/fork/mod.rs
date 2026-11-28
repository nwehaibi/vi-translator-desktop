//! The Xbox fork's own additions (see FORK.md at the top of the repository).
//!
//! Everything here is new code kept out of the original files, so updates from the original app merge
//! without conflicts; the original files only call into it from small hooks marked `// fork:`. None of
//! it changes how the app reads, translates or shows anything unless a switch in `fork-settings.json`
//! turns it on (all switches are off by default).
//!
//! - `settings`: the fork's switches (`fork-settings.json` in the app's data folder);
//! - `linelog`: the per-line measurement log (Phase 0.1): what happened to each line read, and why a
//!   line was lost when it was;
//! - `tv_timing`: when a TV was sent a line and when it drew it (Phase 0.4);
//! - `cpu`: how much of the CPU this app and the whole system use (the log's per-second readings, for
//!   the timing settings' calibration, Phase 0.6);
//! - `timeline`: each line's timeline for the log (its English appearing and going between two frames, its
//!   OCR blinks, text on screen nothing was read of);
//! - `advise`: the log's analyzer (sync, lines lost, misreadings, flicker, and advice on the five timing
//!   settings, with how sure it is);
//! - `presence`: text in an area from its pixels only (the log's unread-text finder: a line on screen the
//!   OCR read nothing of, an OCR blink told from the text going).

pub mod advise;
pub mod cpu;
pub mod linelog;
pub mod presence;
pub mod timeline;
pub mod settings;
pub mod tv_timing;

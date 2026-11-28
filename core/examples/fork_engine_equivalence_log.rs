//! fork: the reading's transcript with this copy's measurement log on, written to its folder as the app
//! writes it (see fork_engine/harness.rs and FORK.md). Compared with the log off (`compare … --ignore-log`):
//! the same, but for the log's own records.

#[path = "fork_engine/harness.rs"]
mod harness;

use std::collections::BTreeMap;

use serde_json::{json, Value};
use vi_translator_core::fork::linelog;

fn main() {
    harness::main(harness::Hooks {
        start: |dir| linelog::configure_file(Some(harness::data_dir(dir).join(linelog::DIR)), 16),
        frame: |seq, at| linelog::note_frame(seq, at),
        end: |dir| {
            linelog::configure_file(None, 16);
            // (its writer ends once it has written what it was sent)
            std::thread::sleep(std::time::Duration::from_millis(500));
            let mut events: BTreeMap<String, usize> = BTreeMap::new();
            for f in std::fs::read_dir(harness::data_dir(dir).join(linelog::DIR)).map(|r| r.flatten().map(|e| e.path()).collect::<Vec<_>>()).unwrap_or_default() {
                for l in std::fs::read_to_string(&f).unwrap_or_default().lines() {
                    if let Ok(v) = serde_json::from_str::<Value>(l) {
                        *events.entry(v["ev"].as_str().unwrap_or("?").to_string()).or_default() += 1;
                    }
                }
            }
            // (a log that wrote nothing would prove nothing)
            for kind in ["settings", "seen", "shown", "reads", "tv_sent"] {
                assert!(events.get(kind).copied().unwrap_or(0) > 0, "the log has no \"{kind}\" event: {events:?}");
            }
            eprintln!("the log: {events:?}");
            vec![json!({ "kind": "log", "events": events })]
        },
    });
}

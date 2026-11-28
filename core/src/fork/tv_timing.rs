//! When a TV was sent the overlays' state and when it drew it (the fork's Phase 0.4).
//!
//! - `tv_sent`: the moment the PC wrote a `/tv.json` answer (its state version, and the lines it shows).
//! - `tv_drawn`: the TV page answers back after drawing that state (`/tv/drawn?h=<version>&draw=<ms>`,
//!   sent after two animation frames: once the page has painted). `draw` is the TV's own time from the
//!   answer's arrival to the paint. On this PC's clock the paint was then at about
//!   `sent + (recv − sent − draw) / 2 + draw`: half of the rest is the network one way.
//!
//! This is when the TV's browser drew the line, not when its panel lit up (the screen's own processing
//! comes after; only a camera filming the TV measures that). Both only while the measurement log is on.

use std::sync::Mutex;

use serde_json::json;

use super::linelog;

/// A TV by a short tag of its key (the key itself never goes into the log).
pub fn tv_tag(token: &str) -> String {
    use sha2::Digest as _;
    let h = sha2::Sha256::digest(token.as_bytes());
    h.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// When each TV was last sent each state version (Unix ms), for its `tv_drawn`: a few per TV.
static SENT: Mutex<Vec<(String, String, u64)>> = Mutex::new(Vec::new());

thread_local! {
    /// The key of the TV whose `/tv.json` this thread is answering (see `for_tv`).
    static TV: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Answer a TV's `/tv.json` (`f`) knowing which TV it is (its key), so `sent_now` can say.
pub fn for_tv<R>(token: &str, f: impl FnOnce() -> R) -> R {
    TV.with(|t| *t.borrow_mut() = Some(token.to_string()));
    let r = f();
    TV.with(|t| *t.borrow_mut() = None);
    r
}

/// The state `version` was just written to the TV this thread answers (see `for_tv`): the lines up now
/// were sent to it.
pub fn sent_now(version: &str, live: &crate::live::Live) {
    if !linelog::on() {
        return;
    }
    let Some(token) = TV.with(|t| t.borrow().clone()) else { return };
    // the lines up: by area, each line's `at` (when its English appeared: the line's key in the log)
    let shown: serde_json::Map<String, serde_json::Value> = live.shown.lock().unwrap().iter().map(|(a, ev)| (a.clone(), ev["at"].clone())).collect();
    sent(&token, version, shown);
}

/// The state `version` (showing `shown`: area → line's `at`) was written to the TV with key `token`.
pub fn sent(token: &str, version: &str, shown: serde_json::Map<String, serde_json::Value>) {
    if !linelog::on() {
        return;
    }
    let tv = tv_tag(token);
    let now = linelog::unix_ms();
    {
        let mut s = SENT.lock().unwrap();
        s.retain(|(t, v, _)| !(*t == tv && v == version));
        s.push((tv.clone(), version.to_string(), now));
        if s.len() > 64 {
            s.remove(0);
        }
    }
    linelog::emit("tv_sent", json!({ "tv": tv, "version": version, "shown": shown }));
}

/// The TV with key `token` drew state `version`, `draw_ms` after it arrived (the TV's own clock).
pub fn drawn(token: &str, version: &str, draw_ms: u64) {
    if !linelog::on() {
        return;
    }
    let tv = tv_tag(token);
    let recv = linelog::unix_ms();
    let sent = SENT.lock().unwrap().iter().rev().find(|(t, v, _)| *t == tv && v == version).map(|(_, _, at)| *at);
    let est = sent.map(|s| estimate(s, recv, draw_ms));
    linelog::emit("tv_drawn", json!({ "tv": tv, "version": version, "sent": sent, "recv": recv, "draw_ms": draw_ms, "est": est }));
}

/// When the TV drew it, on this PC's clock: half the time not spent drawing is the way there.
pub fn estimate(sent: u64, recv: u64, draw_ms: u64) -> u64 {
    let network = recv.saturating_sub(sent).saturating_sub(draw_ms);
    sent + network / 2 + draw_ms
}

/// A query's value as a number (0 when it isn't one).
pub fn number(v: &str) -> u64 {
    v.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0).map(|n| n.round() as u64).unwrap_or(0).min(60_000)
}

/// Kept so the TV page knows to answer back (`X-Ack: 1` on its state while the log is on).
pub fn ack_header() -> &'static str {
    if linelog::on() { "X-Ack: 1\r\n" } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_tv_drawn_time_is_the_network_halved_plus_the_drawing() {
        // sent at 1000, the answer back at 1100, 40 ms of it drawing: 30 ms each way
        assert_eq!(estimate(1000, 1100, 40), 1070);
        // a TV clock that says it took longer than the whole round trip: no network time
        assert_eq!(estimate(1000, 1020, 50), 1050);
        assert_eq!(number("12.6"), 13);
        assert_eq!(number("-3"), 0);
        assert_eq!(number("x"), 0);
    }

    #[test]
    fn fork_tv_sent_and_drawn_are_logged_for_a_connected_tv_only() {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        use std::sync::Arc;
        let _g = linelog::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let get = |port: u16, target: &str| -> String {
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c.write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).unwrap();
            let mut all = Vec::new();
            c.read_to_end(&mut all).unwrap();
            String::from_utf8_lossy(&all).to_string()
        };
        let dir = std::env::temp_dir().join(format!("vit-fork-tv-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let live = Arc::new(crate::live::Live::default());
        live.tvs.load(dir.join("tvs.json"));
        live.tvs.add(crate::tv::PairedTv { id: "tv-fork".into(), name: "TV".into(), token: "fork-key-1".into(), off: false });
        let server = crate::tv_server::TvServer::start(0, Arc::new(Mutex::new(crate::config::Config::default())), live.clone(), None).unwrap();
        // with the log off, the page isn't asked to answer back
        assert!(!get(server.port, "/tv.json?t=fork-key-1&h=").contains("X-Ack"));
        let collected = Arc::new(Mutex::new(Vec::new()));
        linelog::set_collector(Some(collected.clone()));
        let answer = get(server.port, "/tv.json?t=fork-key-1&h=");
        assert!(answer.contains("X-Ack: 1\r\n"), "{answer}");
        let version = answer.lines().find_map(|l| l.strip_prefix("X-State: ")).unwrap().trim().to_string();
        assert!(get(server.port, &format!("/tv/drawn?t=fork-key-1&h={version}&draw=12")).starts_with("HTTP/1.1 200"));
        // another key gets nothing, and logs nothing
        assert!(get(server.port, "/tv/drawn?t=wrong&h=x&draw=1").starts_with("HTTP/1.1 403"));
        linelog::set_collector(None);
        server.stop();
        let events = collected.lock().unwrap().clone();
        let tag = tv_tag("fork-key-1");
        assert!(events.iter().any(|e| e["ev"] == "tv_sent" && e["tv"] == tag.as_str() && e["version"] == version.as_str()));
        let drawn = events.iter().find(|e| e["ev"] == "tv_drawn" && e["tv"] == tag.as_str()).expect("tv_drawn");
        assert_eq!(drawn["draw_ms"], 12);
        assert!(drawn["est"].as_u64().unwrap() >= drawn["sent"].as_u64().unwrap() + 12);
        assert!(!events.iter().any(|e| e.to_string().contains("fork-key-1")), "the key never goes into the log");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fork_tv_keys_never_go_into_the_log() {
        let tag = tv_tag("secret-key-123");
        assert_eq!(tag.len(), 8);
        assert!(!tag.contains("secret"));
        assert_eq!(tag, tv_tag("secret-key-123"));
    }
}

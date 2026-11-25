//! The TV side of the home-network server: what the VI Translator app on a TV (Android TV, LG
//! webOS, Samsung Tizen) loads from the "PC" it was connected to (tv.rs) — which may as well be a
//! phone running this core.
//!
//! - `/tv?t=<key>`: the TV page (ui/tv.html, with ui/common.js, fonts.css and the fonts, bundled here);
//! - `/tv.json?t=<key>&h=<version>`: what the overlays show now (`live::overlay_state`), long-polled;
//! - `/tv/forget?t=<key>`: the TV disconnected itself;
//! - `POST /vit-tv/offer` (with `X-VIT`): a Samsung TV offering the code it shows (see tv.rs).
//!
//! The desktop app serves these on its own home-network page server through `route`; a phone runs
//! `TvServer`, which serves them and, while `lan_page` is on, the live page (lan_page.rs).

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::Config;
use crate::lan_page::LivePage;
use crate::live::Live;
use crate::net::{Request, Slot, host_is_address, param, read_request, respond};

/// The page's files, bundled: (path, MIME type, bytes).
const ASSETS: &[(&str, &str, &[u8])] = &[
    ("tv.html", "text/html", include_bytes!("../../ui/tv.html")),
    ("common.js", "text/javascript", include_bytes!("../../ui/common.js")),
    ("fonts.css", "text/css", include_bytes!("../../ui/fonts.css")),
    // the live page (lan_page.rs)
    ("live.html", "text/html", include_bytes!("../../ui/live.html")),
    ("snapshot.js", "text/javascript", include_bytes!("../../ui/snapshot.js")),
    ("i18n.js", "text/javascript", include_bytes!("../../ui/i18n.js")),
    ("i18n/ar.js", "text/javascript", include_bytes!("../../ui/i18n/ar.js")),
    ("i18n/bn.js", "text/javascript", include_bytes!("../../ui/i18n/bn.js")),
    ("i18n/cs.js", "text/javascript", include_bytes!("../../ui/i18n/cs.js")),
    ("i18n/de.js", "text/javascript", include_bytes!("../../ui/i18n/de.js")),
    ("i18n/el.js", "text/javascript", include_bytes!("../../ui/i18n/el.js")),
    ("i18n/es-ES.js", "text/javascript", include_bytes!("../../ui/i18n/es-ES.js")),
    ("i18n/es-MX.js", "text/javascript", include_bytes!("../../ui/i18n/es-MX.js")),
    ("i18n/fa.js", "text/javascript", include_bytes!("../../ui/i18n/fa.js")),
    ("i18n/fil.js", "text/javascript", include_bytes!("../../ui/i18n/fil.js")),
    ("i18n/fr.js", "text/javascript", include_bytes!("../../ui/i18n/fr.js")),
    ("i18n/hi.js", "text/javascript", include_bytes!("../../ui/i18n/hi.js")),
    ("i18n/id.js", "text/javascript", include_bytes!("../../ui/i18n/id.js")),
    ("i18n/it.js", "text/javascript", include_bytes!("../../ui/i18n/it.js")),
    ("i18n/ja.js", "text/javascript", include_bytes!("../../ui/i18n/ja.js")),
    ("i18n/ko.js", "text/javascript", include_bytes!("../../ui/i18n/ko.js")),
    ("i18n/mr.js", "text/javascript", include_bytes!("../../ui/i18n/mr.js")),
    ("i18n/ms.js", "text/javascript", include_bytes!("../../ui/i18n/ms.js")),
    ("i18n/nl.js", "text/javascript", include_bytes!("../../ui/i18n/nl.js")),
    ("i18n/pl.js", "text/javascript", include_bytes!("../../ui/i18n/pl.js")),
    ("i18n/pt-BR.js", "text/javascript", include_bytes!("../../ui/i18n/pt-BR.js")),
    ("i18n/ro.js", "text/javascript", include_bytes!("../../ui/i18n/ro.js")),
    ("i18n/ru.js", "text/javascript", include_bytes!("../../ui/i18n/ru.js")),
    ("i18n/sw.js", "text/javascript", include_bytes!("../../ui/i18n/sw.js")),
    ("i18n/ta.js", "text/javascript", include_bytes!("../../ui/i18n/ta.js")),
    ("i18n/te.js", "text/javascript", include_bytes!("../../ui/i18n/te.js")),
    ("i18n/th.js", "text/javascript", include_bytes!("../../ui/i18n/th.js")),
    ("i18n/tr.js", "text/javascript", include_bytes!("../../ui/i18n/tr.js")),
    ("i18n/uk.js", "text/javascript", include_bytes!("../../ui/i18n/uk.js")),
    ("i18n/ur.js", "text/javascript", include_bytes!("../../ui/i18n/ur.js")),
    ("i18n/vi.js", "text/javascript", include_bytes!("../../ui/i18n/vi.js")),
    ("i18n/zh-Hans.js", "text/javascript", include_bytes!("../../ui/i18n/zh-Hans.js")),
    ("i18n/zh-Hant.js", "text/javascript", include_bytes!("../../ui/i18n/zh-Hant.js")),
    ("fonts/Cairo.ttf", "font/ttf", include_bytes!("../../ui/fonts/Cairo.ttf")),
    ("fonts/Changa.ttf", "font/ttf", include_bytes!("../../ui/fonts/Changa.ttf")),
    ("fonts/Lalezar-Regular.ttf", "font/ttf", include_bytes!("../../ui/fonts/Lalezar-Regular.ttf")),
    ("fonts/NotoSansArabic.ttf", "font/ttf", include_bytes!("../../ui/fonts/NotoSansArabic.ttf")),
    ("fonts/Tajawal-Bold.ttf", "font/ttf", include_bytes!("../../ui/fonts/Tajawal-Bold.ttf")),
    ("fonts/Tajawal-ExtraBold.ttf", "font/ttf", include_bytes!("../../ui/fonts/Tajawal-ExtraBold.ttf")),
    ("fonts/Tajawal-Regular.ttf", "font/ttf", include_bytes!("../../ui/fonts/Tajawal-Regular.ttf")),
    ("fonts/Cairo-OFL.txt", "text/plain", include_bytes!("../../ui/fonts/Cairo-OFL.txt")),
    ("fonts/Changa-OFL.txt", "text/plain", include_bytes!("../../ui/fonts/Changa-OFL.txt")),
    ("fonts/Lalezar-OFL.txt", "text/plain", include_bytes!("../../ui/fonts/Lalezar-OFL.txt")),
    ("fonts/NotoSansArabic-OFL.txt", "text/plain", include_bytes!("../../ui/fonts/NotoSansArabic-OFL.txt")),
    ("fonts/Tajawal-OFL.txt", "text/plain", include_bytes!("../../ui/fonts/Tajawal-OFL.txt")),
];

/// A bundled file of the TV page: its MIME type and bytes.
pub fn asset(path: &str) -> Option<(&'static str, &'static [u8])> {
    ASSETS.iter().find(|(p, _, _)| *p == path).map(|(_, m, b)| (*m, *b))
}

/// Answer with a bundled file of the TV page (404 when there is none of that name).
pub fn serve_bundled(s: TcpStream, path: &str) {
    match asset(path) {
        Some((mime, bytes)) => respond(s, "200 OK", mime, bytes),
        None => respond(s, "404 Not Found", "text/plain", b"not found"),
    }
}

/// The TV routes. `serve` answers with one of the page's files by name ("tv.html"). The stream back
/// when the request is for something else (the caller answers it).
pub fn route(req: &Request, s: TcpStream, cfg: &Mutex<Config>, live: &Live, serve: &dyn Fn(TcpStream, &str)) -> Option<TcpStream> {
    let (path, query) = (req.path.as_str(), req.query.as_str());
    match path {
        // the TV app's page: the overlays over the whole picture, for a connected TV only (its key in t=)
        // fork: (and /tv/drawn, the TV page telling it drew a state: the measurement log's)
        "/tv" | "/tv.json" | "/tv/forget" | "/tv/drawn" if !live.tvs.allowed(param(query, "t")) => respond(s, "403 Forbidden", "text/plain", b"not connected"),
        "/tv" => {
            live.tvs.touch(param(query, "t"));
            serve(s, "tv.html")
        }
        "/tv.json" => {
            live.tvs.touch(param(query, "t"));
            // fork: which TV it is answered for (measurement log)
            crate::fork::tv_timing::for_tv(param(query, "t"), || tv_state(cfg, live, s, param(query, "h")))
        }
        // fork: the TV page drew a state (measurement log, while it's on)
        "/tv/drawn" => {
            crate::fork::tv_timing::drawn(param(query, "t"), param(query, "h"), crate::fork::tv_timing::number(param(query, "draw")));
            respond(s, "200 OK", "text/plain", b"ok")
        }
        // the TV disconnected itself: its key is forgotten
        "/tv/forget" => {
            live.tvs.remove_token(param(query, "t"));
            respond(s, "200 OK", "text/plain", b"ok")
        }
        // a Samsung TV showing a code offers it here (it can't be called: see tv.rs); only the
        // TV app sends X-VIT (a web page can't, and gets nothing)
        "/vit-tv/offer" if req.method == "POST" && req.vit => {
            let (status, body) = live.tvs.offer(&req.body);
            let status = match status {
                200 => "200 OK",
                400 => "400 Bad Request",
                403 => "403 Forbidden",
                429 => "429 Too Many Requests",
                _ => "404 Not Found",
            };
            respond(s, status, "application/json", body.as_bytes())
        }
        _ => return Some(s),
    }
    None
}

/// What the overlays show now, for the TV page, with its version in `X-State`: when the page
/// already has this version (`seen`), the answer waits up to a second for a change (a new line is
/// on the TV at once, and nothing is sent while nothing changes), then is 204 if there was none.
pub fn tv_state(cfg: &Mutex<Config>, live: &Live, mut s: TcpStream, seen: &str) {
    use std::hash::{Hash, Hasher};
    use std::io::Write;
    let start = std::time::Instant::now();
    loop {
        let body = crate::live::overlay_state(cfg, live).to_string();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        body.hash(&mut h);
        let version = format!("{:016x}", h.finish());
        if version != seen {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nX-State: {version}\r\n{}Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
                body.len(),
                crate::fork::tv_timing::ack_header() // fork: the page answers back once drawn (measurement log)
            );
            let _ = s.set_nodelay(true);
            let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(body.as_bytes()));
            crate::fork::tv_timing::sent_now(&version, live); // fork: measurement log
            return;
        }
        if start.elapsed() >= Duration::from_secs(1) {
            let _ = s.write_all(b"HTTP/1.1 204 No Content\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The font families the pages have (fonts.css): what an area's font can be on a device without
/// its own font list.
pub fn bundled_fonts() -> Vec<String> {
    let css = asset("fonts.css").map(|(_, b)| String::from_utf8_lossy(b).to_string()).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for part in css.split("font-family:").skip(1) {
        let name = part.split(';').next().unwrap_or("").trim().trim_matches('"').trim().to_string();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// A request to the standalone server (`TvServer`): the TV routes, the live page while it's on,
/// and the pages' files.
fn handle(cfg: &Mutex<Config>, live: &Live, page: Option<&LivePage>, s: TcpStream) {
    let Some(req) = read_request(&s) else { return };
    // Asked for by an address (this device's, as the TV opens it), not a name: a website can't point a
    // name of its own here and read the page from a browser ("DNS rebinding").
    if !host_is_address(&req.host) {
        return respond(s, "403 Forbidden", "text/plain", b"not allowed");
    }
    let Some(s) = route(&req, s, cfg, live, &serve_bundled) else { return };
    let page = page.filter(|_| cfg.lock().unwrap().lan_page);
    let Some(s) = (match page {
        Some(p) => p.route(&req, s, &serve_bundled),
        None => Some(s),
    }) else {
        return;
    };
    match req.path.as_str() {
        "/common.js" | "/fonts.css" => serve_bundled(s, req.path.trim_start_matches('/')),
        p if p.starts_with("/fonts/") && !p.contains("..") => serve_bundled(s, p.trim_start_matches('/')),
        _ => respond(s, "404 Not Found", "text/plain", b"not found"),
    }
}

/// The TV page served on the home network, on its own (a phone as the "PC"). Stops when dropped
/// (or `stop`).
pub struct TvServer {
    stop: Arc<AtomicBool>,
    pub port: u16,
}

impl TvServer {
    /// Listen on all addresses at `port` (the desktop's default is 8791), in a thread of its own;
    /// with `page`, the live page too while the settings' `lan_page` is on.
    pub fn start(port: u16, cfg: Arc<Mutex<Config>>, live: Arc<Live>, page: Option<Arc<LivePage>>) -> std::io::Result<TvServer> {
        let listener = match TcpListener::bind(("0.0.0.0", port)) {
            Ok(l) => l,
            Err(e) => {
                *live.lan_error.lock().unwrap() = Some(format!("port {port}: {e}"));
                live.lan_running.store(false, Ordering::Relaxed);
                return Err(e);
            }
        };
        listener.set_nonblocking(true)?;
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
        *live.lan_error.lock().unwrap() = None;
        live.lan_running.store(true, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::spawn(move || {
            static ACTIVE: AtomicUsize = AtomicUsize::new(0);
            while !flag.load(Ordering::Relaxed) {
                // accept everything waiting, then look at the stop flag again
                while let Ok((s, _)) = listener.accept() {
                    // (too many at once: closed)
                    let Some(slot) = Slot::take(&ACTIVE) else { continue };
                    let _ = s.set_nonblocking(false);
                    let (c, l, p) = (cfg.clone(), live.clone(), page.clone());
                    std::thread::spawn(move || {
                        let _slot = slot;
                        handle(&c, &l, p.as_deref(), s);
                    });
                }
                std::thread::sleep(Duration::from_millis(60));
            }
            live.lan_running.store(false, Ordering::Relaxed);
        });
        Ok(TvServer { stop, port })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for TvServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn get(port: u16, target: &str) -> (String, Vec<u8>) {
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).unwrap();
        let mut all = Vec::new();
        c.read_to_end(&mut all).unwrap();
        let at = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (String::from_utf8_lossy(&all[..at]).to_string(), all[at + 4..].to_vec())
    }

    #[test]
    fn serves_the_tv_page_to_connected_tvs_only() {
        let cfg = Arc::new(Mutex::new(Config::default()));
        let live = Arc::new(Live::default());
        let dir = std::env::temp_dir().join(format!("vit-tvserver-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        live.tvs.load(dir.join("tvs.json"));
        live.tvs.add(crate::tv::PairedTv { id: "tv1".into(), name: "Living room".into(), token: "key1".into() });
        let server = TvServer::start(0, cfg, live.clone(), None).unwrap();
        assert!(live.lan_running.load(Ordering::Relaxed));
        // not online until its page asks
        assert!(!live.tvs.list()[0].online);
        let (head, _) = get(server.port, "/tv?t=nope");
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
        assert!(!live.tvs.list()[0].online);
        let (head, body) = get(server.port, "/tv?t=key1");
        assert!(head.starts_with("HTTP/1.1 200") && head.contains("text/html"), "{head}");
        assert_eq!(body, asset("tv.html").unwrap().1);
        let (head, body) = get(server.port, "/tv.json?t=key1&h=");
        assert!(head.contains("X-State: "), "{head}");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.get("areas").is_some() && v.get("shown").is_some());
        assert!(live.tvs.list()[0].online);
        // the same version again: nothing new within a second
        let version = head.lines().find_map(|l| l.strip_prefix("X-State: ")).unwrap().trim().to_string();
        let (head, _) = get(server.port, &format!("/tv.json?t=key1&h={version}"));
        assert!(head.starts_with("HTTP/1.1 204"), "{head}");
        let (head, body) = get(server.port, "/fonts/Cairo.ttf");
        assert!(head.starts_with("HTTP/1.1 200") && body.len() > 1000, "{head}");
        let (head, _) = get(server.port, "/live.json");
        assert!(head.starts_with("HTTP/1.1 404"), "{head}");
        let (head, _) = get(server.port, "/tv/forget?t=key1");
        assert!(head.starts_with("HTTP/1.1 200"));
        assert!(!live.tvs.allowed("key1"));
        server.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_local_viewer_key_opens_the_tv_page() {
        let live = Arc::new(Live::default());
        let server = TvServer::start(0, Arc::new(Mutex::new(Config::default())), live.clone(), None).unwrap();
        let key = live.tvs.local_key();
        assert_eq!(key, live.tvs.local_key(), "the same key for the run");
        assert!(get(server.port, &format!("/tv?t={key}")).0.starts_with("HTTP/1.1 200"));
        assert!(live.tvs.list().is_empty(), "not a connected TV");
        let _ = get(server.port, &format!("/tv/forget?t={key}"));
        assert!(live.tvs.allowed(&key), "not forgotten");
    }

    #[test]
    fn the_live_page_is_served_while_it_is_on() {
        let cfg = Arc::new(Mutex::new(Config::default()));
        let live = Arc::new(Live::default());
        let dir = std::env::temp_dir().join(format!("vit-livepage-{}", uuid::Uuid::new_v4().simple()));
        let paths = crate::Paths { data: dir.clone(), db: dir.clone(), config: dir.join("config.json") };
        let page = Arc::new(LivePage { cfg: cfg.clone(), live: live.clone(), paths, sink: Arc::new(crate::NoSink), snapshot: Box::new(|| anyhow::bail!("no picture")) });
        let server = TvServer::start(0, cfg.clone(), live.clone(), Some(page)).unwrap();
        assert!(get(server.port, "/live.json").0.starts_with("HTTP/1.1 404"), "off by default");
        cfg.lock().unwrap().lan_page = true;
        crate::live::push(&live, "subtitles", "Okay...", &[], &[("حسنًا...".into(), None)], 5);
        let (head, body) = get(server.port, "/live.json?after=0");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["lines"][0]["text"], "حسنًا...");
        let (head, body) = get(server.port, "/");
        assert!(head.starts_with("HTTP/1.1 200") && body == asset("live.html").unwrap().1, "{head}");
        assert!(get(server.port, "/i18n/ar.js").0.starts_with("HTTP/1.1 200"));
        assert!(bundled_fonts().iter().any(|f| f == "Cairo"), "{:?}", bundled_fonts());
        server.stop();
    }

    #[test]
    fn every_font_of_the_page_is_bundled() {
        let css = std::str::from_utf8(asset("fonts.css").unwrap().1).unwrap();
        for part in css.split("url(\"").skip(1) {
            let file = part.split('"').next().unwrap();
            assert!(asset(file).is_some(), "{file}");
        }
    }
}

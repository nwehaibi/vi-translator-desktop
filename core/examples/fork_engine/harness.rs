//! fork: this copy's reading does what the original's does (see FORK.md). The same made-up sessions are
//! played through the original's core (built from `.fork/base-commit`) and through this copy's, with the
//! measurement log off and on. Each run writes a transcript, and two transcripts are compared:
//! - every event the app's window is sent (each kind in its own order);
//! - every answer a TV gets for its overlays (`/tv.json`), with one TV switched off at the end;
//! - what the overlays and the home-network page show when the session ends;
//! - the memory's answers for every line played and its counts, in the session and from its file loaded
//!   again; and the files the reading writes in the data folder.
//!
//! Only what is the same from run to run is kept: the times in the events (`at`: when a line's English
//! appeared, Unix ms; the milliseconds in a translation's note), the app's start time (`build`), a TV
//! state's hash (`X-State`) and the memory file's bytes (sealed afresh on each save) are left out.
//! The sessions keep clear of the timing edges: each wait the reader can make (Clear after, its floors,
//! Text stability) is at least 40 ms from a reading pass, so two runs of the same core tell the same story.
//!
//! This file builds against the original's core as it is: it uses nothing of the copy's own (the copy's
//! log is switched on around the same sessions by `fork_engine_equivalence_log.rs`, its records "log").

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::Digest as _;
use vi_translator_core::config::Config;
use vi_translator_core::engine::{self, Context, Input};
use vi_translator_core::frame::FrameData;
use vi_translator_core::live::Live;
use vi_translator_core::memory::Memory;
use vi_translator_core::ocr::{Ocr, RawWord, TextEngine};
use vi_translator_core::tv_server::TvServer;
use vi_translator_core::{Paths, Sink};

const W: u32 = 1920;
const H: u32 = 1080;
/// The TV the sessions' overlays go to (its key never leaves this run).
const TV_KEY: &str = "equivalence-tv-key";

/// What a variant does around the sessions (the log variant switches the copy's log on).
pub struct Hooks {
    /// before a session is played (the session's own folder)
    pub start: fn(&Path),
    /// a frame captured: its number, and when (Unix ms)
    pub frame: fn(u64, u64),
    /// after a session: records of the variant's own (kind "log"), compared only when asked
    pub end: fn(&Path) -> Vec<Value>,
}

#[allow(dead_code)]
pub const NO_HOOKS: Hooks = Hooks { start: |_| {}, frame: |_, _| {}, end: |_| Vec::new() };

/// The OCR: the line it's told, centred at the bottom of whatever it's given (where GTA VI's line 1 is).
struct Scripted(Arc<Mutex<Option<String>>>);

impl TextEngine for Scripted {
    fn read(&self, _bgra: &[u8], w: u32, h: u32) -> anyhow::Result<Vec<RawWord>> {
        let Some(line) = self.0.lock().unwrap().clone() else { return Ok(Vec::new()) };
        let words: Vec<String> = line.split(' ').map(str::to_string).collect();
        let (ww, lh) = (w as f64 * 0.05, 24.0);
        let start = w as f64 / 2.0 - ww * words.len() as f64 / 2.0;
        Ok(words.iter().enumerate().map(|(i, t)| RawWord { line: 0, text: t.clone(), x: start + i as f64 * ww, y: h as f64 - lh * 1.5, w: ww * 0.9, h: lh }).collect())
    }
    fn max_dim(&self) -> u32 {
        4000
    }
}

/// The window's events, each kind in its own order (they come from more than one thread: two kinds
/// sent at nearly the same moment can come in either order).
#[derive(Default)]
struct Recorder(Mutex<BTreeMap<String, Vec<Value>>>);

impl Sink for Recorder {
    fn emit(&self, event: &str, mut payload: Value) {
        norm(&mut payload);
        self.0.lock().unwrap().entry(event.to_string()).or_default().push(payload);
    }
}

/// Leave out what differs from run to run by its nature: times (`at`), the app's start (`build`), and the
/// milliseconds in a translation's note for the window (`stats`: "memory hit · ready 16 ms after the
/// English" — the words kept, the number not).
fn norm(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.remove("at");
            m.remove("build");
            if let Some(Value::String(st)) = m.get_mut("stats") {
                *st = without_ms(st);
            }
            m.values_mut().for_each(norm);
        }
        Value::Array(a) => a.iter_mut().for_each(norm),
        _ => {}
    }
}

/// "ready 16 ms after" → "ready N ms after" (a number just before " ms").
fn without_ms(s: &str) -> String {
    let mut out = String::new();
    let mut num = String::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        if c.is_ascii_digit() || (c == '.' && !num.is_empty()) {
            num.push(c);
            continue;
        }
        if !num.is_empty() {
            out.push_str(if c == ' ' && rest.starts_with("ms") { "N" } else { &num });
            num.clear();
        }
        out.push(c);
    }
    out.push_str(&num);
    out
}

/// A 5×7 font of capitals: enough to draw a subtitle a picture of text shows.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'W' => [17, 17, 17, 21, 21, 27, 17],
        _ => [0; 7],
    }
}

/// A frame: dark, with `text` drawn where the subtitles' line 1 is (white over a dark box), or none.
fn frame(text: Option<&str>, seq: u64) -> Arc<FrameData> {
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for px in rgba.chunks_mut(4) {
        px.copy_from_slice(&[35, 38, 45, 255]);
    }
    if let Some(text) = text {
        let scale = 3u32;
        let (cw, ch) = (6 * scale, 7 * scale);
        let tw = cw * text.chars().count() as u32;
        let (x0, y0) = ((W - tw) / 2, (0.896 * H as f64) as u32 - ch / 2);
        let mut put = |x: u32, y: u32, c: [u8; 3]| {
            let i = ((y * W + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&c);
        };
        for y in y0 - 6..y0 + ch + 6 {
            for x in x0 - 9..x0 + tw + 9 {
                put(x, y, [20, 22, 30]);
            }
        }
        for (n, c) in text.chars().enumerate() {
            for (row, bits) in glyph(c).iter().enumerate() {
                for col in 0..5u32 {
                    if bits & (16 >> col) != 0 {
                        for dy in 0..scale {
                            for dx in 0..scale {
                                put(x0 + n as u32 * cw + col * scale + dx, y0 + row as u32 * scale + dy, [240, 240, 240]);
                            }
                        }
                    }
                }
            }
        }
    }
    Arc::new(FrameData { w: W, h: H, rgba, seq })
}

/// One moment of a session: the text drawn on the frame (None: none), what the OCR reads off it (None:
/// nothing), or no picture at all; and whether a TV asks for its overlays then.
#[derive(Clone)]
struct Moment {
    drawn: Option<String>,
    read: Option<String>,
    no_picture: bool,
    tv: bool,
}

/// A line up for `n` frames, read as it is; a TV asks at its 8th frame (it's up on the TV by then).
fn line(en: &str, n: usize) -> Vec<Moment> {
    (0..n).map(|k| Moment { drawn: Some(en.into()), read: Some(en.into()), no_picture: false, tv: k == 8 }).collect()
}

fn gap(n: usize) -> Vec<Moment> {
    vec![Moment { drawn: None, read: None, no_picture: false, tv: false }; n]
}

/// A session's lines: (English, the memory knows it with its speaker's name, without it).
struct Session {
    name: &'static str,
    frame_ms: u64,
    /// the reading's timing set by hand (interval, Text stability, Clear after), or None: the app's own
    custom: Option<(u64, u64, u64)>,
    known: Vec<(String, bool, bool)>,
    moments: Vec<Moment>,
}

fn script(name: &'static str, frame_ms: u64, custom: Option<(u64, u64, u64)>) -> Session {
    let l = |i: u32| format!("Tourists: Line number {i} is on the screen");
    let mut m = Vec::new();
    let mut known = Vec::new();
    // 1. a line the memory knows
    m.extend(line(&l(1), 16));
    known.push((l(1), true, true));
    m.extend(gap(5));
    // 2. an OCR blink of 2 frames in its middle (shorter than any Clear after: it stays up)
    let mut b = line(&l(2), 16);
    b[10..12].iter_mut().for_each(|x| x.read = None);
    m.extend(b);
    known.push((l(2), true, true));
    m.extend(gap(5));
    // 3. a blink of 5 frames (longer than every Clear after here: it goes, and comes back)
    let mut b = line(&l(3), 18);
    b[10..15].iter_mut().for_each(|x| x.read = None);
    m.extend(b);
    known.push((l(3), true, true));
    m.extend(gap(5));
    // 4. misread for its first 2 frames
    let mut b = line(&l(4), 16);
    b[0..2].iter_mut().for_each(|x| x.read = Some("Tourists: Lome mumbor 4 os an tha scraan".into()));
    m.extend(b);
    known.push((l(4), true, true));
    m.extend(gap(5));
    // 5. a line the memory doesn't know (this build's translator: none here, an error)
    m.extend(line("Strangers: Totally fresh words never seen", 16));
    m.extend(gap(5));
    // 6. no picture at all for 5 passes in its middle (the source gone a moment)
    let mut b = line(&l(6), 18);
    b[10..15].iter_mut().for_each(|x| x.no_picture = true);
    m.extend(b);
    known.push((l(6), true, true));
    m.extend(gap(5));
    // 7. a reading that isn't words, 8. a too-short one
    m.extend(line("@@ ## %% &&", 10));
    m.extend(gap(5));
    m.extend(line("Ok", 10));
    m.extend(gap(5));
    // 9. a line the memory knows without its speaker's name only
    m.extend(line("Jason: Line number 9 is on the screen", 16));
    known.push(("Jason: Line number 9 is on the screen".into(), false, true));
    m.extend(gap(5));
    // 10., 11. and 11b.: lines straight after one another, no gap — 11 a digit away from 10 (the reader
    // takes it for the same line read again), 11b another line altogether (it takes the place of the one up)
    m.extend(line(&l(10), 16));
    m.extend(line(&l(11), 16));
    m.extend(line("Locals: Another line straight after it", 16));
    known.push((l(10), true, true));
    known.push((l(11), true, true));
    known.push(("Locals: Another line straight after it".into(), true, true));
    m.extend(gap(5));
    // 12. the last line, and the screen empty after it
    m.extend(line(&l(12), 16));
    known.push((l(12), true, true));
    m.extend(gap(20));
    Session { name, frame_ms, custom, known, moments: m }
}

/// The sessions: the player's own timing (Clear after 70: 250 for the subtitles, Text stability 0) at a
/// frame every 100 ms, and the app's own (1.5.0: Clear after 300, Text stability 80) every 200 ms.
fn sessions() -> Vec<Session> {
    vec![script("player-timing", 100, Some((16, 0, 70))), script("app-timing", 200, None)]
}

/// A request to the TV server: its status line, its headers (name → value), its body.
fn get(port: u16, target: &str) -> (String, BTreeMap<String, String>, String) {
    let mut c = TcpStream::connect(("127.0.0.1", port)).expect("the TV server");
    c.write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).unwrap();
    let mut all = Vec::new();
    c.read_to_end(&mut all).unwrap();
    let all = String::from_utf8_lossy(&all).to_string();
    let (head, body) = all.split_once("\r\n\r\n").unwrap_or((&all, ""));
    let mut lines = head.lines();
    let status = lines.next().unwrap_or("").to_string();
    let headers = lines.filter_map(|l| l.split_once(": ")).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    (status, headers, body.to_string())
}

/// A TV's answer, as compared: the state's hash and the body's length (times in it) left out; the copy's
/// log has the TV page answer back (`X-Ack`): a record of its own ("log").
fn tv_answer(port: u16, label: &str, out: &mut Vec<Value>) {
    let (status, mut headers, body) = get(port, &format!("/tv.json?t={TV_KEY}&h="));
    if headers.remove("X-Ack").is_some() {
        out.push(json!({ "kind": "log", "tv_ack": label }));
    }
    if let Some(v) = headers.get_mut("X-State") {
        *v = "<state>".into();
    }
    headers.remove("Content-Length");
    let mut body: Value = serde_json::from_str(&body).unwrap_or(Value::String(body));
    norm(&mut body);
    out.push(json!({ "kind": "tv", "at": label, "status": status, "headers": headers, "body": body }));
}

/// Every file under `dir`, by its path from `dir`, with its SHA-256.
fn files(dir: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, d: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let b = std::fs::read(&p).unwrap_or_default();
                let h: String = sha2::Sha256::digest(&b).iter().map(|x| format!("{x:02x}")).collect();
                out.insert(p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"), h);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn play(s: &Session, dir: &Path, hooks: &Hooks) -> Vec<Value> {
    let _ = std::fs::remove_dir_all(dir);
    let paths = Paths { data: dir.join("data"), db: dir.join("db"), config: dir.join("config.json") };
    std::fs::create_dir_all(&paths.data).unwrap();
    std::fs::create_dir_all(&paths.db).unwrap();
    let mut memory = Memory::load(paths.db.clone(), "ar");
    for (i, (en, with_name, sentence)) in s.known.iter().enumerate() {
        let ar = vec![(format!("سطر {}", i + 1), None)];
        if *sentence {
            let sentence = en.split_once(": ").map(|(_, t)| t.to_string()).unwrap_or(en.clone());
            memory.put(&sentence, &ar, "reviewed");
        }
        if *with_name {
            memory.put(en, &ar, "reviewed");
        }
    }
    let mut cfg = Config::default();
    for a in cfg.areas.iter_mut() {
        a.enabled = a.id == "subtitles";
    }
    if let Some((interval, stable, clear)) = s.custom {
        cfg.timing_custom = Some(true);
        cfg.interval_ms = interval;
        cfg.stable_ms = stable;
        cfg.clear_after_ms = clear;
    }
    let cfg = Arc::new(Mutex::new(cfg));
    let live = Arc::new(Live::default());
    let tvs = dir.join("tvs.json");
    std::fs::write(&tvs, json!([{ "id": "tv-1", "name": "TV", "token": TV_KEY, "off": false }]).to_string()).unwrap();
    live.tvs.load(tvs);
    let server = TvServer::start(0, cfg.clone(), live.clone(), None).expect("the TV server");
    let rec = Arc::new(Recorder::default());
    let ctx = Context { cfg: cfg.clone(), live: live.clone(), memory: Arc::new(Mutex::new(memory)), sink: rec.clone(), paths: paths.clone() };
    let said = Arc::new(Mutex::new(None));
    let ocr = Ocr::with_engine(Box::new(Scripted(said.clone())));
    (hooks.start)(dir);
    let mut out = Vec::new();
    let mut reader = engine::start(ctx.clone());
    let t0 = Instant::now();
    for (k, m) in s.moments.iter().enumerate() {
        let seq = k as u64 + 1;
        let due = t0 + Duration::from_millis(s.frame_ms * seq);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if m.no_picture {
            let _ = reader.step(Input::Nothing);
        } else {
            let f = frame(m.drawn.as_deref(), seq);
            (hooks.frame)(seq, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64);
            *said.lock().unwrap() = m.read.clone();
            let _ = reader.step(Input::Frame(&f, &ocr));
        }
        if m.tv {
            tv_answer(server.port, &format!("frame {seq}"), &mut out);
        }
    }
    drop(reader);
    std::thread::sleep(Duration::from_millis(300));
    tv_answer(server.port, "the end", &mut out);
    // (the TV switched off in the app: it's sent the overlays off)
    live.tvs.set_on("tv-1", false);
    tv_answer(server.port, "the end, the TV switched off", &mut out);
    server.stop();
    for (event, list) in std::mem::take(&mut *rec.0.lock().unwrap()) {
        for (i, payload) in list.into_iter().enumerate() {
            out.push(json!({ "kind": "event", "event": event, "n": i, "payload": payload }));
        }
    }
    let mut shown = serde_json::to_value(live.shown.lock().unwrap().clone()).unwrap();
    let mut on_screen = serde_json::to_value(live.on_screen.lock().unwrap().clone()).unwrap();
    let mut covers = serde_json::to_value(live.covers.lock().unwrap().clone()).unwrap();
    let mut recent = serde_json::to_value(live.live_log.lock().unwrap().iter().cloned().collect::<Vec<_>>()).unwrap();
    for v in [&mut shown, &mut on_screen, &mut covers, &mut recent] {
        norm(v);
    }
    out.push(json!({ "kind": "live", "shown": shown, "on_screen": on_screen, "covers": covers, "recent": recent }));
    // The memory: its answers for every line read, its counts; and the same from its file loaded again (the
    // file itself is sealed with a fresh nonce on each save: its bytes differ from run to run, never what
    // it keeps).
    let mut asked: Vec<String> = s.moments.iter().filter_map(|m| m.read.clone()).collect();
    asked.dedup();
    let answers = |m: &Memory| -> Vec<Value> { asked.iter().map(|en| json!([en, m.get(en, &[], &[])])).collect() };
    {
        let m = ctx.memory.lock().unwrap();
        let st = m.stats();
        out.push(json!({ "kind": "memory", "in": "the session", "entries": st.entries, "reviewed": st.reviewed, "hits": st.hits, "misses": st.misses, "answers": answers(&m) }));
    }
    drop(ctx);
    let again = Memory::load(paths.db.clone(), "ar");
    let st = again.stats();
    out.push(json!({ "kind": "memory", "in": "its file loaded again", "entries": st.entries, "reviewed": st.reviewed, "answers": answers(&again) }));
    let db: Vec<String> = files(&paths.db).into_keys().collect();
    let (log, data): (BTreeMap<_, _>, BTreeMap<_, _>) = files(&paths.data).into_iter().partition(|(p, _)| p.starts_with("lines/"));
    out.push(json!({ "kind": "files", "data": data, "db": db }));
    if !log.is_empty() {
        out.push(json!({ "kind": "log", "files": log.len() }));
    }
    out.extend((hooks.end)(dir));
    for r in out.iter_mut() {
        r["session"] = json!(s.name);
    }
    out
}

fn usage() -> ! {
    eprintln!("usage: play <transcript.jsonl> [--core original|copy] | compare <a.jsonl> <b.jsonl> [--ignore-log]");
    std::process::exit(2)
}

/// Which core this was built against: this copy's answers a TV's `/tv/drawn`, the original's doesn't
/// know it. (Two checkouts of the workspace built in one target folder can take each other's core
/// for their own — the same package, the same paths in it: a run says which it has.)
fn core_is_the_copys() -> bool {
    let dir = std::env::temp_dir().join(format!("vit-fork-engine-core-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tvs = dir.join("tvs.json");
    std::fs::write(&tvs, json!([{ "id": "tv-1", "name": "TV", "token": TV_KEY, "off": false }]).to_string()).unwrap();
    let live = Arc::new(Live::default());
    live.tvs.load(tvs);
    let server = TvServer::start(0, Arc::new(Mutex::new(Config::default())), live, None).expect("the TV server");
    let (status, _, _) = get(server.port, &format!("/tv/drawn?t={TV_KEY}&h=0&draw=0"));
    server.stop();
    let _ = std::fs::remove_dir_all(dir);
    status.contains(" 200 ")
}

fn read_lines(p: &str) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}")).lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

pub fn main(hooks: Hooks) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("play") => {
            let Some(out) = args.get(1) else { usage() };
            let copy = core_is_the_copys();
            eprintln!("the core: {}", if copy { "this copy's" } else { "the original's" });
            if let Some(want) = args.iter().position(|a| a == "--core").and_then(|i| args.get(i + 1)) {
                if (want == "copy") != copy {
                    eprintln!("not the core asked for ({want}): built against the other one?");
                    std::process::exit(3);
                }
            }
            let base = std::env::temp_dir().join(format!("vit-fork-engine-eq-{}", std::process::id()));
            let mut all = Vec::new();
            for s in sessions() {
                let t = Instant::now();
                let records = play(&s, &base.join(s.name), &hooks);
                let count = |k: &str| records.iter().filter(|r| r["kind"] == k).count();
                eprintln!("{}: {} events, {} TV answers ({:.0} s)", s.name, count("event"), count("tv"), t.elapsed().as_secs_f64());
                all.extend(records);
            }
            let text: String = all.iter().map(|r| r.to_string() + "\n").collect();
            std::fs::write(out, text).unwrap();
            let _ = std::fs::remove_dir_all(base);
        }
        Some("compare") => {
            let (Some(a), Some(b)) = (args.get(1), args.get(2)) else { usage() };
            let ignore_log = args.iter().any(|x| x == "--ignore-log");
            let keep = |v: Vec<Value>| -> Vec<Value> { v.into_iter().filter(|r| !(ignore_log && r["kind"] == "log")).collect() };
            let (ra, rb) = (keep(read_lines(a)), keep(read_lines(b)));
            let mut differ = 0;
            for i in 0..ra.len().max(rb.len()) {
                let (x, y) = (ra.get(i), rb.get(i));
                if x != y {
                    differ += 1;
                    if differ <= 12 {
                        eprintln!("record {i} differs:\n  {a}: {}\n  {b}: {}", x.map(|v| v.to_string()).unwrap_or("(none)".into()), y.map(|v| v.to_string()).unwrap_or("(none)".into()));
                    }
                }
            }
            let kinds = |r: &[Value]| -> BTreeMap<String, usize> {
                let mut m = BTreeMap::new();
                for x in r {
                    *m.entry(x["kind"].as_str().unwrap_or("").to_string()).or_default() += 1;
                }
                m
            };
            eprintln!("{} and {} records ({:?}); {differ} differ", ra.len(), rb.len(), kinds(&ra));
            std::process::exit(if differ == 0 { 0 } else { 1 });
        }
        _ => usage(),
    }
}

#[allow(dead_code)]
pub fn data_dir(session_dir: &Path) -> PathBuf {
    session_dir.join("data")
}

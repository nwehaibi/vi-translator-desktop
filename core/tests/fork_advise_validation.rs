//! fork: the log's measurements and the analyzer's model checked against the reader itself (see FORK.md).
//!
//! A made-up session the truth of which is known — lines up and gone at known frames, OCR blinks of 1, 3, 5
//! and 8 frames while a line is up (its text still drawn: the unread-text finder sees it), read by the
//! app's own reader at 20 frames a second — is played with Clear after at 70 ms and again at 300 ms:
//! - the English's appearance the log tells (between two frames) holds each line's true first frame;
//! - what the analyzer predicts from the 70 ms session for 300 ms (flickers; how much later the Arabic goes)
//!   is what the 300 ms session does;
//! - the log costs the reading next to nothing: the same session with the log off and on, each reading's
//!   time and each line's way to the screen compared.
//!
//! In real time (about two minutes): `cargo test -p vi-translator-core --test fork_advise_validation -- --ignored --nocapture`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use vi_translator_core::config::Config;
use vi_translator_core::engine::{self, Context, Input};
use vi_translator_core::fork::{advise, linelog};
use vi_translator_core::frame::FrameData;
use vi_translator_core::live::Live;
use vi_translator_core::memory::Memory;
use vi_translator_core::ocr::{Ocr, RawWord, TextEngine};
use vi_translator_core::{Paths, Sink};

const FRAME_MS: u64 = 50;
const W: u32 = 1920;
const H: u32 = 1080;

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

/// When each line went up on screen (Arabic sent to the overlays), by its English.
#[derive(Default)]
struct Shown(Mutex<Vec<(Instant, String, bool)>>);

impl Sink for Shown {
    fn emit(&self, event: &str, payload: Value) {
        if event == "translation" {
            let up = payload["segments"].as_array().is_some_and(|s| !s.is_empty());
            self.0.lock().unwrap().push((Instant::now(), payload["en"].as_str().unwrap_or("").to_string(), up));
        }
    }
}

/// A 5×7 font of capitals: enough to draw a subtitle the unread-text finder sees.
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
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'W' => [17, 17, 17, 21, 21, 27, 17],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'X' => [17, 17, 10, 4, 10, 17, 17],
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

/// The session: (line, frames it's up, OCR blink of n frames in its middle or none), with 12 frames of
/// nothing between lines.
fn script() -> Vec<(String, u64, u64)> {
    let blinks = [0u64, 1, 3, 5, 8, 0, 1, 3, 5, 8, 0, 3, 8, 1, 5, 0];
    blinks.iter().enumerate().map(|(i, &b)| (format!("Tourists: Line number {i} is on the screen"), 40, b)).collect()
}

/// Truth of a run: each line's first frame's capture time; what the app showed when (the sink); each
/// reading pass's own time.
struct Run {
    events: Vec<Value>,
    first_frame: Vec<(String, u64)>,
    shown: Vec<(Instant, String, bool)>,
    up_at: Vec<(String, Instant)>,
    step_us: Vec<f64>,
}

fn run(dir: &std::path::Path, clear_after: u64, log: bool) -> Run {
    let _ = std::fs::remove_dir_all(dir);
    let paths = Paths { data: dir.join("data"), db: dir.join("db"), config: dir.join("config.json") };
    std::fs::create_dir_all(&paths.data).unwrap();
    std::fs::create_dir_all(&paths.db).unwrap();
    let mut memory = Memory::load(paths.db.clone(), "ar");
    // (the memory keeps a subtitle's sentence without its speaker's name: the app looks it up so)
    for (line, _, _) in script() {
        let sentence = line.split_once(": ").map(|(_, s)| s.to_string()).unwrap_or(line.clone());
        memory.put(&sentence, &vec![("سطر".to_string(), None)], "reviewed");
        memory.put(&line, &vec![("سطر".to_string(), None)], "reviewed");
    }
    let mut cfg = Config::default();
    for a in cfg.areas.iter_mut() {
        a.enabled = a.id == "subtitles";
    }
    cfg.timing_custom = Some(true);
    cfg.interval_ms = 16;
    cfg.stable_ms = 0;
    cfg.clear_after_ms = clear_after;
    let shown = Arc::new(Shown::default());
    let ctx = Context { cfg: Arc::new(Mutex::new(cfg)), live: Arc::new(Live::default()), memory: Arc::new(Mutex::new(memory)), sink: shown.clone(), paths };
    let said = Arc::new(Mutex::new(None));
    let ocr = Ocr::with_engine(Box::new(Scripted(said.clone())));
    let events = Arc::new(Mutex::new(Vec::new()));
    if log {
        linelog::set_collector(Some(events.clone()));
    }
    let mut reader = engine::start(ctx);
    let (mut seq, mut first_frame, mut up_at, mut step_us) = (0u64, Vec::new(), Vec::new(), Vec::new());
    let t0 = Instant::now();
    let mut tick = |text: Option<&str>, read: bool, seq: &mut u64| -> (u64, Instant) {
        *seq += 1;
        let due = t0 + Duration::from_millis(FRAME_MS * *seq);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let f = frame(text, *seq);
        let captured = linelog::unix_ms();
        linelog::note_frame(*seq, captured);
        *said.lock().unwrap() = if read { text.map(str::to_string) } else { None };
        let s0 = Instant::now();
        let _ = reader.step(Input::Frame(&f, &ocr));
        step_us.push(s0.elapsed().as_secs_f64() * 1e6);
        (captured, Instant::now())
    };
    for _ in 0..10 {
        tick(None, true, &mut seq);
    }
    for (line, frames, blink) in script() {
        let mid = frames / 2;
        for k in 0..frames {
            let read = blink == 0 || k < mid || k >= mid + blink;
            let (cap, at) = tick(Some(&line), read, &mut seq);
            if k == 0 {
                first_frame.push((line.clone(), cap));
                up_at.push((line.clone(), at));
            }
        }
        for _ in 0..12 {
            tick(None, true, &mut seq);
        }
    }
    for _ in 0..20 {
        tick(None, true, &mut seq);
    }
    drop(reader);
    std::thread::sleep(Duration::from_millis(200));
    if log {
        linelog::set_collector(None);
    }
    let ev = events.lock().unwrap().clone();
    let shown = shown.0.lock().unwrap().clone();
    Run { events: ev, first_frame, shown, up_at, step_us }
}

fn seg(r: &Value) -> &Value {
    &r["segments"][0]
}

#[test]
#[ignore = "real time, about two minutes: run with --ignored"]
fn fork_the_analyzers_model_holds_against_the_reader() {
    let base = std::env::temp_dir().join(format!("vit-fork-advise-{}", std::process::id()));
    let o = advise::Options::default();
    let script = script();
    // flickers expected: a blink longer than Clear after hides the line (and it comes back)
    let expect = |ca: u64| script.iter().filter(|(_, _, b)| *b > 0 && b * FRAME_MS > ca).count() as u64;

    let a = run(&base.join("a"), 70, true);
    if let Some(dump) = std::env::var_os("FORK_DUMP") {
        std::fs::write(dump, a.events.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    }
    let ra = advise::analyze(&a.events, &o);
    if std::env::var_os("FORK_ONLY_A").is_some() {
        return;
    }
    let b = run(&base.join("b"), 300, true);
    let rb = advise::analyze(&b.events, &o);
    let c = run(&base.join("c"), 70, false);

    // 1. the English's appearance: each line's true first frame within the two frames the log says
    let sent: Vec<&Value> = a.events.iter().filter(|e| e["ev"] == "sent").collect();
    let mut held = 0;
    for (line, truth) in &a.first_frame {
        let s = sent.iter().find(|e| e["en"] == line.as_str()).unwrap_or_else(|| panic!("{line} not sent"));
        let (after, by) = (s["appear_after"].as_u64().unwrap(), s["appear_by"].as_u64().unwrap());
        assert!(after < *truth && *truth <= by, "{line}: truth {truth} not in ({after}, {by}]");
        held += 1;
    }
    eprintln!("appearance: {held} of {} lines' true first frame within the log's two frames", a.first_frame.len());

    // 2. flickers: as the script makes them, at each Clear after
    let flickers = |r: &Value| seg(r)["flicker"]["by_cause"].as_object().map(|m| m.values().filter_map(|v| v.as_u64()).sum::<u64>()).unwrap_or(0);
    eprintln!("flickers at 70: {} (expected {}); at 300: {} (expected {})", flickers(&ra), expect(70), flickers(&rb), expect(300));
    assert_eq!(flickers(&ra), expect(70));
    assert_eq!(flickers(&rb), expect(300));
    // (an OCR blink over text still drawn: an OCR miss, by the unread-text finder)
    assert_eq!(seg(&ra)["flicker"]["by_cause"]["ocr_miss"].as_u64(), Some(expect(70)), "{}", seg(&ra)["flicker"]);

    // 3. the model: what the 70 ms session predicts for 300 ms is what 300 ms did
    let advice_a = ra["advice"].as_array().unwrap().iter().find(|x| x["setting"] == "clear_after_ms").unwrap().clone();
    let row = |ca: f64| advice_a["evidence"]["table"].as_array().unwrap().iter().find(|r| r["clear_after_ms"].as_f64() == Some(ca)).cloned().unwrap();
    let predicted = row(300.0)["flickers"].as_f64().unwrap() as u64;
    eprintln!("predicted from the 70 ms session for 300 ms: {predicted} flickers (it did {})", flickers(&rb));
    assert_eq!(predicted, flickers(&rb));
    let (end_a, end_b) = (seg(&ra)["sync"]["end_here_ms"]["p50"].as_f64().unwrap(), seg(&rb)["sync"]["end_here_ms"]["p50"].as_f64().unwrap());
    let shift = end_b - end_a;
    eprintln!("the Arabic's end after the English's: {end_a} ms at 70, {end_b} ms at 300 (predicted +230, measured {shift:+.0})");
    assert!((shift - 230.0).abs() <= 60.0, "{shift}");
    eprintln!("advice from the 70 ms session: {} → {} ({})", advice_a["now"], advice_a["value"], advice_a["confidence"]);

    // 4. the log's own cost: each reading's time, and each line's way to the screen, log off and on
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let latency = |r: &Run| {
        let mut out = Vec::new();
        for (line, up) in &r.up_at {
            if let Some((t, _, _)) = r.shown.iter().find(|(t, en, on)| en == line && *on && t >= up) {
                out.push(t.duration_since(*up).as_secs_f64() * 1000.0);
            }
        }
        out
    };
    let (on_step, off_step) = (mean(&a.step_us), mean(&c.step_us));
    let (on_lat, off_lat) = (mean(&latency(&a)), mean(&latency(&c)));
    eprintln!("a reading pass: {off_step:.0} µs with the log off, {on_step:.0} µs on (+{:.0} µs); a line to the screen: {off_lat:.1} ms off, {on_lat:.1} ms on", on_step - off_step);
    // (the pass here reads a made-up frame in no time: the log's own work stands out the most here)
    assert!(on_step - off_step < 2000.0, "the log adds under 2 ms a pass");
    assert!(on_lat - off_lat < 15.0, "a line reaches the screen as fast with the log on");
    let _ = std::fs::remove_dir_all(base);
}

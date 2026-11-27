//! fork: the log's measurements and the analyzer's model checked against the reader itself (see FORK.md).
//!
//! A made-up session the truth of which is known — lines up and gone at known frames, OCR blinks of 1, 3, 5
//! and 8 frames while a line is up (its text still drawn: the unread-text finder sees it), read by the
//! app's own reader at 20 frames a second — is played with Clear after at 70 ms and again at 230 ms:
//! - the English's appearance the log tells (between two frames) holds each line's true first frame;
//! - what the analyzer predicts from the 70 ms session for 230 ms (flickers; how much later the Arabic goes)
//!   is what the 230 ms session does;
//! - the log costs the reading next to nothing: the same session with the log off and on, each reading's
//!   time and each line's way to the screen compared.
//!
//! And Text stability: lines the memory doesn't know, and misreadings of 2 and 4 frames before lines it
//! knows, at 12.5 frames a second (80 ms: a wait of 100 ms is two passes, not 100 ms), with the stability
//! at 0 and at 100 ms: what the analyzer predicts from the first for the second (misreadings shown, new
//! lines lost, how much later a new line goes) is what the second does.
//!
//! In real time (about four minutes): `cargo test -p vi-translator-core --test fork_advise_validation -- --ignored --nocapture`.

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

/// (the log's collector and the reader's timings are the process's own: one session at a time)
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
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

/// A line of a made-up session: its English (drawn on the frames), the frames it's up, an OCR blink of
/// `blink` frames in its middle (0: none), a misreading the OCR gives for its first frames (or none), and
/// whether the memory knows it.
#[derive(Clone)]
struct Line {
    en: String,
    frames: u64,
    blink: u64,
    misread: Option<(String, u64)>,
    known: bool,
}

/// The Clear after session: 16 lines the memory knows, 40 frames each, OCR blinks of 0–8 frames.
fn script() -> Vec<Line> {
    let blinks = [0u64, 1, 3, 5, 8, 0, 1, 3, 5, 8, 0, 3, 8, 1, 5, 0];
    blinks.iter().enumerate().map(|(i, &b)| Line { en: format!("Tourists: Line number {i} is on the screen"), frames: 40, blink: b, misread: None, known: true }).collect()
}

/// The Text stability session: in turn a line the memory knows misread for its first 2 frames, one misread
/// for its first 4, and a line the memory doesn't know; 30 frames each. (The misreading is far enough from
/// the line for the memory not to take it for it — under 0.86 alike — and near enough for the analyzer to
/// tell it's a reading of it.)
fn stability_script() -> Vec<Line> {
    (0..12u64)
        .map(|i| match i % 3 {
            0 | 1 => Line {
                en: format!("Tourists: Line number {i} is on the screen"),
                frames: 30,
                blink: 0,
                misread: Some((format!("Tourists: Lome mumbor {i} os an tha scraan"), if i % 3 == 0 { 2 } else { 4 })),
                known: true,
            },
            _ => Line { en: format!("Strangers: Totally fresh words {i} never seen"), frames: 30, blink: 0, misread: None, known: false },
        })
        .collect()
}

/// How a session is played: a frame every `frame_ms`, the three reading timings, the log on or off, frames
/// of nothing between lines.
struct Setup {
    frame_ms: u64,
    clear_after: u64,
    stable: u64,
    log: bool,
    between: u64,
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

fn run(dir: &std::path::Path, script: &[Line], set: &Setup) -> Run {
    let _ = std::fs::remove_dir_all(dir);
    let paths = Paths { data: dir.join("data"), db: dir.join("db"), config: dir.join("config.json") };
    std::fs::create_dir_all(&paths.data).unwrap();
    std::fs::create_dir_all(&paths.db).unwrap();
    let mut memory = Memory::load(paths.db.clone(), "ar");
    // (the memory keeps a subtitle's sentence without its speaker's name: the app looks it up so)
    for l in script.iter().filter(|l| l.known) {
        let sentence = l.en.split_once(": ").map(|(_, s)| s.to_string()).unwrap_or(l.en.clone());
        memory.put(&sentence, &vec![("سطر".to_string(), None)], "reviewed");
        memory.put(&l.en, &vec![("سطر".to_string(), None)], "reviewed");
    }
    let mut cfg = Config::default();
    for a in cfg.areas.iter_mut() {
        a.enabled = a.id == "subtitles";
    }
    cfg.timing_custom = Some(true);
    cfg.interval_ms = 16;
    cfg.stable_ms = set.stable;
    cfg.clear_after_ms = set.clear_after;
    let shown = Arc::new(Shown::default());
    let ctx = Context { cfg: Arc::new(Mutex::new(cfg)), live: Arc::new(Live::default()), memory: Arc::new(Mutex::new(memory)), sink: shown.clone(), paths };
    let said = Arc::new(Mutex::new(None));
    let ocr = Ocr::with_engine(Box::new(Scripted(said.clone())));
    let events = Arc::new(Mutex::new(Vec::new()));
    if set.log {
        linelog::set_collector(Some(events.clone()));
    }
    let mut reader = engine::start(ctx);
    let (mut seq, mut first_frame, mut up_at, mut step_us) = (0u64, Vec::new(), Vec::new(), Vec::new());
    let t0 = Instant::now();
    // (a frame with `text` drawn, the OCR reading `read` off it)
    let mut tick = |text: Option<&str>, read: Option<&str>, seq: &mut u64| -> (u64, Instant) {
        *seq += 1;
        let due = t0 + Duration::from_millis(set.frame_ms * *seq);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let f = frame(text, *seq);
        let captured = linelog::unix_ms();
        linelog::note_frame(*seq, captured);
        *said.lock().unwrap() = read.map(str::to_string);
        let s0 = Instant::now();
        let _ = reader.step(Input::Frame(&f, &ocr));
        step_us.push(s0.elapsed().as_secs_f64() * 1e6);
        (captured, Instant::now())
    };
    for _ in 0..10 {
        tick(None, None, &mut seq);
    }
    for l in script {
        let mid = l.frames / 2;
        for k in 0..l.frames {
            let blinking = l.blink > 0 && k >= mid && k < mid + l.blink;
            let read = match &l.misread {
                _ if blinking => None,
                Some((m, n)) if k < *n => Some(m.as_str()),
                _ => Some(l.en.as_str()),
            };
            let (cap, at) = tick(Some(&l.en), read, &mut seq);
            if k == 0 {
                first_frame.push((l.en.clone(), cap));
                up_at.push((l.en.clone(), at));
            }
        }
        for _ in 0..set.between {
            tick(None, None, &mut seq);
        }
    }
    for _ in 0..20 {
        tick(None, None, &mut seq);
    }
    drop(reader);
    std::thread::sleep(Duration::from_millis(200));
    if set.log {
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
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let base = std::env::temp_dir().join(format!("vit-fork-advise-{}", std::process::id()));
    let o = advise::Options::default();
    let script = script();
    let set = |clear_after: u64, log: bool| Setup { frame_ms: FRAME_MS, clear_after, stable: 0, log, between: 12 };
    // flickers expected: a blink longer than Clear after hides the line (and it comes back)
    let expect = |ca: u64| script.iter().filter(|l| l.blink > 0 && l.blink * FRAME_MS > ca).count() as u64;
    // (Clear after B: in the middle of a pass step — 230 is 4.6 passes of 50 ms — where the passes' jitter
    // can't move it; 300, a whole 6 passes, would go one way or the other)
    const B: u64 = 230;

    let a = run(&base.join("a"), &script, &set(70, true));
    if let Some(dump) = std::env::var_os("FORK_DUMP") {
        std::fs::write(dump, a.events.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    }
    let ra = advise::analyze(&a.events, &o);
    if std::env::var_os("FORK_ONLY_A").is_some() {
        return;
    }
    let b = run(&base.join("b"), &script, &set(B, true));
    let rb = advise::analyze(&b.events, &o);
    let c = run(&base.join("c"), &script, &set(70, false));

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
    eprintln!("flickers at 70: {} (expected {}); at {B}: {} (expected {})", flickers(&ra), expect(70), flickers(&rb), expect(B));
    assert_eq!(flickers(&ra), expect(70));
    assert_eq!(flickers(&rb), expect(B));
    // (an OCR blink over text still drawn: an OCR miss, by the unread-text finder)
    assert_eq!(seg(&ra)["flicker"]["by_cause"]["ocr_miss"].as_u64(), Some(expect(70)), "{}", seg(&ra)["flicker"]);

    // 3. the model: what the 70 ms session predicts for B is what B did
    let advice_a = ra["advice"].as_array().unwrap().iter().find(|x| x["setting"] == "clear_after_ms").unwrap().clone();
    let row = |ca: f64| advice_a["evidence"]["table"].as_array().unwrap().iter().find(|r| r["clear_after_ms"].as_f64() == Some(ca)).cloned().unwrap();
    let predicted = row(B as f64)["flickers"].as_f64().unwrap() as u64;
    eprintln!("predicted from the 70 ms session for {B} ms: {predicted} flickers (it did {})", flickers(&rb));
    assert_eq!(predicted, flickers(&rb));
    let (end_a, end_b) = (seg(&ra)["sync"]["end_here_ms"]["p50"].as_f64().unwrap(), seg(&rb)["sync"]["end_here_ms"]["p50"].as_f64().unwrap());
    let shift = end_b - end_a;
    // (the passes' steps: 230 is 4 passes of 50 ms, 70 is 1 — 150 ms later, not the 160 of a model without them)
    let predicted_shift = row(B as f64)["end_lag_p50_ms"].as_f64().unwrap() - row(70.0)["end_lag_p50_ms"].as_f64().unwrap();
    eprintln!("the Arabic's end after the English's: {end_a} ms at 70, {end_b} ms at {B} (predicted {predicted_shift:+.0}, measured {shift:+.0})");
    assert!((shift - predicted_shift).abs() <= 15.0, "predicted {predicted_shift}, measured {shift}");
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

#[test]
#[ignore = "real time, about a minute and a half: run with --ignored"]
fn fork_the_analyzers_stability_model_holds_against_the_reader() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let base = std::env::temp_dir().join(format!("vit-fork-stable-{}", std::process::id()));
    let o = advise::Options::default();
    let script = stability_script();
    // 12.5 frames a second: a pass every 80 ms, so a wait of 100 ms is two passes (160 ms), not 100
    let set = |stable: u64| Setup { frame_ms: 80, clear_after: 70, stable, log: true, between: 10 };
    let s0 = run(&base.join("s0"), &script, &set(0));
    let s100 = run(&base.join("s100"), &script, &set(100));
    let r0 = advise::analyze(&s0.events, &o);

    // what the reader did: the misreadings it sent (shown, then replaced by the line), and each new line's
    // wait from its first reading to its sending
    let misread_sent = |r: &Run| r.events.iter().filter(|e| e["ev"] == "sent" && e["en"].as_str().is_some_and(|t| t.contains("mumbor"))).count() as u64;
    let waits = |r: &Run| -> Vec<f64> {
        script
            .iter()
            .filter(|l| !l.known)
            .filter_map(|l| {
                let at = |k: &str, field: &str| r.events.iter().find(|e| e["ev"] == k && e[field] == l.en.as_str()).and_then(|e| e["t"].as_f64());
                Some(at("sent", "en")? - at("seen", "text")?)
            })
            .collect()
    };
    let new_lines = script.iter().filter(|l| !l.known).count();
    let (w0, w100) = (waits(&s0), waits(&s100));
    assert_eq!((w0.len(), w100.len()), (new_lines, new_lines), "every new line sent: {w0:?} {w100:?}");

    // what the analyzer predicts from the session at 0, for 0 and for 100
    let st = r0["advice"].as_array().unwrap().iter().find(|x| x["setting"] == "stable_ms").unwrap().clone();
    assert_ne!(st["confidence"], "insufficient", "{st}");
    let row = |v: f64| st["evidence"]["table"].as_array().unwrap().iter().find(|r| r["stable_ms"].as_f64() == Some(v)).cloned().unwrap();
    let (p0, p100) = (row(0.0), row(100.0));
    eprintln!(
        "misreadings shown: at 0 {} (predicted {}), at 100 {} (predicted {}); new lines lost at 100: predicted {}",
        misread_sent(&s0),
        p0["premature"],
        misread_sent(&s100),
        p100["premature"],
        p100["lost"]
    );
    assert_eq!(p0["premature"].as_u64(), Some(misread_sent(&s0)));
    assert_eq!(p100["premature"].as_u64(), Some(misread_sent(&s100)));
    // (the 2-frame misreadings not sent at 100, the 4-frame ones still sent: the passes, not the milliseconds)
    assert_eq!((misread_sent(&s0), misread_sent(&s100)), (8, 4));
    assert_eq!(p100["lost"].as_u64(), Some(0));
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let shift = mean(&w100) - mean(&w0);
    let predicted = (p100["added_delay_ms"].as_f64().unwrap() - p0["added_delay_ms"].as_f64().unwrap()) / new_lines as f64;
    eprintln!("a new line's wait: {:.0} ms at 0, {:.0} ms at 100 (predicted {predicted:+.0}, measured {shift:+.0})", mean(&w0), mean(&w100));
    assert!((shift - predicted).abs() <= 20.0, "predicted {predicted}, measured {shift}");
    let _ = std::fs::remove_dir_all(base);
}

//! fork: the measurement log (Phase 0.1) only records. The same line read, translated from the memory,
//! shown and gone, with the log off and on: the app shows exactly the same in each area; with it on, the
//! log tells the line's way (seen, sent, translated from the reviewed line, shown, hidden).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use vi_translator_core::config::Config;
use vi_translator_core::engine::{self, Context, Input};
use vi_translator_core::fork::linelog;
use vi_translator_core::frame::FrameData;
use vi_translator_core::live::Live;
use vi_translator_core::memory::Memory;
use vi_translator_core::ocr::{Ocr, RawWord, TextEngine};
use vi_translator_core::{Paths, Sink};

const LINE: &str = "Meet me at the docks.";
const ARABIC: &str = "قابلني عند الأرصفة.";

/// The line centred at the bottom of whatever it's given (where GTA VI's line 1 is), while `on`.
struct Fake(Arc<AtomicBool>);

impl TextEngine for Fake {
    fn read(&self, _bgra: &[u8], w: u32, h: u32) -> anyhow::Result<Vec<RawWord>> {
        if !self.0.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        let words: Vec<&str> = LINE.split(' ').collect();
        let (ww, lh) = (w as f64 * 0.05, 24.0);
        let start = w as f64 / 2.0 - ww * words.len() as f64 / 2.0;
        Ok(words
            .iter()
            .enumerate()
            .map(|(i, t)| RawWord { line: 0, text: t.to_string(), x: start + i as f64 * ww, y: h as f64 - lh * 1.5, w: ww * 0.9, h: lh })
            .collect())
    }
    fn max_dim(&self) -> u32 {
        4000
    }
}

/// What the app was told to show: (area, English, translation) of each "translation" event.
#[derive(Default)]
struct Shown(Mutex<Vec<(String, String, String)>>);

impl Sink for Shown {
    fn emit(&self, event: &str, payload: Value) {
        if event != "translation" {
            return;
        }
        let text: String = payload["segments"].as_array().map(|s| s.iter().filter_map(|seg| seg[0].as_str()).collect()).unwrap_or_default();
        self.0.lock().unwrap().push((payload["area"].as_str().unwrap_or("").to_string(), payload["en"].as_str().unwrap_or("").to_string(), text));
    }
}

/// The line on screen for a while, then gone: what the app showed.
fn run(dir: &std::path::Path) -> Vec<(String, String, String)> {
    let _ = std::fs::remove_dir_all(dir);
    let paths = Paths { data: dir.join("data"), db: dir.join("db"), config: dir.join("config.json") };
    std::fs::create_dir_all(&paths.data).unwrap();
    std::fs::create_dir_all(&paths.db).unwrap();
    let mut memory = Memory::load(paths.db.clone(), "ar");
    memory.put(LINE, &vec![(ARABIC.to_string(), None)], "reviewed");
    let shown = Arc::new(Shown::default());
    let ctx = Context {
        cfg: Arc::new(Mutex::new(Config::default())),
        live: Arc::new(Live::default()),
        memory: Arc::new(Mutex::new(memory)),
        sink: shown.clone(),
        paths,
    };
    let on = Arc::new(AtomicBool::new(true));
    let ocr = Ocr::with_engine(Box::new(Fake(on.clone())));
    let mut reader = engine::start(ctx);
    let (w, h) = (1920u32, 1080u32);
    let frame = |seq: u64| Arc::new(FrameData { w, h, rgba: vec![0u8; (w * h * 4) as usize], seq });
    let start = Instant::now();
    let mut seq = 0u64;
    let mut step = |until: &dyn Fn(&[(String, String, String)]) -> bool| {
        loop {
            seq += 1;
            linelog::note_frame(seq, linelog::unix_ms());
            let wait = reader.step(Input::Frame(&frame(seq), &ocr));
            if until(&shown.0.lock().unwrap()) || start.elapsed() > Duration::from_secs(60) {
                break;
            }
            std::thread::sleep(wait.min(Duration::from_millis(100)));
        }
    };
    // up until its translation is shown
    step(&|s| s.iter().any(|(a, _, t)| a == "subtitles" && !t.is_empty()));
    std::thread::sleep(Duration::from_millis(300));
    // gone: until every area's translation is taken down (the subtitles' and the bottom instructions',
    // which read it too)
    on.store(false, Ordering::Relaxed);
    step(&|s| {
        let mut last: BTreeMap<&str, bool> = BTreeMap::new();
        for (a, _, t) in s {
            last.insert(a.as_str(), t.is_empty());
        }
        !last.is_empty() && last.values().all(|gone| *gone)
    });
    drop(reader);
    let out = shown.0.lock().unwrap().clone();
    out
}

#[test]
fn fork_the_measurement_log_changes_nothing_shown_and_tells_each_lines_way() {
    let base = std::env::temp_dir().join(format!("vit-fork-measure-{}", std::process::id()));
    // the log off
    let off = run(&base.join("off"));
    assert!(off.iter().any(|(a, en, t)| a == "subtitles" && en == LINE && t == ARABIC), "{off:?}");
    // the log on
    let events = Arc::new(Mutex::new(Vec::new()));
    linelog::set_collector(Some(events.clone()));
    let with_log = run(&base.join("on"));
    linelog::set_collector(None);
    // (each area's in its order: two areas' lines go up and down on timers of their own, either first)
    let by_area = |shown: &[(String, String, String)]| {
        let mut m: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (a, en, t) in shown {
            m.entry(a.clone()).or_default().push((en.clone(), t.clone()));
        }
        m
    };
    assert_eq!(by_area(&off), by_area(&with_log), "the log only records: the app shows exactly the same");

    let ev = events.lock().unwrap().clone();
    let of = |kind: &str| ev.iter().filter(|e| e["ev"] == kind && e["area"] == "subtitles").cloned().collect::<Vec<_>>();
    let seen = of("seen");
    assert!(seen.iter().any(|e| e["text"] == LINE && e["captured"].as_u64().is_some() && e["ocr_ms"].as_f64().is_some()), "{seen:?}");
    let sent = of("sent");
    let line = sent.iter().find(|e| e["en"] == LINE).expect("sent to the translator");
    let seq = line["seq"].as_u64().unwrap();
    assert!(line["at"].as_u64().unwrap() > 0);
    let translated = of("translated").into_iter().find(|e| e["seq"] == seq).expect("translated");
    assert_eq!(translated["ok"], true);
    assert_eq!(translated["hit"], true, "from the memory");
    assert_eq!(translated["sources"][0], "reviewed");
    let up = of("shown").into_iter().find(|e| e["seq"] == seq).expect("shown");
    assert_eq!(up["late"], false);
    let down = of("hidden").into_iter().find(|e| e["seq"] == seq).expect("hidden");
    assert!(down["shown_ms"].as_u64().unwrap() >= 300);
    assert!(ev.iter().any(|e| e["ev"] == "reads" || e["ev"] == "seen"), "readings are counted");
    let _ = std::fs::remove_dir_all(base);
}

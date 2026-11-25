//! fork: the measurement tools (Phases 0.2 and 0.3 of the fork's plan; see FORK.md). Command line only;
//! nothing here runs in the app itself.
//!
//! - `--replay-video <video> [--fps 30] [--rate 1.0] [--tail 5] [--engine <id>] [--gold gold.json]
//!   [--report report.json] [--data <folder>] [--db <folder>] [--work <folder>]`: the video played
//!   through the app's own reading, as if it were captured live (frames at their own pace, the reading
//!   taking the newest one, the app's timing, translator and presenter), with the measurement log
//!   collecting every line's way. On a working copy of the app's data and translation memory (or of the
//!   folders given: a frozen copy, for runs to compare): the originals are only read, never changed. The
//!   cached downloads (shared translations, layout words) are used as the app uses them; nothing is
//!   downloaded. With `--gold`, measured against the lines really on screen.
//! - `--evaluate <events.jsonl> --gold gold.json [--report report.json]`: the same measurement of a log
//!   written before (a replay's events, or the app's own lines.log from a recorded session, with
//!   `--start <video start, Unix ms>`).
//! - `--gold-draft <video> --out draft.json [--fps 10]`: a first draft of the reference: every line read
//!   in the enabled areas, with when it was on screen — short lines included — to check by hand.
//!
//! The reference (gold.json): a list of {id, area, start_ms, end_ms, text, short?} in the video's time.
//! A line counts as detected when a reading of it (≥ 85 % alike, as a part of a stacked block too) was
//! seen while it was up (from 200 ms before to 2.5 s after); translated when its line got a translation;
//! shown when that translation was up for at least `--min-visible` ms (300).

use std::collections::{BTreeMap, HashMap};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use vi_translator_core::config::{self, Config};
use vi_translator_core::engine::{self, Context, Input};
use vi_translator_core::fork::linelog;
use vi_translator_core::frame::FrameData;
use vi_translator_core::live::Live;
use vi_translator_core::memory::Memory;
use vi_translator_core::ocr::{self, Crop, Ocr};
use vi_translator_core::{NoSink, Paths, Sink, sync};

use crate::ocr_engine::PlatformOcr;

/// The fork's measurement commands: Some(exit code) when one was given.
pub fn cli(args: &[String]) -> Option<i32> {
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let num = |name: &str, def: f64| opt(name).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite() && *v > 0.0).unwrap_or(def);
    let finish = |r: anyhow::Result<()>| match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    if let Some(video) = opt("--replay-video") {
        let o = ReplayOptions {
            fps: num("--fps", 30.0),
            rate: num("--rate", 1.0),
            tail_s: num("--tail", 5.0),
            engine: opt("--engine"),
            gold: opt("--gold").map(PathBuf::from),
            report: opt("--report").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("replay-report.json")),
            work: opt("--work").map(PathBuf::from),
            data: opt("--data").map(PathBuf::from).unwrap_or_else(crate::data_dir),
            db: opt("--db").map(PathBuf::from).unwrap_or_else(crate::db_dir),
            min_visible_ms: num("--min-visible", 300.0) as u64,
        };
        return Some(finish(replay(&video, &o)));
    }
    if let Some(events) = opt("--evaluate") {
        let Some(gold) = opt("--gold") else {
            eprintln!("usage: --evaluate <events.jsonl> --gold gold.json [--report report.json] [--start <Unix ms>] [--rate 1]");
            return Some(2);
        };
        let report = opt("--report").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("evaluation.json"));
        let start = opt("--start").and_then(|v| v.parse::<u64>().ok());
        return Some(finish(evaluate_files(Path::new(&events), Path::new(&gold), &report, start, num("--rate", 1.0), num("--min-visible", 300.0) as u64)));
    }
    if let Some(video) = opt("--gold-draft") {
        let Some(out) = opt("--out") else {
            eprintln!("usage: --gold-draft <video> --out draft.json [--fps 10]");
            return Some(2);
        };
        return Some(finish(gold_draft(&video, Path::new(&out), num("--fps", 10.0))));
    }
    None
}

struct ReplayOptions {
    fps: f64,
    rate: f64,
    tail_s: f64,
    engine: Option<String>,
    gold: Option<PathBuf>,
    report: PathBuf,
    work: Option<PathBuf>,
    /// the folders copied for the run (the app's own, unless given)
    data: PathBuf,
    db: PathBuf,
    min_visible_ms: u64,
}

// ------------------------------------------------------------------------------------- video

/// A video's picture size (ffprobe).
fn video_size(file: &str) -> anyhow::Result<(u32, u32)> {
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0", file])
        .output()
        .map_err(|e| anyhow::anyhow!("ffprobe ({e}): install FFmpeg and put it on the PATH"))?;
    let dims = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    dims.split_once(',')
        .and_then(|(a, b)| Some((a.trim().parse::<u32>().ok()?, b.trim().parse::<u32>().ok()?)))
        .ok_or_else(|| anyhow::anyhow!("no video size in {dims:?}"))
}

/// The video's frames as RGBA, `fps` a second (ffmpeg).
fn decode(file: &str, fps: f64) -> anyhow::Result<(std::process::Child, std::process::ChildStdout)> {
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i", file, "-an", "-vf", &format!("fps={fps}"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("ffmpeg ({e}): install FFmpeg and put it on the PATH"))?;
    let pipe = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no ffmpeg output"))?;
    Ok((child, pipe))
}

// ------------------------------------------------------------------------------------- working copy

/// A working folder's mark: only a folder with it is emptied for a new run.
const MARK: &str = ".vit-replay";

/// The app's data and translation memory (`data`, `db`) copied into `work` (the models linked, not
/// copied, where the system allows): what the replay reads and may change. The originals are only read.
fn working_copy(work: &Path, data: &Path, db: &Path) -> anyhow::Result<Paths> {
    let (wdata, wdb) = (work.join("data"), work.join("db"));
    // (a folder of something else is never emptied)
    let used = std::fs::read_dir(work).map(|mut d| d.next().is_some()).unwrap_or(false);
    if used && !work.join(MARK).exists() {
        anyhow::bail!("{} isn't empty and isn't a replay's working folder: choose another (--work)", work.display());
    }
    let _ = std::fs::remove_dir_all(work);
    std::fs::create_dir_all(&wdata)?;
    std::fs::create_dir_all(&wdb)?;
    std::fs::write(work.join(MARK), b"")?;
    // the data folder's own files (not the logs, not other folders: the Xbox player's browser profile …)
    for e in std::fs::read_dir(&data).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_file() && !name.ends_with(".log") && !name.ends_with(".log.1") {
            std::fs::copy(&p, wdata.join(&name))?;
        }
    }
    // the offline translator's models: linked (read only), else copied
    link_tree(&data.join("models"), &wdata.join("models"))?;
    for e in std::fs::read_dir(&db).into_iter().flatten().flatten() {
        if e.path().is_file() {
            std::fs::copy(e.path(), wdb.join(e.file_name()))?;
        }
    }
    Ok(Paths { data: wdata.clone(), db: wdb, config: wdata.join("config.json") })
}

fn link_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let (p, q) = (e.path(), to.join(e.file_name()));
        if p.is_dir() {
            link_tree(&p, &q)?;
        } else if std::fs::hard_link(&p, &q).is_err() {
            std::fs::copy(&p, &q)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------- replay

fn replay(video: &str, o: &ReplayOptions) -> anyhow::Result<()> {
    let (w, h) = video_size(video)?;
    let work = o.work.clone().unwrap_or_else(|| std::env::temp_dir().join(format!("vit-replay-{}", std::process::id())));
    let paths = working_copy(&work, &o.data, &o.db)?;
    let mut cfg = Config::load(&paths.config);
    // (nothing downloaded: the cached downloads only)
    cfg.server_url = "http://127.0.0.1:9".into();
    cfg.debug_save_frames = false;
    if let Some(e) = &o.engine {
        cfg.engine = e.clone();
    }
    if !matches!(cfg.engine.as_str(), "local") {
        eprintln!("note: lines the memory doesn't have go to \"{}\", as in the app (an online service is billed as when playing)", cfg.engine);
    }
    let lang = cfg.target_lang.clone();
    let engine_id = cfg.engine.clone();
    let cfg = Arc::new(Mutex::new(cfg));
    let memory = Arc::new(Mutex::new(Memory::load(paths.db.clone(), &lang)));
    let sink: Arc<dyn Sink> = Arc::new(NoSink);
    let events = Arc::new(Mutex::new(Vec::new()));
    linelog::set_collector(Some(events.clone()));
    {
        // the cached downloads, as the app applies them at start (then it would download: nowhere to here)
        let (s, c, m, p) = (sink.clone(), cfg.clone(), memory.clone(), paths.clone());
        std::thread::spawn(move || sync::run(s, c, m, p));
        std::thread::sleep(Duration::from_secs(3));
    }
    eprintln!("replay: {video} ({w}x{h}, {} fps, ×{}), working copy in {}", o.fps, o.rate, work.display());

    // frames at their own pace, the newest kept (as the capture does)
    let latest: Arc<Mutex<Option<Arc<FrameData>>>> = Arc::new(Mutex::new(None));
    let done = Arc::new(AtomicBool::new(false));
    let start_unix = Arc::new(AtomicU64::new(0));
    let (mut child, mut pipe) = decode(video, o.fps)?;
    let decoder = {
        let (latest, done, start_unix) = (latest.clone(), done.clone(), start_unix.clone());
        let (fps, rate) = (o.fps, o.rate);
        std::thread::spawn(move || {
            let mut n = 0u64;
            let mut t0 = None::<Instant>;
            loop {
                let mut buf = vec![0u8; (w * h * 4) as usize];
                if pipe.read_exact(&mut buf).is_err() {
                    break;
                }
                let t0 = *t0.get_or_insert_with(|| {
                    start_unix.store(linelog::unix_ms(), Ordering::Relaxed);
                    Instant::now()
                });
                let due = t0 + Duration::from_secs_f64(n as f64 / (fps * rate));
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                n += 1;
                linelog::note_frame(n, linelog::unix_ms());
                *latest.lock().unwrap() = Some(Arc::new(FrameData { w, h, rgba: buf, seq: n }));
            }
            done.store(true, Ordering::Relaxed);
            n
        })
    };
    linelog::emit("replay", json!({ "video": video, "fps": o.fps, "rate": o.rate, "engine": engine_id, "app": vi_translator_core::VERSION }));

    // the reading, as the app's (main.rs ocr_loop): the newest frame, the wait the reading asks for
    Ocr::init_thread();
    let ocr = Ocr::new()?;
    let ctx = Context { cfg: cfg.clone(), live: Arc::new(Live::default()), memory: memory.clone(), sink, paths: paths.clone() };
    let mut reader = engine::start(ctx);
    let mut last_seq = 0u64;
    let mut ended_at: Option<Instant> = None;
    let black = Arc::new(FrameData { w, h, rgba: vec![0u8; (w * h * 4) as usize], seq: u64::MAX / 2 });
    let mut tail_seq = black.seq;
    loop {
        let mut frame = latest.lock().unwrap().clone();
        let give_up = Instant::now() + Duration::from_millis(120);
        while frame.as_ref().is_some_and(|f| f.seq == last_seq) && Instant::now() < give_up && !done.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
            frame = latest.lock().unwrap().clone();
        }
        // after the video: a black picture for the tail (the lines up end as when the game takes them away)
        if done.load(Ordering::Relaxed) && frame.as_ref().is_none_or(|f| f.seq == last_seq) {
            let at = *ended_at.get_or_insert_with(Instant::now);
            if at.elapsed() >= Duration::from_secs_f64(o.tail_s) {
                break;
            }
            tail_seq += 1;
            linelog::note_frame(tail_seq, linelog::unix_ms());
            frame = Some(Arc::new(FrameData { w, h, rgba: black.rgba.clone(), seq: tail_seq }));
        }
        let wait = match &frame {
            Some(f) => {
                last_seq = f.seq;
                reader.step(Input::Frame(f, &ocr))
            }
            None => reader.step(Input::Nothing),
        };
        std::thread::sleep(wait);
    }
    let frames = decoder.join().unwrap_or(0);
    let _ = child.wait();
    // (when the video's first frame was taken: the reference's times count from it)
    let start = start_unix.load(Ordering::Relaxed);
    linelog::emit("replay_end", json!({ "frames": frames, "start_unix_ms": start }));
    drop(reader);
    std::thread::sleep(Duration::from_millis(300));
    linelog::set_collector(None);
    let events = std::mem::take(&mut *events.lock().unwrap());

    // the events, one per line, next to the report
    let events_file = o.report.with_extension("events.jsonl");
    std::fs::write(&events_file, events.iter().map(|e| format!("{e}\n")).collect::<String>())?;
    eprintln!("replay: {frames} frames, {} events → {}", events.len(), events_file.display());
    let mut report = json!({ "video": video, "fps": o.fps, "rate": o.rate, "engine": engine_id, "app": vi_translator_core::VERSION, "start_unix_ms": start, "reads": reads_summary(&events) });
    if let Some(gold) = &o.gold {
        let gold = load_gold(gold)?;
        let ev = evaluate(&events, &gold, start, o.rate, o.min_visible_ms);
        print_summary(&ev);
        report["evaluation"] = ev;
    }
    std::fs::write(&o.report, serde_json::to_string_pretty(&report)?)?;
    eprintln!("replay: report → {}", o.report.display());
    // (a working folder of its own goes; one given is kept, to look into)
    if o.work.is_none() {
        let _ = std::fs::remove_dir_all(&work);
    }
    Ok(())
}

/// What the readings took over the run (from the log's `reads` events).
fn reads_summary(events: &[Value]) -> Value {
    let (mut n, mut ocr, mut ocr_max, mut passes, mut pass, mut pass_max, mut suppressed) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0u64);
    for e in events.iter().filter(|e| e["ev"] == "reads") {
        let (en, pn) = (e["n"].as_f64().unwrap_or(0.0), e["pass_n"].as_f64().unwrap_or(0.0));
        n += en;
        ocr += en * e["ocr_avg"].as_f64().unwrap_or(0.0);
        ocr_max = ocr_max.max(e["ocr_max"].as_f64().unwrap_or(0.0));
        passes += pn;
        pass += pn * e["pass_avg"].as_f64().unwrap_or(0.0);
        pass_max = pass_max.max(e["pass_max"].as_f64().unwrap_or(0.0));
        suppressed += e["suppressed"].as_u64().unwrap_or(0);
    }
    let avg = |t: f64, n: f64| if n > 0.0 { (t / n * 10.0).round() / 10.0 } else { 0.0 };
    json!({ "area_reads": n, "ocr_avg_ms": avg(ocr, n), "ocr_max_ms": ocr_max, "passes": passes, "pass_avg_ms": avg(pass, passes), "pass_max_ms": pass_max, "filtered_suppressed": suppressed })
}

// ------------------------------------------------------------------------------------- evaluation

#[derive(Deserialize, Clone, Debug)]
pub struct GoldLine {
    #[serde(default)]
    pub id: Value,
    pub area: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    #[serde(default)]
    pub short: Option<bool>,
}

fn load_gold(path: &Path) -> anyhow::Result<Vec<GoldLine>> {
    let v: Vec<GoldLine> = serde_json::from_slice(&std::fs::read(path)?).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok(v)
}

fn evaluate_files(events: &Path, gold: &Path, report: &Path, start: Option<u64>, rate: f64, min_visible_ms: u64) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(events)?;
    let events: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    // (a replay's events say when its video started: its first frame's capture)
    let start = start
        .or_else(|| events.iter().find(|e| e["ev"] == "replay_end").and_then(|e| e["start_unix_ms"].as_u64()).filter(|t| *t > 0))
        .ok_or_else(|| anyhow::anyhow!("when did the video start? give --start <Unix ms>"))?;
    let ev = evaluate(&events, &load_gold(gold)?, start, rate, min_visible_ms);
    print_summary(&ev);
    std::fs::write(report, serde_json::to_string_pretty(&json!({ "start_unix_ms": start, "rate": rate, "reads": reads_summary(&events), "evaluation": ev }))?)?;
    eprintln!("evaluation → {}", report.display());
    Ok(())
}

/// Similarity of two normalised texts, 0..1 (1 − edit distance / the longer's length).
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() || b.is_empty() {
        return if a == b { 1.0 } else { 0.0 };
    }
    1.0 - edit_distance(&a, &b) as f64 / a.len().max(b.len()) as f64
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// How well `line` is found in `text` (a reading, maybe a block of stacked lines), 0..1: the best of
/// the whole text and every run of about as many words.
fn found_in(text: &str, line: &str) -> f64 {
    let (t, l) = (linelog::norm(text), linelog::norm(line));
    if l.is_empty() || t.is_empty() {
        return 0.0;
    }
    // (whole words: "no" is not in "now")
    if format!(" {t} ").contains(&format!(" {l} ")) {
        return 1.0;
    }
    let mut best = similarity(&t, &l);
    let words: Vec<&str> = t.split(' ').collect();
    let n = l.split(' ').count();
    for size in n.saturating_sub(1).max(1)..=(n + 1) {
        for start in 0..words.len().saturating_sub(size - 1) {
            let end = (start + size).min(words.len());
            best = best.max(similarity(&words[start..end].join(" "), &l));
        }
    }
    best
}

const ALIKE: f64 = 0.85;

/// The measurement: each reference line's way through the app, and the totals.
pub fn evaluate(events: &[Value], gold: &[GoldLine], start_unix: u64, rate: f64, min_visible_ms: u64) -> Value {
    let at = |video_ms: u64| start_unix + (video_ms as f64 / rate) as u64;
    let of = |kind: &'static str| events.iter().filter(move |e| e["ev"] == kind);
    let t = |e: &Value| e["t"].as_u64().unwrap_or(0);
    let mut lines = Vec::new();
    let mut reasons: BTreeMap<String, u64> = BTreeMap::new();
    for g in gold {
        let (from, to) = (at(g.start_ms).saturating_sub(200), at(g.end_ms) + 2500);
        let within = |ms: u64| ms >= from && ms <= to;
        let seen = of("seen").any(|e| e["area"] == g.area.as_str() && within(e["captured"].as_u64().unwrap_or(t(e))) && found_in(e["text"].as_str().unwrap_or(""), &g.text) >= ALIKE);
        // its line sent: the whole text or one of its parts
        let sent: Vec<&Value> = of("sent")
            .filter(|e| e["area"] == g.area.as_str() && within(e["at"].as_u64().unwrap_or(t(e))))
            .filter(|e| {
                let parts = e["parts"].as_array().cloned().unwrap_or_default();
                found_in(e["en"].as_str().unwrap_or(""), &g.text) >= ALIKE || parts.iter().any(|p| found_in(p.as_str().unwrap_or(""), &g.text) >= ALIKE)
            })
            .collect();
        let seqs: Vec<u64> = sent.iter().filter_map(|e| e["seq"].as_u64()).collect();
        let by_seq = |kind: &'static str| of(kind).filter(|e| e["area"] == g.area.as_str() && e["seq"].as_u64().is_some_and(|s| seqs.contains(&s))).cloned().collect::<Vec<_>>();
        let translated_ok = by_seq("translated").iter().any(|e| e["ok"] == true);
        let errors: Vec<String> = by_seq("translated").iter().filter(|e| e["ok"] == false).filter_map(|e| e["error_kind"].as_str().map(String::from)).collect();
        // shown: up long enough to be read (its hidden event, or still up at the end)
        let hidden = by_seq("hidden");
        let mut shown: Vec<(u64, u64, bool)> = by_seq("shown")
            .iter()
            .map(|e| {
                let (seq, up) = (e["seq"].as_u64().unwrap_or(0), t(e));
                let dur = hidden.iter().find(|x| x["seq"].as_u64() == Some(seq)).and_then(|x| x["shown_ms"].as_u64()).unwrap_or(u64::MAX);
                (up, dur, e["late"] == true)
            })
            .collect();
        shown.sort_by_key(|s| s.0);
        let visible = shown.iter().find(|s| s.1 >= min_visible_ms).copied();
        let reason: Option<String> = if visible.is_some() {
            None
        } else if let Some(s) = shown.first() {
            Some(if s.2 { "presenter:late_hidden" } else { "presenter:too_short" }.into())
        } else if translated_ok {
            if by_seq("replaced").iter().any(|_| true) {
                Some("presenter:replaced_before_show".into())
            } else if by_seq("dropped").iter().any(|_| true) {
                Some("presenter:cleared".into())
            } else {
                Some("presenter:not_shown".into())
            }
        } else if let Some(kind) = errors.first() {
            Some(format!("translate:{kind}"))
        } else if !sent.is_empty() {
            Some(if by_seq("skipped").is_empty() { "translate:pending" } else { "skipped:superseded" }.into())
        } else {
            // never sent: a filter that left it out, else read but never settled, else never read
            let filtered = of("filtered")
                .filter(|e| e["area"] == g.area.as_str() && within(t(e)) && found_in(e["text"].as_str().unwrap_or(""), &g.text) >= ALIKE)
                .find_map(|e| e["reason"].as_str().map(|r| format!("filtered:{r}")));
            Some(filtered.unwrap_or_else(|| if seen { "not_settled".into() } else { "not_seen".into() }))
        };
        if let Some(r) = &reason {
            *reasons.entry(r.clone()).or_default() += 1;
        }
        let latency = visible.map(|s| s.0.saturating_sub(at(g.start_ms)));
        // how well it was read: the closest reading sent of it
        let cer = sent
            .iter()
            .flat_map(|e| {
                let mut texts = vec![e["en"].as_str().unwrap_or("").to_string()];
                texts.extend(e["parts"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)));
                texts
            })
            .map(|text| {
                let (r, l): (Vec<char>, Vec<char>) = (linelog::norm(&text).chars().collect(), linelog::norm(&g.text).chars().collect());
                edit_distance(&r, &l) as f64 / l.len().max(1) as f64
            })
            .fold(None, |best: Option<f64>, c| Some(best.map_or(c, |b| b.min(c))));
        let short = g.short.unwrap_or_else(|| g.text.chars().filter(|c| c.is_alphabetic()).count() < 4);
        lines.push(json!({
            "id": g.id, "area": g.area, "text": g.text, "short": short, "start_ms": g.start_ms, "end_ms": g.end_ms,
            "detected": seen || !sent.is_empty(), "translated": translated_ok, "shown": visible.is_some(),
            "on_time": visible.map(|s| s.0 <= at(g.end_ms)), "latency_ms": latency,
            "shown_ms": visible.map(|s| if s.1 == u64::MAX { Value::Null } else { json!(s.1) }), "reason": reason,
            "cer": cer.map(|c| (c * 1000.0).round() / 1000.0), "seqs": seqs,
        }));
    }
    // shown lines out of order (an older line's translation after a newer one's), by area
    let mut violations = Vec::new();
    let mut last: HashMap<String, u64> = HashMap::new();
    let mut shown_all: Vec<&Value> = of("shown").collect();
    shown_all.sort_by_key(|e| t(e));
    for e in &shown_all {
        let (area, seq) = (e["area"].as_str().unwrap_or("").to_string(), e["seq"].as_u64().unwrap_or(0));
        if let Some(prev) = last.get(&area).copied().filter(|p| seq < *p) {
            violations.push(json!({ "area": area, "seq": seq, "after": prev, "t": t(e) }));
        }
        let entry = last.entry(area).or_insert(seq);
        *entry = (*entry).max(seq);
    }
    // translations shown for something not in the reference (in the reference's areas, while it covers)
    let areas: std::collections::HashSet<&str> = gold.iter().map(|g| g.area.as_str()).collect();
    let (gold_from, gold_to) = (gold.iter().map(|g| at(g.start_ms)).min().unwrap_or(0), gold.iter().map(|g| at(g.end_ms)).max().unwrap_or(0) + 3000);
    let wrong: Vec<Value> = shown_all
        .iter()
        .filter(|e| areas.contains(e["area"].as_str().unwrap_or("")) && t(e) >= gold_from && t(e) <= gold_to)
        .filter(|e| {
            let en = e["en"].as_str().unwrap_or("");
            let when = e["at"].as_u64().unwrap_or(t(e));
            !gold.iter().any(|g| g.area == e["area"].as_str().unwrap_or("") && when + 500 >= at(g.start_ms) && when <= at(g.end_ms) + 3000 && found_in(en, &g.text) >= ALIKE)
        })
        .map(|e| json!({ "area": e["area"], "seq": e["seq"], "en": e["en"], "t": e["t"] }))
        .collect();
    let totals = |pick: &dyn Fn(&Value) -> bool| {
        let sel: Vec<&Value> = lines.iter().filter(|l| pick(l)).collect();
        let n = sel.len() as f64;
        let count = |k: &str| sel.iter().filter(|l| l[k] == true).count() as f64;
        let rate = |c: f64| if n > 0.0 { (c / n * 1000.0).round() / 1000.0 } else { 0.0 };
        let mut lat: Vec<u64> = sel.iter().filter_map(|l| l["latency_ms"].as_u64()).collect();
        lat.sort_unstable();
        let pct = |p: f64| lat.get(((lat.len() as f64 - 1.0) * p).round() as usize).copied();
        let cers: Vec<f64> = sel.iter().filter_map(|l| l["cer"].as_f64()).collect();
        json!({
            "lines": n, "D": rate(count("detected")), "T": rate(count("translated")), "S": rate(count("shown")),
            "S_on_time": rate(sel.iter().filter(|l| l["on_time"] == true).count() as f64),
            "S_late": rate(sel.iter().filter(|l| l["on_time"] == false).count() as f64),
            "latency_ms": { "p50": pct(0.5), "p90": pct(0.9), "n": lat.len() },
            "cer_avg": if cers.is_empty() { Value::Null } else { json!((cers.iter().sum::<f64>() / cers.len() as f64 * 1000.0).round() / 1000.0) },
        })
    };
    let mut by_area = serde_json::Map::new();
    for a in &areas {
        let a = a.to_string();
        by_area.insert(a.clone(), totals(&|l: &Value| l["area"] == a.as_str()));
    }
    json!({
        "summary": totals(&|_| true),
        "by_area": by_area,
        "short_lines": totals(&|l: &Value| l["short"] == true),
        "long_lines": totals(&|l: &Value| l["short"] == false),
        "order_violations": violations.len(), "violations": violations,
        "wrong_displays": wrong.len(), "wrong": wrong,
        "loss_reasons": reasons,
        "min_visible_ms": min_visible_ms,
        "lines": lines,
    })
}

fn print_summary(ev: &Value) {
    let s = &ev["summary"];
    eprintln!(
        "reference lines: {} · detected {} · translated {} · shown {} (on time {}, late {}) · latency p50 {} ms p90 {} ms · order violations {} · wrong displays {} · CER {}",
        s["lines"], s["D"], s["T"], s["S"], s["S_on_time"], s["S_late"], s["latency_ms"]["p50"], s["latency_ms"]["p90"], ev["order_violations"], ev["wrong_displays"], s["cer_avg"]
    );
    if let Some(r) = ev["loss_reasons"].as_object() {
        for (k, v) in r {
            eprintln!("  lost: {k} × {v}");
        }
    }
}

// ------------------------------------------------------------------------------------- reference draft

struct Open {
    area: String,
    key: String,
    texts: HashMap<String, u32>,
    start_ms: u64,
    last_ms: u64,
    frames: u32,
}

/// A first draft of the reference: each line read in the enabled areas (stacked subtitles line by line,
/// short ones too), with when it was on screen. To check by hand: a reading is not the truth.
fn gold_draft(video: &str, out: &Path, fps: f64) -> anyhow::Result<()> {
    let cfg = Config::load(&crate::config_path());
    let areas: Vec<config::Area> = cfg.areas.iter().filter(|a| a.enabled).cloned().collect();
    let (w, h) = video_size(video)?;
    let (mut child, mut pipe) = decode(video, fps)?;
    Ocr::init_thread();
    let ocr = Ocr::new()?;
    let step_ms = (1000.0 / fps) as u64;
    let mut frame = vec![0u8; (w * h * 4) as usize];
    let (mut open, mut done): (Vec<Open>, Vec<Open>) = (Vec::new(), Vec::new());
    let mut n = 0u64;
    while pipe.read_exact(&mut frame).is_ok() {
        let t = n * 1000 / fps.max(0.001) as u64;
        let t = if fps.fract() == 0.0 { t } else { (n as f64 * 1000.0 / fps) as u64 };
        n += 1;
        for area in &areas {
            for text in read_lines(&ocr, &cfg, area, &frame, w, h) {
                let key = linelog::norm(&text);
                if key.is_empty() {
                    continue;
                }
                match open.iter_mut().find(|o| o.area == area.id && similarity(&o.key, &key) >= ALIKE) {
                    Some(o) => {
                        o.last_ms = t;
                        o.frames += 1;
                        *o.texts.entry(text).or_default() += 1;
                    }
                    None => open.push(Open { area: area.id.clone(), key, texts: HashMap::from([(text, 1)]), start_ms: t, last_ms: t, frames: 1 }),
                }
            }
        }
        // gone for two frames: closed
        let (keep, gone): (Vec<Open>, Vec<Open>) = open.into_iter().partition(|o| o.last_ms + 2 * step_ms >= t);
        open = keep;
        done.extend(gone);
        if n % 600 == 0 {
            eprintln!("gold-draft: {:.0} min", t as f64 / 60000.0);
        }
    }
    let _ = child.wait();
    done.extend(open);
    done.sort_by_key(|o| (o.start_ms, o.area.clone()));
    let lines: Vec<Value> = done
        .iter()
        .filter(|o| o.frames >= 2 || fps < 4.0)
        .enumerate()
        .map(|(i, o)| {
            // the reading seen most often
            let text = o.texts.iter().max_by_key(|(t, c)| (**c, t.len())).map(|(t, _)| t.clone()).unwrap_or_default();
            let short = text.chars().filter(|c| c.is_alphabetic()).count() < 4;
            json!({ "id": i + 1, "area": o.area, "start_ms": o.start_ms, "end_ms": o.last_ms + step_ms, "text": text, "short": short, "frames": o.frames, "check": true })
        })
        .collect();
    std::fs::write(out, serde_json::to_string_pretty(&lines)?)?;
    eprintln!("gold-draft: {} lines → {} (check each: text, times; delete what isn't the game's line; add missed ones)", lines.len(), out.display());
    Ok(())
}

/// The lines read in an area of a frame, as the reading takes them (stacked areas line by line), without
/// the reading's own filters: a short line ("No.") is kept for the reference.
fn read_lines(ocr: &Ocr, cfg: &Config, area: &config::Area, frame: &[u8], w: u32, h: u32) -> Vec<String> {
    let join = |words: &[ocr::Word]| ocr::fix_contractions(&words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "));
    let letters = |t: &str| t.chars().any(|c| c.is_alphabetic());
    if cfg.fx(&area.id).stacks(&area.id) {
        let grown = config::stack_grown(area);
        let (crop, words, _) = engine::read_stacked(ocr, frame, w, h, area, &grown);
        let Some(crop) = crop else { return Vec::new() };
        let words = ocr::bottom_stack(&words, crop.h as f64);
        let n = words.iter().map(|w| w.line + 1).max().unwrap_or(0);
        return (0..n)
            .map(|l| join(&words.iter().filter(|w| w.line == l).cloned().collect::<Vec<_>>()))
            .filter(|t| letters(t) && ocr::looks_like_words(t))
            .collect();
    }
    let Some(crop) = Crop::from_frame(frame, w, h, area.x, area.y, area.w, area.h) else { return Vec::new() };
    let Ok((mut words, _)) = ocr.recognize_checked(&crop) else { return Vec::new() };
    if area.centered_only {
        words = ocr::centered_words(&words, area.x.clamp(0.0, 1.0) * w as f64, w as f64);
    }
    let text = join(&words);
    if letters(&text) && ocr::looks_like_words(&text) { vec![text] } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str, fields: Value) -> Value {
        let mut v = fields;
        v["ev"] = json!(kind);
        v
    }

    #[test]
    fn fork_a_line_is_found_in_a_block_and_in_a_misreading() {
        assert_eq!(found_in("Tourist: Eyes on the road! Lucia: No.", "No."), 1.0);
        assert!(found_in("Meet me at the dacks.", "Meet me at the docks.") >= ALIKE);
        assert!(found_in("Get in the car.", "Meet me at the docks.") < ALIKE);
        // (whole words: "No." is not in "now")
        assert!(found_in("Get in the car, now!", "No.") < ALIKE);
    }

    #[test]
    fn fork_the_measurement_follows_each_reference_line() {
        let s = 1_000_000u64; // the video's start (Unix ms)
        let gold = vec![
            GoldLine { id: json!(1), area: "subtitles".into(), start_ms: 1000, end_ms: 3000, text: "Meet me at the docks.".into(), short: None },
            GoldLine { id: json!(2), area: "subtitles".into(), start_ms: 4000, end_ms: 4600, text: "No.".into(), short: None },
            GoldLine { id: json!(3), area: "subtitles".into(), start_ms: 5000, end_ms: 6000, text: "Get in the car, now!".into(), short: None },
        ];
        let events = vec![
            ev("seen", json!({ "area": "subtitles", "text": "Meet me at the docks.", "captured": s + 1050, "t": s + 1100 })),
            ev("sent", json!({ "area": "subtitles", "seq": 1, "en": "Meet me at the docks.", "parts": [], "at": s + 1050, "t": s + 1200 })),
            ev("translated", json!({ "area": "subtitles", "seq": 1, "ok": true, "hit": true, "t": s + 1210 })),
            ev("shown", json!({ "area": "subtitles", "seq": 1, "en": "Meet me at the docks.", "at": s + 1050, "late": false, "t": s + 1250 })),
            ev("hidden", json!({ "area": "subtitles", "seq": 1, "shown_ms": 1800, "t": s + 3050 })),
            // "No." read, but left out for being too short
            ev("filtered", json!({ "area": "subtitles", "reason": "clean", "text": "No.", "t": s + 4100 })),
            // the third: sent, translated late, its English gone: up for 40 ms
            ev("sent", json!({ "area": "subtitles", "seq": 3, "en": "Get in the car, now!", "parts": [], "at": s + 5050, "t": s + 5150 })),
            ev("translated", json!({ "area": "subtitles", "seq": 3, "ok": true, "hit": false, "t": s + 6400 })),
            ev("shown", json!({ "area": "subtitles", "seq": 3, "en": "Get in the car, now!", "at": s + 5050, "late": true, "t": s + 6420 })),
            ev("hidden", json!({ "area": "subtitles", "seq": 3, "shown_ms": 40, "t": s + 6460 })),
            // something shown that isn't on screen, and after a newer line (seq 3): out of order
            ev("shown", json!({ "area": "subtitles", "seq": 2, "en": "Grrr bzzt", "at": s + 4500, "late": false, "t": s + 6500 })),
        ];
        let r = evaluate(&events, &gold, s, 1.0, 300);
        assert_eq!(r["summary"]["lines"], 3.0);
        assert_eq!(r["summary"]["S"], 0.333);
        assert_eq!(r["lines"][0]["latency_ms"], 250);
        assert_eq!(r["lines"][0]["on_time"], true);
        assert_eq!(r["lines"][1]["reason"], "filtered:clean");
        assert_eq!(r["lines"][1]["short"], true);
        assert_eq!(r["lines"][2]["reason"], "presenter:late_hidden");
        assert_eq!(r["loss_reasons"]["filtered:clean"], 1);
        // seq 2 shown after seq 3: an order violation
        assert_eq!(r["order_violations"], 1);
        assert_eq!(r["wrong_displays"], 1);
        assert_eq!(r["short_lines"]["lines"], 1.0);
    }
}

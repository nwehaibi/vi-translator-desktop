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
        let sets = match settings_given(args) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e:#}");
                return Some(2);
            }
        };
        let o = ReplayOptions {
            fps: opt("--fps").and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite() && *v > 0.0),
            sets,
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
    if let Some(plan) = opt("--sweep") {
        let out = opt("--out-dir").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("sweep"));
        return Some(finish(sweep(Path::new(&plan), &out)));
    }
    if let Some(log) = opt("--summarize") {
        let report = opt("--report").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("session-summary.json"));
        let (from, to) = (opt("--from").and_then(|v| v.parse::<u64>().ok()), opt("--to").and_then(|v| v.parse::<u64>().ok()));
        return Some(finish(summarize(Path::new(&log), from, to, &report)));
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
    /// frames a second from the video (None: the capture rate in use, Capture FPS, as the capture would)
    fps: Option<f64>,
    /// the timing settings changed for this run (`--set key=value`)
    sets: Vec<(String, u64)>,
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

// ------------------------------------------------------------------------------------- settings

/// The five timing settings (Source → Timing), with the ranges of their sliders (ui/app.js renderTiming).
pub const TIMING_KEYS: [(&str, u64, u64); 5] =
    [("capture_fps", 1, 60), ("preview_fps", 5, 60), ("interval_ms", 16, 1000), ("stable_ms", 0, 1500), ("clear_after_ms", 50, 3000)];

/// The `--set key=value` given (each a timing setting, within its slider's range).
fn settings_given(args: &[String]) -> anyhow::Result<Vec<(String, u64)>> {
    let mut out = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if a != "--set" {
            continue;
        }
        let kv = args.get(i + 1).ok_or_else(|| anyhow::anyhow!("--set needs key=value"))?;
        let (k, v) = kv.split_once('=').ok_or_else(|| anyhow::anyhow!("--set {kv}: key=value"))?;
        let Some((_, lo, hi)) = TIMING_KEYS.iter().find(|(name, _, _)| *name == k) else {
            anyhow::bail!("--set {k}: one of {}", TIMING_KEYS.map(|t| t.0).join(", "));
        };
        let n: u64 = v.trim().parse().map_err(|_| anyhow::anyhow!("--set {kv}: a whole number"))?;
        if n < *lo || n > *hi {
            anyhow::bail!("--set {kv}: from {lo} to {hi} (the slider's range)");
        }
        out.push((k.to_string(), n));
    }
    Ok(out)
}

/// Change the settings as moving their sliders does: the three reading timings become the player's own
/// (timing_custom), starting from those in use (the game's, from its layout, where it has them).
fn apply_settings(cfg: &mut Config, sets: &[(String, u64)]) {
    if sets.iter().any(|(k, _)| matches!(k.as_str(), "interval_ms" | "stable_ms" | "clear_after_ms")) && cfg.timing_custom != Some(true) {
        let (i, s, c) = cfg.timing();
        (cfg.interval_ms, cfg.stable_ms, cfg.clear_after_ms) = (i, s, c);
        cfg.timing_custom = Some(true);
    }
    for (k, v) in sets {
        match k.as_str() {
            "capture_fps" => cfg.capture_fps = *v as u32,
            "preview_fps" => cfg.preview_fps = *v as u32,
            "interval_ms" => cfg.interval_ms = *v,
            "stable_ms" => cfg.stable_ms = *v,
            "clear_after_ms" => cfg.clear_after_ms = *v,
            _ => {}
        }
    }
}

/// The timing in use (what a report says the run had).
fn settings_in_use(cfg: &Config) -> Value {
    let (interval, stable, clear) = cfg.timing();
    json!({
        "capture_fps": cfg.capture_fps, "preview_fps": cfg.preview_fps, "interval_ms": interval, "stable_ms": stable,
        "clear_after_ms": clear, "timing_custom": cfg.timing_custom == Some(true),
    })
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
    // the settings for this run, over what the layout set (as the player's sliders over the game's timing)
    let (fps, settings) = {
        let mut c = cfg.lock().unwrap();
        apply_settings(&mut c, &o.sets);
        if o.sets.iter().any(|(k, _)| k == "preview_fps") {
            eprintln!("note: Preview FPS changes nothing here (no preview while replaying): measure it in the app");
        }
        (o.fps.unwrap_or(f64::from(c.capture_fps.clamp(1, 60))), settings_in_use(&c))
    };
    eprintln!("replay: {video} ({w}x{h}, {fps} fps, ×{}), settings {settings}, working copy in {}", o.rate, work.display());
    let (cpu0, wall0) = (vi_translator_core::fork::cpu::now(), Instant::now());

    // frames at their own pace, the newest kept (as the capture does)
    let latest: Arc<Mutex<Option<Arc<FrameData>>>> = Arc::new(Mutex::new(None));
    let done = Arc::new(AtomicBool::new(false));
    let start_unix = Arc::new(AtomicU64::new(0));
    let (mut child, mut pipe) = decode(video, fps)?;
    let decoder = {
        let (latest, done, start_unix) = (latest.clone(), done.clone(), start_unix.clone());
        let rate = o.rate;
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
    linelog::emit("replay", json!({ "video": video, "fps": fps, "rate": o.rate, "engine": engine_id, "app": vi_translator_core::VERSION, "settings": settings }));

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
    // the CPU over the run: this app's (the reading, the translator, taking the frames) and everything's
    // (ffmpeg's decoding too, standing in for the capture)
    let wall_ms = wall0.elapsed().as_secs_f64() * 1000.0;
    let cpu = match (cpu0, vi_translator_core::fork::cpu::now()) {
        (Some(a), Some(b)) => {
            let (app, all) = vi_translator_core::fork::cpu::share(&a, &b).unwrap_or((0.0, 0.0));
            json!({ "app_pct": app, "all_pct": all, "app_core_pct": ((b.process_ms - a.process_ms) / wall_ms * 1000.0).round() / 10.0 })
        }
        _ => Value::Null,
    };
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
    let mut report = json!({
        "video": video, "fps": fps, "rate": o.rate, "engine": engine_id, "app": vi_translator_core::VERSION, "start_unix_ms": start,
        "settings": settings, "cpu": cpu, "reads": reads_summary(&events),
    });
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
    // (each second's: this app's share of the machine, everything's, frames captured)
    let per_second = |k: &str| {
        let v: Vec<f64> = events.iter().filter(|e| e["ev"] == "reads").filter_map(|e| e[k].as_f64()).collect();
        spread(&v)
    };
    json!({
        "area_reads": n, "ocr_avg_ms": avg(ocr, n), "ocr_max_ms": ocr_max, "passes": passes, "pass_avg_ms": avg(pass, passes), "pass_max_ms": pass_max,
        "filtered_suppressed": suppressed, "cpu_app_pct": per_second("cpu_app"), "cpu_all_pct": per_second("cpu_all"), "fps": per_second("fps"),
    })
}

/// {p50, p90, max, avg, n} of some values (nulls when there are none).
fn spread(v: &[f64]) -> Value {
    if v.is_empty() {
        return json!({ "p50": null, "p90": null, "max": null, "avg": null, "n": 0 });
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| s[((s.len() as f64 - 1.0) * p).round() as usize];
    let r = |x: f64| (x * 10.0).round() / 10.0;
    json!({ "p50": r(pct(0.5)), "p90": r(pct(0.9)), "max": r(s[s.len() - 1]), "avg": r(s.iter().sum::<f64>() / s.len() as f64), "n": s.len() })
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
        // how long its translation stayed after the English went (negative: taken down before, by the next line)
        let linger = visible.filter(|s| s.1 != u64::MAX).map(|s| (s.0 + s.1) as i64 - at(g.end_ms) as i64);
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
            "linger_ms": linger, "sends": sent.len(), "shows": shown.len(),
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
            // (after the English went; sent to the translator and shown, a line: more than once is a line
            // sent half-read, or blinking off and on)
            "linger_ms": spread(&sel.iter().filter_map(|l| l["linger_ms"].as_f64()).collect::<Vec<_>>()),
            "sends_avg": avg_of(&sel, "sends"), "shows_avg": avg_of(&sel, "shows"),
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

/// The average of a count over lines that were detected (a lost line's zero would hide the extra ones).
fn avg_of(lines: &[&Value], key: &str) -> Value {
    let v: Vec<f64> = lines.iter().filter(|l| l["detected"] == true).filter_map(|l| l[key].as_f64()).collect();
    if v.is_empty() { Value::Null } else { json!((v.iter().sum::<f64>() / v.len() as f64 * 100.0).round() / 100.0) }
}

fn print_summary(ev: &Value) {
    let s = &ev["summary"];
    eprintln!(
        "reference lines: {} · detected {} · translated {} · shown {} (on time {}, late {}) · latency p50 {} ms p90 {} ms · stays after the English p50 {} ms · sent {}× shown {}× a line · order violations {} · wrong displays {} · CER {}",
        s["lines"], s["D"], s["T"], s["S"], s["S_on_time"], s["S_late"], s["latency_ms"]["p50"], s["latency_ms"]["p90"], s["linger_ms"]["p50"], s["sends_avg"], s["shows_avg"],
        ev["order_violations"], ev["wrong_displays"], s["cer_avg"]
    );
    if let Some(r) = ev["loss_reasons"].as_object() {
        for (k, v) in r {
            eprintln!("  lost: {k} × {v}");
        }
    }
}

// ------------------------------------------------------------------------------------- sweep

/// `--sweep plan.json --out-dir <folder>`: the same video replayed with each run's settings, every run
/// `repeat` times, in a shuffled order each round (a slow drift — the PC warming up — spread over every
/// run, not on the last ones), with a pause between runs for the PC to cool down. Each run is a process
/// of its own (nothing carried over), its report kept in the folder (a sweep stopped part-way goes on
/// where it stopped when started again); then a table of them all: sweep.json and sweep.csv.
///
/// ```json
/// { "video": "calibration.mp4", "gold": "calibration-gold.json", "data": "frozen/data", "db": "frozen/db",
///   "repeat": 2, "cooldown_s": 30, "base": { "preview_fps": 30 },
///   "runs": [ { "name": "current" }, { "name": "interval-100", "set": { "interval_ms": 100 } } ] }
/// ```
/// (paths from the plan's folder; `data`, `db` optional: the app's own folders, copied for each run)
#[derive(Deserialize)]
struct SweepPlan {
    video: String,
    gold: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    db: Option<String>,
    #[serde(default)]
    repeat: Option<u32>,
    #[serde(default)]
    cooldown_s: Option<u64>,
    #[serde(default)]
    tail_s: Option<f64>,
    #[serde(default)]
    base: BTreeMap<String, u64>,
    runs: Vec<SweepRun>,
}

#[derive(Deserialize, Clone)]
struct SweepRun {
    name: String,
    #[serde(default)]
    set: BTreeMap<String, u64>,
}

/// A run's settings checked as `--set` checks them.
fn check_settings(name: &str, set: &BTreeMap<String, u64>) -> anyhow::Result<()> {
    for (k, v) in set {
        let Some((_, lo, hi)) = TIMING_KEYS.iter().find(|(key, _, _)| key == k) else {
            anyhow::bail!("run {name}: {k} is not one of {}", TIMING_KEYS.map(|t| t.0).join(", "));
        };
        if v < lo || v > hi {
            anyhow::bail!("run {name}: {k} {v} is outside {lo}..{hi} (the slider's range)");
        }
    }
    Ok(())
}

/// The order of the runs in round `round` (the same every time: a sweep can be repeated or resumed).
fn shuffled(n: usize, round: u32) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ (u64::from(round) + 1).wrapping_mul(0xD1B5_4A32_D192_ED03);
    for i in (1..n).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        order.swap(i, (x % (i as u64 + 1)) as usize);
    }
    order
}

fn sweep(plan_file: &Path, out: &Path) -> anyhow::Result<()> {
    let plan: SweepPlan = serde_json::from_slice(&std::fs::read(plan_file)?).map_err(|e| anyhow::anyhow!("{}: {e}", plan_file.display()))?;
    let here = plan_file.parent().map(Path::to_path_buf).unwrap_or_default();
    let at = |p: &str| if Path::new(p).is_absolute() { PathBuf::from(p) } else { here.join(p) };
    let (video, gold) = (at(&plan.video), at(&plan.gold));
    anyhow::ensure!(video.is_file(), "no video {}", video.display());
    anyhow::ensure!(gold.is_file(), "no reference {}", gold.display());
    anyhow::ensure!(!plan.runs.is_empty(), "no runs");
    check_settings("base", &plan.base)?;
    let mut names = std::collections::HashSet::new();
    for r in &plan.runs {
        anyhow::ensure!(!r.name.is_empty() && r.name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)), "run name {:?}: letters, digits, - _ . only", r.name);
        anyhow::ensure!(names.insert(r.name.clone()), "run name {} twice", r.name);
        check_settings(&r.name, &r.set)?;
    }
    std::fs::create_dir_all(out)?;
    let exe = std::env::current_exe()?;
    let (repeat, cooldown) = (plan.repeat.unwrap_or(1).clamp(1, 20), Duration::from_secs(plan.cooldown_s.unwrap_or(30).min(600)));
    let total = plan.runs.len() * repeat as usize;
    let mut done = 0;
    for round in 0..repeat {
        for i in shuffled(plan.runs.len(), round) {
            let run = &plan.runs[i];
            done += 1;
            let report = out.join(format!("{}.{}.json", run.name, round + 1));
            if report.is_file() {
                eprintln!("sweep {done}/{total}: {} (round {}) done before", run.name, round + 1);
                continue;
            }
            // (the run's own settings over the base)
            let mut set = plan.base.clone();
            set.extend(run.set.clone());
            eprintln!("sweep {done}/{total}: {} (round {}) {set:?}", run.name, round + 1);
            let mut cmd = std::process::Command::new(&exe);
            cmd.arg("--replay-video").arg(&video).arg("--gold").arg(&gold).arg("--report").arg(&report);
            if let Some(d) = &plan.data {
                cmd.arg("--data").arg(at(d));
            }
            if let Some(d) = &plan.db {
                cmd.arg("--db").arg(at(d));
            }
            if let Some(t) = plan.tail_s {
                cmd.arg("--tail").arg(t.to_string());
            }
            for (k, v) in &set {
                cmd.arg("--set").arg(format!("{k}={v}"));
            }
            let status = cmd.status()?;
            if !status.success() {
                eprintln!("sweep: {} (round {}) failed ({status}): left out", run.name, round + 1);
                let _ = std::fs::remove_file(&report);
            }
            if done < total {
                std::thread::sleep(cooldown);
            }
        }
    }
    // the table: every run, then each run's average over the rounds
    let mut rows = Vec::new();
    for round in 0..repeat {
        for run in &plan.runs {
            let report = out.join(format!("{}.{}.json", run.name, round + 1));
            if let Some(r) = std::fs::read(&report).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
                rows.push(sweep_row(&run.name, round + 1, &r));
            }
        }
    }
    let averages: Vec<Value> = plan.runs.iter().filter_map(|run| average_rows(&run.name, &rows.iter().filter(|r| r["name"] == run.name.as_str()).collect::<Vec<_>>())).collect();
    std::fs::write(out.join("sweep.json"), serde_json::to_string_pretty(&json!({ "plan": plan_file.display().to_string(), "averages": averages, "runs": rows }))?)?;
    std::fs::write(out.join("sweep.csv"), to_csv(&averages))?;
    eprintln!("\n{:<22} {:>5} {:>5} {:>5} {:>4} {:>6} {:>7} {:>7} {:>7} {:>7} {:>7}", "run", "int", "stab", "clear", "fps", "S", "p50", "p90", "stays", "cpuApp", "cpuAll");
    for a in &averages {
        let n = |k: &str| a[k].as_f64().map(|v| format!("{v}")).unwrap_or_else(|| "-".into());
        eprintln!(
            "{:<22} {:>5} {:>5} {:>5} {:>4} {:>6} {:>7} {:>7} {:>7} {:>7} {:>7}",
            a["name"].as_str().unwrap_or(""), n("interval_ms"), n("stable_ms"), n("clear_after_ms"), n("capture_fps"), n("S"), n("latency_p50"), n("latency_p90"), n("linger_p50"), n("cpu_app_pct"), n("cpu_all_pct")
        );
    }
    eprintln!("sweep: {} runs → {}", rows.len(), out.join("sweep.csv").display());
    Ok(())
}

/// One run of a sweep as a row of numbers (the settings it had, how it did, what it cost).
fn sweep_row(name: &str, round: u32, r: &Value) -> Value {
    let (st, ev, s) = (&r["settings"], &r["evaluation"], &r["evaluation"]["summary"]);
    let mut row = json!({
        "name": name, "round": round,
        "capture_fps": st["capture_fps"], "interval_ms": st["interval_ms"], "stable_ms": st["stable_ms"], "clear_after_ms": st["clear_after_ms"],
        "D": s["D"], "T": s["T"], "S": s["S"], "S_on_time": s["S_on_time"], "S_late": s["S_late"],
        "latency_p50": s["latency_ms"]["p50"], "latency_p90": s["latency_ms"]["p90"],
        "linger_p50": s["linger_ms"]["p50"], "linger_p90": s["linger_ms"]["p90"], "sends_avg": s["sends_avg"], "shows_avg": s["shows_avg"],
        "short_S": ev["short_lines"]["S"], "cer_avg": s["cer_avg"], "order_violations": ev["order_violations"], "wrong_displays": ev["wrong_displays"],
        "cpu_app_pct": r["cpu"]["app_pct"], "cpu_all_pct": r["cpu"]["all_pct"], "cpu_app_core_pct": r["cpu"]["app_core_pct"],
        "ocr_avg_ms": r["reads"]["ocr_avg_ms"], "pass_avg_ms": r["reads"]["pass_avg_ms"], "fps_p50": r["reads"]["fps"]["p50"],
    });
    // each area's shown share (the dialogue's subtitles first of all)
    if let (Some(by_area), Some(obj)) = (ev["by_area"].as_object(), row.as_object_mut()) {
        for (area, t) in by_area {
            obj.insert(format!("S_{area}"), t["S"].clone());
            obj.insert(format!("latency_p50_{area}"), t["latency_ms"]["p50"].clone());
        }
    }
    row
}

/// A run's rounds averaged (numbers only; the lowest and highest latency and CPU kept too).
fn average_rows(name: &str, rows: &[&Value]) -> Option<Value> {
    let first = rows.first()?;
    let mut out = serde_json::Map::new();
    out.insert("name".into(), json!(name));
    out.insert("rounds".into(), json!(rows.len()));
    for (k, _) in first.as_object()?.iter().filter(|(k, _)| *k != "name" && *k != "round") {
        let v: Vec<f64> = rows.iter().filter_map(|r| r[k].as_f64()).collect();
        let avg = if v.is_empty() { Value::Null } else { json!((v.iter().sum::<f64>() / v.len() as f64 * 1000.0).round() / 1000.0) };
        out.insert(k.clone(), avg);
        if matches!(k.as_str(), "latency_p50" | "latency_p90" | "cpu_app_pct" | "cpu_all_pct") && !v.is_empty() {
            let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(*x), hi.max(*x)));
            out.insert(format!("{k}_min"), json!(lo));
            out.insert(format!("{k}_max"), json!(hi));
        }
    }
    Some(Value::Object(out))
}

/// Rows as CSV: the run and its settings first, then how it did and what it cost, then the rest.
fn to_csv(rows: &[Value]) -> String {
    const FIRST: [&str; 18] = [
        "name", "rounds", "interval_ms", "stable_ms", "clear_after_ms", "capture_fps", "S", "S_on_time", "S_late", "D", "T",
        "latency_p50", "latency_p90", "linger_p50", "linger_p90", "cpu_app_pct", "cpu_all_pct", "cpu_app_core_pct",
    ];
    let Some(keys) = rows.first().and_then(|r| r.as_object()).map(|o| o.keys().cloned().collect::<Vec<_>>()) else { return String::new() };
    let mut cols: Vec<String> = FIRST.iter().filter(|k| keys.iter().any(|x| x == *k)).map(|k| k.to_string()).collect();
    cols.extend(keys.into_iter().filter(|k| !FIRST.contains(&k.as_str())));
    let cell = |v: &Value| match v {
        Value::Null => String::new(),
        Value::String(s) => format!("\"{}\"", s.replace('"', "\"\"")),
        other => other.to_string(),
    };
    let mut out = cols.join(",") + "\n";
    for r in rows {
        out += &cols.iter().map(|c| cell(&r[c.as_str()])).collect::<Vec<_>>().join(",");
        out += "\n";
    }
    out
}

// ------------------------------------------------------------------------------------- a session's log

/// `--summarize lines.log [--from <Unix ms>] [--to <Unix ms>] [--report out.json]`: what the
/// measurement log says about a session played with the app (no reference needed): how fast lines went
/// up after their English appeared, what the CPU did (this app and the whole PC, the Xbox player's
/// video included), each second's reading times and frames, the TV's drawing, what was left out and
/// why — over the whole session and minute by minute (a PC slowing down as it warms up shows there).
fn summarize(log: &Path, from: Option<u64>, to: Option<u64>, report: &Path) -> anyhow::Result<()> {
    // the older copy first (lines.log.1), then the log
    let mut events: Vec<Value> = Vec::new();
    for f in [linelog::rotated(log), log.to_path_buf()] {
        if let Ok(text) = std::fs::read_to_string(&f) {
            events.extend(text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()));
        }
    }
    let t = |e: &Value| e["t"].as_u64().unwrap_or(0);
    events.retain(|e| from.is_none_or(|f| t(e) >= f) && to.is_none_or(|x| t(e) <= x));
    events.sort_by_key(|e| t(e));
    anyhow::ensure!(!events.is_empty(), "no events in {} (is the measurement log on?)", log.display());
    let summary = session_summary(&events);
    eprintln!(
        "session: {} min · lines shown {} (late {}) · English → Arabic p50 {} ms p90 {} ms · CPU this app p50 {} % · the whole PC p50 {} % p90 {} % · frames {} /s · reading pass p50 {} ms",
        summary["minutes"], summary["counts"]["shown"], summary["counts"]["late"], summary["latency_ms"]["p50"], summary["latency_ms"]["p90"],
        summary["cpu_app_pct"]["p50"], summary["cpu_all_pct"]["p50"], summary["cpu_all_pct"]["p90"], summary["fps"]["p50"], summary["pass_avg_ms"]["p50"]
    );
    std::fs::write(report, serde_json::to_string_pretty(&summary)?)?;
    eprintln!("session summary → {}", report.display());
    Ok(())
}

pub fn session_summary(events: &[Value]) -> Value {
    let t = |e: &Value| e["t"].as_u64().unwrap_or(0);
    let of = |kind: &'static str| events.iter().filter(move |e| e["ev"] == kind);
    let nums = |kind: &'static str, f: &dyn Fn(&Value) -> Option<f64>| of(kind).filter_map(f).collect::<Vec<f64>>();
    let (t0, t1) = (events.first().map(t).unwrap_or(0), events.last().map(t).unwrap_or(0));
    let count = |kind: &'static str| of(kind).count();
    let mut reasons: BTreeMap<String, u64> = BTreeMap::new();
    for e in of("filtered") {
        *reasons.entry(e["reason"].as_str().unwrap_or("?").to_string()).or_default() += 1;
    }
    let latency = |e: &Value| Some(t(e).checked_sub(e["at"].as_u64()?)? as f64);
    // minute by minute: the CPU, the reading, the lines and how fast they went up
    let mut minutes: BTreeMap<u64, (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for e in events {
        let m = (t(e).saturating_sub(t0)) / 60_000;
        let slot = minutes.entry(m).or_default();
        if e["ev"] == "reads" {
            slot.0.extend(e["cpu_app"].as_f64());
            slot.1.extend(e["cpu_all"].as_f64());
            slot.2.extend(e["pass_avg"].as_f64());
        } else if e["ev"] == "shown" {
            slot.3.extend(latency(e));
        }
    }
    let avg = |v: &[f64]| if v.is_empty() { Value::Null } else { json!((v.iter().sum::<f64>() / v.len() as f64 * 10.0).round() / 10.0) };
    let timeline: Vec<Value> = minutes
        .iter()
        .map(|(m, (app, all, pass, lat))| json!({ "minute": m, "cpu_app_pct": avg(app), "cpu_all_pct": avg(all), "pass_avg_ms": avg(pass), "shown": lat.len(), "latency_ms_avg": avg(lat) }))
        .collect();
    json!({
        "from_unix_ms": t0, "to_unix_ms": t1, "minutes": ((t1 - t0) as f64 / 60_000.0 * 10.0).round() / 10.0,
        "counts": {
            "seen": count("seen"), "sent": count("sent"), "translated": count("translated"),
            "from_memory": of("translated").filter(|e| e["hit"] == true).count(), "shown": count("shown"),
            "late": of("shown").filter(|e| e["late"] == true).count(), "skipped": count("skipped"),
            "replaced": count("replaced"), "dropped": count("dropped"), "hidden": count("hidden"),
        },
        // (from the English appearing — as first read — to its Arabic up)
        "latency_ms": spread(&nums("shown", &latency)),
        // (from the frame's capture to its first reading: the frame's age and the reading)
        "first_read_ms": spread(&nums("seen", &|e| Some(t(e).checked_sub(e["captured"].as_u64()?)? as f64))),
        "translate_ms": {
            "memory": spread(&nums("translated", &|e| (e["hit"] == true).then(|| e["ms"].as_f64()).flatten())),
            "translator": spread(&nums("translated", &|e| (e["hit"] == false && e["ok"] == true).then(|| e["ms"].as_f64()).flatten())),
        },
        "shown_ms": spread(&nums("hidden", &|e| e["shown_ms"].as_f64())),
        "cpu_app_pct": spread(&nums("reads", &|e| e["cpu_app"].as_f64())),
        "cpu_all_pct": spread(&nums("reads", &|e| e["cpu_all"].as_f64())),
        "fps": spread(&nums("reads", &|e| e["fps"].as_f64())),
        "pass_avg_ms": spread(&nums("reads", &|e| e["pass_avg"].as_f64())),
        "ocr_avg_ms": spread(&nums("reads", &|e| e["ocr_avg"].as_f64())),
        "tv": {
            "sent_to_drawn_ms": spread(&nums("tv_drawn", &|e| Some(e["est"].as_f64()? - e["sent"].as_f64()?))),
            "draw_ms": spread(&nums("tv_drawn", &|e| e["draw_ms"].as_f64())),
        },
        "filtered": reasons,
        "timeline": timeline,
    })
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
    fn fork_settings_are_set_as_their_sliders_set_them() {
        let args: Vec<String> = ["x", "--set", "interval_ms=100", "--set", "capture_fps=12"].iter().map(|s| s.to_string()).collect();
        let sets = settings_given(&args).unwrap();
        assert_eq!(sets, vec![("interval_ms".to_string(), 100), ("capture_fps".to_string(), 12)]);
        // (outside a slider's range, or not a setting: refused)
        for bad in ["interval_ms=5", "clear_after_ms=4000", "speed=3", "stable_ms=abc", "stable_ms"] {
            let a: Vec<String> = ["x", "--set", bad].iter().map(|s| s.to_string()).collect();
            assert!(settings_given(&a).is_err(), "{bad}");
        }
        // the game's timing in use (its layout's): moving one slider keeps the other two as they were in use
        let mut c = Config::default();
        c.timing_custom = Some(false);
        let gta = c.builtins.iter_mut().find(|p| p.id == c.profile).expect("the profile in use");
        gta.timing = config::Timing { interval_ms: 40, stable_ms: 200, clear_after_ms: 500 };
        apply_settings(&mut c, &sets);
        assert_eq!(c.timing(), (100, 200, 500));
        assert_eq!((c.timing_custom, c.capture_fps), (Some(true), 12));
        assert_eq!(settings_in_use(&c)["interval_ms"], 100);
    }

    #[test]
    fn fork_a_sweep_shuffles_each_round_the_same_way_every_time() {
        for round in 0..4 {
            let a = shuffled(7, round);
            assert_eq!(a, shuffled(7, round), "the same order every time");
            let mut sorted = a.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..7).collect::<Vec<_>>(), "every run once");
        }
        assert_ne!(shuffled(7, 0), shuffled(7, 1), "another order each round");
        // the rounds averaged, the spread of latency and CPU kept
        let rows = [json!({ "name": "a", "round": 1, "S": 0.9, "latency_p50": 150.0, "cpu_app_pct": 20.0 }), json!({ "name": "a", "round": 2, "S": 0.8, "latency_p50": 170.0, "cpu_app_pct": 30.0 })];
        let avg = average_rows("a", &rows.iter().collect::<Vec<_>>()).unwrap();
        assert_eq!((avg["S"].as_f64(), avg["latency_p50"].as_f64(), avg["latency_p50_min"].as_f64(), avg["cpu_app_pct_max"].as_f64()), (Some(0.85), Some(160.0), Some(150.0), Some(30.0)));
        assert_eq!(to_csv(&[avg]).lines().count(), 2);
    }

    #[test]
    fn fork_the_example_sweep_plan_is_one_the_sweep_takes() {
        let plan: SweepPlan = serde_json::from_str(include_str!("../../.fork/sweep-plan.example.json")).expect("the plan reads");
        check_settings("base", &plan.base).unwrap();
        for r in &plan.runs {
            check_settings(&r.name, &r.set).unwrap();
        }
        assert!(plan.runs.iter().any(|r| r.set.is_empty()), "the settings in use, to compare with");
    }

    #[test]
    fn fork_a_sessions_log_is_summed_up() {
        let s = 2_000_000u64;
        let events = vec![
            ev("reads", json!({ "cpu_app": 20.0, "cpu_all": 60.0, "fps": 12.0, "pass_avg": 90.0, "ocr_avg": 30.0, "t": s })),
            ev("seen", json!({ "area": "subtitles", "captured": s + 900, "t": s + 1000 })),
            ev("translated", json!({ "area": "subtitles", "seq": 1, "ok": true, "hit": true, "ms": 0.4, "t": s + 1010 })),
            ev("shown", json!({ "area": "subtitles", "seq": 1, "at": s + 1000, "late": false, "t": s + 1150 })),
            ev("filtered", json!({ "area": "subtitles", "reason": "clean", "t": s + 2000 })),
            ev("reads", json!({ "cpu_app": 40.0, "cpu_all": 80.0, "fps": 12.0, "pass_avg": 110.0, "ocr_avg": 35.0, "t": s + 61_000 })),
            ev("tv_drawn", json!({ "sent": (s + 1200) as f64, "est": (s + 1260) as f64, "draw_ms": 30.0, "t": s + 1300 })),
        ];
        let r = session_summary(&events);
        assert_eq!(r["counts"]["shown"], 1);
        assert_eq!(r["latency_ms"]["p50"], 150.0);
        assert_eq!(r["first_read_ms"]["p50"], 100.0);
        assert_eq!(r["cpu_all_pct"]["max"], 80.0);
        assert_eq!(r["tv"]["sent_to_drawn_ms"]["p50"], 60.0);
        assert_eq!(r["filtered"]["clean"], 1);
        assert_eq!(r["timeline"].as_array().map(Vec::len), Some(2), "minute by minute");
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
        // its Arabic stayed 50 ms after the English went (up at 1250 for 1800 ms; the English gone at 3000)
        assert_eq!(r["lines"][0]["linger_ms"], 50);
        assert_eq!((r["lines"][0]["sends"].as_u64(), r["lines"][0]["shows"].as_u64()), (Some(1), Some(1)));
        assert_eq!(r["summary"]["sends_avg"], 1.0);
    }
}

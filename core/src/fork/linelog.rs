//! The per-line measurement log (the fork's Phase 0.1): what happened to each line the app read —
//! when it was first seen, sent to the translator, translated (from where), shown, hidden, sent to and
//! drawn on a TV — and, when a line was lost, why (a filter, a newer line, a late translation …).
//!
//! It only records: nothing the app reads, translates or shows depends on it. Off by default (the
//! `line_log` switch in fork-settings.json); while it's off, every call returns at once.
//!
//! One JSON object per line of `lines.log` in the data folder, with `t` (Unix ms) and `ev` (the kind):
//! - `log`         {app, state}: the log was turned on;
//! - `reads`       once a second: {n, ocr_avg, ocr_max, pass_n, pass_avg, pass_max, areas, suppressed}
//!                 (areas read with OCR, and passes of the reading);
//! - `seen`        {area, text, frame, captured, ocr_ms, sure}: a new reading of an area (a candidate);
//! - `filtered`    {area, reason, text}: text read but left out (see REASONS);
//! - `sent`        {area, seq, en, parts, at, frame, captured}: a line sent to the translator;
//! - `skipped`     {area, seq, en, reason}: a line not translated (a newer one replaced it first);
//! - `translated`  {area, seq, ok, ms, hit, sources, error_kind, error}: its translation;
//! - `dropped`     {area, seq, reason}: a translation the presenter turned down (a page took over);
//! - `replaced`    {area, seq, by}: a translation waiting to go up, replaced by a newer one first;
//! - `shown`       {area, seq, at, en, late}: a translation up (`late`: its English had already gone);
//! - `hidden`      {area, seq, shown_ms}: a translation taken down;
//! - `cleared`     {upto, reason}: everything up taken down (a page took over, another language);
//! - `page`        {id, on}: a game page (menu …) shown or gone;
//! - `tv_sent`     {tv, version, shown}: the overlays' state sent to a TV ({area: line's `at`});
//! - `tv_drawn`    {tv, version, sent, recv, draw_ms, est}: the TV drew it (`est`: when, on this PC's clock).
//!
//! Version 2 (`log` {version: 2}) adds, for each line's timeline (see timeline.rs and FORK.md):
//! - `seen`        + {prev_captured, prev_text, known}: the frame read before (its time, text in it or
//!                 not: the English appeared between them), and whether the memory knows the reading;
//! - `sent`        + {appear_after, appear_by}: the frames the line appeared between;
//! - `shown`       + {ar}: the Arabic as drawn;
//! - `gap`         {area, seq, frames, last_present, first_absent, last_absent, back, text_px, other}: an OCR
//!                 blink of the line up (`text_px`: frames whose pixels still showed text);
//! - `ended`       {area, seq, how, by, last_present, first_absent, absent_frames, absent_text_px,
//!                 absent_other}: the line up ended (`how`: gone, replaced, page, language);
//! - `unread`      {area, from, to, frames, score, reasons, then_read}: text on screen the reading accepted
//!                 nothing of (a line lost), with the filters' reasons meanwhile;
//! - `reads`       + {stream_fps, copy_ms, preview_ms, age_ms, idle_ms, same_frame, done_ms, reader_cpu_ms,
//!                 log_ms, det, detect_ms}: the source's frames, what taking one costs, a pass's frame age
//!                 and wait, the reading thread's CPU, the log's own cost, the unread-text finder.
//!
//! One file per session (each time the log is turned on): `lines/lines-<UTC date and time>.log` in the data
//! folder; at `line_log_mb` MB it goes on in a further part (`…-part2.log`, `…-part3.log` …), nothing is
//! deleted (the log's older single file, `lines.log` and `lines.log.1`, is left as it is and still read by
//! the tools). Readings are summed up once a second, not written one by one; repeated `filtered` events for
//! the same text are written once.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Why text read in an area was left out (`filtered` events), as the reading decides it.
pub const REASONS: [&str; 7] = ["clean", "not_centered", "no_bottom_line", "help_anchor", "help_not_sentence", "not_words", "reviewed_only"];

/// The folder of the log's sessions in the data folder (one file per session, and its further parts).
pub const DIR: &str = "lines";

/// The log's older single file (before sessions), still read by the tools (`session_files`).
pub const FILE: &str = "lines.log";

/// The most parts a session's file goes on in (a guard against a full disk: at 16 MB a part, far more than
/// any session writes — about 3 MB in half an hour).
const MAX_PARTS: u32 = 256;

static ON: AtomicBool = AtomicBool::new(false);

struct Sinks {
    /// the sessions' folder written to (by its own thread), the size limit of a part and the way to it
    file: Option<(PathBuf, u64, mpsc::Sender<String>)>,
    /// events kept in memory too (the replay tool, tests)
    collector: Option<Arc<Mutex<Vec<Value>>>>,
}

static SINKS: Mutex<Sinks> = Mutex::new(Sinks { file: None, collector: None });

/// Is the log on? (Every hook checks this first: off, nothing else runs.)
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

fn refresh(s: &Sinks) {
    ON.store(s.file.is_some() || s.collector.is_some(), Ordering::Relaxed);
    // (a log started again says the settings in use again, and its per-second readings start afresh: not
    // over the time it was off)
    *SETTINGS.lock().unwrap() = None;
    *READS.lock().unwrap() = None;
    super::timeline::reset();
    SOURCE_BAD_LAST.store(u64::MAX, Ordering::Relaxed);
}

/// Write the log in the folder `dir` (None: stop writing it): a new session's file, going on in further
/// parts of `max_mb` MB each (nothing deleted). The same folder and size again: the session goes on.
pub fn configure_file(dir: Option<PathBuf>, max_mb: u32) {
    let max = u64::from(max_mb.clamp(1, 64)) * 1024 * 1024;
    {
        let mut s = SINKS.lock().unwrap();
        if let (Some((d, m, _)), Some(nd)) = (&s.file, &dir) {
            if d == nd && *m == max {
                return;
            }
        }
        // (the old writer ends once it has written what it was sent)
        s.file = dir.map(|d| {
            let (tx, rx) = mpsc::channel::<String>();
            let stem = session_stem(&d, unix_ms());
            let dc = d.clone();
            let _ = std::thread::Builder::new().name("lines.log".into()).spawn(move || write_loop(&dc, &stem, max, rx));
            (d, max, tx)
        });
        refresh(&s);
    }
    if on() {
        // (the machine's logical CPUs: what a share of "the whole machine" is a share of)
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
        emit("log", json!({ "app": crate::VERSION, "state": "on", "version": 2, "cpus": cpus }));
    }
}

/// Keep the events in `collector` too (None: stop): the replay tool and tests read them there.
pub fn set_collector(collector: Option<Arc<Mutex<Vec<Value>>>>) {
    let mut s = SINKS.lock().unwrap();
    s.collector = collector;
    refresh(&s);
}

/// The older copy the log's older single file (`lines.log`) kept when it rotated.
pub fn rotated(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

/// A UTC time as `YYYY-MM-DD_HH-MM-SSZ` (a file name's: no colons).
pub fn utc_stamp(unix_ms: u64) -> String {
    let secs = unix_ms / 1000;
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // (days since 1970-01-01 to a civil date: H. Hinnant's algorithm)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// A new session's name in `dir`: `lines-<UTC time>` (another session started the same second: `-2` …).
fn session_stem(dir: &Path, now_ms: u64) -> String {
    let base = format!("lines-{}", utc_stamp(now_ms));
    let mut stem = base.clone();
    let mut n = 2;
    while dir.join(format!("{stem}.log")).exists() {
        stem = format!("{base}-{n}");
        n += 1;
    }
    stem
}

/// A session's part `k` (1: its first file).
fn part_path(dir: &Path, stem: &str, k: u32) -> PathBuf {
    if k <= 1 { dir.join(format!("{stem}.log")) } else { dir.join(format!("{stem}-part{k}.log")) }
}

/// Every file of the session `path` is part of, in order: the session's parts (`lines-….log`,
/// `…-part2.log` …), or the older single file with its older copy first (`lines.log.1`, `lines.log`).
pub fn session_files(path: &Path) -> Vec<PathBuf> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    if let Some(stem) = name.strip_suffix(".log").filter(|n| n.starts_with("lines-")) {
        // (any part given: the session's own name)
        let stem = match stem.rsplit_once("-part") {
            Some((s, k)) if k.parse::<u32>().is_ok() => s,
            _ => stem,
        };
        return (1..=MAX_PARTS).map(|k| part_path(&dir, stem, k)).take_while(|p| p.exists()).collect();
    }
    [rotated(path), path.to_path_buf()].into_iter().filter(|p| p.exists()).collect()
}

fn write_loop(dir: &Path, stem: &str, max: u64, rx: mpsc::Receiver<String>) {
    let open = |p: &Path| std::fs::OpenOptions::new().create(true).append(true).open(p);
    let _ = std::fs::create_dir_all(dir);
    let mut part = 1u32;
    let Ok(mut f) = open(&part_path(dir, stem, part)) else { return };
    let mut size = 0u64;
    for line in rx {
        let n = line.len() as u64 + 1;
        // (a part full: the session goes on in the next one; nothing is deleted)
        if size > 0 && size + n > max && part < MAX_PARTS {
            part += 1;
            let Ok(nf) = open(&part_path(dir, stem, part)) else { return };
            f = nf;
            let head = json!({ "ev": "log", "state": "part", "part": part, "session": stem, "t": unix_ms() }).to_string();
            size = if f.write_all(head.as_bytes()).and_then(|_| f.write_all(b"\n")).is_ok() { head.len() as u64 + 1 } else { 0 };
        }
        if f.write_all(line.as_bytes()).and_then(|_| f.write_all(b"\n")).is_ok() {
            size += n;
        }
    }
}

/// Now, in Unix ms.
pub fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// An instant of this run, in Unix ms.
pub fn at_ms(t: Instant) -> u64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    now.saturating_sub(t.elapsed()).as_millis() as u64
}

/// Record an event (`fields`: a JSON object; `t` and `ev` are added).
pub fn emit(ev: &str, fields: Value) {
    if !on() {
        return;
    }
    let t0 = Instant::now();
    let mut obj = match fields {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert("t".into(), json!(unix_ms()));
    obj.insert("ev".into(), json!(ev));
    let v = Value::Object(obj);
    let s = SINKS.lock().unwrap();
    if let Some(c) = &s.collector {
        c.lock().unwrap().push(v.clone());
    }
    if let Some((_, _, tx)) = &s.file {
        let _ = tx.send(v.to_string());
    }
    drop(s);
    add_self(t0.elapsed());
}

thread_local! {
    /// the time this thread spent in the log's own work since its last per-second readings (the reading's
    /// thread: what the log costs the reading)
    static SELF_TIME: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
    /// this pass's start (Unix ms), the last pass's end and frame (the reading's thread)
    static PASS: std::cell::Cell<(u64, Option<Instant>, Option<Instant>, Option<u64>)> = const { std::cell::Cell::new((0, None, None, None)) };
}

/// Time spent in the log's own work on this thread.
pub(crate) fn add_self(d: Duration) {
    SELF_TIME.with(|s| s.set(s.get() + d));
}

/// A text as it goes into the log: at most 300 characters.
pub fn excerpt(text: &str) -> String {
    text.chars().take(300).collect()
}

/// A text with only its letters and digits, lower case, one space between words: what tells two
/// readings of the same line apart from two different lines (for the log's repeats).
pub fn norm(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------------------------------- frames

/// A full frame was captured now: its number, and its size (the size read for text: the game window's own
/// for a window). (The capture's hook: `note_frame` with the time, and the size for the per-second readings.)
pub fn note_captured(seq: u64, w: u32, h: u32) {
    if !on() {
        return;
    }
    FRAME_W.store(w, Ordering::Relaxed);
    FRAME_H.store(h, Ordering::Relaxed);
    note_frame(seq, unix_ms());
}

/// A preview frame was made (the picture in the app's window: made only while it's watched).
pub fn note_preview() {
    if on() {
        PREVIEWS_NOW.fetch_add(1, Ordering::Relaxed);
    }
}

/// A frame arrived from the source (the window's capture: all of them, not only those taken).
pub fn note_arrived() {
    if on() {
        ARRIVED_NOW.fetch_add(1, Ordering::Relaxed);
    }
}

/// Taking a full frame (copying it out of the capture) took `d`.
pub fn note_copy_cost(d: Duration) {
    if on() {
        COPY_US.fetch_add(d.as_micros() as u64, Ordering::Relaxed);
        COPY_N.fetch_add(1, Ordering::Relaxed);
    }
}

/// (1.5.0) Called on the source's own thread at each of its frames (the app's Xbox player: on its decoder's
/// thread): that thread's CPU, sampled four times a second at most, goes into the second's readings.
pub fn note_source_cpu() {
    if !on() {
        return;
    }
    thread_local! {
        static LAST: std::cell::Cell<Option<(Instant, f64)>> = const { std::cell::Cell::new(None) };
    }
    LAST.with(|last| {
        let now = Instant::now();
        if last.get().is_some_and(|(t, _)| now.duration_since(t) < Duration::from_millis(250)) {
            return;
        }
        if let Some(ms) = super::cpu::thread_ms() {
            if let Some((_, before)) = last.get() {
                SOURCE_CPU_US.fetch_add(((ms - before).max(0.0) * 1000.0) as u64, Ordering::Relaxed);
            }
            last.set(Some((now, ms)));
        }
    });
}

/// (1.5.0) The source's frames it couldn't decode so far (a running count: the app's Xbox player's). The
/// first count told after the log starts is where it counts from; a count lower than the last (a new
/// stream) starts again from there.
pub fn note_source_bad_frames(total: u64) {
    if !on() {
        return;
    }
    let prev = SOURCE_BAD_LAST.swap(total, Ordering::Relaxed);
    if prev != u64::MAX && total > prev {
        SOURCE_BAD_NEW.fetch_add(total - prev, Ordering::Relaxed);
    }
}

/// Making a preview frame (shrinking it, or encoding it as JPEG) took `d`.
pub fn note_preview_cost(d: Duration) {
    if on() {
        PREVIEW_US.fetch_add(d.as_micros() as u64, Ordering::Relaxed);
    }
}

/// The timing in use (Source → Timing, the 4K picture): written as a `settings` event when it changes,
/// and when the log starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub capture_fps: u32,
    pub preview_fps: u32,
    pub picture_4k: bool,
    pub interval_ms: u64,
    pub stable_ms: u64,
    pub clear_after_ms: u64,
    /// the player's own timing (else the game's, from its layout)
    pub custom: bool,
    /// (1.5.0) when the English counts as gone as the reader applies it: the subtitles (lines that stack
    /// up: Clear after, 250 ms at the least) and the other areas (350 at the least)
    pub clear_subtitles_ms: u64,
    pub clear_other_ms: u64,
    /// (1.5.0) the subtitles' dropout floor itself (250 ms): a Clear after below it does what it does
    pub clear_floor_ms: u64,
    /// the picture's source, its kind only (`source_kind`): never a window's title
    pub source: &'static str,
}

/// The kind of a source as Source names it (`Config::window_title`): the app's own Xbox player
/// ("xbox:", 1.5.0), the PS5's, OBS, a capture card ("device:"), a screen ("screen:"), else a window (the
/// Xbox app's, Microsoft's web player, …) — its title never goes into the log.
pub fn source_kind(source: &str) -> &'static str {
    match source.split_once(':').map(|(k, _)| k) {
        _ if source.is_empty() => "none",
        Some("xbox") => "xbox",
        Some("ps5") => "ps5",
        Some("obs") => "obs",
        Some("device") => "device",
        Some("screen") => "screen",
        _ => "window",
    }
}

static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);

pub fn note_settings(s: Settings) {
    if !on() {
        return;
    }
    {
        let mut last = SETTINGS.lock().unwrap();
        if *last == Some(s) {
            return;
        }
        *last = Some(s);
    }
    emit(
        "settings",
        json!({
            "capture_fps": s.capture_fps, "preview_fps": s.preview_fps, "picture_4k": s.picture_4k, "interval_ms": s.interval_ms,
            "stable_ms": s.stable_ms, "clear_after_ms": s.clear_after_ms, "timing_custom": s.custom,
            "clear_subtitles_ms": s.clear_subtitles_ms, "clear_other_ms": s.clear_other_ms, "clear_floor_ms": s.clear_floor_ms, "source": s.source,
        }),
    );
}

/// When each recent frame was captured (its number, Unix ms): a line's first sighting is measured from
/// when its frame was taken, not from when it was read.
static FRAMES: Mutex<VecDeque<(u64, u64)>> = Mutex::new(VecDeque::new());

/// A frame was captured now (the capture calls it, with the frame's number).
pub fn note_frame(seq: u64, captured_ms: u64) {
    if !on() {
        return;
    }
    FRAMES_NOW.fetch_add(1, Ordering::Relaxed);
    let mut f = FRAMES.lock().unwrap();
    if f.len() >= 512 {
        f.pop_front();
    }
    f.push_back((seq, captured_ms));
}

/// When frame `seq` was captured (Unix ms), if it was noted.
pub fn captured_of(seq: u64) -> Option<u64> {
    FRAMES.lock().unwrap().iter().rev().find(|(s, _)| *s == seq).map(|(_, ms)| *ms)
}

// ------------------------------------------------------------------------------------- readings

/// Frames captured since the last per-second readings (their `fps`), preview frames made (`preview_fps`),
/// and the last full frame's size.
static FRAMES_NOW: AtomicU32 = AtomicU32::new(0);
static PREVIEWS_NOW: AtomicU32 = AtomicU32::new(0);
/// frames arrived from the source, and the time spent taking full frames and making preview ones (µs)
static ARRIVED_NOW: AtomicU32 = AtomicU32::new(0);
static COPY_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static COPY_N: AtomicU32 = AtomicU32::new(0);
static PREVIEW_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// (1.5.0) the source's own thread — the app's Xbox player decoding the stream (OpenH264, on the CPU) —
/// its CPU time (µs), and the frames it couldn't decode (its running count, as last told)
static SOURCE_CPU_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// (the running count as last told — `u64::MAX`: none yet since the log started —, and the frames it
/// grew by since the last second's readings)
static SOURCE_BAD_LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
static SOURCE_BAD_NEW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FRAME_W: AtomicU32 = AtomicU32::new(0);
static FRAME_H: AtomicU32 = AtomicU32::new(0);

#[derive(Default)]
struct Reads {
    since: Option<Instant>,
    /// the CPU times when this second's readings began
    cpu: Option<super::cpu::Times>,
    n: u32,
    total: f64,
    max: f64,
    passes: u32,
    pass_total: f64,
    pass_max: f64,
    areas: HashMap<String, (u32, f64)>,
    /// each part of the passes (pages, phone, chat, areas, and what comes before them): total ms
    stages: HashMap<&'static str, f64>,
    suppressed: u32,
    /// the reading thread's own CPU time when this second's readings began (ms)
    thread_cpu: Option<f64>,
    /// a pass's frame: its age when the pass began (ms), the time since the pass before ended (ms), and
    /// passes on a frame read before (no new one came)
    age: (u32, f64),
    idle: (u32, f64),
    same_frame: u32,
    /// each area: the time from the pass's start to its reading done (ms)
    done: HashMap<String, (u32, f64)>,
    /// the unread-text finder: the pixels' text × the reading's (both, pixels only, reading only, neither),
    /// and its time
    det: [u32; 4],
    detect: (u32, Duration),
}

static READS: Mutex<Option<Reads>> = Mutex::new(None);

fn with_reads(f: impl FnOnce(&mut Reads)) {
    let event = {
        let mut guard = READS.lock().unwrap();
        let r = guard.get_or_insert_with(Reads::default);
        let now = Instant::now();
        let since = *r.since.get_or_insert_with(|| {
            r.cpu = super::cpu::now();
            r.thread_cpu = super::cpu::thread_ms();
            FRAMES_NOW.store(0, Ordering::Relaxed);
            PREVIEWS_NOW.store(0, Ordering::Relaxed);
            ARRIVED_NOW.store(0, Ordering::Relaxed);
            COPY_US.store(0, Ordering::Relaxed);
            COPY_N.store(0, Ordering::Relaxed);
            PREVIEW_US.store(0, Ordering::Relaxed);
            SOURCE_CPU_US.store(0, Ordering::Relaxed);
            SOURCE_BAD_NEW.store(0, Ordering::Relaxed);
            SELF_TIME.with(|s| s.set(Duration::ZERO));
            now
        });
        f(r);
        let span = now.duration_since(since);
        if span < Duration::from_secs(1) {
            return;
        }
        // (this app's share of the whole machine, and everything's, over this second; frames captured a second)
        let cpu_now = super::cpu::now();
        let shares = r.cpu.as_ref().zip(cpu_now.as_ref()).and_then(|(a, b)| super::cpu::share(a, b));
        let fps = round1(f64::from(FRAMES_NOW.swap(0, Ordering::Relaxed)) / span.as_secs_f64());
        let previews = PREVIEWS_NOW.swap(0, Ordering::Relaxed);
        let preview_fps = round1(f64::from(previews) / span.as_secs_f64());
        let (fw, fh) = (FRAME_W.load(Ordering::Relaxed), FRAME_H.load(Ordering::Relaxed));
        let areas: serde_json::Map<String, Value> = r
            .areas
            .iter()
            .map(|(a, (n, t))| (a.clone(), json!({ "n": n, "avg": round1(t / f64::from((*n).max(1))) })))
            .collect();
        let secs = span.as_secs_f64();
        // (the previews counted above: read once — loaded again after it was set to 0, preview_ms was always empty)
        let (copy_us, copy_n) = (COPY_US.swap(0, Ordering::Relaxed), COPY_N.swap(0, Ordering::Relaxed));
        let preview_us = PREVIEW_US.swap(0, Ordering::Relaxed);
        let avg = |(n, t): (u32, f64)| (n > 0).then(|| round1(t / f64::from(n)));
        let thread_cpu = super::cpu::thread_ms();
        let log_ms = SELF_TIME.with(|s| s.replace(Duration::ZERO)).as_secs_f64() * 1000.0;
        // (1.5.0) a source with a thread of its own (the app's Xbox player) told its CPU this second
        let source_cpu = SOURCE_CPU_US.load(Ordering::Relaxed) > 0;
        let ev = json!({
            "n": r.n, "ocr_avg": round1(r.total / f64::from(r.n.max(1))), "ocr_max": round1(r.max),
            "pass_n": r.passes, "pass_avg": round1(r.pass_total / f64::from(r.passes.max(1))), "pass_max": round1(r.pass_max),
            "areas": areas, "suppressed": r.suppressed,
            // (each part's time a pass, ms: where a pass's time goes)
            "stages": r.stages.iter().map(|(k, v)| ((*k).to_string(), json!(round1(v / f64::from(r.passes.max(1)))))).collect::<serde_json::Map<String, Value>>(),
            "cpu_app": shares.map(|s| s.0), "cpu_all": shares.map(|s| s.1), "fps": fps, "preview_fps": preview_fps,
            "frame_w": (fw > 0).then_some(fw), "frame_h": (fh > 0).then_some(fh),
            // (v2) frames arriving from the source a second; taking a full frame and making a preview one (ms each)
            "stream_fps": round1(f64::from(ARRIVED_NOW.swap(0, Ordering::Relaxed)) / secs),
            "copy_ms": (copy_n > 0).then(|| round1(copy_us as f64 / 1000.0 / f64::from(copy_n))),
            "preview_ms": (previews > 0 && preview_us > 0).then(|| round1(preview_us as f64 / 1000.0 / f64::from(previews))),
            // (1.5.0) the source's own thread (the app's Xbox player decoding the stream): its CPU this second
            // (ms), and the frames it couldn't decode this second
            "source_cpu_ms": source_cpu.then_some(round1(SOURCE_CPU_US.swap(0, Ordering::Relaxed) as f64 / 1000.0)),
            "bad_frames": source_cpu.then_some(SOURCE_BAD_NEW.swap(0, Ordering::Relaxed)),
            // (v2) a pass's frame: its age when the pass began, the wait since the pass before, passes on an old frame
            "age_ms": avg(r.age), "idle_ms": avg(r.idle), "same_frame": r.same_frame,
            "done_ms": r.done.iter().map(|(a, v)| (a.clone(), json!(avg(*v)))).collect::<serde_json::Map<String, Value>>(),
            // (v2) the reading thread's CPU this second, and the log's own work on it (ms)
            "reader_cpu_ms": r.thread_cpu.zip(thread_cpu).map(|(a, b)| round1((b - a).max(0.0))),
            "log_ms": round1(log_ms),
            // (v2) the unread-text finder: text in the pixels × text read (tt, tn: pixels only, nt: read only, nn)
            "det": { "tt": r.det[0], "tn": r.det[1], "nt": r.det[2], "nn": r.det[3] },
            "detect_ms": (r.detect.0 > 0).then(|| (r.detect.1.as_secs_f64() * 1000.0 / f64::from(r.detect.0) * 1000.0).round() / 1000.0),
        });
        *r = Reads { since: Some(now), cpu: cpu_now, thread_cpu, ..Reads::default() };
        ev
    };
    emit("reads", event);
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// An area was read with OCR, taking `ms`.
pub fn read_sample(area: &str, ms: f64) {
    if !on() {
        return;
    }
    with_reads(|r| {
        r.n += 1;
        r.total += ms;
        r.max = r.max.max(ms);
        let e = r.areas.entry(area.to_string()).or_default();
        e.0 += 1;
        e.1 += ms;
    });
}

thread_local! {
    /// the reading pass's part going on now, and since when
    static STAGE: std::cell::RefCell<Option<(&'static str, Instant)>> = const { std::cell::RefCell::new(None) };
}

/// A reading pass starts (its first part: what comes before the pages).
pub fn pass_start() {
    if on() {
        let now = Instant::now();
        STAGE.with(|s| *s.borrow_mut() = Some(("setup", now)));
        PASS.with(|p| {
            let (_, _, end, frame) = p.get();
            p.set((unix_ms(), Some(now), end, frame));
        });
    }
}

/// The pass that began at `pass_start` read frame `seq`: how old the frame was then, how long since the
/// pass before ended, and whether it's the frame that pass read (no new one came).
pub(crate) fn note_pass_frame(seq: Option<u64>) {
    let (start_ms, start, end, last) = PASS.with(|p| p.get());
    let age = seq.and_then(captured_of).filter(|_| start_ms > 0).map(|c| start_ms.saturating_sub(c) as f64);
    let idle = start.zip(end).map(|(s, e)| s.saturating_duration_since(e).as_secs_f64() * 1000.0);
    let same = seq.is_some() && seq == last;
    PASS.with(|p| p.set((start_ms, start, end, seq.or(last))));
    if let Some(r) = READS.lock().unwrap().as_mut() {
        if let Some(a) = age {
            r.age.0 += 1;
            r.age.1 += a;
        }
        if let Some(i) = idle {
            r.idle.0 += 1;
            r.idle.1 += i;
        }
        r.same_frame += u32::from(same);
    }
}

/// An area's reading is done (its time since the pass began).
pub(crate) fn note_area_done(area: &str) {
    let Some(start) = PASS.with(|p| p.get().1) else { return };
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    if let Some(r) = READS.lock().unwrap().as_mut() {
        let e = r.done.entry(area.to_string()).or_default();
        e.0 += 1;
        e.1 += ms;
    }
}

/// The unread-text finder's verdicts this pass (both, pixels only, reading only, neither), and its time.
pub(crate) fn note_presence(counts: [u32; 4], took: Duration) {
    let n: u32 = counts.iter().sum();
    if let Some(r) = READS.lock().unwrap().as_mut() {
        for (a, b) in r.det.iter_mut().zip(counts) {
            *a += b;
        }
        r.detect.0 += n;
        r.detect.1 += took;
    }
}

/// The pass goes on to its part `name` (the part before it ends now).
pub fn stage(name: &'static str) {
    if !on() {
        return;
    }
    let now = Instant::now();
    if let Some((prev, since)) = STAGE.with(|s| s.borrow_mut().replace((name, now))) {
        let ms = (now - since).as_secs_f64() * 1000.0;
        if let Some(r) = READS.lock().unwrap().as_mut() {
            *r.stages.entry(prev).or_default() += ms;
        }
    }
}

/// A pass of the reading (every area, the pages, the phone) took `ms`.
pub fn pass_sample(ms: f64) {
    if !on() {
        return;
    }
    PASS.with(|p| {
        let (s, st, _, f) = p.get();
        p.set((s, st, Some(Instant::now()), f));
    });
    // (its last part ends now)
    let last = STAGE.with(|s| s.borrow_mut().take()).map(|(name, since)| (name, since.elapsed().as_secs_f64() * 1000.0));
    with_reads(|r| {
        if let Some((name, ms)) = last {
            *r.stages.entry(name).or_default() += ms;
        }
        r.passes += 1;
        r.pass_total += ms;
        r.pass_max = r.pass_max.max(ms);
    });
}

fn note_suppressed() {
    if let Some(r) = READS.lock().unwrap().as_mut() {
        r.suppressed += 1;
    }
}

// ------------------------------------------------------------------------------------- filtered

struct Filters {
    /// the last text written for each (area, reason), and when
    last: HashMap<(String, String), (String, Instant)>,
    /// this second's `filtered` events (at most CAP)
    window: (Option<Instant>, u32),
}

static FILTERS: Mutex<Option<Filters>> = Mutex::new(None);
/// The most `filtered` events written a second (more are counted in `reads.suppressed`).
const CAP: u32 = 20;

/// Text read in `area` was left out for `reason` (one of REASONS). The same text again within 5 s is
/// not written again; another text for the same area and reason within 300 ms is only counted.
pub fn filtered(area: &str, reason: &str, text: &str) {
    if !on() {
        return;
    }
    super::timeline::note_reason(area, reason);
    let key = norm(text);
    if key.is_empty() {
        return;
    }
    let now = Instant::now();
    {
        let mut guard = FILTERS.lock().unwrap();
        let st = guard.get_or_insert_with(|| Filters { last: HashMap::new(), window: (None, 0) });
        let k = (area.to_string(), reason.to_string());
        if let Some((last, at)) = st.last.get(&k) {
            if *last == key && now.duration_since(*at) < Duration::from_secs(5) {
                return;
            }
            if now.duration_since(*at) < Duration::from_millis(300) {
                drop(guard);
                note_suppressed();
                return;
            }
        }
        let start = *st.window.0.get_or_insert(now);
        if now.duration_since(start) >= Duration::from_secs(1) {
            st.window = (Some(now), 0);
        }
        if st.window.1 >= CAP {
            drop(guard);
            note_suppressed();
            return;
        }
        st.window.1 += 1;
        st.last.insert(k, (key, now));
    }
    emit("filtered", json!({ "area": area, "reason": reason, "text": excerpt(text) }));
}

/// The kind of a translation that wasn't shown, from its error (see translate.rs: an answer in English,
/// a made-up sentence, anything else).
pub fn error_kind(message: &str) -> &'static str {
    if message.contains("came back in English") {
        "english_leak"
    } else if message.contains("far longer than the line") {
        "made_up"
    } else {
        "error"
    }
}

/// Tests that turn the log on run one at a time (the log is the whole process's).
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn collect<R>(f: impl FnOnce() -> R) -> (R, Vec<Value>) {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let c = Arc::new(Mutex::new(Vec::new()));
        set_collector(Some(c.clone()));
        let r = f();
        set_collector(None);
        let v = c.lock().unwrap().clone();
        (r, v)
    }

    #[test]
    fn fork_linelog_is_silent_while_off() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!on());
        emit("seen", json!({ "area": "subtitles" }));
        note_frame(7, 1);
        assert_eq!(captured_of(7), None, "nothing kept while off");
    }

    #[test]
    fn fork_linelog_tells_the_settings_once_and_again_when_they_change() {
        let base = Settings { capture_fps: 12, preview_fps: 60, picture_4k: true, interval_ms: 16, stable_ms: 0, clear_after_ms: 70, custom: false, clear_subtitles_ms: 250, clear_other_ms: 350, clear_floor_ms: 250, source: "xbox" };
        let (_, v) = collect(|| {
            note_settings(base);
            note_settings(base);
            note_settings(Settings { interval_ms: 100, custom: true, ..base });
        });
        let s: Vec<&Value> = v.iter().filter(|e| e["ev"] == "settings").collect();
        assert_eq!(s.len(), 2, "{v:?}");
        assert_eq!((s[0]["picture_4k"].as_bool(), s[0]["clear_after_ms"].as_u64()), (Some(true), Some(70)));
        assert_eq!((s[1]["interval_ms"].as_u64(), s[1]["timing_custom"].as_bool()), (Some(100), Some(true)));
        // (1.5.0) as the reader applies Clear after, and the source's kind
        assert_eq!((s[0]["clear_subtitles_ms"].as_u64(), s[0]["clear_other_ms"].as_u64(), s[0]["source"].as_str()), (Some(250), Some(350), Some("xbox")));
        // a log started again says them again
        let (_, v) = collect(|| note_settings(base));
        assert_eq!(v.iter().filter(|e| e["ev"] == "settings").count(), 1);
    }

    #[test]
    fn fork_linelog_names_a_source_by_its_kind_only() {
        assert_eq!(source_kind("xbox:"), "xbox");
        assert_eq!(source_kind("ps5:"), "ps5");
        assert_eq!(source_kind("obs:Game capture"), "obs");
        assert_eq!(source_kind("device:USB Video"), "device");
        assert_eq!(source_kind("screen:1"), "screen");
        // (a window: its title, whatever it says, stays out)
        assert_eq!(source_kind("Xbox"), "window");
        assert_eq!(source_kind("My: private window"), "window");
        assert_eq!(source_kind(""), "none");
    }

    #[test]
    fn fork_linelog_tells_the_sources_own_thread() {
        let (_, v) = collect(|| {
            read_sample("subtitles", 30.0);
            note_source_bad_frames(3);
            // (the source's thread: busy a while between two of its frames)
            note_source_cpu();
            let t = Instant::now();
            let mut x = 0u64;
            while t.elapsed() < Duration::from_millis(300) {
                x = x.wrapping_mul(31).wrapping_add(7);
            }
            std::hint::black_box(x);
            note_source_cpu();
            note_source_bad_frames(5);
            std::thread::sleep(Duration::from_millis(750));
            read_sample("subtitles", 30.0);
        });
        let r = v.iter().find(|e| e["ev"] == "reads").expect("a second's readings");
        assert!(r["source_cpu_ms"].as_f64().is_some_and(|ms| ms > 100.0), "{r}");
        assert_eq!(r["bad_frames"].as_u64(), Some(2), "{r}");
    }

    #[test]
    fn fork_linelog_counts_preview_frames_and_keeps_the_frame_size() {
        let (_, v) = collect(|| {
            // (the second's readings start with the first reading)
            read_sample("subtitles", 30.0);
            note_captured(1, 2560, 1600);
            for _ in 0..5 {
                note_preview();
                note_preview_cost(Duration::from_millis(4));
            }
            std::thread::sleep(Duration::from_millis(1050));
            read_sample("subtitles", 30.0);
        });
        let r = v.iter().find(|e| e["ev"] == "reads").expect("a second's readings");
        assert_eq!((r["frame_w"].as_u64(), r["frame_h"].as_u64()), (Some(2560), Some(1600)));
        assert!(r["preview_fps"].as_f64().is_some_and(|p| p > 0.0 && p <= 5.0), "{r}");
        // (a preview frame's cost: 4 ms each)
        assert_eq!(r["preview_ms"].as_f64(), Some(4.0), "{r}");
        assert!(r["fps"].as_f64().is_some_and(|p| p > 0.0), "{r}");
    }

    #[test]
    fn fork_linelog_tells_where_a_pass_spends_its_time() {
        let (_, v) = collect(|| {
            // (the second's readings start with the first reading)
            read_sample("subtitles", 1.0);
            for _ in 0..2 {
                pass_start();
                stage("pages");
                std::thread::sleep(Duration::from_millis(40));
                stage("areas");
                std::thread::sleep(Duration::from_millis(15));
                pass_sample(55.0);
            }
            std::thread::sleep(Duration::from_millis(1000));
            pass_start();
            pass_sample(0.0);
        });
        let r = v.iter().find(|e| e["ev"] == "reads").expect("a second's readings");
        let (pages, areas) = (r["stages"]["pages"].as_f64().unwrap_or(0.0), r["stages"]["areas"].as_f64().unwrap_or(0.0));
        // (a pass's average: two of 40 and 15 ms, and one of nothing)
        assert!(pages >= 25.0 && areas >= 9.0 && pages > areas, "{r}");
    }

    #[test]
    fn fork_linelog_events_carry_time_and_kind() {
        let (_, v) = collect(|| {
            emit("seen", json!({ "area": "subtitles", "text": "Go to the boat." }));
            note_frame(42, 1_700_000_000_000);
            assert_eq!(captured_of(42), Some(1_700_000_000_000));
        });
        let e = v.iter().find(|e| e["ev"] == "seen").expect("the event");
        assert_eq!(e["area"], "subtitles");
        assert!(e["t"].as_u64().unwrap() > 1_600_000_000_000);
    }

    #[test]
    fn fork_linelog_writes_the_same_filtered_text_once() {
        let (_, v) = collect(|| {
            for _ in 0..5 {
                filtered("fork-test-area", "clean", "No.");
            }
            filtered("fork-test-area", "clean", "");
        });
        let n = v.iter().filter(|e| e["ev"] == "filtered" && e["area"] == "fork-test-area").count();
        assert_eq!(n, 1);
    }

    #[test]
    fn fork_linelog_session_goes_on_in_parts_and_loses_nothing() {
        let dir = std::env::temp_dir().join(format!("vit-linelog-{}", uuid::Uuid::new_v4().simple()));
        let stem = session_stem(&dir, 1_795_729_676_497);
        assert_eq!(stem, "lines-2026-11-26_21-47-56Z", "the session's UTC time in its name");
        let (tx, rx) = mpsc::channel::<String>();
        for i in 0..20 {
            tx.send(format!("{{\"n\":{i},\"pad\":\"{}\"}}", "x".repeat(40))).unwrap();
        }
        drop(tx);
        write_loop(&dir, &stem, 300, rx);
        let files = session_files(&dir.join(format!("{stem}-part2.log")));
        assert!(files.len() >= 3, "{files:?}");
        assert_eq!(files[0], dir.join(format!("{stem}.log")), "any part finds the whole session, in order");
        let all: Vec<Value> = files.iter().flat_map(|f| std::fs::read_to_string(f).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect::<Vec<Value>>()).collect();
        let ns: Vec<u64> = all.iter().filter_map(|e| e["n"].as_u64()).collect();
        assert_eq!(ns, (0..20).collect::<Vec<u64>>(), "every line kept, in order");
        assert!(all.iter().any(|e| e["ev"] == "log" && e["state"] == "part" && e["part"] == 2), "a part says it is one");
        for f in &files {
            assert!(std::fs::metadata(f).unwrap().len() <= 300 + 100, "{f:?} within the limit (and its header)");
        }
        // a second session the same second: a name of its own
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(session_stem(&dir, 1_795_729_676_497), format!("{stem}-2"));
        // the older single file: its older copy first
        let old = dir.join(FILE);
        std::fs::write(&old, "{}\n").unwrap();
        std::fs::write(rotated(&old), "{}\n").unwrap();
        assert_eq!(session_files(&old), vec![rotated(&old), old.clone()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fork_linelog_utc_stamps() {
        assert_eq!(utc_stamp(0), "1970-01-01_00-00-00Z");
        assert_eq!(utc_stamp(951_782_400_000), "2000-02-29_00-00-00Z");
        assert_eq!(utc_stamp(1_795_773_617_119), "2026-11-27_10-00-17Z");
    }

    #[test]
    fn fork_linelog_tells_why_a_translation_was_refused() {
        assert_eq!(error_kind("the translation came back in English, not shown: x"), "english_leak");
        assert_eq!(error_kind("the translation is far longer than the line, not shown: x"), "made_up");
        assert_eq!(error_kind("HTTP 500"), "error");
    }
}

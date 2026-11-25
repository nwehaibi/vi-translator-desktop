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
//! The file rotates at `line_log_mb` MB (it and `lines.log.1` are kept). Readings are summed up once a
//! second, not written one by one; repeated `filtered` events for the same text are written once.

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Why text read in an area was left out (`filtered` events), as the reading decides it.
pub const REASONS: [&str; 7] = ["clean", "not_centered", "no_bottom_line", "help_anchor", "help_not_sentence", "not_words", "reviewed_only"];

/// The log's file name in the data folder.
pub const FILE: &str = "lines.log";

static ON: AtomicBool = AtomicBool::new(false);

struct Sinks {
    /// the file written to (by its own thread), its size limit and the way to it
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
}

/// Write the log to `path` (None: stop writing it), rotating at `max_mb` MB.
pub fn configure_file(path: Option<PathBuf>, max_mb: u32) {
    let max = u64::from(max_mb.clamp(1, 64)) * 1024 * 1024;
    {
        let mut s = SINKS.lock().unwrap();
        if let (Some((p, m, _)), Some(np)) = (&s.file, &path) {
            if p == np && *m == max {
                return;
            }
        }
        // (the old writer ends once it has written what it was sent)
        s.file = path.map(|p| {
            let (tx, rx) = mpsc::channel::<String>();
            let pc = p.clone();
            let _ = std::thread::Builder::new().name("lines.log".into()).spawn(move || write_loop(&pc, max, rx));
            (p, max, tx)
        });
        refresh(&s);
    }
    if on() {
        emit("log", json!({ "app": crate::VERSION, "state": "on" }));
    }
}

/// Keep the events in `collector` too (None: stop): the replay tool and tests read them there.
pub fn set_collector(collector: Option<Arc<Mutex<Vec<Value>>>>) {
    let mut s = SINKS.lock().unwrap();
    s.collector = collector;
    refresh(&s);
}

/// The older copy kept when the log rotates.
pub fn rotated(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".1");
    PathBuf::from(s)
}

fn write_loop(path: &Path, max: u64, rx: mpsc::Receiver<String>) {
    let open = |p: &Path| std::fs::OpenOptions::new().create(true).append(true).open(p);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(mut f) = open(path) else { return };
    let mut size = f.metadata().map(|m| m.len()).unwrap_or(0);
    for line in rx {
        let n = line.len() as u64 + 1;
        if size > 0 && size + n > max {
            drop(f);
            let old = rotated(path);
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(path, &old);
            let Ok(nf) = open(path) else { return };
            f = nf;
            size = 0;
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

/// When each recent frame was captured (its number, Unix ms): a line's first sighting is measured from
/// when its frame was taken, not from when it was read.
static FRAMES: Mutex<VecDeque<(u64, u64)>> = Mutex::new(VecDeque::new());

/// A frame was captured now (the capture calls it, with the frame's number).
pub fn note_frame(seq: u64, captured_ms: u64) {
    if !on() {
        return;
    }
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

#[derive(Default)]
struct Reads {
    since: Option<Instant>,
    n: u32,
    total: f64,
    max: f64,
    passes: u32,
    pass_total: f64,
    pass_max: f64,
    areas: HashMap<String, (u32, f64)>,
    suppressed: u32,
}

static READS: Mutex<Option<Reads>> = Mutex::new(None);

fn with_reads(f: impl FnOnce(&mut Reads)) {
    let event = {
        let mut guard = READS.lock().unwrap();
        let r = guard.get_or_insert_with(Reads::default);
        let now = Instant::now();
        let since = *r.since.get_or_insert(now);
        f(r);
        if now.duration_since(since) < Duration::from_secs(1) {
            return;
        }
        let areas: serde_json::Map<String, Value> = r
            .areas
            .iter()
            .map(|(a, (n, t))| (a.clone(), json!({ "n": n, "avg": round1(t / f64::from((*n).max(1))) })))
            .collect();
        let ev = json!({
            "n": r.n, "ocr_avg": round1(r.total / f64::from(r.n.max(1))), "ocr_max": round1(r.max),
            "pass_n": r.passes, "pass_avg": round1(r.pass_total / f64::from(r.passes.max(1))), "pass_max": round1(r.pass_max),
            "areas": areas, "suppressed": r.suppressed,
        });
        *r = Reads { since: Some(now), ..Reads::default() };
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

/// A pass of the reading (every area, the pages, the phone) took `ms`.
pub fn pass_sample(ms: f64) {
    if !on() {
        return;
    }
    with_reads(|r| {
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
    fn fork_linelog_file_rotates_and_keeps_one_older_copy() {
        let dir = std::env::temp_dir().join(format!("vit-linelog-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        let (tx, rx) = mpsc::channel::<String>();
        for i in 0..20 {
            tx.send(format!("{{\"n\":{i},\"pad\":\"{}\"}}", "x".repeat(40))).unwrap();
        }
        drop(tx);
        write_loop(&path, 300, rx);
        let now = std::fs::read_to_string(&path).unwrap();
        let old = std::fs::read_to_string(rotated(&path)).unwrap();
        assert!(now.len() <= 300 && old.len() <= 300, "each file within the limit");
        assert!(now.contains("\"n\":19"), "the newest line is in the file");
        assert!(!now.contains("\"n\":0,") && !old.contains("\"n\":0,"), "the oldest went with the rotation");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fork_linelog_tells_why_a_translation_was_refused() {
        assert_eq!(error_kind("the translation came back in English, not shown: x"), "english_leak");
        assert_eq!(error_kind("the translation is far longer than the line, not shown: x"), "made_up");
        assert_eq!(error_kind("HTTP 500"), "error");
    }
}

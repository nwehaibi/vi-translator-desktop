//! fork: the measurement log's analyzer (see FORK.md): what a session played with the app says about how
//! well the Arabic kept with the English, and what to change in the five timing settings (Capture FPS,
//! Preview FPS, Reading interval, Text stability, Clear after) for the best of it on this PC.
//!
//! From the log alone (no video): each line's way (read, sent, translated, shown, drawn on the TV, ended),
//! its English appearing and going (between two frames' capture times), the Arabic going up and down on the
//! TV (the TV's own report), lines lost and why, misreadings, flicker and its cause; then the settings'
//! effects worked out from what was measured — a model of each setting as the app's code applies it — with
//! how sure that is, and "not enough data" when it can't be told.
//!
//! The session is cut where the settings changed (the log says them), so a session played with two values
//! is compared part by part. It reads the log; it changes nothing.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::linelog;

/// What the advice weighs (each a line's worth in ms of delay: what one flicker or one lost line counts
/// against the ms an Arabic line comes late or stays on).
#[derive(Clone, Debug)]
pub struct Options {
    /// minutes to add to UTC for the times shown (the player's clock)
    pub tz_minutes: i32,
    /// a flicker (the Arabic going and coming back while its English stays) = this many ms of delay
    pub flicker_ms: f64,
    /// a line lost (never shown) = this many ms of delay
    pub lost_ms: f64,
    /// a premature line (a misreading shown, then replaced) = this many ms of delay
    pub premature_ms: f64,
    /// how much more of the whole machine (percentage points) a faster capture may take
    pub cpu_points: f64,
}

impl Default for Options {
    fn default() -> Self {
        // (a flicker breaks the reading of a line: worth far more than its Arabic staying a little longer; a
        // line lost is the worst: never read at all)
        Self { tz_minutes: 0, flicker_ms: 2000.0, lost_ms: 5000.0, premature_ms: 1000.0, cpu_points: 5.0 }
    }
}

// ------------------------------------------------------------------------------------- reading the log

/// Every event of the sessions in `paths` (a session's file or any of its parts, the older `lines.log`, or a
/// folder of them), in time order.
pub fn load(paths: &[PathBuf]) -> anyhow::Result<Vec<Value>> {
    let mut files: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut names: Vec<PathBuf> = std::fs::read_dir(p)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| f.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("lines") && n.ends_with(".log")))
                .collect();
            names.sort();
            files.extend(names);
        } else {
            for f in linelog::session_files(p) {
                if !files.contains(&f) {
                    files.push(f);
                }
            }
        }
    }
    let mut events: Vec<Value> = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f)?;
        events.extend(text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()));
    }
    anyhow::ensure!(!events.is_empty(), "no events in {files:?} (was the measurement log on?)");
    events.sort_by_key(t);
    Ok(events)
}

fn t(e: &Value) -> u64 {
    e["t"].as_u64().unwrap_or(0)
}
fn f(e: &Value, k: &str) -> Option<f64> {
    e[k].as_f64()
}
fn u(e: &Value, k: &str) -> Option<u64> {
    e[k].as_u64()
}
fn s<'a>(e: &'a Value, k: &str) -> &'a str {
    e[k].as_str().unwrap_or("")
}
fn kind<'a>(e: &'a Value) -> &'a str {
    s(e, "ev")
}

fn quant(v: &[f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let k = (v.len() - 1) as f64 * p;
    let (i, j) = (k.floor() as usize, k.ceil() as usize);
    Some(r1(v[i] + (v[j] - v[i]) * (k - k.floor())))
}
fn r1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}
fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| r1(v.iter().sum::<f64>() / v.len() as f64))
}
fn spread(v: &[f64]) -> Value {
    json!({ "n": v.len(), "p50": quant(v, 0.5), "p90": quant(v, 0.9), "mean": mean(v) })
}

/// A text with only its letters and digits, lower case.
fn norm(text: &str) -> String {
    linelog::norm(text)
}

/// How alike two texts are, 0..1 (edit distance over the longer one's length, letters and digits only).
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (norm(a).chars().collect(), norm(b).chars().collect());
    if a.is_empty() || b.is_empty() {
        return if a.is_empty() && b.is_empty() { 1.0 } else { 0.0 };
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[b.len()] as f64 / a.len().max(b.len()) as f64
}

/// One reading the same line as the other, or a part of it (a line read before its speaker's name, a block
/// one line of which came first)?
fn related(a: &str, b: &str) -> bool {
    let (na, nb) = (norm(a), norm(b));
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    similarity(a, b) >= 0.6 || (na.len() >= 4 && nb.contains(&na)) || (nb.len() >= 4 && na.contains(&nb))
}

/// The speaker's name read wrong at the start ("ason:", ".Jason:", "ILocals:"): its first letter lost, or a
/// mark before it.
fn name_misread(en: &str) -> bool {
    let Some((head, _)) = en.split_once(':') else { return false };
    let head = head.trim();
    if head.is_empty() || head.contains(' ') || head.chars().count() > 14 {
        return false;
    }
    let first = head.chars().next().unwrap_or(' ');
    let letters: String = head.chars().filter(|c| c.is_alphabetic()).collect();
    // (a mark before the name, a name starting with a small letter: its capital lost)
    !first.is_alphabetic() || first.is_lowercase() || letters.chars().count() != head.chars().count() || {
        // (two capitals at the start: a stroke read as I or O before the name)
        let mut c = head.chars();
        matches!((c.next(), c.next()), (Some(a), Some(b)) if (a == 'I' || a == 'O' || a == 'l') && b.is_uppercase())
    }
}

// ------------------------------------------------------------------------------------- lines

/// A line: one sent to the translator, with everything that happened to it.
#[derive(Clone, Debug, Default)]
struct Line {
    area: String,
    seq: u64,
    en: String,
    at: u64,
    sent_t: u64,
    /// the English appeared between these two frames' capture times
    appear: Option<(f64, f64)>,
    /// its first reading's time (the read), and the readings of it before it was sent
    first_read_t: Option<u64>,
    first_captured: Option<u64>,
    variants: usize,
    hit: Option<bool>,
    sources: Vec<String>,
    tr_ms: Option<f64>,
    tr_ok: Option<bool>,
    tr_t: Option<u64>,
    shown_t: Option<u64>,
    late: bool,
    ar: String,
    /// when its Arabic came down here, and how (hidden, replaced by the next line, cleared)
    down_t: Option<u64>,
    down_how: &'static str,
    /// the English went (v2): how, and between which frames
    ended_how: Option<String>,
    gone: Option<(f64, f64)>,
    absent_text_px: u64,
    gaps: Vec<(f64, u64, u64)>,
    tv_on: Option<f64>,
    tv_off: Option<f64>,
    skipped: bool,
    replaced: bool,
    dropped: bool,
}

impl Line {
    fn en_on(&self) -> Option<f64> {
        self.appear.map(|(a, b)| (a + b) / 2.0)
    }
    fn en_off(&self) -> Option<f64> {
        self.gone.map(|(a, b)| (a + b) / 2.0)
    }
}

/// Every line of `ev` (one segment), in the order they were sent.
fn lines_of(ev: &[Value], frame_ms: f64) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut by_seq: HashMap<u64, usize> = HashMap::new();
    for e in ev.iter().filter(|e| kind(e) == "sent") {
        let Some(seq) = u(e, "seq") else { continue };
        by_seq.insert(seq, lines.len());
        lines.push(Line { area: s(e, "area").into(), seq, en: s(e, "en").into(), at: u(e, "at").unwrap_or(0), sent_t: t(e), down_how: "", ..Line::default() });
    }
    // the readings of each area, in order (for each line, its first sighting)
    let mut seen: HashMap<&str, Vec<&Value>> = HashMap::new();
    for e in ev.iter().filter(|e| kind(e) == "seen") {
        seen.entry(s(e, "area")).or_default().push(e);
    }
    let sent_at: HashMap<&str, Vec<u64>> = {
        let mut m: HashMap<&str, Vec<u64>> = HashMap::new();
        for e in ev.iter().filter(|e| kind(e) == "sent") {
            m.entry(s(e, "area")).or_default().push(t(e));
        }
        m
    };
    for l in lines.iter_mut() {
        let Some(rs) = seen.get(l.area.as_str()) else { continue };
        // the reading that was sent: the last one up to the send, alike
        let Some(mut i) = rs.iter().rposition(|r| t(r) <= l.sent_t + 5 && related(s(r, "text"), &l.en)) else { continue };
        let others: &[u64] = sent_at.get(l.area.as_str()).map(|v| v.as_slice()).unwrap_or(&[]);
        let mut chain = 1;
        // back over the readings of the same line (a speaker's name read a moment later, a misreading),
        // until the area had no text before it, or another line was sent in between
        while i > 0 {
            let (r, p) = (rs[i], rs[i - 1]);
            if r["prev_text"] == false || t(r) - t(p) > 1500 || !related(s(p, "text"), &l.en) {
                break;
            }
            if others.iter().any(|&x| x > t(p) && x < t(r) && x != l.sent_t) {
                break;
            }
            i -= 1;
            chain += 1;
        }
        let first = rs[i];
        l.variants = chain;
        l.first_read_t = Some(t(first));
        l.first_captured = u(first, "captured");
        // (v2: the frame before; v1: a frame's time before it)
        l.appear = match (f(first, "prev_captured"), f(first, "captured")) {
            (Some(a), Some(b)) if b >= a => Some((a, b)),
            (None, Some(b)) => Some((b - frame_ms, b)),
            _ => None,
        };
    }
    for e in ev {
        let Some(&i) = u(e, "seq").and_then(|q| by_seq.get(&q)) else { continue };
        let l = &mut lines[i];
        match kind(e) {
            "translated" => {
                l.hit = e["hit"].as_bool();
                l.sources = e["sources"].as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("?").to_string()).collect()).unwrap_or_default();
                l.tr_ms = f(e, "ms");
                l.tr_ok = e["ok"].as_bool();
                l.tr_t = Some(t(e));
            }
            "shown" => {
                l.shown_t = Some(t(e));
                l.late = e["late"] == true;
                l.ar = s(e, "ar").to_string();
            }
            "hidden" if l.down_t.is_none() => {
                l.down_t = Some(t(e));
                l.down_how = "hidden";
            }
            "skipped" => l.skipped = true,
            "replaced" => l.replaced = true,
            "dropped" => l.dropped = true,
            "ended" => {
                l.ended_how = Some(s(e, "how").to_string());
                if let (Some(a), Some(b)) = (f(e, "last_present"), f(e, "first_absent")) {
                    l.gone = Some((a, b.max(a)));
                }
                l.absent_text_px = u(e, "absent_text_px").unwrap_or(0);
            }
            "gap" => {
                if let (Some(a), Some(b)) = (f(e, "last_present"), f(e, "last_absent")) {
                    l.gaps.push((b - a, u(e, "frames").unwrap_or(0), u(e, "text_px").unwrap_or(0)));
                }
            }
            _ => {}
        }
    }
    // the Arabic replaced by the next line of its area (no `hidden` for it), or everything cleared
    let clears: Vec<u64> = ev.iter().filter(|e| kind(e) == "cleared").map(t).collect();
    let mut shown_by_area: HashMap<String, Vec<(u64, usize)>> = HashMap::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(st) = l.shown_t {
            shown_by_area.entry(l.area.clone()).or_default().push((st, i));
        }
    }
    for v in shown_by_area.values_mut() {
        v.sort();
        for w in 0..v.len() {
            let (st, i) = v[w];
            let next = v.get(w + 1).map(|x| x.0);
            let clear = clears.iter().copied().find(|&c| c >= st);
            let hidden = lines[i].down_t;
            let (end, how) = [(hidden, "hidden"), (next, "replaced"), (clear, "cleared")]
                .into_iter()
                .filter_map(|(x, h)| x.map(|x| (x, h)))
                .min_by_key(|x| x.0)
                .unwrap_or((0, ""));
            if end > 0 {
                lines[i].down_t = Some(end);
                lines[i].down_how = how;
            }
        }
    }
    // the TV: the first state sent with the line (its `at`) and the first without it after it came down
    let mut drawn: HashMap<(String, String), f64> = HashMap::new();
    for e in ev.iter().filter(|e| kind(e) == "tv_drawn") {
        if let Some(est) = f(e, "est") {
            drawn.entry((s(e, "tv").to_string(), s(e, "version").to_string())).or_insert(est);
        }
    }
    let tvs: Vec<&Value> = ev.iter().filter(|e| kind(e) == "tv_sent").collect();
    let tv_ts: Vec<u64> = tvs.iter().map(|e| t(e)).collect();
    for l in lines.iter_mut() {
        let Some(st) = l.shown_t else { continue };
        let from = tv_ts.partition_point(|&x| x + 5 < st);
        for e in tvs[from..].iter().take(200) {
            if e["shown"][&l.area].as_u64() == Some(l.at) {
                l.tv_on = drawn.get(&(s(e, "tv").to_string(), s(e, "version").to_string())).copied();
                break;
            }
        }
        if let Some(dt) = l.down_t {
            let from = tv_ts.partition_point(|&x| x + 5 < dt);
            for e in tvs[from..].iter().take(200) {
                if e["shown"][&l.area].as_u64() != Some(l.at) {
                    l.tv_off = drawn.get(&(s(e, "tv").to_string(), s(e, "version").to_string())).copied();
                    break;
                }
            }
        }
    }
    lines
}

// ------------------------------------------------------------------------------------- a segment

/// The per-second readings of a segment, gathered.
#[derive(Default)]
struct Seconds {
    n: usize,
    preview_on: usize,
    cpu_app: Vec<f64>,
    cpu_all: Vec<f64>,
    fps: Vec<f64>,
    stream_fps: Vec<f64>,
    passes: Vec<f64>,
    pass: Vec<f64>,
    copy_ms: Vec<f64>,
    preview_ms: Vec<f64>,
    age_ms: Vec<f64>,
    idle_ms: Vec<f64>,
    same_frame: f64,
    reader_cpu: Vec<f64>,
    log_ms: Vec<f64>,
    detect_ms: Vec<f64>,
    done: HashMap<String, Vec<f64>>,
    stages: BTreeMap<String, Vec<f64>>,
    det: [u64; 4],
    frame: BTreeMap<String, usize>,
}

fn seconds_of(ev: &[Value]) -> Seconds {
    let mut x = Seconds::default();
    for e in ev.iter().filter(|e| kind(e) == "reads") {
        x.n += 1;
        let preview = f(e, "preview_fps").is_some_and(|p| p > 0.0);
        if preview {
            x.preview_on += 1;
            x.preview_ms.extend(f(e, "preview_ms"));
        } else {
            // (the CPU without the preview's share: the play as it's meant to be)
            x.cpu_app.extend(f(e, "cpu_app"));
            x.cpu_all.extend(f(e, "cpu_all"));
        }
        x.fps.extend(f(e, "fps"));
        x.stream_fps.extend(f(e, "stream_fps").filter(|v| *v > 0.0));
        x.passes.extend(f(e, "pass_n"));
        x.pass.extend(f(e, "pass_avg").filter(|_| u(e, "pass_n").unwrap_or(0) > 0));
        x.copy_ms.extend(f(e, "copy_ms"));
        x.age_ms.extend(f(e, "age_ms"));
        x.idle_ms.extend(f(e, "idle_ms"));
        x.same_frame += f(e, "same_frame").unwrap_or(0.0);
        x.reader_cpu.extend(f(e, "reader_cpu_ms"));
        x.log_ms.extend(f(e, "log_ms"));
        x.detect_ms.extend(f(e, "detect_ms"));
        for (a, v) in e["done_ms"].as_object().into_iter().flatten() {
            x.done.entry(a.clone()).or_default().extend(v.as_f64());
        }
        for (a, v) in e["stages"].as_object().into_iter().flatten() {
            x.stages.entry(a.clone()).or_default().extend(v.as_f64());
        }
        for (i, k) in ["tt", "tn", "nt", "nn"].iter().enumerate() {
            x.det[i] += e["det"][k].as_u64().unwrap_or(0);
        }
        if let (Some(w), Some(h)) = (u(e, "frame_w"), u(e, "frame_h")) {
            *x.frame.entry(format!("{w}x{h}")).or_default() += 1;
        }
    }
    x
}

/// What a segment's settings were.
#[derive(Clone, Copy, Debug, Default)]
struct Timing {
    capture_fps: f64,
    preview_fps: f64,
    interval_ms: f64,
    stable_ms: f64,
    clear_after_ms: f64,
}

fn timing_of(e: Option<&Value>) -> Timing {
    let g = |k: &str, d: f64| e.and_then(|e| f(e, k)).unwrap_or(d);
    Timing { capture_fps: g("capture_fps", 12.0), preview_fps: g("preview_fps", 60.0), interval_ms: g("interval_ms", 16.0), stable_ms: g("stable_ms", 0.0), clear_after_ms: g("clear_after_ms", 70.0) }
}

/// A recommendation: the value, why, what it should do, how sure, and the test that confirms it.
fn advice(setting: &str, now: f64, value: Option<f64>, confidence: &str, why: String, effect: String, evidence: Value, test: String) -> Value {
    json!({ "setting": setting, "now": now, "value": value, "confidence": confidence, "why": why, "effect": effect, "evidence": evidence, "test": test })
}

/// The capture's real frame interval for a Capture FPS setting: the first frame the source sends once the
/// setting's interval has passed (capture.rs `full_due`), the source sending one every `src_ms`.
fn capture_interval(setting: f64, src_ms: f64) -> f64 {
    let want = 1000.0 / setting.max(1.0);
    let k = (want / src_ms).ceil();
    // (right at a multiple, the source's own jitter makes it one frame more about half the time)
    let frac = want / src_ms - (want / src_ms).floor();
    if !(0.03..=0.97).contains(&frac) { (k + 0.5) * src_ms } else { k * src_ms }
}

fn analyze_segment(ev: &[Value], timing: Timing, cpus: Option<f64>, o: &Options) -> Value {
    let sec = seconds_of(ev);
    let minutes = ev.first().zip(ev.last()).map(|(a, b)| (t(b).saturating_sub(t(a))) as f64 / 60_000.0).unwrap_or(0.0);
    let fps = quant(&sec.fps, 0.5).unwrap_or(0.0);
    let frame_ms = if fps > 0.0 { 1000.0 / fps } else { 1000.0 / timing.capture_fps.max(1.0) };
    let lines = lines_of(ev, frame_ms);
    let subs: Vec<&Line> = lines.iter().filter(|l| l.area == "subtitles").collect();
    let shown: Vec<&&Line> = subs.iter().filter(|l| l.shown_t.is_some()).collect();

    // ---- sync: the Arabic against the English (the English as the PC's stream shows it)
    let start_pc: Vec<f64> = shown.iter().filter_map(|l| Some(l.shown_t? as f64 - l.en_on()?)).collect();
    let start_tv: Vec<f64> = shown.iter().filter_map(|l| Some(l.tv_on? - l.en_on()?)).collect();
    let bracket: Vec<f64> = shown.iter().filter_map(|l| l.appear.map(|(a, b)| b - a)).collect();
    // where the start's time goes: the frame's wait, the reading, the settling, the translation, the
    // presenter, the TV
    let parts = |g: &dyn Fn(&Line) -> Option<f64>| spread(&shown.iter().filter_map(|l| g(l)).filter(|v| *v >= 0.0).collect::<Vec<f64>>());
    let breakdown = json!({
        "frame_wait": parts(&|l| Some(l.first_captured? as f64 - l.en_on()?)),
        "reading": parts(&|l| Some(l.first_read_t? as f64 - l.first_captured? as f64)),
        "settling": parts(&|l| Some(l.sent_t as f64 - l.first_read_t? as f64)),
        "translation": parts(&|l| Some(l.tr_t? as f64 - l.sent_t as f64)),
        "presenter": parts(&|l| Some(l.shown_t? as f64 - l.tr_t? as f64)),
        "tv": parts(&|l| Some(l.tv_on? - l.shown_t? as f64)),
    });
    // the end: lines whose English went (not replaced by the next), Arabic down on the TV (or here)
    let gone: Vec<&&Line> = shown.iter().copied().filter(|l| l.ended_how.as_deref() == Some("gone") && l.gone.is_some()).collect();
    let end_tv: Vec<f64> = gone.iter().filter_map(|l| Some(l.tv_off? - l.en_off()?)).collect();
    let end_pc: Vec<f64> = gone.iter().filter_map(|l| Some(l.down_t? as f64 - l.en_off()?)).collect();

    // ---- flicker: a line's Arabic down, and the same line up again within 1.5 s
    let mut flicker: BTreeMap<&str, u64> = BTreeMap::new();
    let mut spans: Vec<f64> = Vec::new();
    let mut ocr_miss_spans: Vec<f64> = Vec::new();
    let mut examples: Vec<Value> = Vec::new();
    let clears: Vec<u64> = ev.iter().filter(|e| kind(e) == "cleared").map(t).collect();
    let nbl: Vec<u64> = ev.iter().filter(|e| kind(e) == "filtered" && s(e, "area") == "subtitles" && s(e, "reason") == "no_bottom_line").map(t).collect();
    for w in shown.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (Some(down), Some(up)) = (a.down_t, b.shown_t) else { continue };
        if a.down_how != "hidden" && a.down_how != "cleared" {
            continue;
        }
        if up < down || up - down > 1500 || similarity(&a.en, &b.en) < 0.9 {
            continue;
        }
        // (how long the English wasn't read: from the last frame it was read in to the frame before it was
        // again; v1 logs: the Arabic's own gap)
        let span = match (a.gone, b.appear) {
            (Some((last, _)), Some((back, _))) => Some(back - last),
            _ => None,
        };
        // the cause; or no flicker: the line said again after a real gap (the screen empty a while)
        let cause = if clears.iter().any(|&c| c + 50 >= down && c <= down + 50) {
            "page"
        } else if nbl.iter().any(|&x| x + 300 >= down && x <= up) {
            "rejected"
        } else if a.ended_how.is_some() {
            if a.absent_text_px > 0 {
                // (the text still on screen while it wasn't read: an OCR miss)
                "ocr_miss"
            } else if span.is_some_and(|sp| sp <= 250.0) {
                // (the English itself away a moment: a line sliding, a blink of the game's)
                "text_blink"
            } else {
                continue;
            }
        } else if up - down <= 400 {
            "unknown"
        } else {
            continue;
        };
        *flicker.entry(cause).or_default() += 1;
        if let Some(sp) = span {
            spans.push(sp);
            if cause == "ocr_miss" {
                ocr_miss_spans.push(sp);
            }
        }
        if examples.len() < 12 {
            examples.push(json!({ "at": fmt_time(down, o), "en": a.en, "cause": cause, "gone_ms": up - down }));
        }
    }
    // the same line replaced by another reading of it (its Arabic changing a moment after it went up)
    let variant_swaps = shown
        .windows(2)
        .filter(|w| w[0].down_how == "replaced" && w[1].shown_t.zip(w[0].shown_t).is_some_and(|(b, a)| b - a < 600) && similarity(&w[0].en, &w[1].en) >= 0.7 && norm(&w[0].en) != norm(&w[1].en))
        .count();
    // the OCR blinks the line stayed up over (v2): how long, and the text still there or not
    let gaps: Vec<&(f64, u64, u64)> = subs.iter().flat_map(|l| l.gaps.iter()).collect();
    spans.extend(gaps.iter().map(|g| g.0));
    // (the blinks over text still on screen: the ones that hid the line, and the ones it stayed up over)
    ocr_miss_spans.extend(gaps.iter().filter(|g| g.2 > 0).map(|g| g.0));

    // ---- lines lost, and why
    let unread: Vec<&Value> = ev.iter().filter(|e| kind(e) == "unread" && s(e, "area") == "subtitles").collect();
    let mut unread_reasons: BTreeMap<String, u64> = BTreeMap::new();
    for e in &unread {
        for (r, n) in e["reasons"].as_object().into_iter().flatten() {
            *unread_reasons.entry(r.clone()).or_default() += n.as_u64().unwrap_or(0);
        }
    }
    let mut filtered: BTreeMap<String, u64> = BTreeMap::new();
    for e in ev.iter().filter(|e| kind(e) == "filtered" && s(e, "area") == "subtitles") {
        *filtered.entry(s(e, "reason").to_string()).or_default() += 1;
    }
    let short: Vec<&&Line> = shown.iter().copied().filter(|l| l.down_how == "hidden" && l.down_t.zip(l.shown_t).is_some_and(|(d, s)| d - s < 300)).collect();
    let lost = json!({
        "unread": { "n": unread.len(), "frames": unread.iter().map(|e| u(e, "frames").unwrap_or(0)).sum::<u64>(),
                    "read_after": unread.iter().filter(|e| e["then_read"] == true).count(), "reasons": unread_reasons,
                    "examples": unread.iter().take(8).map(|e| json!({ "at": fmt_time(u(e, "from").unwrap_or(0), o), "frames": u(e, "frames") })).collect::<Vec<_>>() },
        "filtered": filtered,
        "skipped": subs.iter().filter(|l| l.skipped).count(),
        "replaced_before_shown": subs.iter().filter(|l| l.replaced && l.shown_t.is_none()).count(),
        "dropped": subs.iter().filter(|l| l.dropped).count(),
        "translation_failed": subs.iter().filter(|l| l.tr_ok == Some(false)).count(),
        "late": shown.iter().filter(|l| l.late).count(),
        "shown_under_300ms": short.len(),
    });

    // ---- misreadings
    let name_cut: Vec<&&Line> = subs.iter().filter(|l| name_misread(&l.en)).collect();
    let misread_to_translator: Vec<&&Line> = subs
        .iter()
        .filter(|l| l.hit == Some(false) && subs.iter().any(|m| m.hit == Some(true) && m.seq != l.seq && m.sent_t.abs_diff(l.sent_t) < 10_000 && similarity(&m.en, &l.en) >= 0.85))
        .collect();
    let seen: Vec<&Value> = ev.iter().filter(|e| kind(e) == "seen" && s(e, "area") == "subtitles").collect();
    let unsure = seen.iter().filter(|e| e["sure"] == false).count();
    let misread = json!({
        "speaker_name_cut": name_cut.len(),
        "misread_sent_to_translator": misread_to_translator.len(),
        "unsure_readings_pct": (!seen.is_empty()).then(|| r1(unsure as f64 * 100.0 / seen.len() as f64)),
        "readings_per_line": mean(&subs.iter().map(|l| l.variants as f64).filter(|v| *v > 0.0).collect::<Vec<_>>()),
        "examples": name_cut.iter().chain(misread_to_translator.iter()).take(10).map(|l| json!({ "at": fmt_time(l.sent_t, o), "en": l.en, "ar": l.ar })).collect::<Vec<_>>(),
    });

    // ---- the unread-text finder against the reading (v2)
    let det_total: u64 = sec.det.iter().sum();
    let detector = (det_total > 0).then(|| {
        json!({
            "both": sec.det[0], "pixels_only": sec.det[1], "reading_only": sec.det[2], "neither": sec.det[3],
            // (text read: the pixels saw it too; no text read: the pixels saw none)
            "pixels_saw_read_text_pct": (sec.det[0] + sec.det[2] > 0).then(|| r1(sec.det[0] as f64 * 100.0 / (sec.det[0] + sec.det[2]) as f64)),
            "pixels_quiet_when_nothing_read_pct": (sec.det[1] + sec.det[3] > 0).then(|| r1(sec.det[3] as f64 * 100.0 / (sec.det[1] + sec.det[3]) as f64)),
            "ms_a_call": mean(&sec.detect_ms),
        })
    });

    // ---- the PC
    let passes_s = mean(&sec.passes).unwrap_or(0.0);
    let pass_p50 = quant(&sec.pass, 0.5).unwrap_or(0.0);
    let pass_p90 = quant(&sec.pass, 0.9).unwrap_or(0.0);
    let reader_cpu = mean(&sec.reader_cpu);
    let pc = json!({
        "cpu_app_pct": spread(&sec.cpu_app), "cpu_all_pct": spread(&sec.cpu_all), "cpus": cpus,
        "capture_fps": spread(&sec.fps), "stream_fps": spread(&sec.stream_fps), "reads_a_second": passes_s,
        "pass_ms": spread(&sec.pass), "stages_ms": sec.stages.iter().map(|(k, v)| (k.clone(), spread(v))).collect::<serde_json::Map<String, Value>>(),
        "subtitles_read_done_ms": sec.done.get("subtitles").map(|v| spread(v)),
        "frame_age_ms": spread(&sec.age_ms), "idle_before_pass_ms": spread(&sec.idle_ms), "passes_on_old_frame": sec.same_frame,
        "reader_cpu_ms_a_second": reader_cpu, "copy_ms_a_frame": mean(&sec.copy_ms),
        "preview_seconds": sec.preview_on, "preview_ms_a_frame": mean(&sec.preview_ms),
        "log_ms_a_second": mean(&sec.log_ms), "frame_sizes": sec.frame,
    });

    // ------------------------------------------------------------------------- the five settings
    let mut recs: Vec<Value> = Vec::new();
    let src_ms = quant(&sec.stream_fps, 0.5).filter(|v| *v >= 20.0).map(|v| 1000.0 / v);
    let v2 = !sec.stream_fps.is_empty() || !sec.reader_cpu.is_empty();
    let read_ms = if passes_s > 0.0 { 1000.0 / passes_s } else { frame_ms };

    // 1. Capture FPS: the frames' age against the CPU a frame costs (taking it, reading it)
    if sec.n >= 60 && fps > 0.0 {
        let src = src_ms.unwrap_or(1000.0 / 60.0);
        // (the model's interval for the setting in use, against what was measured: its correction)
        let model_now = capture_interval(timing.capture_fps, src);
        let corr = (frame_ms / model_now).clamp(0.8, 1.25);
        let per_read = reader_cpu.filter(|_| passes_s > 0.0).map(|c| c / passes_s);
        let copy = mean(&sec.copy_ms).unwrap_or(0.0);
        let cpu_ms = |interval: f64| {
            let reads = (1000.0 / interval).min(if pass_p50 > 0.0 { 1000.0 / pass_p50 } else { f64::MAX });
            per_read.map(|p| p * reads + copy * 1000.0 / interval)
        };
        let now_cpu = cpu_ms(frame_ms);
        let cores = cpus.unwrap_or(4.0).max(1.0);
        // (the reading can't keep up with a capture when a pass takes most of a frame's interval)
        let keeps_up = |interval: f64| pass_p50 <= interval * 0.85;
        // the setting in use, as measured: its frames' interval, a line's wait for the next frame
        let now_wait = frame_ms / 2.0 + if keeps_up(frame_ms) { 0.0 } else { (pass_p50 - frame_ms).max(0.0) };
        // candidates: the settings that land cleanly on the source's frames (k of them a capture)
        let mut table: Vec<Value> = Vec::new();
        let mut best: Option<(f64, f64, f64, f64)> = None;
        for k in 2..=9 {
            let setting = (1000.0 / ((f64::from(k) - 0.5) * src)).round();
            if !(5.0..=60.0).contains(&setting) || table.iter().any(|x| x["setting"].as_f64() == Some(setting)) {
                continue;
            }
            let interval = capture_interval(setting, src) * corr;
            let reader_bound = !keeps_up(interval);
            // (a line's start waits for the next frame: half an interval on average, and a reading that can't
            // keep up adds its own)
            let wait = interval / 2.0 + if reader_bound { (pass_p50 - interval).max(0.0) } else { 0.0 };
            let extra_pts = match (cpu_ms(interval), now_cpu) {
                (Some(c), Some(n)) => Some(r1((c - n) / 10.0 / cores)),
                _ => None,
            };
            let ok = !reader_bound && extra_pts.is_none_or(|p| p <= o.cpu_points);
            table.push(json!({ "setting": setting, "frames_a_second": r1(1000.0 / interval), "mean_wait_ms": r1(wait), "reader_bound": reader_bound, "cpu_points_more": extra_pts, "within_budget": ok }));
            // (only a clear gain: 5 ms of a line's wait at least)
            if ok && wait < now_wait - 5.0 && best.is_none_or(|b| wait < b.1) {
                best = Some((setting, wait, 1000.0 / interval, extra_pts.unwrap_or(0.0)));
            }
        }
        let cpu_known = per_read.is_some();
        let (value, conf, why, effect) = match best {
            Some((v, w, rate, pts)) => (
                Some(v),
                if v2 && cpu_known { "medium" } else { "low" },
                format!(
                    "الالتقاط يجري الآن بمعدل {:.1} إطار في الثانية من إعداد {}، لأن الإطار يُؤخذ مع أول إطار يصل من البث بعد مضي الفاصل كاملًا، والبث يرسل إطارًا كل {:.1} مللي ثانية. القيمة {} تقع في منتصف شبكة إطارات البث، فتعطي نحو {:.1} إطار في الثانية بانتظام، والقراءة تجاريها (الدورة p50 ‏{:.0} مللي ثانية).{}",
                    fps, timing.capture_fps, src, v, rate, pass_p50,
                    if cpu_known { "" } else { " كلفة المعالج لم تُقس (سجل الإصدار الأول)، فالزيادة فيها غير معروفة." }
                ),
                format!("يقصر انتظار السطر الجديد للإطار التالي من نحو {:.0} إلى نحو {:.0} مللي ثانية في المتوسط{}.", now_wait, w, if cpu_known { format!("، بزيادة تقديرية نحو {pts:.1} نقطة في معالج الجهاز") } else { String::new() }),
            ),
            None => (
                Some(timing.capture_fps),
                if v2 && cpu_known { "medium" } else { "low" },
                format!(
                    "لا قيمة أخرى تقصّر الانتظار 5 مللي ثانية على الأقل ضمن قدرة القراءة (الدورة p50 ‏{:.0} مللي ثانية) وحد المعالج: الالتقاط الآن {:.1} إطار في الثانية.",
                    pass_p50, fps
                ),
                format!("لا تغيير: انتظار السطر للإطار التالي نحو {:.0} مللي ثانية في المتوسط.", now_wait),
            ),
        };
        recs.push(advice(
            "capture_fps",
            timing.capture_fps,
            value,
            conf,
            why,
            effect,
            json!({ "capture_fps_measured": fps, "source_frame_ms": src_ms.map(r1), "source_assumed_60": src_ms.is_none(), "pass_p50": pass_p50, "pass_p90": pass_p90, "reader_cpu_ms_a_read": per_read.map(r1), "copy_ms": copy, "candidates": table }),
            "جلسة لعب عادية بالقيمة المقترحة، ثم أقارن المعدل الفعلي وزمن الظهور والمعالج بهذه الجلسة.".into(),
        ));
    } else {
        recs.push(advice("capture_fps", timing.capture_fps, None, "insufficient", "البيانات غير كافية: أقل من دقيقة من القراءات.".into(), String::new(), json!({ "seconds": sec.n }), "جلسة لعب 10 دقائق على الأقل.".into()));
    }

    // 2. Preview FPS: the preview isn't read: only its CPU counts
    {
        let share = if sec.n > 0 { sec.preview_on as f64 / sec.n as f64 } else { 0.0 };
        let cost = mean(&sec.preview_ms);
        let cores = cpus.unwrap_or(4.0).max(1.0);
        let (value, conf, why, effect) = match cost {
            Some(c) if sec.preview_on >= 20 => {
                // (at most 5 % of the machine while the window shows it)
                let budget_ms = 0.05 * 1000.0 * cores;
                let v = (budget_ms / c.max(0.1)).floor().clamp(10.0, 60.0);
                (Some(v), "high", format!("المعاينة لا تؤثر في القراءة ولا الترجمة، وكلفتها {c:.1} مللي ثانية لكل إطار. القيمة {v} تبقيها في حدود 5% من الجهاز حين تكون نافذة التطبيق ظاهرة."), format!("المعاينة ظهرت {:.0}% من الوقت؛ تأثيرها على المعالج فقط، ولا أثر لها على التزامن.", share * 100.0))
            }
            _ if share < 0.1 => (Some(timing.preview_fps), "high", format!("نافذة التطبيق كانت مصغّرة {:.0}% من الوقت، والمعاينة لا تُصنع إلا وهي ظاهرة، فلا أثر لهذا الإعداد في جلستك.", (1.0 - share) * 100.0), "لا أثر على التزامن ولا على المعالج ما دامت النافذة مصغّرة. لتحديد أفضل قيمة للمعاينة الظاهرة: اترك النافذة ظاهرة دقيقة أو دقيقتين أثناء اللعب.".into()),
            _ => (None, "insufficient", "البيانات غير كافية لقياس كلفة إطار المعاينة (سجل الإصدار الأول لا يقيسها).".into(), String::new()),
        };
        recs.push(advice("preview_fps", timing.preview_fps, value, conf, why, effect, json!({ "preview_seconds": sec.preview_on, "seconds": sec.n, "preview_ms_a_frame": cost }), "اترك نافذة التطبيق ظاهرة دقيقتين أثناء اللعب مرة واحدة لقياس كلفة المعاينة.".into()));
    }

    // 3. Reading interval: does it hold the reading back?
    {
        let idle = mean(&sec.idle_ms);
        let bound = if pass_p50 >= frame_ms * 0.9 { "reader" } else if timing.interval_ms > frame_ms { "interval" } else { "frames" };
        let value = if bound == "interval" { (frame_ms * 0.8).floor().max(16.0) } else { 16.0_f64.min(timing.interval_ms).max(16.0) };
        let why = match bound {
            "frames" => format!("القراءة تنتظر كل إطار جديد: الدورة p50 ‏{pass_p50:.0} مللي ثانية أقصر من فترة الإطار ({frame_ms:.0}). فالفاصل لا يؤخر شيئًا ما دام أقل من فترة الإطار."),
            "reader" => format!("الدورة ({pass_p50:.0} مللي ثانية) أطول من فترة الإطار ({frame_ms:.0})، فالقراءة متصلة والفاصل لا أثر له. المحدِّد زمن القراءة نفسه."),
            _ => format!("الفاصل ({:.0}) أطول من فترة الإطار ({frame_ms:.0})، فيؤخر قراءة إطارات جاهزة.", timing.interval_ms),
        };
        recs.push(advice(
            "interval_ms",
            timing.interval_ms,
            Some(value),
            "high",
            why,
            if bound == "interval" { format!("تُقرأ كل الإطارات الملتقطة، ويقصر الانتظار بنحو {:.0} مللي ثانية.", (timing.interval_ms - frame_ms) / 2.0) } else { "لا تغيير في التزامن: الفاصل ليس المحدِّد. خفض الحمل يكون بـ Capture FPS لا بالفاصل.".into() },
            json!({ "bound": bound, "pass_p50": pass_p50, "frame_ms": r1(frame_ms), "idle_before_pass_ms": idle, "reads_a_second": passes_s }),
            "لا يحتاج اختبارًا مستقلًا: يُتحقق منه مع أي جلسة.".into(),
        ));
    }

    // 4. Text stability: a new (unknown) reading waits this long before it's sent
    recs.push(stability_advice(&seen, ev, timing, read_ms, o));

    // 5. Clear after: the Arabic goes this long after its English was last read
    recs.push(clear_after_advice(&spans, &ocr_miss_spans, &gone, &end_tv, &end_pc, shown.len(), minutes, timing, read_ms, o));

    json!({
        "minutes": r1(minutes),
        "settings": { "capture_fps": timing.capture_fps, "preview_fps": timing.preview_fps, "interval_ms": timing.interval_ms, "stable_ms": timing.stable_ms, "clear_after_ms": timing.clear_after_ms },
        "log_version": if v2 { 2 } else { 1 },
        "lines": { "sent": subs.len(), "shown": shown.len(), "from_memory": shown.iter().filter(|l| l.hit == Some(true)).count(), "translator": shown.iter().filter(|l| l.hit == Some(false)).count(), "a_minute": (minutes > 0.0).then(|| r1(shown.len() as f64 / minutes)) },
        "sync": {
            "start_on_tv_ms": spread(&start_tv), "start_here_ms": spread(&start_pc), "start_breakdown_ms": breakdown,
            "end_on_tv_ms": spread(&end_tv), "end_here_ms": spread(&end_pc), "english_appear_bracket_ms": spread(&bracket),
        },
        "flicker": { "by_cause": flicker, "a_minute": (minutes > 0.0).then(|| r1(flicker.values().sum::<u64>() as f64 / minutes)), "variant_swaps": variant_swaps, "examples": examples },
        "lost": lost,
        "misread": misread,
        "detector": detector,
        "pc": pc,
        "recommendations": recs,
    })
}

fn stability_advice(seen: &[&Value], ev: &[Value], timing: Timing, read_ms: f64, o: &Options) -> Value {
    // each reading: how long it stayed (until the next reading of the area, or the area went quiet), known
    // or not, sure or not; and whether it was the line's last reading (the real one) or a passing one
    let ends: Vec<u64> = ev.iter().filter(|e| kind(e) == "ended" && s(e, "area") == "subtitles").map(t).collect();
    let mut unknown: Vec<(f64, bool, bool)> = Vec::new(); // (stayed ms, sure, passing)
    let has_known = seen.iter().any(|e| e["known"].is_boolean());
    for (i, r) in seen.iter().enumerate() {
        if r["known"] != false {
            continue;
        }
        let next = seen.get(i + 1);
        let end = ends.iter().copied().find(|&x| x > t(r));
        let until = match (next.map(|n| t(n)), end) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => continue,
        };
        let stayed = until.saturating_sub(t(r)) as f64;
        let passing = next.is_some_and(|n| t(n) - t(r) <= 1500 && related(s(n, "text"), s(r, "text")) && n["known"] == true);
        unknown.push((stayed, r["sure"] != false, passing));
    }
    let n = unknown.len();
    if !has_known || n < 10 {
        return advice(
            "stable_ms",
            timing.stable_ms,
            None,
            "insufficient",
            if has_known { format!("البيانات غير كافية: {n} قراءة جديدة فقط لم تعرفها الذاكرة (الحد الأدنى 10).") } else { "البيانات غير كافية: سجل الإصدار الأول لا يقول أي القراءات تعرفها الذاكرة.".into() },
            String::new(),
            json!({ "unknown_readings": n }),
            "جلسة لعب أطول، أو في أماكن فيها حوار جديد لم تُترجم أسطره من قبل.".into(),
        );
    }
    // The reader sends a new reading at a reading pass only, the k-th after its first (k = the wait over the
    // passes' interval R, rounded up; 1 at the least: two readings of it), if it's still what's read then —
    // so it stayed k + 1 passes (half a pass's margin either way: the passes' jitter); k − 1 passes later
    // than with no wait.
    let r = read_ms.max(1.0);
    let mut table = Vec::new();
    let mut best: Option<(f64, f64)> = None;
    for &sv in &[0.0f64, 50.0, 100.0, 150.0, 200.0, 300.0, 400.0] {
        let (mut premature, mut lost, mut delay) = (0u32, 0u32, 0.0f64);
        let passes = |w: f64| (w / r).ceil().max(1.0);
        for &(stayed, sure, passing) in &unknown {
            let k = passes(if sure { sv } else { sv * 3.0 });
            let sent = stayed >= (k + 0.5) * r;
            if passing && sent {
                premature += 1;
            }
            if !passing {
                if sent {
                    delay += (k - 1.0) * r;
                } else {
                    lost += 1;
                }
            }
        }
        let cost = f64::from(premature) * o.premature_ms + f64::from(lost) * o.lost_ms + delay;
        table.push(json!({ "stable_ms": sv, "passes_sure": passes(sv), "passes_unsure": passes(sv * 3.0), "premature": premature, "lost": lost, "added_delay_ms": r1(delay), "cost": r1(cost) }));
        if best.is_none_or(|b| cost < b.1) {
            best = Some((sv, cost));
        }
    }
    let (v, _) = best.unwrap_or((timing.stable_ms, 0.0));
    let row = |x: f64| table.iter().find(|r| r["stable_ms"].as_f64() == Some(x)).cloned().unwrap_or_default();
    let (now, rec) = (row(timing.stable_ms), row(v));
    advice(
        "stable_ms",
        timing.stable_ms,
        Some(v),
        if n >= 30 { "medium" } else { "low" },
        format!(
            "من {n} قراءة لم تعرفها الذاكرة: القيمة {v} توازن بين عرض قراءات ناقصة أو خاطئة ثم استبدالها، وتأخير الأسطر الجديدة فعلًا، وفقد أسطر قصيرة. الانتظار يتم بخطوات دورة القراءة ({r:.0} مللي ثانية)، وقراءتان على الأقل. الأسطر المعروفة للذاكرة تُعرض من أول قراءة في كل الأحوال."
        ),
        format!(
            "قراءات معروضة قبل أوانها: {} ← {}. أسطر جديدة لا تُعرض: {} ← {}. تأخير مضاف للأسطر الجديدة: {} ← {} مللي ثانية إجمالًا.",
            now["premature"], rec["premature"], now["lost"], rec["lost"], now["added_delay_ms"], rec["added_delay_ms"]
        ),
        json!({ "unknown_readings": n, "read_ms": r1(read_ms), "table": table }),
        "جلسة لعب بالقيمة المقترحة، ثم أقارن طلبات المترجم والاستبدال السريع والأسطر الجديدة المعروضة.".into(),
    )
}

#[allow(clippy::too_many_arguments)]
fn clear_after_advice(spans: &[f64], ocr_miss: &[f64], gone: &[&&Line], end_tv: &[f64], end_pc: &[f64], shown: usize, minutes: f64, timing: Timing, read_ms: f64, o: &Options) -> Value {
    let ends = if end_tv.len() >= end_pc.len() { end_tv } else { end_pc };
    if spans.len() < 5 || ends.len() < 20 {
        return advice(
            "clear_after_ms",
            timing.clear_after_ms,
            None,
            "insufficient",
            format!("البيانات غير كافية: {} انقطاع قراءة قصير و{} نهاية سطر مقيسة (الحد الأدنى 5 و20، ويحتاجان سجل الإصدار 2).", spans.len(), ends.len()),
            String::new(),
            json!({ "blinks": spans.len(), "ends": ends.len() }),
            "جلسة لعب 15 دقيقة بالسجل الإصدار 2.".into(),
        );
    }
    // The reader tells the English gone at a reading pass only: the Arabic goes at the first pass more than
    // Clear after since the English was last read, and stays up over a blink whose last unread pass came
    // sooner (`span`: from the last pass the line was read in to the last one it wasn't). So the setting
    // works in steps of the passes' interval R: any value from k·R to (k+1)·R does the same — the Arabic
    // stays up over blinks of up to k passes, and goes k passes after its English.
    let r = read_ms.max(1.0);
    let step = |a: f64| (a / r).floor();
    let base_end = quant(ends, 0.5).unwrap_or(0.0);
    // candidates: every 10 ms from 30 to 500 and the value in use (what each would do, to compare), of which
    // the middle of each step is the one advised (the furthest from the passes' jitter either way)
    let middles: Vec<f64> = (0..50)
        .map(f64::from)
        .map(|k| (k, ((k + 0.5) * r / 10.0).round() * 10.0))
        .filter(|&(k, a)| (30.0..=500.0).contains(&a) && step(a) == k)
        .map(|(_, a)| a)
        .collect();
    let mut cands: Vec<(f64, bool)> = (3..=50).map(|k| f64::from(k) * 10.0).map(|a| (a, middles.contains(&a))).collect();
    if !cands.iter().any(|c| c.0 == timing.clear_after_ms) {
        cands.push((timing.clear_after_ms, false));
        cands.sort_by(|x, y| x.0.total_cmp(&y.0));
    }
    let mut table = Vec::new();
    let mut best: Option<(f64, f64)> = None;
    for &(a, middle) in &cands {
        let flickers = spans.iter().filter(|&&sp| sp > a).count() as f64;
        let shift = (step(a) - step(timing.clear_after_ms)) * r;
        let late: f64 = ends.iter().map(|e| (e + shift).max(0.0)).sum();
        let cost = flickers * o.flicker_ms + late;
        table.push(json!({ "clear_after_ms": a, "passes": step(a), "flickers": flickers, "flickers_a_minute": r1(flickers / minutes.max(0.1)), "end_lag_p50_ms": r1(base_end + shift), "cost": r1(cost) }));
        if middle && best.is_none_or(|b| cost < b.1) {
            best = Some((a, cost));
        }
    }
    let (mid, _) = best.unwrap_or((timing.clear_after_ms, 0.0));
    // the value in use, if it's in the best step already and clear of its edges: nothing to change
    let cur = timing.clear_after_ms;
    let clear_of_edges = (cur - step(cur) * r).min((step(cur) + 1.0) * r - cur) >= 0.2 * r;
    let v = if step(cur) == step(mid) && clear_of_edges { cur } else { mid };
    let row = |x: f64| table.iter().find(|r| r["clear_after_ms"].as_f64() == Some(x)).cloned().unwrap_or_default();
    let (now, rec) = (row(cur), row(v));
    let p95_miss = quant(ocr_miss, 0.95);
    let (lo, hi) = (step(v) * r, (step(v) + 1.0) * r);
    advice(
        "clear_after_ms",
        cur,
        Some(v),
        if spans.len() >= 10 && ends.len() >= 30 { "high" } else { "medium" },
        format!(
            "القراءة تجري كل {r:.0} مللي ثانية، ولا يحكم التطبيق باختفاء النص إلا عند قراءة، فهذا الإعداد يعمل بخطوات من {r:.0}: كل القيم من {lo:.0} إلى أقل من {hi:.0} متكافئة، {}. خلال الجلسة انقطعت قراءة سطر ظاهر {} مرة، منها {} والنص ما زال على الشاشة (خطأ قراءة). القيمة {v} أقل كلفة وفق المعيار: الوميض مقابل بقاء العربية بعد الإنجليزية (الوميض الواحد يُحسب بـ {:.1} ثانية من البقاء). هذه القيمة لمعدل الالتقاط الحالي: إن تغيّر Capture FPS تتغير الخطوة.",
            if step(v) < 1.0 {
                "تُخفي العربية عند أول قراءة لا يُرى فيها النص، فأي انقطاع قراءة يسبب وميضًا".to_string()
            } else {
                format!("تُبقي العربية فوق انقطاع قراءة حتى {n}، وتؤخر إخفاءها {n} بعد اختفاء الإنجليزية", n = passes_ar(step(v)))
            },
            spans.len(),
            ocr_miss.len(),
            o.flicker_ms / 1000.0
        ),
        format!(
            "الوميض: {} ← {} في الدقيقة. تأخر اختفاء العربية عن الإنجليزية (p50): {} ← {} مللي ثانية.",
            now["flickers_a_minute"], rec["flickers_a_minute"], now["end_lag_p50_ms"], rec["end_lag_p50_ms"]
        ),
        json!({ "blinks": spans.len(), "blinks_text_still_there": ocr_miss.len(), "blink_p95_text_still_there_ms": p95_miss, "ends": ends.len(), "lines_shown": shown, "gone_lines": gone.len(), "pass_ms": r1(r), "table": table }),
        "جلسة لعب بالقيمة المقترحة، ثم أقارن الوميض وتأخر الاختفاء. والحكم الأخير لعينك على التلفزيون.".into(),
    )
}

/// "n passes" in Arabic, its noun as the number wants it.
fn passes_ar(n: f64) -> String {
    match n as u64 {
        0 => "لا دورة".into(),
        1 => "دورة واحدة".into(),
        2 => "دورتين".into(),
        k @ 3..=10 => format!("{k} دورات"),
        k => format!("{k} دورة"),
    }
}

fn fmt_time(unix_ms: u64, o: &Options) -> String {
    let ms = (unix_ms as i64 + i64::from(o.tz_minutes) * 60_000).max(0) as u64;
    let st = linelog::utc_stamp(ms);
    // "YYYY-MM-DD_HH-MM-SSZ" → "HH:MM:SS"
    st.get(11..19).map(|x| x.replace('-', ":")).unwrap_or(st)
}

// ------------------------------------------------------------------------------------- the whole log

/// The analysis of `events`: each segment (cut where the settings changed or the log started again), the
/// lines of them all for review, and the advice from the longest segment played with the latest settings.
pub fn analyze(events: &[Value], o: &Options) -> Value {
    let cpus = events.iter().rev().find(|e| kind(e) == "log" && e["cpus"].as_u64().is_some_and(|c| c > 0)).and_then(|e| f(e, "cpus"));
    // the cuts: a settings change, or the log starting again
    let mut cuts: Vec<(usize, Option<&Value>)> = vec![(0, None)];
    let mut current: Option<&Value> = None;
    for (i, e) in events.iter().enumerate() {
        match kind(e) {
            "settings" => {
                current = Some(e);
                cuts.push((i, current));
            }
            "log" if s(e, "state") == "on" => cuts.push((i, current)),
            _ => {}
        }
    }
    cuts.dedup_by_key(|c| c.0);
    let mut segments = Vec::new();
    for w in 0..cuts.len() {
        let (a, set) = cuts[w];
        let b = cuts.get(w + 1).map(|c| c.0).unwrap_or(events.len());
        let ev = &events[a..b];
        // (under a minute of readings: a change being made, not a part to judge)
        let reads = ev.iter().filter(|e| kind(e) == "reads").count();
        if reads < 60 {
            continue;
        }
        let set = set.or_else(|| events[..=a.min(events.len() - 1)].iter().rev().find(|e| kind(e) == "settings"));
        let mut r = analyze_segment(ev, timing_of(set), cpus, o);
        r["from"] = json!(fmt_time(t(&ev[0]), o));
        r["to"] = json!(fmt_time(t(&ev[ev.len() - 1]), o));
        segments.push(r);
    }
    // (no part long enough: the whole log as one, its advice saying where the data falls short)
    if segments.is_empty() && !events.is_empty() {
        let set = events.iter().rev().find(|e| kind(e) == "settings");
        let mut r = analyze_segment(events, timing_of(set), cpus, o);
        r["from"] = json!(fmt_time(t(&events[0]), o));
        r["to"] = json!(fmt_time(t(&events[events.len() - 1]), o));
        r["short"] = json!(true);
        segments.push(r);
    }
    let main = segments.iter().max_by(|a, b| a["minutes"].as_f64().unwrap_or(0.0).total_cmp(&b["minutes"].as_f64().unwrap_or(0.0))).cloned();
    json!({
        "tz_minutes": o.tz_minutes,
        "segments": segments,
        "advice": main.as_ref().map(|m| m["recommendations"].clone()),
        "advice_from": main.as_ref().map(|m| json!({ "from": m["from"], "to": m["to"], "minutes": m["minutes"] })),
    })
}

/// Each line for review (CSV): when, where, the English as read, the Arabic as shown, from where, and its
/// timing and flags.
pub fn review_csv(events: &[Value], o: &Options) -> String {
    let fps = events.iter().filter(|e| kind(e) == "reads").filter_map(|e| f(e, "fps")).collect::<Vec<_>>();
    let frame_ms = quant(&fps, 0.5).filter(|v| *v > 0.0).map(|v| 1000.0 / v).unwrap_or(100.0);
    let lines = lines_of(events, frame_ms);
    let q = |x: &str| format!("\"{}\"", x.replace('"', "\"\""));
    let mut out = String::from("time,area,english,arabic,source,start_on_tv_ms,start_here_ms,shown_ms,end_on_tv_ms,flags\n");
    for l in &lines {
        let Some(st) = l.shown_t else { continue };
        let mut flags = Vec::new();
        if l.hit == Some(false) {
            flags.push("translator");
        }
        if name_misread(&l.en) {
            flags.push("name_misread");
        }
        if l.late {
            flags.push("late");
        }
        if l.down_t.is_some_and(|d| d - st < 300) && l.down_how == "hidden" {
            flags.push("under_300ms");
        }
        if l.variants > 1 {
            flags.push("several_readings");
        }
        let opt = |v: Option<f64>| v.map(|x| format!("{x:.0}")).unwrap_or_default();
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{}\n",
            fmt_time(st, o),
            l.area,
            q(&l.en),
            q(&l.ar),
            q(&l.sources.join("+")),
            opt(l.tv_on.zip(l.en_on()).map(|(a, b)| a - b)),
            opt(l.en_on().map(|b| st as f64 - b)),
            opt(l.down_t.map(|d| (d - st) as f64)),
            opt(l.tv_off.zip(l.en_off()).map(|(a, b)| a - b)),
            flags.join(" ")
        ));
    }
    out
}

/// The analysis as text, in Arabic: the session, the sync, what was lost and why, and each setting's
/// advice with its evidence and how sure it is.
pub fn render_ar(r: &Value) -> String {
    let mut o = String::new();
    let conf = |c: &str| match c {
        "high" => "عالية (مدعومة بالسجل)",
        "medium" => "متوسطة (تقدير من السجل، يحتاج اختبارًا)",
        "low" => "منخفضة (تقدير ضعيف، يحتاج اختبارًا)",
        _ => "البيانات غير كافية",
    };
    let name = |k: &str| match k {
        "capture_fps" => "Capture FPS",
        "preview_fps" => "Preview FPS",
        "interval_ms" => "Reading interval",
        "stable_ms" => "Text stability",
        "clear_after_ms" => "Clear after",
        _ => "?",
    };
    let v = |x: &Value| if x.is_null() { "—".to_string() } else { x.to_string() };
    o.push_str("تقرير جلسة القياس\n=================\n");
    for (i, seg) in r["segments"].as_array().into_iter().flatten().enumerate() {
        o.push_str(&format!(
            "\nالجزء {} ({} – {}، {} دقيقة، السجل الإصدار {})\nالإعدادات: Capture FPS {} · Preview FPS {} · Reading interval {} · Text stability {} · Clear after {}\n",
            i + 1,
            seg["from"].as_str().unwrap_or(""),
            seg["to"].as_str().unwrap_or(""),
            seg["minutes"],
            seg["log_version"],
            seg["settings"]["capture_fps"],
            seg["settings"]["preview_fps"],
            seg["settings"]["interval_ms"],
            seg["settings"]["stable_ms"],
            seg["settings"]["clear_after_ms"]
        ));
        let sy = &seg["sync"];
        o.push_str(&format!(
            "- الأسطر المعروضة: {} ({} في الدقيقة)، من الذاكرة {}، من المترجم {}\n",
            seg["lines"]["shown"], v(&seg["lines"]["a_minute"]), seg["lines"]["from_memory"], seg["lines"]["translator"]
        ));
        o.push_str(&format!(
            "- ظهور العربية بعد الإنجليزية: على التلفزيون p50 {} / p90 {} مللي ثانية؛ في التطبيق p50 {} (الإنجليزية محصورة بين إطارين يفصلهما p50 {} مللي ثانية)\n",
            v(&sy["start_on_tv_ms"]["p50"]), v(&sy["start_on_tv_ms"]["p90"]), v(&sy["start_here_ms"]["p50"]), v(&sy["english_appear_bracket_ms"]["p50"])
        ));
        let b = &sy["start_breakdown_ms"];
        o.push_str(&format!(
            "  تفكيكه (p50): انتظار الإطار {} · القراءة {} · الاستقرار {} · الترجمة {} · المقدّم {} · التلفزيون {}\n",
            v(&b["frame_wait"]["p50"]), v(&b["reading"]["p50"]), v(&b["settling"]["p50"]), v(&b["translation"]["p50"]), v(&b["presenter"]["p50"]), v(&b["tv"]["p50"])
        ));
        o.push_str(&format!(
            "- اختفاء العربية بعد الإنجليزية: على التلفزيون p50 {} / p90 {} مللي ثانية (لـ {} سطرًا اختفت إنجليزيته)؛ في التطبيق p50 {} / p90 {} (لـ {} سطرًا)\n",
            v(&sy["end_on_tv_ms"]["p50"]),
            v(&sy["end_on_tv_ms"]["p90"]),
            sy["end_on_tv_ms"]["n"],
            v(&sy["end_here_ms"]["p50"]),
            v(&sy["end_here_ms"]["p90"]),
            sy["end_here_ms"]["n"]
        ));
        o.push_str(&format!("- الوميض: {} في الدقيقة، حسب السبب: {}؛ واستبدال سريع بقراءة أخرى: {}\n", v(&seg["flicker"]["a_minute"]), seg["flicker"]["by_cause"], seg["flicker"]["variant_swaps"]));
        let l = &seg["lost"];
        o.push_str(&format!(
            "- الفقد: نص ظاهر لم يُقرأ {} مرة ({} إطارًا، قُرئ لاحقًا {})، أسبابه {}؛ رُفض بقاعدة {}؛ تخطّاه أحدث {}؛ متأخر {}؛ عُرض أقل من 0.3 ثانية {}\n",
            l["unread"]["n"], l["unread"]["frames"], l["unread"]["read_after"], l["unread"]["reasons"], l["filtered"], l["skipped"], l["late"], l["shown_under_300ms"]
        ));
        let m = &seg["misread"];
        o.push_str(&format!(
            "- أخطاء القراءة: اسم متحدث مقطوع {}، قراءة خاطئة ذهبت للمترجم {}، قراءات غير مؤكدة {}%\n",
            m["speaker_name_cut"], m["misread_sent_to_translator"], v(&m["unsure_readings_pct"])
        ));
        if let Some(d) = seg["detector"].as_object() {
            o.push_str(&format!(
                "- كاشف النص: رأى النص في {}% من القراءات الناجحة، ولم يرَ نصًا في {}% مما لم يُقرأ فيه شيء، وزمنه {} مللي ثانية للقراءة\n",
                v(&d["pixels_saw_read_text_pct"]), v(&d["pixels_quiet_when_nothing_read_pct"]), v(&d["ms_a_call"])
            ));
        }
        let pc = &seg["pc"];
        o.push_str(&format!(
            "- الجهاز: المعالج للتطبيق p50 {}% وللجهاز {}%؛ الالتقاط {} إطار/ث من بث {}؛ الدورة p50 {} مللي ثانية؛ معالج خيط القراءة {} مللي ثانية/ث؛ كلفة السجل نفسه {} مللي ثانية/ث\n",
            v(&pc["cpu_app_pct"]["p50"]), v(&pc["cpu_all_pct"]["p50"]), v(&pc["capture_fps"]["p50"]), v(&pc["stream_fps"]["p50"]), v(&pc["pass_ms"]["p50"]), v(&pc["reader_cpu_ms_a_second"]), v(&pc["log_ms_a_second"])
        ));
    }
    if let Some(recs) = r["advice"].as_array() {
        o.push_str(&format!(
            "\nالتوصيات (من الجزء {} – {}، {} دقيقة)\n----------------------------------------\n",
            r["advice_from"]["from"].as_str().unwrap_or(""),
            r["advice_from"]["to"].as_str().unwrap_or(""),
            r["advice_from"]["minutes"]
        ));
        for a in recs {
            o.push_str(&format!(
                "\n{}: الحالية {} ← المقترحة {}\n  الثقة: {}\n  السبب: {}\n",
                name(a["setting"].as_str().unwrap_or("")),
                a["now"],
                v(&a["value"]),
                conf(a["confidence"].as_str().unwrap_or("")),
                a["why"].as_str().unwrap_or("")
            ));
            if let Some(e) = a["effect"].as_str().filter(|e| !e.is_empty()) {
                o.push_str(&format!("  الأثر المتوقع: {e}\n"));
            }
            o.push_str(&format!("  للتأكد: {}\n", a["test"].as_str().unwrap_or("")));
        }
    }
    o
}

/// Analyze the logs at `paths` and write the report (JSON), its Arabic text and the lines' review sheet into
/// `out`: the three files' paths.
pub fn run(paths: &[PathBuf], out: &Path, o: &Options) -> anyhow::Result<(PathBuf, PathBuf, PathBuf)> {
    let events = load(paths)?;
    let report = analyze(&events, o);
    std::fs::create_dir_all(out)?;
    let (j, a, c) = (out.join("advice.json"), out.join("advice-ar.txt"), out.join("lines-review.csv"));
    std::fs::write(&j, serde_json::to_string_pretty(&report)?)?;
    std::fs::write(&a, render_ar(&report))?;
    // (UTF-8 with its mark: a spreadsheet opens the Arabic right)
    std::fs::write(&c, format!("\u{feff}{}", review_csv(&events, o)))?;
    Ok((j, a, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_advise_tells_alike_texts_and_misread_names() {
        assert!(similarity("Jason: You good?", "Jason: You good?") > 0.99);
        assert!(similarity("ason: Uh-huh, got it", "Jason: Uh-huh, got it") > 0.9);
        assert!(similarity("Local: Fuck off!", "Jason: Shut up.") < 0.5);
        assert!(related("Morning.", "Jason: Morning."));
        for bad in ["ason: Uh-huh", ".Jason: fuck you.", "'Local: Weird...", "ILocals: Yeah.", "IOLocaIs: Yo", "ocals: Whatever"] {
            assert!(name_misread(bad), "{bad}");
        }
        for good in ["Jason: Uh-huh", "Locals: Yeah.", "Tourists: Move.", "No name here", "Cop: Stop the vehicle!"] {
            assert!(!name_misread(good), "{good}");
        }
    }

    #[test]
    fn fork_advise_knows_how_the_capture_lands_on_the_sources_frames() {
        let src = 1000.0 / 60.0;
        // 12 is right at 5 frames: half the time 6 (≈ 10.9 a second, measured 10)
        assert!((capture_interval(12.0, src) - 5.5 * src).abs() < 0.01);
        // 13 lands cleanly on 5 frames (12 a second), 17 on 4 (15), 24 on 3 (20)
        assert!((capture_interval(13.0, src) - 5.0 * src).abs() < 0.01);
        assert!((capture_interval(17.0, src) - 4.0 * src).abs() < 0.01);
        assert!((capture_interval(24.0, src) - 3.0 * src).abs() < 0.01);
    }

    /// A made-up session: `n` lines read, sent, shown and gone on a 10-a-second capture, some with an OCR
    /// blink of `blink` ms (the text still there), the TV drawing each state 60 ms after it's sent.
    pub(crate) fn session(n: u64, blink_every: u64, blink_frames: u64, clear_after: u64) -> Vec<Value> {
        let mut ev = Vec::new();
        let base = 1_800_000_000_000u64;
        let e = |t: u64, v: Value| {
            let mut v = v;
            v["t"] = json!(t);
            v
        };
        ev.push(e(base, json!({ "ev": "log", "state": "on", "version": 2, "cpus": 4 })));
        ev.push(e(base + 1, json!({ "ev": "settings", "capture_fps": 12, "preview_fps": 60, "interval_ms": 16, "stable_ms": 0, "clear_after_ms": clear_after })));
        for sec in 0..(n * 4 + 10) {
            ev.push(e(base + 1000 + sec * 1000, json!({ "ev": "reads", "fps": 10.0, "stream_fps": 60.0, "pass_n": 10, "pass_avg": 70.0, "cpu_app": 18.0, "cpu_all": 42.0, "reader_cpu_ms": 600.0, "copy_ms": 2.0, "age_ms": 30.0, "idle_ms": 30.0, "log_ms": 0.4, "preview_fps": 0.0, "det": { "tt": 8, "tn": 0, "nt": 0, "nn": 2 }, "detect_ms": 0.1 })));
        }
        let mut tv = 0u64;
        for i in 0..n {
            let t0 = base + 2000 + i * 4000;
            let seq = i + 1;
            let en = format!("Jason: Line number {i} of the test.");
            // first read on the frame captured at t0 (the frame before at t0 − 100, without text)
            ev.push(e(t0 + 60, json!({ "ev": "seen", "area": "subtitles", "text": en, "captured": t0, "prev_captured": t0 - 100, "prev_text": false, "known": true, "sure": true })));
            ev.push(e(t0 + 61, json!({ "ev": "sent", "area": "subtitles", "seq": seq, "en": en, "at": t0 + 60, "captured": t0, "appear_after": t0 - 100, "appear_by": t0 })));
            ev.push(e(t0 + 62, json!({ "ev": "translated", "area": "subtitles", "seq": seq, "hit": true, "ok": true, "ms": 0.1, "sources": ["reviewed"] })));
            ev.push(e(t0 + 70, json!({ "ev": "shown", "area": "subtitles", "seq": seq, "at": t0 + 60, "en": en, "late": false, "ar": "سطر" })));
            tv += 1;
            ev.push(e(t0 + 80, json!({ "ev": "tv_sent", "tv": "a", "version": format!("v{tv}"), "shown": { "subtitles": t0 + 60 } })));
            ev.push(e(t0 + 140, json!({ "ev": "tv_drawn", "tv": "a", "version": format!("v{tv}"), "est": (t0 + 140) as f64 })));
            if blink_every > 0 && i % blink_every == 0 {
                let last = t0 + 1000;
                ev.push(e(last + 100 * (blink_frames + 1) + 60, json!({ "ev": "gap", "area": "subtitles", "seq": seq, "frames": blink_frames, "last_present": last, "first_absent": last + 100, "last_absent": last + 100 * blink_frames, "back": last + 100 * (blink_frames + 1), "text_px": blink_frames })));
            }
            // gone: last read on the frame at t0 + 2000, the next without it
            let last = t0 + 2000;
            ev.push(e(last + clear_after + 160, json!({ "ev": "hidden", "area": "subtitles", "seq": seq, "shown_ms": 2000 })));
            ev.push(e(last + clear_after + 200, json!({ "ev": "ended", "area": "subtitles", "seq": seq, "how": "gone", "last_present": last, "first_absent": last + 100, "absent_frames": 2, "absent_text_px": 0 })));
            tv += 1;
            ev.push(e(last + clear_after + 170, json!({ "ev": "tv_sent", "tv": "a", "version": format!("v{tv}"), "shown": {} })));
            ev.push(e(last + clear_after + 230, json!({ "ev": "tv_drawn", "tv": "a", "version": format!("v{tv}"), "est": (last + clear_after + 230) as f64 })));
        }
        ev.sort_by_key(t);
        ev
    }

    #[test]
    fn fork_advise_measures_the_sync_of_a_made_up_session() {
        let ev = session(40, 4, 2, 70);
        let r = analyze(&ev, &Options::default());
        let seg = &r["segments"][0];
        assert_eq!(seg["lines"]["shown"], 40);
        // the English appeared between t0 − 100 and t0 (its middle t0 − 50); the TV drew it at t0 + 140
        assert_eq!(seg["sync"]["start_on_tv_ms"]["p50"].as_f64(), Some(190.0), "{}", seg["sync"]);
        assert_eq!(seg["sync"]["start_here_ms"]["p50"].as_f64(), Some(120.0));
        // gone between t0 + 2000 and + 2100 (middle + 2050); down on the TV at + 2000 + 70 + 230
        assert_eq!(seg["sync"]["end_on_tv_ms"]["p50"].as_f64(), Some(250.0));
        assert_eq!(seg["sync"]["start_breakdown_ms"]["tv"]["p50"].as_f64(), Some(70.0));
        // ten OCR blinks of 200 ms (the text still there), none hidden at 70? (hidden would be a new line: none)
        let ca = r["advice"].as_array().unwrap().iter().find(|a| a["setting"] == "clear_after_ms").unwrap().clone();
        assert_eq!(ca["evidence"]["blinks"], 10, "{ca}");
        assert_eq!(ca["evidence"]["blinks_text_still_there"], 10);
        // a blink of 200 ms (2 frames) flickers below 200: the advice covers it, no more
        let v = ca["value"].as_f64().unwrap();
        assert!((200.0..=250.0).contains(&v), "{ca}");
        assert_ne!(ca["confidence"], "insufficient");
        // the other four have an answer each (a value, or "not enough data" said)
        for k in ["capture_fps", "preview_fps", "interval_ms", "stable_ms"] {
            let a = r["advice"].as_array().unwrap().iter().find(|a| a["setting"] == k).unwrap();
            assert!(a["value"].is_number() || a["confidence"] == "insufficient", "{a}");
        }
        let text = render_ar(&r);
        assert!(text.contains("Clear after") && text.contains("الثقة"), "{text}");
        let csv = review_csv(&ev, &Options::default());
        assert_eq!(csv.lines().count(), 41, "a line each, and the header");
    }

    #[test]
    fn fork_advise_says_when_there_is_not_enough_data() {
        let ev = session(3, 0, 0, 70);
        let r = analyze(&ev, &Options::default());
        let ca = r["advice"].as_array().unwrap().iter().find(|a| a["setting"] == "clear_after_ms").unwrap();
        assert_eq!(ca["confidence"], "insufficient");
        assert!(ca["value"].is_null());
        let st = r["advice"].as_array().unwrap().iter().find(|a| a["setting"] == "stable_ms").unwrap();
        assert_eq!(st["confidence"], "insufficient");
    }

    fn rec<'a>(r: &'a Value, setting: &str) -> &'a Value {
        r["advice"].as_array().unwrap().iter().find(|a| a["setting"] == setting).unwrap()
    }
    fn row<'a>(a: &'a Value, key: &str, v: f64) -> &'a Value {
        a["evidence"]["table"].as_array().unwrap().iter().find(|r| r[key].as_f64() == Some(v)).unwrap_or_else(|| panic!("no {key} {v}: {a}"))
    }

    #[test]
    fn fork_advise_clear_after_works_in_steps_of_the_reading_passes() {
        // ten 1-frame blinks (a pass apart: 100 ms on a 10-a-second capture) in 40 lines, Clear after 70
        let r = analyze(&session(40, 4, 1, 70), &Options::default());
        let ca = rec(&r, "clear_after_ms");
        assert_eq!(ca["evidence"]["pass_ms"].as_f64(), Some(100.0), "{ca}");
        // anything under 100 hides the line at the blink's pass: 50 and 70 alike
        assert_eq!(row(ca, "clear_after_ms", 50.0)["flickers"], 10.0);
        assert_eq!(row(ca, "clear_after_ms", 70.0)["flickers"], 10.0);
        // 110 to 190 keeps it up, and hides it a whole pass (100 ms) later after its English, not 60 or 120
        let lag = |v: f64| row(ca, "clear_after_ms", v)["end_lag_p50_ms"].as_f64().unwrap();
        assert_eq!(row(ca, "clear_after_ms", 130.0)["flickers"], 0.0);
        assert_eq!(lag(130.0) - lag(70.0), 100.0);
        assert_eq!(lag(190.0) - lag(70.0), 100.0);
        // 300 is three passes: 300 ms later than 70 (not the 230 of a model without the passes)
        assert_eq!(lag(300.0) - lag(70.0), 300.0);
        // advised: the middle of the best step (10 flickers cost more than 100 ms over 40 lines)
        assert_eq!(ca["value"].as_f64(), Some(150.0), "{ca}");

        // the same blinks over 120 lines: 100 ms a line costs more than 5 flickers, the step in use is best;
        // 70 is clear of the step's edges: kept
        let r = analyze(&session(120, 24, 1, 70), &Options::default());
        assert_eq!(rec(&r, "clear_after_ms")["value"].as_f64(), Some(70.0), "{}", rec(&r, "clear_after_ms"));
        // 100 is right on an edge (the pass at 100 ± a few ms decides): the step's middle instead
        let r = analyze(&session(120, 24, 1, 100), &Options::default());
        assert_eq!(rec(&r, "clear_after_ms")["value"].as_f64(), Some(50.0), "{}", rec(&r, "clear_after_ms"));
    }

    #[test]
    fn fork_advise_stability_works_in_steps_of_the_reading_passes() {
        // 10 a second (100 ms a pass); new lines the memory doesn't know staying 2 passes, and misreadings
        // staying 2 passes before the line the memory knows
        let mut ev = session(30, 0, 0, 70);
        let base = 1_800_000_000_000u64 + 200_000;
        for i in 0..6u64 {
            let t0 = base + i * 4000;
            ev.push(json!({ "ev": "seen", "t": t0, "area": "subtitles", "text": format!("Strangers: Brand new line {i}."), "known": false, "sure": true }));
            ev.push(json!({ "ev": "ended", "t": t0 + 200, "area": "subtitles", "seq": 100 + i, "how": "gone" }));
            let t1 = t0 + 2000;
            ev.push(json!({ "ev": "seen", "t": t1, "area": "subtitles", "text": format!("Jasom: Lime numbor {i} of tha test."), "known": false, "sure": true }));
            ev.push(json!({ "ev": "seen", "t": t1 + 200, "area": "subtitles", "text": format!("Jason: Line number {i} of the test."), "known": true, "sure": true }));
        }
        ev.sort_by_key(t);
        let r = analyze(&ev, &Options::default());
        let st = rec(&r, "stable_ms");
        // 0 and 50: sent at the second reading (both stayed for it): 6 new lines on time, 6 misreadings shown
        for v in [0.0, 50.0] {
            assert_eq!((row(st, "stable_ms", v)["premature"].as_u64(), row(st, "stable_ms", v)["lost"].as_u64()), (Some(6), Some(0)), "{st}");
        }
        // 100: still the second reading (100 ms is one pass); 150: the third, which none of them stayed for
        assert_eq!(row(st, "stable_ms", 100.0)["passes_sure"].as_f64(), Some(1.0));
        assert_eq!(row(st, "stable_ms", 150.0)["passes_sure"].as_f64(), Some(2.0));
        assert_eq!((row(st, "stable_ms", 150.0)["premature"].as_u64(), row(st, "stable_ms", 150.0)["lost"].as_u64()), (Some(0), Some(6)), "{st}");
    }
}

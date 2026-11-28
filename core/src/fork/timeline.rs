//! fork: each line's timeline for the measurement log (v2; see FORK.md). From what each area's reading
//! gave on each pass (a hook just before the reading decides) and from the frame's pixels:
//!
//! - when a line's English appeared: between the last frame read without it and the first with it (the
//!   `seen` and `sent` events carry both frames' capture times);
//! - each OCR blink while it was up: frames it wasn't read in although it came back (`gap`), and whether
//!   the pixels still showed text then (an OCR miss) or not (the text really went for a moment);
//! - when it ended (`ended`): gone, replaced by the next line, or cleared by a page — between the last
//!   frame it was read in and the first frame without it;
//! - text on screen the reading accepted nothing of (`unread`: a line lost, with the filters' reasons
//!   meanwhile), found by the pixels (fork::presence) in areas whose lines stack up (the subtitles).
//!
//! It only records: nothing the app reads, translates or shows depends on it, and while the log is off
//! every call returns at once.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::json;

use super::{linelog, presence};
use crate::frame::FrameData;

/// The fewest frames text is on screen unread for it to count as a line lost (fewer: a fade, a flash).
const UNREAD_FRAMES: u32 = 3;

#[derive(Clone, Copy, Default)]
struct Read {
    captured: Option<u64>,
    text: bool,
}

/// Frames the line up wasn't read in (yet).
#[derive(Default)]
struct Gap {
    first: Option<u64>,
    last: Option<u64>,
    frames: u32,
    /// of them, frames whose pixels still showed text
    det: u32,
    /// of them, frames another text was read in
    other: u32,
}

/// Text on screen the reading accepted nothing of, frame after frame.
#[derive(Default)]
struct Unread {
    first: Option<u64>,
    last: Option<u64>,
    frames: u32,
    score: u32,
    reasons: HashMap<String, u32>,
}

#[derive(Clone, Copy)]
struct Outcome {
    captured: Option<u64>,
    text: bool,
    same: bool,
    current: u64,
}

#[derive(Default)]
struct Track {
    rect: [f64; 4],
    centred: bool,
    stacks: bool,
    prev: Option<Read>,
    cur: Option<Read>,
    /// the reading now becoming a line: the frame before it (and whether it had text) and its first frame
    appear: Option<(Option<u64>, Option<u64>, bool)>,
    /// the line up (its seq; 0: none), the last frame it was read in, its blink going on
    unit: u64,
    last_present: Option<u64>,
    gap: Option<Gap>,
    unread: Option<Unread>,
    /// this pass's reading, and the filters' reasons met in it
    pass: Option<Outcome>,
    reasons: HashMap<String, u32>,
}

static TRACKS: Mutex<Option<HashMap<String, Track>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut HashMap<String, Track>) -> R) -> R {
    let mut g = TRACKS.lock().unwrap();
    f(g.get_or_insert_with(HashMap::new))
}

/// Forget every area's timeline (the log started again).
pub(crate) fn reset() {
    *TRACKS.lock().unwrap() = None;
}

/// What an area's reading gave this pass, just before the reading decides: `rect` the area (x, y, w, h
/// fractions of the frame; its own, not grown), `centred` its text must be in the middle (subtitles),
/// `stacks` its lines stack up (the unread-text finder looks there), `text` something was read, `same` it
/// is the line up (`current_seq`, 0: none), `frame` the frame's number.
#[allow(clippy::too_many_arguments)]
pub fn area_read(area: &str, rect: [f64; 4], centred: bool, stacks: bool, text: bool, same: bool, current_seq: u64, frame: Option<u64>) {
    if !linelog::on() {
        return;
    }
    let t0 = Instant::now();
    let captured = frame.and_then(linelog::captured_of);
    with(|m| {
        let t = m.entry(area.to_string()).or_default();
        t.rect = rect;
        t.centred = centred;
        t.stacks = stacks;
        t.prev = t.cur;
        t.cur = Some(Read { captured, text });
        t.pass = Some(Outcome { captured, text, same, current: current_seq });
    });
    linelog::note_area_done(area);
    linelog::add_self(t0.elapsed());
}

/// A new reading of `area` (the `seen` event): the frame read before it — its capture time, and whether
/// it had text (a line changing) or not (a line appearing). Kept for the line it may become (`appear`).
pub fn on_seen(area: &str) -> (Option<u64>, bool) {
    if !linelog::on() {
        return (None, false);
    }
    with(|m| {
        let t = m.entry(area.to_string()).or_default();
        let prev = t.prev.unwrap_or_default();
        t.appear = Some((prev.captured, t.cur.and_then(|c| c.captured), prev.text));
        (prev.captured, prev.text)
    })
}

/// The line now sent in `area` appeared between these frames' capture times (the frame before its
/// reading's first, and that first): for the `sent` event.
pub fn appear(area: &str) -> (Option<u64>, Option<u64>) {
    if !linelog::on() {
        return (None, None);
    }
    with(|m| m.get(area).and_then(|t| t.appear).map(|(a, b, _)| (a, b)).unwrap_or((None, None)))
}

/// A filter left text out in `area` this pass (counted for a line lost: why it wasn't read).
pub(crate) fn note_reason(area: &str, reason: &str) {
    with(|m| *m.entry(area.to_string()).or_default().reasons.entry(reason.to_string()).or_default() += 1);
}

/// Everything up was cleared (`reason`: a page took over, another language): each line up ended.
pub fn cleared(reason: &str) {
    if !linelog::on() {
        return;
    }
    let mut out = Vec::new();
    with(|m| {
        for (area, t) in m.iter_mut() {
            if t.unit != 0 {
                out.push(ended_event(area, t, reason, None));
                t.unit = 0;
                t.last_present = None;
                t.gap = None;
            }
        }
    });
    for e in out {
        linelog::emit("ended", e);
    }
}

fn ended_event(area: &str, t: &Track, how: &str, by: Option<u64>) -> serde_json::Value {
    let g = t.gap.as_ref();
    json!({
        "area": area, "seq": t.unit, "how": how, "by": by,
        // (the English went between these frames)
        "last_present": t.last_present, "first_absent": g.and_then(|g| g.first),
        "absent_frames": g.map(|g| g.frames), "absent_text_px": g.map(|g| g.det), "absent_other": g.map(|g| g.other),
    })
}

/// The pass is over (`frame`: the frame it read): each area's reading this pass makes its line's timeline,
/// with the unread-text finder's look at the frame.
pub fn pass_end(frame: Option<&FrameData>) {
    if !linelog::on() {
        return;
    }
    let t0 = Instant::now();
    linelog::note_pass_frame(frame.map(|f| f.seq));
    let mut events: Vec<(&'static str, serde_json::Value)> = Vec::new();
    let mut detect = std::time::Duration::ZERO;
    let mut counts = [0u32; 4];
    with(|m| {
        for (area, t) in m.iter_mut() {
            let Some(o) = t.pass.take() else {
                t.reasons.clear();
                continue;
            };
            // the pixels (the subtitles: lines that stack up)
            let det = match (frame, t.stacks) {
                (Some(f), true) => {
                    let d0 = Instant::now();
                    let p = presence::text_in(f, t.rect, t.centred);
                    detect += d0.elapsed();
                    Some(p)
                }
                _ => None,
            };
            if let Some(p) = det {
                counts[match (p.present, o.text) {
                    (true, true) => 0,
                    (true, false) => 1,
                    (false, true) => 2,
                    (false, false) => 3,
                }] += 1;
            }
            // the line up changed since the last pass: the one before ended (gone, or replaced)
            if o.current != t.unit {
                if t.unit != 0 {
                    let how = if o.current == 0 { "gone" } else { "replaced" };
                    events.push(("ended", ended_event(area, t, how, (o.current != 0).then_some(o.current))));
                }
                t.unit = o.current;
                t.last_present = None;
                t.gap = None;
            }
            if t.unit != 0 {
                if o.same {
                    // read again: a blink is over (it came back)
                    if let Some(g) = t.gap.take() {
                        events.push((
                            "gap",
                            json!({
                                "area": area, "seq": t.unit, "frames": g.frames, "last_present": t.last_present,
                                "first_absent": g.first, "last_absent": g.last, "back": o.captured,
                                "text_px": g.det, "other": g.other,
                            }),
                        ));
                    }
                    t.last_present = o.captured.or(t.last_present);
                } else {
                    let g = t.gap.get_or_insert_with(Gap::default);
                    if g.first.is_none() {
                        g.first = o.captured;
                    }
                    g.last = o.captured;
                    g.frames += 1;
                    g.det += u32::from(det.is_some_and(|p| p.present));
                    g.other += u32::from(o.text);
                }
            }
            // text on screen, nothing accepted, no line up: a line being lost (or a sign, a menu)
            let unread_now = o.current == 0 && !o.text && det.is_some_and(|p| p.present);
            if unread_now {
                let u = t.unread.get_or_insert_with(Unread::default);
                if u.first.is_none() {
                    u.first = o.captured;
                }
                u.last = o.captured;
                u.frames += 1;
                u.score = u.score.max(det.map(|p| p.score).unwrap_or(0));
                for (r, n) in t.reasons.drain() {
                    *u.reasons.entry(r).or_default() += n;
                }
            } else if let Some(u) = t.unread.take() {
                if u.frames >= UNREAD_FRAMES {
                    events.push((
                        "unread",
                        json!({ "area": area, "from": u.first, "to": u.last, "frames": u.frames, "score": u.score, "reasons": u.reasons, "then_read": o.text }),
                    ));
                }
            }
            t.reasons.clear();
        }
    });
    linelog::note_presence(counts, detect);
    for (ev, e) in events {
        linelog::emit(ev, e);
    }
    linelog::add_self(t0.elapsed());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn collect(f: impl FnOnce()) -> Vec<serde_json::Value> {
        let _g = linelog::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let c = Arc::new(Mutex::new(Vec::new()));
        linelog::set_collector(Some(c.clone()));
        reset();
        f();
        linelog::set_collector(None);
        let v = c.lock().unwrap().clone();
        v
    }

    /// A pass on frame `seq` (captured at `seq` × 100 ms) where the area read `text` (`same`: the line up, `cur`).
    fn pass(f: Option<&FrameData>, seq: u64, text: bool, same: bool, cur: u64) {
        linelog::note_frame(seq, seq * 100);
        area_read("subtitles", [0.10, 0.80, 0.80, 0.118], true, true, text, same, cur, Some(seq));
        pass_end(f);
    }

    #[test]
    fn fork_timeline_tells_a_lines_blinks_and_its_end() {
        let v = collect(|| {
            // nothing, then a line read (it becomes line 7), up for a while with an OCR blink of 2 frames, gone
            pass(None, 1, false, false, 0);
            pass(None, 2, true, false, 0);
            let (before, had) = on_seen("subtitles");
            assert_eq!((before, had), (Some(100), false), "the frame before it, without text");
            assert_eq!(appear("subtitles"), (Some(100), Some(200)));
            pass(None, 3, true, true, 7);
            pass(None, 4, false, false, 7);
            pass(None, 5, false, false, 7);
            pass(None, 6, true, true, 7);
            pass(None, 7, false, false, 7);
            pass(None, 8, false, false, 0);
        });
        let gap = v.iter().find(|e| e["ev"] == "gap").expect("a blink");
        assert_eq!((gap["frames"].as_u64(), gap["last_present"].as_u64(), gap["first_absent"].as_u64(), gap["back"].as_u64()), (Some(2), Some(300), Some(400), Some(600)));
        let end = v.iter().find(|e| e["ev"] == "ended").expect("its end");
        assert_eq!((end["seq"].as_u64(), end["how"].as_str()), (Some(7), Some("gone")));
        assert_eq!((end["last_present"].as_u64(), end["first_absent"].as_u64()), (Some(600), Some(700)), "between the last frame with it and the first without");
    }

    #[test]
    fn fork_timeline_tells_a_line_replaced_and_lines_cleared() {
        let v = collect(|| {
            pass(None, 1, true, true, 3);
            // another text read (the next line), sent: line 4 up
            pass(None, 2, true, false, 3);
            pass(None, 3, true, true, 4);
            cleared("page");
        });
        let ends: Vec<_> = v.iter().filter(|e| e["ev"] == "ended").collect();
        assert_eq!(ends.len(), 2, "{v:?}");
        assert_eq!((ends[0]["seq"].as_u64(), ends[0]["how"].as_str(), ends[0]["by"].as_u64()), (Some(3), Some("replaced"), Some(4)));
        assert_eq!((ends[0]["last_present"].as_u64(), ends[0]["first_absent"].as_u64()), (Some(100), Some(200)));
        assert_eq!((ends[1]["seq"].as_u64(), ends[1]["how"].as_str()), (Some(4), Some("page")));
    }

    #[test]
    fn fork_timeline_finds_text_on_screen_nothing_was_read_of() {
        use super::presence::tests::{scene, write};
        let mut with_line = scene(1790, 1006, 3);
        write(&mut with_line, "JASON: TRY ME TOMORROW", (0.896 * 1006.0) as u32, 3, [240, 240, 240], true);
        let empty = scene(1790, 1006, 3);
        let v = collect(|| {
            // four frames with a subtitle on screen, the reading accepting none of it (a filter's reason once)
            for seq in 1..=4 {
                linelog::note_frame(seq, seq * 100);
                area_read("subtitles", [0.10, 0.80, 0.80, 0.118], true, true, false, false, 0, Some(seq));
                if seq == 2 {
                    note_reason("subtitles", "no_bottom_line");
                }
                pass_end(Some(&with_line));
            }
            pass(Some(&empty), 5, false, false, 0);
        });
        let u = v.iter().find(|e| e["ev"] == "unread").expect("text unread");
        assert_eq!((u["frames"].as_u64(), u["from"].as_u64(), u["to"].as_u64()), (Some(4), Some(100), Some(400)));
        assert_eq!(u["reasons"]["no_bottom_line"], 1);
        // and an OCR blink over the line still there: told apart from the line going
        let v = collect(|| {
            linelog::note_frame(1, 100);
            area_read("subtitles", [0.10, 0.80, 0.80, 0.118], true, true, true, true, 9, Some(1));
            pass_end(Some(&with_line));
            linelog::note_frame(2, 200);
            area_read("subtitles", [0.10, 0.80, 0.80, 0.118], true, true, false, false, 9, Some(2));
            pass_end(Some(&with_line));
            linelog::note_frame(3, 300);
            area_read("subtitles", [0.10, 0.80, 0.80, 0.118], true, true, true, true, 9, Some(3));
            pass_end(Some(&with_line));
        });
        let g = v.iter().find(|e| e["ev"] == "gap").expect("a blink");
        assert_eq!(g["text_px"], 1, "the text was still there: an OCR miss");
    }
}

//! fork: is there text in an area of the frame, from its pixels only (no OCR)? For the measurement log
//! (see FORK.md): a line that's on screen but the OCR read nothing usable (a dialogue line lost), and an
//! OCR blink (the text still there, not read on one frame) told from the text really going.
//!
//! GTA VI's subtitles are white (the speaker's name in a colour) on a dark see-through box, centred. What
//! is counted: bright pixels with a dark one a few pixels to their side — the sides of letters' strokes —
//! row by row; text is a band of such rows (a line's height), centred in the area, neither the whole area
//! high (a fence, a tree) nor its whole width. Sampled every few pixels: a fraction of a millisecond.
//!
//! It only looks: nothing the app reads or shows depends on it (the log calls it while it's on).

use crate::frame::FrameData;

/// Every how many pixels a row and a column are looked at.
const STEP: u32 = 2;
/// How far to the side the dark pixel is looked for (a stroke's side).
const SIDE: u32 = 3;

/// What the pixels of an area say.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Presence {
    /// text there (a band of stroke sides)
    pub present: bool,
    /// the stroke sides counted in the best band (0: none)
    pub score: u32,
    /// that band's top and bottom, as fractions of the area's height
    pub band: Option<(f64, f64)>,
}

/// Is a pixel part of a letter: white, or a bright colour (a speaker's name)?
#[inline]
fn bright(r: u8, g: u8, b: u8) -> bool {
    let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
    lo >= 175 || (hi >= 190 && hi - lo >= 60)
}

/// The brightness of a pixel (0–255).
#[inline]
fn luma(r: u8, g: u8, b: u8) -> u32 {
    (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000
}

/// Text in the area `rect` (x, y, w, h fractions of the frame)? `centred`: it must be in the area's middle
/// (subtitles).
pub fn text_in(frame: &FrameData, rect: [f64; 4], centred: bool) -> Presence {
    let (fw, fh) = (frame.w, frame.h);
    if fw == 0 || fh == 0 || frame.rgba.len() < (fw as usize) * (fh as usize) * 4 {
        return Presence::default();
    }
    let x0 = ((rect[0].clamp(0.0, 1.0)) * f64::from(fw)) as u32;
    let y0 = ((rect[1].clamp(0.0, 1.0)) * f64::from(fh)) as u32;
    let x1 = (((rect[0] + rect[2]).clamp(0.0, 1.0)) * f64::from(fw)) as u32;
    let y1 = (((rect[1] + rect[3]).clamp(0.0, 1.0)) * f64::from(fh)) as u32;
    if x1 <= x0 + 4 * SIDE || y1 <= y0 + 2 * STEP {
        return Presence::default();
    }
    let px = |x: u32, y: u32| {
        let i = ((y as usize) * (fw as usize) + x as usize) * 4;
        (frame.rgba[i], frame.rgba[i + 1], frame.rgba[i + 2])
    };
    // each sampled row: its stroke sides, and where they start and end
    let rows = ((y1 - y0) / STEP) as usize;
    let mut count = vec![0u32; rows];
    let mut span = vec![(u32::MAX, 0u32); rows];
    for (ri, y) in (y0..y1).step_by(STEP as usize).enumerate().take(rows) {
        for x in (x0 + SIDE..x1 - SIDE).step_by(STEP as usize) {
            let (r, g, b) = px(x, y);
            if !bright(r, g, b) {
                continue;
            }
            let l = luma(r, g, b);
            let (a, c) = (px(x - SIDE, y), px(x + SIDE, y));
            let dark = |p: (u8, u8, u8)| luma(p.0, p.1, p.2) + 80 <= l && luma(p.0, p.1, p.2) <= 110;
            if dark(a) || dark(c) {
                count[ri] += 1;
                span[ri] = (span[ri].0.min(x), span[ri].1.max(x));
            }
        }
    }
    let width = x1 - x0;
    let samples = (width / STEP).max(1);
    // a row counts with this many sides: a few letters' worth
    let min_row = (samples / 160).max(4);
    // bands: runs of rows that count (a row's gap allowed: thin letters miss a sampled row)
    let mut best = Presence::default();
    let mut i = 0;
    while i < rows {
        if count[i] < min_row {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        let mut j = i + 1;
        while j < rows && (count[j] >= min_row || (j + 1 < rows && count[j + 1] >= min_row)) {
            if count[j] >= min_row {
                end = j;
            }
            j += 1;
        }
        i = j;
        let n_rows = end - start + 1;
        // (a line's height at least: 3 sampled rows, 6 px)
        if n_rows < 3 {
            continue;
        }
        // (not the whole area high: a fence, a tree, a lit wall)
        if n_rows * 10 > rows * 7 {
            continue;
        }
        let (sx, ex) = (start..=end).fold((u32::MAX, 0u32), |(a, b), k| if count[k] > 0 { (a.min(span[k].0), b.max(span[k].1)) } else { (a, b) });
        if ex <= sx {
            continue;
        }
        let band_w = ex - sx;
        // (a few letters wide, not the area's whole width)
        if band_w * 100 < width * 3 || band_w * 100 > width * 96 {
            continue;
        }
        if centred {
            let mid = f64::from(sx + ex) / 2.0;
            let rel = (mid - f64::from(x0)) / f64::from(width);
            if !(0.3..=0.7).contains(&rel) {
                continue;
            }
        }
        let score: u32 = count[start..=end].iter().sum();
        if score > best.score {
            best = Presence { present: true, score, band: Some((start as f64 / rows as f64, (end + 1) as f64 / rows as f64)) };
        }
    }
    best
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 5×7 font of the letters the tests write (each row: 5 bits, the left one first).
    fn glyph(c: char) -> [u8; 7] {
        match c {
            'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
            'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
            'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
            'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
            'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
            'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
            'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
            'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
            'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
            'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
            'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
            'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
            'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
            'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
            'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
            ':' => [0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000],
            _ => [0; 7],
        }
    }

    /// A frame of `w`×`h` filled by `bg(x, y)`.
    pub(crate) fn frame(w: u32, h: u32, bg: impl Fn(u32, u32) -> [u8; 3]) -> FrameData {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let c = bg(x, y);
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&c);
                rgba[i + 3] = 255;
            }
        }
        FrameData { w, h, rgba, seq: 1 }
    }

    /// Write `text` centred at height `cy` (letters `scale` px a dot), in `color`, over a dark see-through
    /// box (as GTA VI's subtitles), or without the box.
    pub(crate) fn write(f: &mut FrameData, text: &str, cy: u32, scale: u32, color: [u8; 3], with_box: bool) {
        let (cw, ch) = (6 * scale, 7 * scale);
        let tw = cw * text.chars().count() as u32;
        let x0 = (f.w - tw) / 2;
        let y0 = cy - ch / 2;
        let w = f.w;
        let mut put = |x: u32, y: u32, c: [u8; 3], alpha: f64| {
            if x < f.w && y < f.h {
                let i = ((y * w + x) * 4) as usize;
                for k in 0..3 {
                    f.rgba[i + k] = (f64::from(f.rgba[i + k]) * (1.0 - alpha) + f64::from(c[k]) * alpha) as u8;
                }
            }
        };
        if with_box {
            for y in y0.saturating_sub(2 * scale)..y0 + ch + 2 * scale {
                for x in x0.saturating_sub(3 * scale)..x0 + tw + 3 * scale {
                    put(x, y, [25, 30, 45], 0.72);
                }
            }
        }
        for (n, c) in text.chars().enumerate() {
            let g = glyph(c);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..5u32 {
                    if bits & (0b10000 >> col) != 0 {
                        for dy in 0..scale {
                            for dx in 0..scale {
                                put(x0 + n as u32 * cw + col * scale + dx, y0 + row as u32 * scale + dy, color, 1.0);
                            }
                        }
                    }
                }
            }
        }
    }

    /// A busy scene: a sky gradient, a wall of noise, a car's white stripe, a dark fence.
    pub(crate) fn scene(w: u32, h: u32, seed: u32) -> FrameData {
        frame(w, h, |x, y| {
            let n = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663) ^ seed.wrapping_mul(83_492_791)) % 61;
            let base = if y < h / 3 { 150 + (y * 90 / h) as u8 } else { 70 + n as u8 };
            let mut c = [base, base.saturating_add(10), base.saturating_add(25)];
            // a long white stripe (a road's line)
            if (y / 3) % 40 == 30 {
                c = [235, 235, 235];
            }
            c
        })
    }

    const AREA: [f64; 4] = [0.10, 0.80, 0.80, 0.118];

    #[test]
    fn fork_presence_finds_a_subtitle_and_nothing_where_there_is_none() {
        for seed in 0..4 {
            let mut f = scene(1790, 1006, seed);
            assert!(!text_in(&f, AREA, true).present, "a busy scene alone (seed {seed}): {:?}", text_in(&f, AREA, true));
            write(&mut f, "JASON: TRY ME TOMORROW", (0.896 * 1006.0) as u32, 3, [240, 240, 240], true);
            let p = text_in(&f, AREA, true);
            assert!(p.present, "a white line over its box (seed {seed}): {p:?}");
        }
        // a dark scene, a line without its box, in a speaker's colour
        let mut f = frame(1790, 1006, |_, _| [20, 22, 30]);
        assert!(!text_in(&f, AREA, true).present);
        write(&mut f, "LOCALS: NO", (0.9 * 1006.0) as u32, 3, [200, 230, 80], false);
        assert!(text_in(&f, AREA, true).present, "{:?}", text_in(&f, AREA, true));
    }

    #[test]
    fn fork_presence_is_not_fooled_by_a_fence_or_a_bright_wall() {
        // a fence: thin white bars on dark, the whole area high
        let f = frame(1790, 1006, |x, _| if x % 9 < 2 { [230, 230, 230] } else { [30, 30, 30] });
        assert!(!text_in(&f, AREA, true).present, "{:?}", text_in(&f, AREA, true));
        // a bright wall: white everywhere, no stroke sides
        let f = frame(1790, 1006, |_, _| [235, 235, 230]);
        assert!(!text_in(&f, AREA, true).present);
        // a sign at the side (not centred): not a subtitle
        let mut f = frame(1790, 1006, |_, _| [20, 22, 30]);
        let mut side = frame(1790, 1006, |_, _| [20, 22, 30]);
        write(&mut side, "STORE", (0.9 * 1006.0) as u32, 3, [240, 240, 240], true);
        // (moved to the left edge of the area)
        let shift = 640usize;
        for y in 0..1006usize {
            for x in 0..(1790 - shift) {
                let (i, j) = ((y * 1790 + x) * 4, (y * 1790 + x + shift) * 4);
                f.rgba[i..i + 4].copy_from_slice(&side.rgba[j..j + 4].to_vec());
            }
        }
        assert!(!text_in(&f, AREA, true).present, "{:?}", text_in(&f, AREA, true));
        assert!(text_in(&f, AREA, false).present, "anywhere in an area that isn't centred");
    }

    #[test]
    fn fork_presence_sees_text_fade_out() {
        // the line dimming (a fade): seen while it's still clear, not once it's faint
        let at = |level: u8| {
            let mut f = scene(1790, 1006, 7);
            write(&mut f, "TOURISTS: MOVE", (0.896 * 1006.0) as u32, 3, [level, level, level], true);
            text_in(&f, AREA, true).present
        };
        assert!(at(240) && at(200));
        assert!(!at(120), "a faint line is no line");
    }
}

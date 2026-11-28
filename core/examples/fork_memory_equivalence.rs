//! fork: the memory-equivalence tool (Phase 1.6 of the fork's plan; see FORK.md).
//!
//! The same inputs — a copy of the translation-db folder and a frozen download (the kept pack) — loaded
//! the way the app loads them (the memory, then the download over it), and every lookup answered: what
//! `get` gives (the Arabic and its source) and `line_of`, for each line the memory and the pack have and
//! for misreadings of a sample of them; and what the memory file keeps afterwards (a download takes the
//! lines it covers out of it). Run with the original app's core and with this copy's: the two answers
//! must be the same, line for line. The translations are written as fingerprints only (SHA-256): the
//! dump holds no translation, the encrypted memory and downloads stay unreadable.
//!
//! Only what the original core had at the fork's base (.fork/base-commit) is used, so this same file
//! builds in both:
//!
//! ```text
//! git worktree add ../vit-base "$(cat .fork/base-commit)"
//! cp core/examples/fork_memory_equivalence.rs ../vit-base/core/examples/
//! (cd ../vit-base && cargo run --release -p vi-translator-core --example fork_memory_equivalence -- dump --db <db copy> --data <data copy> --out base.jsonl)
//! cargo run --release -p vi-translator-core --example fork_memory_equivalence -- dump --db <db copy> --data <data copy> --out fork.jsonl
//! cargo run --release -p vi-translator-core --example fork_memory_equivalence -- compare base.jsonl fork.jsonl
//! ```
//!
//! - `dump --db <folder> [--data <folder> | --pack-jsonl <file>] [--game <id>] [--lang ar] [--variants 2000] --out <file>`
//!   `--db`: the translation-db folder (Documents\VI Translator\translation-db); `--data`: the app's data
//!   folder (%APPDATA%\com.vitranslator.app), for the download kept there (sync-state.json and the pack);
//!   `--pack-jsonl`: a download as plain entries instead (the synthetic test). The folders are only read:
//!   the memory is loaded from a temporary copy (loading may convert an old file; a download rewrites the
//!   memory file).
//! - `compare <a.jsonl> <b.jsonl> [--label <name>] [--summary-to <file>]`: the differences, by kind:
//!   - the lookups of the lines the memory and the download have (exact: the key itself), the counts
//!     (the head) and what the memory file keeps: these say whether the memory is the same;
//!   - the lookups of misreadings (approximate: `near`), apart, by how they differ. The original core's
//!     approximate lookup is not the same from one run to the next: it compares at most 600 candidates
//!     (memory.rs NEAR_TRIES), in the order of a HashMap (Rust's: seeded at random in each process), and
//!     among equally close lines the first one compared wins. So two runs of the same core can answer a
//!     misreading differently — the original's against itself too (`memory-check.cmd` measures it).
//!   Exit 0: all the same; 1: only misreadings differ; 2: exact lookups, counts or the file differ;
//!   3: a file could not be read.
//! - `sample <folder>`: synthetic inputs (a memory of every source, reviewed lines, a download that
//!   replaces some), for the tool's own test: `<folder>/db` and `<folder>/pack.jsonl`.
//! - `sample-ties <folder>`: synthetic inputs where the approximate lookup has ties and more than 600
//!   candidates (as some misreadings in a player's memory do): `<folder>/db`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use vi_translator_core::memory::{self, Entry, Memory};
use vi_translator_core::{secure, sync};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let code = match args.first().map(String::as_str) {
        Some("dump") => {
            let (Some(db), Some(out)) = (opt("--db"), opt("--out")) else { usage() };
            let lang = opt("--lang").unwrap_or_else(|| "ar".into());
            let variants = opt("--variants").and_then(|v| v.parse().ok()).unwrap_or(2000);
            match dump(Path::new(&db), opt("--data").map(PathBuf::from), opt("--pack-jsonl").map(PathBuf::from), opt("--game"), &lang, variants, Path::new(&out)) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            }
        }
        Some("compare") => {
            let (Some(a), Some(b)) = (args.get(1), args.get(2)) else { usage() };
            let label = opt("--label").unwrap_or_else(|| format!("{a} / {b}"));
            compare(Path::new(a), Path::new(b), &label, opt("--summary-to").map(PathBuf::from))
        }
        Some("sample-ties") => {
            let Some(dir) = args.get(1) else { usage() };
            match sample_ties(Path::new(dir)) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            }
        }
        Some("sample") => {
            let Some(dir) = args.get(1) else { usage() };
            match sample(Path::new(dir)) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            }
        }
        _ => usage(),
    };
    std::process::exit(code);
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  dump --db <folder> [--data <folder> | --pack-jsonl <file>] [--game <id>] [--lang ar] [--variants 2000] --out <file>\n  compare <a.jsonl> <b.jsonl> [--label <name>] [--summary-to <file>]\n  sample <folder>\n  sample-ties <folder>"
    );
    std::process::exit(2);
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let (p, q) = (e.path(), to.join(e.file_name()));
        if p.is_dir() {
            copy_tree(&p, &q)?;
        } else {
            std::fs::copy(&p, &q)?;
        }
    }
    Ok(())
}

/// The entries of a memory file (sealed lines), as kept.
fn file_entries(path: &Path, lang: &str) -> Vec<Entry> {
    let kind = format!("memory/{lang}");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| secure::open(&kind, l).and_then(|j| serde_json::from_str::<Entry>(&j).ok()))
        .collect()
}

/// Every English key the folder's memory has: its file, the reviewed lines (Arabic), the old plain files.
fn memory_keys(db: &Path, lang: &str) -> BTreeSet<String> {
    let mut keys: BTreeSet<String> = file_entries(&db.join(format!("memory-{lang}.dat")), lang).into_iter().map(|e| e.en).collect();
    let plain = |file: PathBuf, raw: bool, keys: &mut BTreeSet<String>| {
        for l in std::fs::read_to_string(file).unwrap_or_default().lines() {
            if let Ok(e) = serde_json::from_str::<Entry>(l) {
                keys.insert(if raw { memory::key_of(&e.en) } else { e.en });
            }
        }
    };
    let legacy = if lang == "ar" { db.join("memory.jsonl") } else { db.join(format!("memory-{lang}.jsonl")) };
    plain(legacy, false, &mut keys);
    if lang == "ar" {
        plain(db.join("reviewed.jsonl"), true, &mut keys);
    }
    keys.retain(|k| !k.is_empty());
    keys
}

/// Misreadings of a line, as the reading makes them (always the same ones for the same line): capitals
/// and punctuation, one letter misread, two words run together, the last letter lost.
fn variants(key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let words: Vec<&str> = key.split(' ').collect();
    let mut first = key.chars();
    if let Some(c) = first.next() {
        out.push(format!("{}{}.", c.to_uppercase(), first.as_str()));
    }
    // the longest plain word, one letter in it misread
    if let Some((i, w)) = words.iter().enumerate().filter(|(_, w)| !w.starts_with('{') && w.chars().count() >= 4).max_by_key(|(i, w)| (w.chars().count(), std::cmp::Reverse(*i))) {
        let cs: Vec<char> = w.chars().collect();
        let at = cs.len() / 2;
        let swap = match cs[at] {
            'o' => 'a',
            'a' => 'o',
            'e' => 'c',
            'c' => 'e',
            'l' => 'i',
            'i' => 'l',
            'n' => 'h',
            'h' => 'n',
            _ => 'x',
        };
        let mut misread = cs.clone();
        misread[at] = swap;
        let mut ws: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        ws[i] = misread.into_iter().collect();
        out.push(ws.join(" "));
    }
    if words.len() >= 3 {
        let mid = words.len() / 2;
        out.push(format!("{}{}", words[..mid].join(" "), words[mid..].join(" ")));
    }
    if key.chars().count() >= 8 {
        let mut cs: Vec<char> = key.chars().collect();
        cs.pop();
        out.push(cs.into_iter().collect());
    }
    out
}

/// A translation's fingerprint (the first 16 hex digits of its SHA-256): equal translations, equal
/// fingerprints; the translation itself not written.
fn fingerprint(text: &str) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(text.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn dump(db: &Path, data: Option<PathBuf>, pack_jsonl: Option<PathBuf>, game: Option<String>, lang: &str, n_variants: usize, out: &Path) -> Result<(), String> {
    if !db.is_dir() {
        return Err(format!("no folder {}", db.display()));
    }
    // a copy of the memory: the given folder is only read
    let work = std::env::temp_dir().join(format!("vit-memeq-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    copy_tree(db, &work).map_err(|e| format!("copying {}: {e}", db.display()))?;
    let result = (|| {
        let mut probes = memory_keys(&work, lang);
        let memory_lines = probes.len();
        let mut m = Memory::load(work.clone(), lang);
        let loaded = m.stats();
        // the download, as the app applies it after loading the memory
        let pack: Option<Vec<Entry>> = match (&data, &pack_jsonl) {
            (Some(d), _) => {
                let game = game.clone().unwrap_or_else(|| vi_translator_core::config::Config::load(&d.join("config.json")).game());
                let p = sync::kept_pack_in(d, &game, lang);
                if p.is_none() {
                    eprintln!("note: no download kept in {} for {game}/{lang}: the memory alone", d.display());
                }
                p
            }
            (None, Some(f)) => Some(
                std::fs::read_to_string(f)
                    .map_err(|e| format!("{}: {e}", f.display()))?
                    .lines()
                    .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
                    .collect(),
            ),
            (None, None) => None,
        };
        let pack_lines = pack.as_ref().map_or(0, Vec::len);
        if let Some(entries) = pack {
            probes.extend(entries.iter().map(|e| e.en.clone()).filter(|k| !k.is_empty()));
            m.set_shared(lang, entries);
        }
        // (the lines the memory and the download have: their lookups are of the key itself)
        let keys_of_memory: BTreeSet<String> = probes.clone();
        // misreadings of a sample (the same lines every time: every n-th, in order)
        let keys: Vec<String> = probes.iter().cloned().collect();
        let step = (keys.len() / n_variants.max(1)).max(1);
        let mut misread = 0;
        for k in keys.iter().step_by(step).take(n_variants) {
            for v in variants(k) {
                misread += usize::from(probes.insert(v));
            }
        }
        let icons = memory::slots(9);
        let mut lines = Vec::with_capacity(probes.len() + 2);
        let stats = m.stats();
        lines.push(json!({
            "lang": lang, "memory_lines": memory_lines, "pack_lines": pack_lines, "misreadings": misread, "probes": probes.len(),
            "loaded": { "entries": loaded.entries, "reviewed": loaded.reviewed },
            "after_download": { "entries": stats.entries, "reviewed": stats.reviewed },
        }));
        let mut found = 0usize;
        for q in &probes {
            let got = m.get(q, &icons, &[]).map(|(segs, src)| json!([fingerprint(&memory::slot_text(&segs)), src]));
            found += usize::from(got.is_some());
            let kind = if keys_of_memory.contains(q) { "key" } else { "misreading" };
            lines.push(json!({ "q": q, "kind": kind, "get": got, "line_of": m.line_of(q) }));
        }
        // what the memory file keeps (sorted: its order is the order lines were added)
        let mut kept: Vec<(String, String, String)> = file_entries(&work.join(format!("memory-{lang}.dat")), lang).into_iter().map(|e| (e.en, fingerprint(&e.ar), e.source)).collect();
        kept.sort();
        lines.push(json!({ "file_keeps": kept.len(), "file": kept }));
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(out, text).map_err(|e| format!("{}: {e}", out.display()))?;
        eprintln!(
            "memory: {memory_lines} lines, download: {pack_lines}, misreadings: {misread} → {} lookups ({found} found), the file keeps {} → {}",
            probes.len(),
            kept.len(),
            out.display()
        );
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// How two dumps differ, by kind (see the top of this file).
fn compare(a: &Path, b: &Path, label: &str, summary_to: Option<PathBuf>) -> i32 {
    let read = |p: &Path| -> Result<BTreeMap<String, Value>, String> {
        let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut out = BTreeMap::new();
        for (i, l) in text.lines().enumerate() {
            let v: Value = serde_json::from_str(l).map_err(|e| format!("{} line {}: {e}", p.display(), i + 1))?;
            // a lookup by its question; the head and the file by their place
            let key = v["q"].as_str().map(|q| format!("q:{q}")).unwrap_or_else(|| if v.get("file").is_some() { "file".into() } else { "head".into() });
            out.insert(key, v);
        }
        Ok(out)
    };
    let (x, y) = match (read(a), read(b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: {e}");
            return 3;
        }
    };
    // a lookup's kind: as the dump says, or — a dump from before — a key when either side found the line
    // itself (an exact lookup)
    let kind_of = |q: &str, va: Option<&Value>, vb: Option<&Value>| -> &'static str {
        if let Some(k) = va.or(vb).and_then(|v| v["kind"].as_str()) {
            return if k == "key" { "key" } else { "misreading" };
        }
        let key = memory::key_of(q);
        let exact = |v: Option<&Value>| v.is_some_and(|v| v["line_of"].as_str() == Some(key.as_str()));
        if exact(va) || exact(vb) { "key" } else { "misreading" }
    };
    let (mut keys_total, mut misreadings_total) = (0usize, 0usize);
    for (k, v) in &x {
        if let Some(q) = k.strip_prefix("q:") {
            if kind_of(q, Some(v), y.get(k)) == "key" {
                keys_total += 1;
            } else {
                misreadings_total += 1;
            }
        }
    }
    // the differences, each in its class
    let mut classes: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let all: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
    for k in all {
        let (va, vb) = (x.get(k), y.get(k));
        if va == vb {
            continue;
        }
        let show = || format!("{k}\n      a: {}\n      b: {}", va.map_or("(missing)".into(), |v| v.to_string()), vb.map_or("(missing)".into(), |v| v.to_string()));
        let class = match k.strip_prefix("q:") {
            None if k == "head" => "head (the counts)",
            None => "file (what the memory file keeps)",
            Some(q) if kind_of(q, va, vb) == "key" => "keys (exact lookups)",
            Some(_) => {
                let got = |v: Option<&Value>| v.map(|v| v["get"].clone()).unwrap_or(Value::Null);
                let (ga, gb) = (got(va), got(vb));
                if ga.is_null() || gb.is_null() {
                    "misreading: found by one run only"
                } else if ga == gb {
                    "misreading: another line, the same translation and source"
                } else if ga[0] == gb[0] {
                    "misreading: the same translation, another source"
                } else {
                    "misreading: another translation"
                }
            }
        };
        classes.entry(class).or_default().push(show());
    }
    let count = |prefix: &str| classes.iter().filter(|(c, _)| c.starts_with(prefix)).map(|(_, v)| v.len()).sum::<usize>();
    let (keys, head, file, misreadings) = (count("keys"), count("head"), count("file"), count("misreading"));
    eprintln!("compare: {label}");
    eprintln!("  keys (exact lookups):           {keys} different of {keys_total}");
    eprintln!("  head (the counts):              {}", if head == 0 { "the same" } else { "DIFFERENT" });
    eprintln!("  file (what the memory keeps):   {}", if file == 0 { "the same" } else { "DIFFERENT" });
    eprintln!("  misreadings (approximate):      {misreadings} different of {misreadings_total}");
    for (c, v) in classes.iter().filter(|(c, _)| c.starts_with("misreading")) {
        eprintln!("      {:<56} {}", c.trim_start_matches("misreading: "), v.len());
    }
    // a few of each, the exact ones first
    for (c, v) in &classes {
        eprintln!("  {c}, e.g.:");
        for s in v.iter().take(if c.starts_with("misreading") { 2 } else { 10 }) {
            eprintln!("    {s}");
        }
    }
    let exact_same = keys == 0 && head == 0 && file == 0;
    let line = format!(
        "{label}: keys {keys}/{keys_total} different, head {}, file {}; misreadings {misreadings}/{misreadings_total} different ({})",
        if head == 0 { "same" } else { "DIFFERENT" },
        if file == 0 { "same" } else { "DIFFERENT" },
        classes.iter().filter(|(c, _)| c.starts_with("misreading")).map(|(c, v)| format!("{} {}", c.trim_start_matches("misreading: "), v.len())).collect::<Vec<_>>().join(", ")
    );
    if let Some(f) = summary_to {
        use std::io::Write as _;
        if let Ok(mut out) = std::fs::OpenOptions::new().create(true).append(true).open(&f) {
            let _ = writeln!(out, "{line}");
        }
    }
    if exact_same && misreadings == 0 {
        eprintln!("RESULT: identical");
        0
    } else if exact_same {
        eprintln!("RESULT: keys, sources and file the same; misreadings differ ({misreadings})");
        1
    } else {
        eprintln!("RESULT: DIFFERENT in keys, counts or file");
        2
    }
}

/// Synthetic inputs where the approximate lookup is not the same from one run to the next, in the ways a
/// player's memory showed: two lines as close to a misreading (with another translation, or the same),
/// and a misreading whose candidates are more than the 600 compared (many lines with its one known word).
fn sample_ties(dir: &Path) -> Result<(), String> {
    let db = dir.join("db");
    let _ = std::fs::remove_dir_all(&db);
    std::fs::create_dir_all(&db).map_err(|e| e.to_string())?;
    let mut lines = vec![
        json!({ "en": "where you at bro", "ar": "أين أنت يا أخي", "source": "reviewed" }),
        json!({ "en": "where you at boy", "ar": "أين أنت يا ولد", "source": "reviewed" }),
        json!({ "en": "motor upgrade stage 4", "ar": "ترقية المحرك المرحلة 4", "source": "reviewed" }),
        json!({ "en": "motor upgrade stage 2", "ar": "ترقية المحرك المرحلة 2", "source": "reviewed" }),
        json!({ "en": "hey how ya doin'", "ar": "مرحبًا، كيف حالك", "source": "reviewed" }),
        json!({ "en": "hey how ya doing", "ar": "مرحبًا، كيف حالك", "source": "reviewed" }),
        json!({ "en": "c'mon what now", "ar": "هيا، ماذا الآن", "source": "reviewed" }),
    ];
    // (1500 lines with "now", as long as "c'monwhat now": its candidates, all as good)
    for i in 0..1500 {
        lines.push(json!({ "en": format!("zq{i:04} ab now"), "ar": format!("سطر {i}"), "source": "reviewed" }));
    }
    std::fs::write(db.join("reviewed.jsonl"), lines.iter().map(|v| format!("{v}\n")).collect::<String>()).map_err(|e| e.to_string())?;
    eprintln!("sample-ties: {}", db.display());
    Ok(())
}

/// Synthetic inputs: a memory with lines of every source (one twice, the better kept), reviewed lines (one
/// over a line of the memory), and a download that replaces some lines and is outranked on others.
fn sample(dir: &Path) -> Result<(), String> {
    let db = dir.join("db");
    let _ = std::fs::remove_dir_all(&db);
    std::fs::create_dir_all(&db).map_err(|e| e.to_string())?;
    let seg = |t: &str| vec![(t.to_string(), None::<String>)];
    {
        let mut m = Memory::load(db.clone(), "ar");
        let lines = [
            ("Meet me at the docks.", "قابلني عند الأرصفة.", "local"),
            ("Meet me at the docks.", "قابلني عند الرصيف.", "deepl"),
            ("Get in the car.", "اركب السيارة.", "local"),
            ("Follow the truck.", "اتبع الشاحنة.", "claude"),
            ("Lose the cops.", "تخلّص من الشرطة.", "google"),
            ("You're not going anywhere.", "لن تذهب إلى أي مكان.", "libre"),
            ("Press ⟦1⟧ to open the map.", "اضغط {1} لفتح الخريطة.", "openai"),
            ("Jason, wait for me!", "جيسون، انتظرني!", "azure"),
            ("Is that the boat?", "هل هذا هو القارب؟", "local"),
        ];
        for (en, ar, src) in lines {
            m.put(en, &seg(ar), src);
        }
    }
    let reviewed = [
        json!({ "en": "Get in the car.", "ar": "اصعد إلى السيارة.", "source": "reviewed" }),
        json!({ "en": "Watch out!", "ar": "انتبه!", "source": "reviewed" }),
    ];
    std::fs::write(db.join("reviewed.jsonl"), reviewed.iter().map(|v| format!("{v}\n")).collect::<String>()).map_err(|e| e.to_string())?;
    let pack = [
        // replaces the memory's own line (better), is outranked by a reviewed one, and new lines
        json!({ "en": memory::key_of("Lose the cops."), "ar": "اهرب من الشرطة.", "source": "reviewed" }),
        json!({ "en": memory::key_of("Get in the car."), "ar": "ادخل السيارة.", "source": "claude" }),
        json!({ "en": memory::key_of("Is that the boat?"), "ar": "أهذا هو القارب؟", "source": "phrasebook" }),
        json!({ "en": memory::key_of("Where are you going?"), "ar": "إلى أين أنت ذاهب؟", "source": "reviewed" }),
        json!({ "en": memory::key_of("Hand me the bag."), "ar": "ناولني الحقيبة.", "source": "deepl" }),
    ];
    std::fs::write(dir.join("pack.jsonl"), pack.iter().map(|v| format!("{v}\n")).collect::<String>()).map_err(|e| e.to_string())?;
    eprintln!("sample: {} and {}", db.display(), dir.join("pack.jsonl").display());
    Ok(())
}

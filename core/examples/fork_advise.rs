//! fork: the measurement log's analyzer from the command line (see FORK.md and core/src/fork/advise.rs).
//!
//! fork_advise <a session's log, any part of it, or a folder of them>... [--tz +3] [--out <folder>]
//!             [--flicker-ms 2000] [--lost-ms 5000] [--premature-ms 1000] [--cpu-points 5]
//!
//! Writes advice.json (everything), advice-ar.txt (the report in Arabic) and lines-review.csv (each line:
//! the English as read, the Arabic as shown, its timing and flags) into the folder (default: here).

use std::path::PathBuf;

use vi_translator_core::fork::advise::{self, Options};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut o = Options::default();
    let mut out = PathBuf::from(".");
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let next = || args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--tz" => {
                o.tz_minutes = (next().replace('+', "").parse::<f64>().unwrap_or(0.0) * 60.0) as i32;
                i += 1;
            }
            "--out" => {
                out = PathBuf::from(next());
                i += 1;
            }
            "--flicker-ms" | "--lost-ms" | "--premature-ms" | "--cpu-points" => {
                let v: f64 = next().parse().unwrap_or(0.0);
                match args[i].as_str() {
                    "--flicker-ms" => o.flicker_ms = v,
                    "--lost-ms" => o.lost_ms = v,
                    "--premature-ms" => o.premature_ms = v,
                    _ => o.cpu_points = v,
                }
                i += 1;
            }
            p => paths.push(PathBuf::from(p)),
        }
        i += 1;
    }
    if paths.is_empty() {
        eprintln!("fork_advise <log or folder>... [--tz +3] [--out <folder>]");
        std::process::exit(2);
    }
    match advise::run(&paths, &out, &o) {
        Ok((j, a, c)) => {
            print!("{}", std::fs::read_to_string(&a).unwrap_or_default());
            eprintln!("\n→ {}\n→ {}\n→ {}", a.display(), j.display(), c.display());
        }
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
    }
}

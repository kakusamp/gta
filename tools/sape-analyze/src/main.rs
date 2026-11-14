//! sape-analyze: summarises SAPE frame captures (`sape_frames_*.csv`).
//!
//! ```text
//! sape-analyze [--warmup N] [--settle N] <capture.csv>...
//! ```
//! * Frames are grouped by the `filter_on` column, so a capture recorded with
//!   `state_filter.ab_interval_s` yields an interleaved A/B comparison of the
//!   same scene.
//! * `--warmup N` drops the first N frames of every file (default 120).
//! * `--settle N` drops N frames after every ON/OFF switch (default 30), so a
//!   transition does not pollute either group.
//!
//! Several files can be passed (e.g. 5 runs of a baseline); they are pooled
//! and also reported individually so run-to-run variance is visible.

use sape_core::stats::{summarize, FrameSummary};
use std::collections::BTreeMap;
use std::process::ExitCode;

struct Args {
    warmup: usize,
    settle: usize,
    files: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        warmup: 120,
        settle: 30,
        files: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--warmup" => {
                a.warmup = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--warmup needs a number")?
            }
            "--settle" => {
                a.settle = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--settle needs a number")?
            }
            "-h" | "--help" => return Err(String::new()),
            _ => a.files.push(arg),
        }
    }
    if a.files.is_empty() {
        return Err("no input files".into());
    }
    Ok(a)
}

#[derive(Default)]
struct Group {
    frame_ms: Vec<f32>,
    spikes: u64,
    draws: u64,
    rs_calls: u64,
    rs_redundant: u64,
    rs_skipped: u64,
}

fn load(path: &str, warmup: usize, settle: usize) -> Result<BTreeMap<u8, Group>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines
        .next()
        .ok_or(format!("{path}: empty file"))?
        .split(',')
        .collect();
    let col = |name: &str| header.iter().position(|h| h.trim() == name);
    let c_ms = col("frame_ms").ok_or(format!("{path}: no frame_ms column"))?;
    let (c_on, c_spike, c_draws, c_rs, c_red, c_skip) = (
        col("filter_on"),
        col("spike"),
        col("draws"),
        col("rs_calls"),
        col("rs_redundant"),
        col("rs_skipped"),
    );

    let mut groups: BTreeMap<u8, Group> = BTreeMap::new();
    let mut last_on: Option<u8> = None;
    let mut since_switch = usize::MAX;
    for (i, line) in lines.enumerate() {
        let f: Vec<&str> = line.split(',').collect();
        let num = |c: Option<usize>| {
            c.and_then(|c| f.get(c))
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(0)
        };
        let Some(ms) = f.get(c_ms).and_then(|v| v.trim().parse::<f32>().ok()) else {
            continue;
        };
        let on = num(c_on) as u8;
        if last_on != Some(on) {
            if last_on.is_some() {
                since_switch = 0;
            }
            last_on = Some(on);
        }
        since_switch = since_switch.saturating_add(1);
        if i < warmup || since_switch <= settle {
            continue;
        }
        let g = groups.entry(on).or_default();
        g.frame_ms.push(ms);
        g.spikes += num(c_spike);
        g.draws += num(c_draws);
        g.rs_calls += num(c_rs);
        g.rs_redundant += num(c_red);
        g.rs_skipped += num(c_skip);
    }
    Ok(groups)
}

fn print_row(label: &str, s: &FrameSummary, g: &Group) {
    let n = s.frames.max(1) as f64;
    println!(
        "{label:<14} {:>7} {:>8.2} {:>8.3} {:>8.2} {:>8.2} {:>8.3} {:>8.3} {:>7.3} {:>6} {:>8.0} {:>8.0} {:>7.1}% {:>8.0}",
        s.frames,
        s.avg_fps,
        s.avg_ms,
        s.low_1pct_fps,
        s.low_01pct_fps,
        s.p99_ms,
        s.max_ms,
        s.stddev_ms,
        g.spikes,
        g.draws as f64 / n,
        g.rs_calls as f64 / n,
        if g.rs_calls > 0 { g.rs_redundant as f64 * 100.0 / g.rs_calls as f64 } else { 0.0 },
        g.rs_skipped as f64 / n,
    );
}

fn header() {
    println!(
        "{:<14} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>7} {:>6} {:>8} {:>8} {:>8} {:>8}",
        "group",
        "frames",
        "avg_fps",
        "avg_ms",
        "1%low",
        "0.1%low",
        "p99_ms",
        "max_ms",
        "sd_ms",
        "spikes",
        "draws/f",
        "rs/f",
        "rs_red",
        "skip/f"
    );
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}");
            }
            eprintln!("usage: sape-analyze [--warmup N] [--settle N] <capture.csv>...");
            return ExitCode::from(2);
        }
    };

    let mut pooled: BTreeMap<u8, Group> = BTreeMap::new();
    let mut scratch = Vec::new();
    header();
    for path in &args.files {
        let groups = match load(path, args.warmup, args.settle) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!("# {name}");
        for (on, g) in groups {
            let s = summarize(&g.frame_ms, &mut scratch);
            print_row(
                if on == 1 {
                    "  filter ON"
                } else {
                    "  filter OFF"
                },
                &s,
                &g,
            );
            let p = pooled.entry(on).or_default();
            p.frame_ms.extend_from_slice(&g.frame_ms);
            p.spikes += g.spikes;
            p.draws += g.draws;
            p.rs_calls += g.rs_calls;
            p.rs_redundant += g.rs_redundant;
            p.rs_skipped += g.rs_skipped;
        }
    }

    if args.files.len() > 1 || pooled.len() > 1 {
        println!("# pooled");
        let mut sums = BTreeMap::new();
        for (on, g) in &pooled {
            let s = summarize(&g.frame_ms, &mut scratch);
            print_row(
                if *on == 1 {
                    "  filter ON"
                } else {
                    "  filter OFF"
                },
                &s,
                g,
            );
            sums.insert(*on, s);
        }
        if let (Some(off), Some(on)) = (sums.get(&0), sums.get(&1)) {
            let d = |a: f64, b: f64| if a != 0.0 { (b - a) * 100.0 / a } else { 0.0 };
            println!(
                "# delta ON vs OFF: avg fps {:+.2}% | 1% low {:+.2}% | 0.1% low {:+.2}% | avg frame time {:+.3} ms | p99 {:+.3} ms",
                d(off.avg_fps, on.avg_fps),
                d(off.low_1pct_fps, on.low_1pct_fps),
                d(off.low_01pct_fps, on.low_01pct_fps),
                on.avg_ms - off.avg_ms,
                on.p99_ms - off.p99_ms
            );
            println!("# note: a difference is only meaningful if it exceeds the run-to-run spread of repeated captures.");
        }
    }
    ExitCode::SUCCESS
}

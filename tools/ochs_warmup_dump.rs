//! `ochs_warmup_dump` CLI：dump 169 preflop class → OCHS N-way cluster 划分。
//!
//! 两种 mode：
//!
//! - `--mode ehs`（默认）：1D-EHS warmup（既有，equity.rs::dump_ochs_warmup）。
//!   ~260 ms wall。
//! - `--mode postflop-hist`：postflop 8-bin equity-histogram warmup
//!   （equity.rs::dump_ochs_warmup_postflop_hist）。`--n-rivers` 控制采样数
//!   （默认 1000）；wall ∝ n_rivers。
//!
//! 输入：`--n-clusters` ∈ [2, 64]，`--mode {ehs|postflop-hist}`，`--n-rivers` u32。
//! 输出 stdout JSON：见 `docs/bucket_feature_design.md` §2.3 schema。
//!
//! 用法：
//!
//! ```bash
//! cargo run --release --bin ochs_warmup_dump -- --n-clusters 8 \
//!     > artifacts/ochs_warmup_8.json
//! cargo run --release --bin ochs_warmup_dump -- --mode postflop-hist \
//!     --n-clusters 8 --n-rivers 1000 \
//!     > artifacts/ochs_warmup_postflop_8_n1000.json
//! ```
//!
//! byte-equal 输出由 `OCHS_TRAINING_SEED` hardcoded 保证。

use std::process::ExitCode;
use std::sync::Arc;

use poker::abstraction::equity::{
    dump_ochs_warmup, dump_ochs_warmup_postflop_hist, OchsPostflopWarmupDump, OchsWarmupDump,
};
use poker::eval::NaiveHandEvaluator;
use poker::{Card, HandEvaluator};

const DEFAULT_N_CLUSTERS: u32 = 8;
const DEFAULT_N_RIVERS: u32 = 1000;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Mode {
    Ehs,
    PostflopHist,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            print_usage(&args);
            return ExitCode::from(2);
        }
    };

    if !(2..=64).contains(&opts.n_clusters) {
        eprintln!(
            "error: --n-clusters must be in [2, 64], got {}",
            opts.n_clusters
        );
        return ExitCode::from(2);
    }

    let t_start = std::time::Instant::now();
    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    match opts.mode {
        Mode::Ehs => {
            let dump: OchsWarmupDump = dump_ochs_warmup(opts.n_clusters, evaluator);
            eprintln!(
                "[ochs_warmup_dump] mode=ehs n_clusters={} elapsed={:?}",
                opts.n_clusters,
                t_start.elapsed()
            );
            print_json_ehs(&dump, opts.n_clusters);
        }
        Mode::PostflopHist => {
            let dump: OchsPostflopWarmupDump =
                dump_ochs_warmup_postflop_hist(opts.n_clusters, opts.n_rivers, evaluator);
            eprintln!(
                "[ochs_warmup_dump] mode=postflop-hist n_clusters={} n_rivers={} elapsed={:?}",
                opts.n_clusters,
                opts.n_rivers,
                t_start.elapsed()
            );
            print_json_postflop(&dump, opts.n_clusters);
        }
    }
    ExitCode::from(0)
}

struct Opts {
    n_clusters: u32,
    mode: Mode,
    n_rivers: u32,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut n_clusters: u32 = DEFAULT_N_CLUSTERS;
    let mut mode: Mode = Mode::Ehs;
    let mut n_rivers: u32 = DEFAULT_N_RIVERS;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--n-clusters" | "-n" => {
                i += 1;
                if i >= args.len() {
                    return Err("--n-clusters requires a value".into());
                }
                n_clusters = args[i]
                    .parse::<u32>()
                    .map_err(|e| format!("--n-clusters parse: {e}"))?;
            }
            "--mode" => {
                i += 1;
                if i >= args.len() {
                    return Err("--mode requires a value (ehs|postflop-hist)".into());
                }
                mode = match args[i].as_str() {
                    "ehs" => Mode::Ehs,
                    "postflop-hist" => Mode::PostflopHist,
                    other => return Err(format!("unknown --mode {other}")),
                };
            }
            "--n-rivers" => {
                i += 1;
                if i >= args.len() {
                    return Err("--n-rivers requires a value".into());
                }
                n_rivers = args[i]
                    .parse::<u32>()
                    .map_err(|e| format!("--n-rivers parse: {e}"))?;
            }
            "-h" | "--help" => {
                print_usage(args);
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg {other}")),
        }
        i += 1;
    }
    Ok(Opts {
        n_clusters,
        mode,
        n_rivers,
    })
}

fn print_usage(args: &[String]) {
    eprintln!(
        "usage: {} [--mode ehs|postflop-hist] --n-clusters N [--n-rivers M]",
        args.first()
            .map(String::as_str)
            .unwrap_or("ochs_warmup_dump")
    );
    eprintln!("  --mode             ehs (default, 1D-EHS warmup) | postflop-hist");
    eprintln!("  --n-clusters N     OCHS opponent cluster count (default 8)");
    eprintln!("  --n-rivers M       postflop-hist only: rivers sampled per class (default 1000)");
}

fn card_str(c: Card) -> String {
    let rank_chars = [
        "2", "3", "4", "5", "6", "7", "8", "9", "T", "J", "Q", "K", "A",
    ];
    let suit_chars = ["c", "d", "h", "s"];
    format!(
        "{}{}",
        rank_chars[c.rank() as usize],
        suit_chars[c.suit() as usize]
    )
}

/// 把 class_id (0..169) 翻成 human-readable hand label，e.g. "AA" / "AKs" / "72o"。
fn class_label(class_id: u8) -> String {
    let rank_chars = [
        "2", "3", "4", "5", "6", "7", "8", "9", "T", "J", "Q", "K", "A",
    ];
    if class_id <= 12 {
        let r = rank_chars[class_id as usize];
        format!("{r}{r}")
    } else if class_id <= 90 {
        let idx = class_id - 13;
        let (high, low) = decode_high_low(idx);
        format!("{}{}s", rank_chars[high as usize], rank_chars[low as usize])
    } else {
        let idx = class_id - 91;
        let (high, low) = decode_high_low(idx);
        format!("{}{}o", rank_chars[high as usize], rank_chars[low as usize])
    }
}

fn decode_high_low(idx: u8) -> (u8, u8) {
    let mut high: u8 = 1;
    while high * (high + 1) / 2 <= idx {
        high += 1;
    }
    (high, idx - high * (high - 1) / 2)
}

fn print_json_ehs(dump: &OchsWarmupDump, n_clusters: u32) {
    print!("{{");
    print!("\"n_clusters\":{n_clusters},");
    print!(
        "\"ochs_training_seed\":\"{:#018x}\",",
        dump.ochs_training_seed
    );
    print!("\"ochs_precompute_iter\":{},", dump.ochs_precompute_iter);
    print!("\"evaluator\":\"NaiveHandEvaluator\",");

    // representative_hole: human-readable pairs
    print!("\"representative_hole\":[");
    for (i, pair) in dump.representative_hole.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        print!("[\"{}\",\"{}\"]", card_str(pair[0]), card_str(pair[1]));
    }
    print!("],");

    // class_labels (169)
    print!("\"class_labels\":[");
    for i in 0..169u8 {
        if i > 0 {
            print!(",");
        }
        print!("\"{}\"", class_label(i));
    }
    print!("],");

    // ehs_per_class
    print!("\"ehs_per_class\":[");
    for (i, e) in dump.ehs_per_class.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        print!("{:.6}", e);
    }
    print!("],");

    // classes_per_cluster
    print!("\"classes_per_cluster\":[");
    for (c, classes) in dump.classes_per_cluster.iter().enumerate() {
        if c > 0 {
            print!(",");
        }
        print!("[");
        for (j, &class_id) in classes.iter().enumerate() {
            if j > 0 {
                print!(",");
            }
            print!("{class_id}");
        }
        print!("]");
    }
    print!("],");

    // cluster_labels: per-cluster list of human-readable hand labels
    print!("\"cluster_labels\":[");
    for (c, classes) in dump.classes_per_cluster.iter().enumerate() {
        if c > 0 {
            print!(",");
        }
        print!("[");
        for (j, &class_id) in classes.iter().enumerate() {
            if j > 0 {
                print!(",");
            }
            print!("\"{}\"", class_label(class_id));
        }
        print!("]");
    }
    print!("],");

    // cluster_centroid_ehs
    print!("\"cluster_centroid_ehs\":[");
    for (c, &v) in dump.cluster_centroid_ehs.iter().enumerate() {
        if c > 0 {
            print!(",");
        }
        print!("{:.6}", v);
    }
    print!("],");

    // cluster summary: per-cluster size + ehs min/max/median
    print!("\"cluster_summary\":[");
    for (c, classes) in dump.classes_per_cluster.iter().enumerate() {
        if c > 0 {
            print!(",");
        }
        let mut ehs_in_cluster: Vec<f64> = classes
            .iter()
            .map(|&class_id| dump.ehs_per_class[class_id as usize])
            .collect();
        ehs_in_cluster.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = ehs_in_cluster.len();
        let median = if n == 0 {
            0.0
        } else if n % 2 == 1 {
            ehs_in_cluster[n / 2]
        } else {
            (ehs_in_cluster[n / 2 - 1] + ehs_in_cluster[n / 2]) / 2.0
        };
        let (min, max) = if n == 0 {
            (0.0, 0.0)
        } else {
            (ehs_in_cluster[0], ehs_in_cluster[n - 1])
        };
        print!(
            "{{\"cluster\":{c},\"size\":{n},\"ehs_min\":{min:.6},\"ehs_max\":{max:.6},\"ehs_median\":{median:.6}}}"
        );
    }
    print!("]");

    println!("}}");
}

fn print_json_postflop(dump: &OchsPostflopWarmupDump, n_clusters: u32) {
    print!("{{");
    print!("\"mode\":\"postflop-hist\",");
    print!("\"n_clusters\":{n_clusters},");
    print!(
        "\"ochs_postflop_training_seed\":\"{:#018x}\",",
        dump.ochs_postflop_training_seed
    );
    print!("\"n_rivers_sampled\":{},", dump.n_rivers_sampled);
    print!("\"evaluator\":\"NaiveHandEvaluator\",");

    print!("\"representative_hole\":[");
    for (i, pair) in dump.representative_hole.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        print!("[\"{}\",\"{}\"]", card_str(pair[0]), card_str(pair[1]));
    }
    print!("],");

    print!("\"class_labels\":[");
    for i in 0..169u8 {
        if i > 0 {
            print!(",");
        }
        print!("\"{}\"", class_label(i));
    }
    print!("],");

    // ehs_mean_per_class
    print!("\"ehs_mean_per_class\":[");
    for (i, e) in dump.ehs_mean_per_class.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        print!("{:.6}", e);
    }
    print!("],");

    // equity_hist_per_class (169 × 8 frequencies)
    print!("\"equity_hist_per_class\":[");
    for (i, h) in dump.equity_hist_per_class.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        print!("[");
        for (b, &v) in h.iter().enumerate() {
            if b > 0 {
                print!(",");
            }
            print!("{:.6}", v);
        }
        print!("]");
    }
    print!("],");

    // classes_per_cluster
    print!("\"classes_per_cluster\":[");
    for (c, classes) in dump.classes_per_cluster.iter().enumerate() {
        if c > 0 {
            print!(",");
        }
        print!("[");
        for (j, &class_id) in classes.iter().enumerate() {
            if j > 0 {
                print!(",");
            }
            print!("{class_id}");
        }
        print!("]");
    }
    print!("],");

    // cluster_labels
    print!("\"cluster_labels\":[");
    for (c, classes) in dump.classes_per_cluster.iter().enumerate() {
        if c > 0 {
            print!(",");
        }
        print!("[");
        for (j, &class_id) in classes.iter().enumerate() {
            if j > 0 {
                print!(",");
            }
            print!("\"{}\"", class_label(class_id));
        }
        print!("]");
    }
    print!("],");

    // cluster_centroid_hist
    print!("\"cluster_centroid_hist\":[");
    for (c, h) in dump.cluster_centroid_hist.iter().enumerate() {
        if c > 0 {
            print!(",");
        }
        print!("[");
        for (b, &v) in h.iter().enumerate() {
            if b > 0 {
                print!(",");
            }
            print!("{:.6}", v);
        }
        print!("]");
    }
    print!("],");

    // cluster_centroid_ehs
    print!("\"cluster_centroid_ehs\":[");
    for (c, &v) in dump.cluster_centroid_ehs.iter().enumerate() {
        if c > 0 {
            print!(",");
        }
        print!("{:.6}", v);
    }
    print!("],");

    // cluster_summary: size + ehs_mean stats + hist std
    print!("\"cluster_summary\":[");
    for (c, classes) in dump.classes_per_cluster.iter().enumerate() {
        if c > 0 {
            print!(",");
        }
        let mut ehs_in_cluster: Vec<f64> = classes
            .iter()
            .map(|&class_id| dump.ehs_mean_per_class[class_id as usize])
            .collect();
        ehs_in_cluster.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = ehs_in_cluster.len();
        let median = if n == 0 {
            0.0
        } else if n % 2 == 1 {
            ehs_in_cluster[n / 2]
        } else {
            (ehs_in_cluster[n / 2 - 1] + ehs_in_cluster[n / 2]) / 2.0
        };
        let (min, max) = if n == 0 {
            (0.0, 0.0)
        } else {
            (ehs_in_cluster[0], ehs_in_cluster[n - 1])
        };
        print!(
            "{{\"cluster\":{c},\"size\":{n},\"ehs_mean_min\":{min:.6},\"ehs_mean_max\":{max:.6},\"ehs_mean_median\":{median:.6}}}"
        );
    }
    print!("]");

    println!("}}");
}

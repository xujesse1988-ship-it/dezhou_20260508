//! `ochs_warmup_dump` CLI：dump 169 preflop class → OCHS N-way cluster 划分
//! （`equity.rs::dump_ochs_warmup` 的 JSON 序列化前端）。
//!
//! 输入：`--n-clusters` ∈ {8, 16}（其他 N 也接受）。
//! 输出 stdout JSON：见 `docs/bucket_feature_design.md` §2.3 schema。
//!
//! 用法：
//!
//! ```bash
//! cargo run --release --bin ochs_warmup_dump -- --n-clusters 8 \
//!     > artifacts/ochs_warmup_8.json
//! cargo run --release --bin ochs_warmup_dump -- --n-clusters 16 \
//!     > artifacts/ochs_warmup_16.json
//! ```
//!
//! ~170 ms wall（OCHS_PRECOMPUTE_ITER=10000 × 169 class × 2 eval7 ≈ 3.4M eval ×
//! ~50 ns/eval）。byte-equal 输出由 `OCHS_TRAINING_SEED` hardcoded 保证。

use std::process::ExitCode;
use std::sync::Arc;

use poker::abstraction::equity::{dump_ochs_warmup, OchsWarmupDump};
use poker::eval::NaiveHandEvaluator;
use poker::{Card, HandEvaluator};

const DEFAULT_N_CLUSTERS: u32 = 8;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let n_clusters = match parse_args(&args) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("error: {e}");
            print_usage(&args);
            return ExitCode::from(2);
        }
    };

    if !(2..=64).contains(&n_clusters) {
        eprintln!("error: --n-clusters must be in [2, 64], got {n_clusters}");
        return ExitCode::from(2);
    }

    let t_start = std::time::Instant::now();
    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    let dump: OchsWarmupDump = dump_ochs_warmup(n_clusters, evaluator);
    eprintln!(
        "[ochs_warmup_dump] n_clusters={n_clusters} elapsed={:?}",
        t_start.elapsed()
    );

    print_json(&dump, n_clusters);
    ExitCode::from(0)
}

fn parse_args(args: &[String]) -> Result<u32, String> {
    let mut n_clusters: u32 = DEFAULT_N_CLUSTERS;
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
            "-h" | "--help" => {
                print_usage(args);
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg {other}")),
        }
        i += 1;
    }
    Ok(n_clusters)
}

fn print_usage(args: &[String]) {
    eprintln!(
        "usage: {} --n-clusters N",
        args.first()
            .map(String::as_str)
            .unwrap_or("ochs_warmup_dump")
    );
    eprintln!("  --n-clusters N   OCHS opponent cluster count (default 8)");
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

fn print_json(dump: &OchsWarmupDump, n_clusters: u32) {
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

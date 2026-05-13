//! `train_bucket_table` CLI（API §5）。
//!
//! 从 RngSource seed → equity Monte Carlo → k-means clustering → 写出 bucket
//! table artifact 到磁盘。
//!
//! 用法（v3 production 默认；§G-batch1 §3.9 \[实现\]）：
//!
//! ```bash
//! cargo run --release --bin train_bucket_table -- \
//!     --seed 0xCAFEBABE \
//!     --flop 500 --turn 500 --river 500 \
//!     --mode production \
//!     --cluster-iter-flop 2000 \
//!     --cluster-iter-turn 5000 \
//!     --cluster-iter-river 10000 \
//!     --output artifacts/bucket_table_demo.bin
//! ```
//!
//! Per-street `--cluster-iter-{flop,turn,river}` flag 默认 [`ClusterIter::production_default`]
//! = `2000/5000/10000`（§G-batch1 §3.9 \[决策\] river ehs² 噪声 σ 从 iter=2000
//! 的 2.2% 降到 iter=10000 的 1.0% < bucket spacing 0.2% × 5x）。Legacy 单值
//! `--cluster-iter N` 一并替换三街为同值（向后兼容 §G-batch1 §3.4-batch2 调用形态）。
//!
//! 同 `(seed, BucketConfig, cluster_iter, mode)` 输出的文件 byte-equal（D-237）。
//!
//! **`--mode`**：默认 `production`，§G-batch1 §3.9 起走 single-phase full N
//! [`TrainingMode::Production`]（D-244-rev2 §5 footnote option (c) 字面落地）；
//! `--mode fixture` 走 K×100 cap 公式（与 stage 2 test fixture / bench / capture
//! 路径 byte-equal）。详见 [`TrainingMode`] 文档。

use std::process::ExitCode;
use std::sync::Arc;

use poker::eval::NaiveHandEvaluator;
use poker::{BucketConfig, BucketTable, ClusterIter, HandEvaluator, TrainingMode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            print_usage(&args);
            return ExitCode::from(2);
        }
    };

    eprintln!(
        "[train_bucket_table] seed={:#018x} bucket_config=({}/{}/{}) \
         cluster_iter=(flop={}, turn={}, river={}) mode={:?} output={:?}",
        opts.seed,
        opts.flop,
        opts.turn,
        opts.river,
        opts.cluster_iter.flop,
        opts.cluster_iter.turn,
        opts.cluster_iter.river,
        opts.mode,
        opts.output
    );

    let config = match BucketConfig::new(opts.flop, opts.turn, opts.river) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: invalid BucketConfig: {e}");
            return ExitCode::from(2);
        }
    };
    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);

    let t0 = std::time::Instant::now();
    let table = BucketTable::train_in_memory_with_mode_iter(
        config,
        opts.seed,
        evaluator,
        opts.cluster_iter,
        opts.mode,
    );
    let t_train = t0.elapsed();
    eprintln!("[train_bucket_table] training complete in {:?}", t_train);

    if let Err(e) = table.write_to_path(&opts.output) {
        eprintln!("error: write_to_path({:?}): {e}", opts.output);
        return ExitCode::from(1);
    }
    let hex: String = table
        .content_hash()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();
    eprintln!(
        "[train_bucket_table] wrote {:?} (BLAKE3={})",
        opts.output, hex
    );
    ExitCode::from(0)
}

#[derive(Debug)]
struct Opts {
    seed: u64,
    flop: u32,
    turn: u32,
    river: u32,
    cluster_iter: ClusterIter,
    mode: TrainingMode,
    output: std::path::PathBuf,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut seed: u64 = 0xCAFE_BABE;
    let mut flop: u32 = 500;
    let mut turn: u32 = 500;
    let mut river: u32 = 500;
    let mut cluster_iter = ClusterIter::production_default();
    let mut iter_overridden_uniform = false;
    let mut iter_overridden_flop = false;
    let mut iter_overridden_turn = false;
    let mut iter_overridden_river = false;
    let mut mode: TrainingMode = TrainingMode::Production;
    let mut output: Option<std::path::PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--seed" => {
                let v = args.get(i + 1).ok_or("--seed expects a value")?;
                seed = parse_u64(v)?;
                i += 2;
            }
            "--flop" => {
                let v = args.get(i + 1).ok_or("--flop expects a value")?;
                flop = v.parse().map_err(|e| format!("--flop: {e}"))?;
                i += 2;
            }
            "--turn" => {
                let v = args.get(i + 1).ok_or("--turn expects a value")?;
                turn = v.parse().map_err(|e| format!("--turn: {e}"))?;
                i += 2;
            }
            "--river" => {
                let v = args.get(i + 1).ok_or("--river expects a value")?;
                river = v.parse().map_err(|e| format!("--river: {e}"))?;
                i += 2;
            }
            "--cluster-iter" => {
                let v = args.get(i + 1).ok_or("--cluster-iter expects a value")?;
                let n: u32 = v.parse().map_err(|e| format!("--cluster-iter: {e}"))?;
                cluster_iter = ClusterIter::uniform(n);
                iter_overridden_uniform = true;
                i += 2;
            }
            "--cluster-iter-flop" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--cluster-iter-flop expects a value")?;
                let n: u32 = v.parse().map_err(|e| format!("--cluster-iter-flop: {e}"))?;
                cluster_iter.flop = n;
                iter_overridden_flop = true;
                i += 2;
            }
            "--cluster-iter-turn" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--cluster-iter-turn expects a value")?;
                let n: u32 = v.parse().map_err(|e| format!("--cluster-iter-turn: {e}"))?;
                cluster_iter.turn = n;
                iter_overridden_turn = true;
                i += 2;
            }
            "--cluster-iter-river" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--cluster-iter-river expects a value")?;
                let n: u32 = v
                    .parse()
                    .map_err(|e| format!("--cluster-iter-river: {e}"))?;
                cluster_iter.river = n;
                iter_overridden_river = true;
                i += 2;
            }
            "--mode" => {
                let v = args.get(i + 1).ok_or("--mode expects a value")?;
                mode = parse_mode(v)?;
                i += 2;
            }
            "--output" => {
                let v = args.get(i + 1).ok_or("--output expects a value")?;
                output = Some(std::path::PathBuf::from(v));
                i += 2;
            }
            "--help" | "-h" => {
                print_usage(args);
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    // 互斥检测：--cluster-iter 与 per-street flag 同时给出会让 caller 意图模糊。
    if iter_overridden_uniform
        && (iter_overridden_flop || iter_overridden_turn || iter_overridden_river)
    {
        return Err("--cluster-iter is mutually exclusive with \
             --cluster-iter-flop/turn/river"
            .into());
    }
    let output = output.ok_or("--output is required")?;
    if cluster_iter.flop == 0 || cluster_iter.turn == 0 || cluster_iter.river == 0 {
        return Err("--cluster-iter{,-flop,-turn,-river} must be >= 1".into());
    }
    Ok(Opts {
        seed,
        flop,
        turn,
        river,
        cluster_iter,
        mode,
        output,
    })
}

fn parse_mode(s: &str) -> Result<TrainingMode, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "fixture" => Ok(TrainingMode::Fixture),
        "production" | "prod" => Ok(TrainingMode::Production),
        other => Err(format!(
            "--mode: unknown value {other:?} (expected 'fixture' or 'production')"
        )),
    }
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| format!("invalid hex u64: {s} ({e})"))
    } else {
        s.parse().map_err(|e| format!("invalid u64: {s} ({e})"))
    }
}

fn print_usage(args: &[String]) {
    let prog = args
        .first()
        .map(|s| s.as_str())
        .unwrap_or("train_bucket_table");
    eprintln!(
        "usage: {prog} --output <path> [--seed <u64>] [--flop <N>] [--turn <N>] [--river <N>]\n\
         \x20             [--cluster-iter <N>] [--cluster-iter-flop <N>]\n\
         \x20             [--cluster-iter-turn <N>] [--cluster-iter-river <N>]\n\
         \x20             [--mode <fixture|production>]\n\
         \n\
         Defaults: seed=0xCAFEBABE, flop=turn=river=500, mode=production.\n\
         cluster-iter defaults: ClusterIter::production_default() = (flop=2000,\n\
           turn=5000, river=10000). Per-street flags override individual streets;\n\
           --cluster-iter sets all three streets to the same value (legacy single-\n\
           value form). The two forms are mutually exclusive.\n\
         seed accepts decimal or 0x-prefixed hex.\n\
         mode=production goes through TrainingMode::Production single-phase full N\n\
           (D-244-rev2 §5 footnote option (c); §G-batch1 §3.9); mode=fixture uses\n\
           K*100 cap (fast, byte-equal with stage 2 test fixture path).\n\
         output file is written atomically (write to <path>.tmp then rename).\n"
    );
}

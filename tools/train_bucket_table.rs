//! `train_bucket_table` CLI（API §5）。
//!
//! 从 RngSource seed → equity Monte Carlo → k-means clustering → 写出 bucket
//! table artifact 到磁盘。
//!
//! 用法：
//!
//! ```bash
//! cargo run --release --bin train_bucket_table -- \
//!     --seed 0xCAFEBABE \
//!     --flop 500 --turn 500 --river 500 \
//!     --mode production \
//!     --output artifacts/bucket_table_demo.bin
//! ```
//!
//! 默认 cluster_iter = 10_000（D-220 训练默认）；CI 短跑可降到 200~1000 以加速。
//! 同 `(seed, BucketConfig, cluster_iter, mode)` 输出的文件 byte-equal（D-237）。
//!
//! **`--mode`**（§G-batch1 §3.3）：默认 `production`，走
//! [`TrainingMode::Production`] = 4×N 全覆盖（workflow §G-batch1 §3.4 字面）；
//! `--mode fixture` 走 K×100 cap 公式（与 stage 2 test fixture / bench / capture
//! 路径 byte-equal）。详见 [`TrainingMode`] 文档。

use std::process::ExitCode;
use std::sync::Arc;

use poker::eval::NaiveHandEvaluator;
use poker::{BucketConfig, BucketTable, HandEvaluator, TrainingMode};

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
        "[train_bucket_table] seed={:#018x} bucket_config=({}/{}/{}) cluster_iter={} mode={:?} output={:?}",
        opts.seed, opts.flop, opts.turn, opts.river, opts.cluster_iter, opts.mode, opts.output
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
    let table = BucketTable::train_in_memory_with_mode(
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
    cluster_iter: u32,
    mode: TrainingMode,
    output: std::path::PathBuf,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut seed: u64 = 0xCAFE_BABE;
    let mut flop: u32 = 500;
    let mut turn: u32 = 500;
    let mut river: u32 = 500;
    let mut cluster_iter: u32 = 10_000;
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
                cluster_iter = v.parse().map_err(|e| format!("--cluster-iter: {e}"))?;
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
    let output = output.ok_or("--output is required")?;
    if cluster_iter == 0 {
        return Err("--cluster-iter must be >= 1".into());
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
         \x20             [--cluster-iter <N>] [--mode <fixture|production>]\n\
         \n\
         Defaults: seed=0xCAFEBABE, flop=turn=river=500, cluster-iter=10000, mode=production.\n\
         seed accepts decimal or 0x-prefixed hex.\n\
         mode=production goes through TrainingMode::Production (4*N candidates per street,\n\
           workflow §G-batch1 §3.4 literal); mode=fixture uses K*100 cap (fast, byte-equal\n\
           with stage 2 test fixture path).\n\
         output file is written atomically (write to <path>.tmp then rename).\n"
    );
}

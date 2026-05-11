//! `bucket_quality_dump` CLI（F3 \[报告\] 一次性 instrumentation；与 `train_bucket_table`
//! 同 tools/ 路径平行）。
//!
//! 读取 `BucketTable` 二进制 artifact → 每条街抽 `--samples` 个 random (board, hole)
//! → 用 `MonteCarloEquity` 计算 EHS → 按 `lookup_table` 分桶 → 输出 per-street
//! per-bucket `std_dev / median / empty_buckets / adjacent_emd` JSON 喂给
//! `tools/bucket_quality_report.py`（C1 §输出 line 318 路径）。
//!
//! 用法：
//!
//! ```bash
//! cargo run --release --bin bucket_quality_dump -- \
//!     --artifact artifacts/bucket_table_default_500_500_500_seed_cafebabe.bin \
//!     --samples 10000 --equity-iter 1000 \
//!     | python3 tools/bucket_quality_report.py \
//!     > docs/pluribus_stage2_bucket_quality.md
//! ```
//!
//! 默认 10000 sample/街 + 1k iter MC，~3 streets × 10000 × 1000 = 30M evaluator calls
//! ≈ 1.5 s release（21M eval/s baseline）+ 1k iter MC overhead。3 街总耗时 ~3 s。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use poker::abstraction::cluster::emd_1d_unit_interval;
use poker::eval::NaiveHandEvaluator;
use poker::rng_substream::{derive_substream_seed, EQUITY_MONTE_CARLO};
use poker::{
    canonical_observation_id, BucketTable, Card, ChaCha20Rng, EquityCalculator, HandEvaluator,
    MonteCarloEquity, RngSource, StreetTag,
};

/// 默认采样规模（10k）/ 默认 EHS 估算 iter 数（1k）。10k random (board, hole)
/// 经 hash-based canonical_observation_id mod street limit (3K/6K/10K) 后大致
/// 让每个 bucket 命中 ~20 sample，足够算 std_dev / median / EMD（与
/// `tests/bucket_quality.rs` 的 1000 sample / 100 bucket = 10 sample/bucket
/// 相比 ~2× 密度）。
const DEFAULT_SAMPLES: u32 = 10_000;
const DEFAULT_EQUITY_ITER: u32 = 1_000;

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

    let table = match BucketTable::open(&opts.artifact) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: failed to open artifact {:?}: {e}", opts.artifact);
            return ExitCode::from(2);
        }
    };
    eprintln!(
        "[bucket_quality_dump] artifact={:?} bucket_config=({}/{}/{}) training_seed={:#018x}",
        opts.artifact,
        table.bucket_count(StreetTag::Flop),
        table.bucket_count(StreetTag::Turn),
        table.bucket_count(StreetTag::River),
        table.training_seed(),
    );

    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
    let calc = MonteCarloEquity::new(Arc::clone(&evaluator)).with_iter(opts.equity_iter);

    // BLAKE3 trailer hex
    let blake3_hex: String = table
        .content_hash()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();

    print!("{{");
    print!(
        "\"bucket_config\":{{\"flop\":{},\"turn\":{},\"river\":{}}},",
        table.bucket_count(StreetTag::Flop),
        table.bucket_count(StreetTag::Turn),
        table.bucket_count(StreetTag::River),
    );
    print!("\"training_seed\":\"{:#018x}\",", table.training_seed());
    print!("\"blake3\":\"{}\",", blake3_hex);
    print!("\"streets\":{{");
    for (i, street) in [StreetTag::Flop, StreetTag::Turn, StreetTag::River]
        .iter()
        .enumerate()
    {
        if i > 0 {
            print!(",");
        }
        let key = match street {
            StreetTag::Flop => "flop",
            StreetTag::Turn => "turn",
            StreetTag::River => "river",
            StreetTag::Preflop => unreachable!(),
        };
        let stats = compute_street_stats(*street, &table, &calc, opts.samples, opts.sample_seed);
        eprintln!(
            "[bucket_quality_dump] {key}: {} samples, {} empty buckets / {} total",
            opts.samples,
            stats.empty_buckets.len(),
            stats.bucket_count,
        );
        print!("\"{}\":", key);
        print!("{{");
        print!("\"std_dev\":[");
        print_f64_list(&stats.std_dev_per_bucket);
        print!("],");
        print!("\"median\":[");
        print_f64_list(&stats.median_per_bucket);
        print!("],");
        print!("\"adjacent_emd\":[");
        print_f64_list(&stats.adjacent_emd);
        print!("],");
        print!("\"empty_buckets\":[");
        for (j, b) in stats.empty_buckets.iter().enumerate() {
            if j > 0 {
                print!(",");
            }
            print!("{}", b);
        }
        print!("]");
        print!("}}");
    }
    print!("}}");
    println!("}}");

    ExitCode::from(0)
}

struct StreetStats {
    bucket_count: usize,
    std_dev_per_bucket: Vec<f64>,
    median_per_bucket: Vec<f64>,
    adjacent_emd: Vec<f64>,
    /// **Inherent unused bucket ids**（validation §3 严格语义）：通过遍历
    /// `lookup_table[0..n_canonical_observation(street)]` 找出从未被任何 obs_id
    /// 映射到的 bucket id。与 "1000 random sample 没命中" 不同——后者受样本规模
    /// 限制，前者是 artifact 内禀属性。
    empty_buckets: Vec<usize>,
}

/// 按 `tests/bucket_quality.rs` 同型采样 + EHS 路径计算 per-bucket std_dev / median /
/// adjacent_emd / empty_buckets。
fn compute_street_stats(
    street: StreetTag,
    table: &BucketTable,
    calc: &MonteCarloEquity,
    n_samples: u32,
    master_seed: u64,
) -> StreetStats {
    let bucket_count = table.bucket_count(street) as usize;
    let board_len = match street {
        StreetTag::Flop => 3usize,
        StreetTag::Turn => 4,
        StreetTag::River => 5,
        StreetTag::Preflop => panic!("compute_street_stats: Preflop not supported"),
    };

    // 1) 采样
    let mut sample_rng =
        ChaCha20Rng::from_seed(derive_substream_seed(master_seed, EQUITY_MONTE_CARLO, 0));
    let mut samples: Vec<(Vec<Card>, [Card; 2])> = Vec::with_capacity(n_samples as usize);
    for _ in 0..n_samples {
        let cards = sample_distinct_cards(&mut sample_rng, board_len + 2);
        let board: Vec<Card> = cards[..board_len].to_vec();
        let hole: [Card; 2] = [cards[board_len], cards[board_len + 1]];
        samples.push((board, hole));
    }

    // 2) per-sample EHS + 分桶
    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(street, board, *hole);
        let bucket = match table.lookup(street, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            master_seed,
            EQUITY_MONTE_CARLO,
            i as u32 + 1, // +1 让与 sampling RNG 子流分离
        ));
        let ehs = calc
            .equity(*hole, board, &mut rng)
            .expect("equity on legal sample");
        by_bucket[bucket].push(ehs);
    }

    // 3) per-bucket std_dev / median (sample-derived)
    let mut std_dev_per_bucket: Vec<f64> = Vec::with_capacity(bucket_count);
    let mut median_per_bucket: Vec<f64> = Vec::with_capacity(bucket_count);
    for samples in by_bucket.iter() {
        if samples.is_empty() {
            std_dev_per_bucket.push(0.0);
            median_per_bucket.push(f64::NAN);
            continue;
        }
        std_dev_per_bucket.push(std_dev(samples));
        median_per_bucket.push(median(samples));
    }

    // 3a) **Inherent unused bucket ids**：遍历 lookup_table 全表找未被命中的
    // bucket id（validation §3 0 空 bucket 严格语义；与 sample-derived "0 sample"
    // 不同——后者受样本规模限制）。
    let n_canonical = table.n_canonical_observation(street);
    let mut hit: Vec<bool> = vec![false; bucket_count];
    for obs_id in 0..n_canonical {
        if let Some(bid) = table.lookup(street, obs_id) {
            if (bid as usize) < bucket_count {
                hit[bid as usize] = true;
            }
        }
    }
    let empty_buckets: Vec<usize> = (0..bucket_count).filter(|&bid| !hit[bid]).collect();

    // 4) 相邻 bucket EMD (k, k+1)
    let mut adjacent_emd: Vec<f64> = Vec::with_capacity(bucket_count.saturating_sub(1));
    for k in 0..bucket_count.saturating_sub(1) {
        let a = &by_bucket[k];
        let b = &by_bucket[k + 1];
        if a.is_empty() || b.is_empty() {
            adjacent_emd.push(0.0);
            continue;
        }
        adjacent_emd.push(emd_1d_unit_interval(a, b));
    }

    StreetStats {
        bucket_count,
        std_dev_per_bucket,
        median_per_bucket,
        adjacent_emd,
        empty_buckets,
    }
}

fn sample_distinct_cards(rng: &mut dyn RngSource, count: usize) -> Vec<Card> {
    let mut available: Vec<u8> = (0..52u8).collect();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let pick = (rng.next_u64() % (available.len() as u64 - i as u64)) as usize;
        let idx = i + pick;
        available.swap(i, idx);
        out.push(Card::from_u8(available[i]).expect("0..52"));
    }
    out
}

fn std_dev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean: f64 = values.iter().sum::<f64>() / values.len() as f64;
    let var: f64 =
        values.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / values.len() as f64;
    var.sqrt()
}

fn median(values: &[f64]) -> f64 {
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        0.5 * (sorted[n / 2 - 1] + sorted[n / 2])
    }
}

fn print_f64_list(values: &[f64]) {
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            print!(",");
        }
        if v.is_nan() {
            print!("null");
        } else {
            print!("{:.6}", v);
        }
    }
}

#[derive(Debug)]
struct Opts {
    artifact: PathBuf,
    samples: u32,
    equity_iter: u32,
    sample_seed: u64,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut artifact: Option<PathBuf> = None;
    let mut samples: u32 = DEFAULT_SAMPLES;
    let mut equity_iter: u32 = DEFAULT_EQUITY_ITER;
    let mut sample_seed: u64 = 0x000C_157D_F10E;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--artifact" => {
                let v = args.get(i + 1).ok_or("--artifact expects a value")?;
                artifact = Some(PathBuf::from(v));
                i += 2;
            }
            "--samples" => {
                let v = args.get(i + 1).ok_or("--samples expects a value")?;
                samples = v.parse().map_err(|e| format!("--samples: {e}"))?;
                i += 2;
            }
            "--equity-iter" => {
                let v = args.get(i + 1).ok_or("--equity-iter expects a value")?;
                equity_iter = v.parse().map_err(|e| format!("--equity-iter: {e}"))?;
                i += 2;
            }
            "--sample-seed" => {
                let v = args.get(i + 1).ok_or("--sample-seed expects a value")?;
                sample_seed = parse_u64(v)?;
                i += 2;
            }
            "--help" | "-h" => {
                print_usage(args);
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let artifact = artifact.ok_or("--artifact is required")?;
    if samples == 0 {
        return Err("--samples must be >= 1".into());
    }
    if equity_iter == 0 {
        return Err("--equity-iter must be >= 1".into());
    }
    Ok(Opts {
        artifact,
        samples,
        equity_iter,
        sample_seed,
    })
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
        .unwrap_or("bucket_quality_dump");
    eprintln!(
        "usage: {prog} --artifact <path> [--samples <N>] [--equity-iter <N>] [--sample-seed <u64>]\n\
         \n\
         Defaults: samples={DEFAULT_SAMPLES}, equity-iter={DEFAULT_EQUITY_ITER}, sample-seed=0x000C157DF10E.\n\
         Outputs JSON on stdout in the format consumed by tools/bucket_quality_report.py.\n"
    );
}

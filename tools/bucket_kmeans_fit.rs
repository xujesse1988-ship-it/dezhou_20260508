//! `bucket_kmeans_fit` CLI（`docs/bucket_feature_design.md` §6.3 Stage 2）。
//!
//! 读取 Stage 1 三街 `features_<street>.bin` → 校验文件 BLAKE3 + header →
//! 抽取 f32 features + 算 per-sample `reorder_key_ehs` → 调
//! [`BucketTable::train_v3_in_memory`] 跑 k-means + reorder + 量化 → 写 v3
//! bucket_table artifact + 旁路 `.b3sum` 文件。
//!
//! 用法：
//!
//! ```bash
//! cargo run --release --bin bucket_kmeans_fit -- \
//!     --feature-flop  artifacts/features_flop.bin  \
//!     --feature-turn  artifacts/features_turn.bin  \
//!     --feature-river artifacts/features_river.bin \
//!     --bucket-flop  500 --bucket-turn 500 --bucket-river 500 \
//!     --training-seed 0xcafebabe \
//!     --output artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav3.bin
//! ```
//!
//! 同 (features_<street>.bin BLAKE3, training_seed, bucket counts) → artifact
//! body BLAKE3 byte-equal（参 `docs/bucket_feature_design.md` §6.5 第 1 条）。
//!
//! reorder_key_ehs 按街分流：
//! - flop / turn：hist 一阶矩 = Σ_k p_k · (k + 0.5) / 8（从 feature 维度 0..8 直接算）
//! - river：[`poker::abstraction::equity::equity_river_exact`] 输出，rayon 并行
//!   enumerate C(45, 2) = 990 opp。123M samples × 990 × 2 ≈ 2.4×10¹¹ eval7，
//!   c6a.8xlarge 32 vCPU ≈ ~15 min。
//!
//! 内存预算（river 主导）：features_f32 Vec 7.88 GB + features_f64 Vec<Vec<f64>>
//! 15.76 GB + reorder_key 1 GB + kmeans assignments 492 MB + chunk accumulator
//! ~MB ≈ peak ~25 GB。c6a.8xlarge 64 GB 充裕；vultr 7.7 GB 跑不动 river。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use rayon::prelude::*;

use poker::abstraction::canonical_enum::{n_canonical_observation, nth_canonical_form};
use poker::abstraction::equity::equity_river_exact;
use poker::abstraction::info::StreetTag;
use poker::eval::NaiveHandEvaluator;
use poker::{BucketConfig, BucketTable, Card, StreetFeaturesV3};

// =============================================================================
// Stage 1 feature file 常量（参 `tools/bucket_features_dump.rs` 头注）
// =============================================================================

const FEATURE_MAGIC: &[u8; 8] = b"PLBKFEAT";
const FEATURE_HEADER_LEN: usize = 0x50; // 80 bytes
const FEATURE_TRAILER_LEN: usize = 32;
const FEATURE_N_DIMS: u32 = 16;
const FEATURE_DTYPE_F32_LE: u32 = 0;
const FEATURE_LAYOUT_HIST_OCHS8: u32 = 0;
const FEATURE_LAYOUT_OCHS16: u32 = 1;
const FEATURE_BYTES_PER_SAMPLE: usize = (FEATURE_N_DIMS as usize) * 4; // 64

// =============================================================================
// CLI
// =============================================================================

#[derive(Debug)]
struct Opts {
    feature_flop: PathBuf,
    feature_turn: PathBuf,
    feature_river: PathBuf,
    bucket_flop: u32,
    bucket_turn: u32,
    bucket_river: u32,
    training_seed: u64,
    output: PathBuf,
    threads: Option<usize>,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut feature_flop: Option<PathBuf> = None;
    let mut feature_turn: Option<PathBuf> = None;
    let mut feature_river: Option<PathBuf> = None;
    let mut bucket_flop: u32 = 500;
    let mut bucket_turn: u32 = 500;
    let mut bucket_river: u32 = 500;
    let mut training_seed: u64 = 0xCAFE_BABE;
    let mut output: Option<PathBuf> = None;
    let mut threads: Option<usize> = None;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        let next = |i: usize| {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{a} expects a value"))
        };
        match a {
            "--feature-flop" => {
                feature_flop = Some(PathBuf::from(next(i)?));
                i += 2;
            }
            "--feature-turn" => {
                feature_turn = Some(PathBuf::from(next(i)?));
                i += 2;
            }
            "--feature-river" => {
                feature_river = Some(PathBuf::from(next(i)?));
                i += 2;
            }
            "--bucket-flop" => {
                bucket_flop = next(i)?
                    .parse()
                    .map_err(|e| format!("--bucket-flop: {e}"))?;
                i += 2;
            }
            "--bucket-turn" => {
                bucket_turn = next(i)?
                    .parse()
                    .map_err(|e| format!("--bucket-turn: {e}"))?;
                i += 2;
            }
            "--bucket-river" => {
                bucket_river = next(i)?
                    .parse()
                    .map_err(|e| format!("--bucket-river: {e}"))?;
                i += 2;
            }
            "--training-seed" => {
                training_seed = parse_u64(&next(i)?)?;
                i += 2;
            }
            "--output" => {
                output = Some(PathBuf::from(next(i)?));
                i += 2;
            }
            "--threads" => {
                threads = Some(next(i)?.parse().map_err(|e| format!("--threads: {e}"))?);
                i += 2;
            }
            "-h" | "--help" => {
                print_usage(args);
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Opts {
        feature_flop: feature_flop.ok_or("--feature-flop is required")?,
        feature_turn: feature_turn.ok_or("--feature-turn is required")?,
        feature_river: feature_river.ok_or("--feature-river is required")?,
        bucket_flop,
        bucket_turn,
        bucket_river,
        training_seed,
        output: output.ok_or("--output is required")?,
        threads,
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
        .map(String::as_str)
        .unwrap_or("bucket_kmeans_fit");
    eprintln!(
        "usage: {prog} --feature-flop <path> --feature-turn <path> --feature-river <path> \\\n\
         \x20    --output <path> [--bucket-flop N] [--bucket-turn N] [--bucket-river N] \\\n\
         \x20    [--training-seed <u64>] [--threads N]\n\
         \n\
         Defaults: bucket counts = 500/500/500, training-seed = 0xCAFEBABE.\n\
         training-seed accepts decimal or 0x-hex.\n\
         output file written atomically (<path>.tmp → rename); a sibling <path>.b3sum\n\
           with the body BLAKE3 (from the artifact trailer) is also emitted.\n",
    );
}

// =============================================================================
// 主流程
// =============================================================================

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

    if let Some(n) = opts.threads {
        if let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()
        {
            eprintln!("warning: failed to set rayon thread pool to {n}: {e}");
        }
    }

    let t_start = Instant::now();
    let config = match BucketConfig::new(opts.bucket_flop, opts.bucket_turn, opts.bucket_river) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: invalid BucketConfig: {e}");
            return ExitCode::from(2);
        }
    };
    eprintln!(
        "[bucket_kmeans_fit] config=(flop={}, turn={}, river={}) training_seed={:#018x} output={}",
        opts.bucket_flop,
        opts.bucket_turn,
        opts.bucket_river,
        opts.training_seed,
        opts.output.display()
    );

    // 1. 读三街 feature 文件 + 校验
    let flop = match load_feature_file(&opts.feature_flop, StreetTag::Flop) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: load_feature_file flop: {e}");
            return ExitCode::from(1);
        }
    };
    let turn = match load_feature_file(&opts.feature_turn, StreetTag::Turn) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: load_feature_file turn: {e}");
            return ExitCode::from(1);
        }
    };
    let river = match load_feature_file(&opts.feature_river, StreetTag::River) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: load_feature_file river: {e}");
            return ExitCode::from(1);
        }
    };

    // 2. 跑 v3 训练
    let t_train = Instant::now();
    let table = BucketTable::train_v3_in_memory(config, opts.training_seed, flop, turn, river);
    eprintln!(
        "[bucket_kmeans_fit] train_v3_in_memory wall={:?}",
        t_train.elapsed()
    );

    // 3. 写到磁盘 + .b3sum
    if let Err(e) = table.write_to_path(&opts.output) {
        eprintln!("error: write_to_path({:?}): {e}", opts.output);
        return ExitCode::from(1);
    }
    let body_hash: [u8; 32] = table.content_hash();
    let hex: String = body_hash.iter().map(|b| format!("{:02x}", b)).collect();
    let b3sum_path = {
        let mut p = opts.output.clone();
        let mut name = p
            .file_name()
            .map(|n| n.to_owned())
            .unwrap_or_else(|| std::ffi::OsString::from("bucket_table.bin"));
        name.push(".b3sum");
        p.set_file_name(name);
        p
    };
    if let Err(e) = std::fs::write(
        &b3sum_path,
        format!(
            "{hex}  {}\n",
            opts.output
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "bucket_table.bin".to_string())
        ),
    ) {
        eprintln!(
            "warning: failed to write {} ({e}). Artifact已成功，仅 .b3sum 旁路缺失。",
            b3sum_path.display()
        );
    }

    eprintln!(
        "[bucket_kmeans_fit] done {} (BLAKE3={}) total_wall={:?}",
        opts.output.display(),
        hex,
        t_start.elapsed()
    );
    ExitCode::from(0)
}

// =============================================================================
// feature file 读取 + 校验
// =============================================================================

fn load_feature_file(path: &Path, expected_street: StreetTag) -> Result<StreetFeaturesV3, String> {
    let t0 = Instant::now();
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let len = bytes.len();
    if len < FEATURE_HEADER_LEN + FEATURE_TRAILER_LEN {
        return Err(format!(
            "{} too short: {} bytes (< header {} + trailer {})",
            path.display(),
            len,
            FEATURE_HEADER_LEN,
            FEATURE_TRAILER_LEN
        ));
    }

    // header 校验
    if bytes[0..8] != *FEATURE_MAGIC {
        return Err(format!(
            "{} bad magic: expected {:?}",
            path.display(),
            FEATURE_MAGIC
        ));
    }
    let schema_version = read_u32_le(&bytes, 0x08);
    if schema_version != 1 {
        return Err(format!(
            "{} schema_version mismatch: expected 1, got {schema_version}",
            path.display()
        ));
    }
    let street_code = read_u32_le(&bytes, 0x0C);
    let expected_code = match expected_street {
        StreetTag::Flop => 0,
        StreetTag::Turn => 1,
        StreetTag::River => 2,
        StreetTag::Preflop => unreachable!(),
    };
    if street_code != expected_code {
        return Err(format!(
            "{} street code mismatch: expected {expected_code} for {expected_street:?}, got {street_code}",
            path.display()
        ));
    }
    let n_canonical = read_u32_le(&bytes, 0x10);
    let expected_n = n_canonical_observation(expected_street);
    if n_canonical != expected_n {
        return Err(format!(
            "{} n_canonical mismatch: expected {expected_n}, got {n_canonical}",
            path.display()
        ));
    }
    let n_dims = read_u32_le(&bytes, 0x14);
    if n_dims != FEATURE_N_DIMS {
        return Err(format!(
            "{} n_dims mismatch: expected {FEATURE_N_DIMS}, got {n_dims}",
            path.display()
        ));
    }
    let dtype = read_u32_le(&bytes, 0x18);
    if dtype != FEATURE_DTYPE_F32_LE {
        return Err(format!(
            "{} dtype mismatch: expected 0 (f32 LE), got {dtype}",
            path.display()
        ));
    }
    let feature_layout = read_u32_le(&bytes, 0x1C);
    let expected_layout = match expected_street {
        StreetTag::Flop | StreetTag::Turn => FEATURE_LAYOUT_HIST_OCHS8,
        StreetTag::River => FEATURE_LAYOUT_OCHS16,
        StreetTag::Preflop => unreachable!(),
    };
    if feature_layout != expected_layout {
        return Err(format!(
            "{} feature_layout mismatch: expected {expected_layout} for {expected_street:?}, got {feature_layout}",
            path.display()
        ));
    }

    // body size 校验
    let expected_body_size = (n_canonical as usize) * FEATURE_BYTES_PER_SAMPLE;
    let actual_body_size = len - FEATURE_HEADER_LEN - FEATURE_TRAILER_LEN;
    if actual_body_size != expected_body_size {
        return Err(format!(
            "{} body size mismatch: expected {expected_body_size} bytes ({n_canonical} × 64), got {actual_body_size}",
            path.display()
        ));
    }

    // trailer BLAKE3 校验（全文件除最后 32 字节）
    let computed_blake3 = blake3::hash(&bytes[..len - FEATURE_TRAILER_LEN]);
    let computed_bytes: [u8; 32] = *computed_blake3.as_bytes();
    let stored_blake3: [u8; 32] = bytes[len - FEATURE_TRAILER_LEN..len]
        .try_into()
        .expect("32 bytes");
    if computed_bytes != stored_blake3 {
        return Err(format!(
            "{} BLAKE3 trailer mismatch: computed {} != stored {}",
            path.display(),
            hex_str(&computed_bytes),
            hex_str(&stored_blake3)
        ));
    }

    eprintln!(
        "[bucket_kmeans_fit] {} loaded n_canonical={} layout={} read_wall={:?}",
        path.display(),
        n_canonical,
        feature_layout,
        t0.elapsed()
    );

    // 整文件 BLAKE3（含 header + body + trailer）用于 v3 artifact header 嵌入。
    // 这是文件本身的 BLAKE3，与 .b3sum 旁路文件锚点一致。
    let file_blake3_arr: [u8; 32] = *blake3::hash(&bytes).as_bytes();

    // body → Vec<[f32; 16]>
    let t_parse = Instant::now();
    let body_offset = FEATURE_HEADER_LEN;
    let mut features_f32: Vec<[f32; 16]> = Vec::with_capacity(n_canonical as usize);
    for i in 0..(n_canonical as usize) {
        let off = body_offset + i * FEATURE_BYTES_PER_SAMPLE;
        let mut row = [0.0_f32; 16];
        for (d, slot) in row.iter_mut().enumerate() {
            let o = off + d * 4;
            *slot = f32::from_le_bytes(bytes[o..o + 4].try_into().expect("4 bytes"));
        }
        features_f32.push(row);
    }
    eprintln!(
        "[bucket_kmeans_fit] {} parse → Vec<[f32; 16]> wall={:?}",
        path.display(),
        t_parse.elapsed()
    );

    // bytes drop
    drop(bytes);

    // reorder_key_ehs：flop/turn 用 hist 一阶矩；river 用 equity_river_exact
    let t_reorder = Instant::now();
    let reorder_key_ehs: Vec<f64> = match expected_street {
        StreetTag::Flop | StreetTag::Turn => features_f32
            .par_iter()
            .map(|row| hist_first_moment(&row[0..8]))
            .collect(),
        StreetTag::River => {
            // 123M samples × 990 opp × 2 eval7 ≈ ~15 min on c6a.8xlarge 32 vCPU
            let evaluator = NaiveHandEvaluator;
            let n = features_f32.len();
            // 进度日志：每 5% / 1M 取大
            let log_every = (n / 20).max(1_000_000);
            (0..n)
                .into_par_iter()
                .map(|cid| {
                    let (board, hole): (Vec<Card>, [Card; 2]) =
                        nth_canonical_form(StreetTag::River, cid as u32);
                    let eq = equity_river_exact(hole, &board, &evaluator);
                    if cid > 0 && cid % log_every == 0 {
                        eprintln!(
                            "[bucket_kmeans_fit] river reorder_key progress {} / {}",
                            cid, n
                        );
                    }
                    eq
                })
                .collect()
        }
        StreetTag::Preflop => unreachable!(),
    };
    eprintln!(
        "[bucket_kmeans_fit] {} reorder_key_ehs wall={:?}",
        path.display(),
        t_reorder.elapsed()
    );

    Ok(StreetFeaturesV3 {
        features_f32,
        reorder_key_ehs,
        feature_blake3: file_blake3_arr,
    })
}

/// 8-bin hist 一阶矩 = Σ_k p_k · (k + 0.5) / 8。
/// hist 是 hero EHS 在 [0, 1] 的 8-bin 频率向量（Σ p_k = 1），一阶矩 = 期望 EHS。
fn hist_first_moment(hist8: &[f32]) -> f64 {
    debug_assert_eq!(hist8.len(), 8);
    let mut sum = 0.0_f64;
    for (k, &p) in hist8.iter().enumerate() {
        sum += (p as f64) * (((k as f64) + 0.5) / 8.0);
    }
    sum
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 4 bytes"))
}

fn hex_str(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

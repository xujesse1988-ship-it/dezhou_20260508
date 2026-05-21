//! `bucket_features_dump` CLI（`docs/bucket_feature_design.md` §6.2 Stage 1）。
//!
//! 每条 postflop street 独立 enumerate canonical id → 算 16-dim feature →
//! 写出 `features_<street>.bin`。
//!
//! 特征构成：
//! - flop / turn：`equity_hist_8 (over 1081 / 46 future outcomes) || OCHS_8 combo` = 16 dim
//! - river：`OCHS_16 combo` = 16 dim
//!
//! 文件格式（per `docs/bucket_feature_design.md` §6.2）：
//!
//! ```text
//! [header 80 bytes]
//!   0x00 magic "PLBKFEAT" (8 B)
//!   0x08 schema_version u32 LE = 1
//!   0x0c street u32 LE (0=flop, 1=turn, 2=river)
//!   0x10 n_canonical u32 LE
//!   0x14 n_dims u32 LE = 16
//!   0x18 dtype u32 LE = 0 (f32 LE)
//!   0x1c feature_layout u32 LE (0 = hist8 || OCHS8, 1 = OCHS16)
//!   0x20 ochs_warmup_blake3 [u8; 32]
//!   0x40 ochs_n_rivers u32 LE
//!   0x44 ochs_n_clusters u32 LE
//!   0x48 pad [u8; 8] = 0
//! [body n_canonical × 64 bytes (16 × f32 LE)]
//! [trailer 32 bytes = BLAKE3(file[..len - 32])]
//! ```
//!
//! 用法：
//!
//! ```bash
//! cargo run --release --bin bucket_features_dump -- \
//!     --street flop \
//!     --warmup-file artifacts/ochs_warmup_postflop_8_n1000.json \
//!     --output artifacts/features_flop.bin
//! ```
//!
//! Chunked resume：part 文件 `<output>.part<idx>` 在中断后保留；下次运行检测
//! 已存在 part 文件（大小匹配）→ 跳过该 chunk。全部 chunks 完成后 concat +
//! 写 header + trailer。
//!
//! byte-equal：给定 (street, n_canonical_override, n_clusters, n_rivers,
//! OCHS_TRAINING_SEED hardcoded, NaiveHandEvaluator)，输出跨架构 / 跨进程
//! byte-equal（§6.5 第 1 条不变量）。

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;

use poker::abstraction::canonical_enum::{n_canonical_observation, nth_canonical_form};
use poker::abstraction::equity::{
    dump_ochs_warmup_postflop_hist, equity_hist_8, ochs_n_combo, OchsPostflopWarmupDump,
};
use poker::abstraction::info::StreetTag;
use poker::eval::NaiveHandEvaluator;
use poker::{Card, HandEvaluator};

// =============================================================================
// 常量
// =============================================================================

const MAGIC: &[u8; 8] = b"PLBKFEAT";
const SCHEMA_VERSION: u32 = 1;
const N_DIMS: u32 = 16;
const DTYPE_F32_LE: u32 = 0;
const FEATURE_LAYOUT_HIST_OCHS8: u32 = 0;
const FEATURE_LAYOUT_OCHS16: u32 = 1;
const HEADER_LEN: usize = 0x50; // 80 bytes
const TRAILER_LEN: usize = 32;
const BYTES_PER_SAMPLE: usize = (N_DIMS as usize) * 4; // 16 × f32 = 64

const DEFAULT_CHUNK_SIZE: u32 = 200_000;
const DEFAULT_N_RIVERS: u32 = 1000;

// =============================================================================
// CLI
// =============================================================================

#[derive(Debug)]
struct Opts {
    street: StreetTag,
    output: PathBuf,
    warmup_file: PathBuf,
    n_clusters: u32,
    n_rivers: u32,
    threads: Option<usize>,
    chunk_size: u32,
    n_canonical_override: Option<u32>,
    keep_parts: bool,
}

fn parse_street(s: &str) -> Result<StreetTag, String> {
    match s {
        "flop" => Ok(StreetTag::Flop),
        "turn" => Ok(StreetTag::Turn),
        "river" => Ok(StreetTag::River),
        other => Err(format!("--street must be flop|turn|river, got {other}")),
    }
}

fn street_code(s: StreetTag) -> u32 {
    match s {
        StreetTag::Flop => 0,
        StreetTag::Turn => 1,
        StreetTag::River => 2,
        StreetTag::Preflop => unreachable!("preflop not supported by canonical_enum path"),
    }
}

fn default_n_clusters_for(s: StreetTag) -> u32 {
    match s {
        StreetTag::Flop | StreetTag::Turn => 8,
        StreetTag::River => 16,
        StreetTag::Preflop => unreachable!(),
    }
}

fn feature_layout_for(s: StreetTag) -> u32 {
    match s {
        StreetTag::Flop | StreetTag::Turn => FEATURE_LAYOUT_HIST_OCHS8,
        StreetTag::River => FEATURE_LAYOUT_OCHS16,
        StreetTag::Preflop => unreachable!(),
    }
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut street: Option<StreetTag> = None;
    let mut output: Option<PathBuf> = None;
    let mut warmup_file: Option<PathBuf> = None;
    let mut n_clusters: Option<u32> = None;
    let mut n_rivers: u32 = DEFAULT_N_RIVERS;
    let mut threads: Option<usize> = None;
    let mut chunk_size: u32 = DEFAULT_CHUNK_SIZE;
    let mut n_canonical_override: Option<u32> = None;
    let mut keep_parts = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--street" => {
                i += 1;
                if i >= args.len() {
                    return Err("--street requires a value".into());
                }
                street = Some(parse_street(&args[i])?);
            }
            "--output" => {
                i += 1;
                if i >= args.len() {
                    return Err("--output requires a value".into());
                }
                output = Some(PathBuf::from(&args[i]));
            }
            "--warmup-file" => {
                i += 1;
                if i >= args.len() {
                    return Err("--warmup-file requires a value".into());
                }
                warmup_file = Some(PathBuf::from(&args[i]));
            }
            "--n-clusters" => {
                i += 1;
                n_clusters = Some(
                    args.get(i)
                        .ok_or_else(|| "--n-clusters needs value".to_string())?
                        .parse::<u32>()
                        .map_err(|e| format!("--n-clusters: {e}"))?,
                );
            }
            "--n-rivers" => {
                i += 1;
                n_rivers = args
                    .get(i)
                    .ok_or_else(|| "--n-rivers needs value".to_string())?
                    .parse::<u32>()
                    .map_err(|e| format!("--n-rivers: {e}"))?;
            }
            "--threads" => {
                i += 1;
                threads = Some(
                    args.get(i)
                        .ok_or_else(|| "--threads needs value".to_string())?
                        .parse::<usize>()
                        .map_err(|e| format!("--threads: {e}"))?,
                );
            }
            "--chunk-size" => {
                i += 1;
                chunk_size = args
                    .get(i)
                    .ok_or_else(|| "--chunk-size needs value".to_string())?
                    .parse::<u32>()
                    .map_err(|e| format!("--chunk-size: {e}"))?;
            }
            "--n-canonical-override" => {
                i += 1;
                n_canonical_override = Some(
                    args.get(i)
                        .ok_or_else(|| "--n-canonical-override needs value".to_string())?
                        .parse::<u32>()
                        .map_err(|e| format!("--n-canonical-override: {e}"))?,
                );
            }
            "--keep-parts" => keep_parts = true,
            "-h" | "--help" => {
                print_usage(args);
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg {other}")),
        }
        i += 1;
    }
    let street = street.ok_or_else(|| "--street is required".to_string())?;
    let output = output.ok_or_else(|| "--output is required".to_string())?;
    let warmup_file = warmup_file.ok_or_else(|| "--warmup-file is required".to_string())?;
    let n_clusters = n_clusters.unwrap_or_else(|| default_n_clusters_for(street));
    if !(2..=64).contains(&n_clusters) {
        return Err(format!("--n-clusters out of [2, 64]: {n_clusters}"));
    }
    if n_rivers == 0 {
        return Err("--n-rivers must be >= 1".into());
    }
    if chunk_size == 0 {
        return Err("--chunk-size must be >= 1".into());
    }
    Ok(Opts {
        street,
        output,
        warmup_file,
        n_clusters,
        n_rivers,
        threads,
        chunk_size,
        n_canonical_override,
        keep_parts,
    })
}

fn print_usage(args: &[String]) {
    eprintln!(
        "usage: {} --street {{flop|turn|river}} --output <path> \\\n         --warmup-file <path> [--n-clusters N] [--n-rivers M] \\\n         [--threads N] [--chunk-size N] [--n-canonical-override N] [--keep-parts]",
        args.first()
            .map(String::as_str)
            .unwrap_or("bucket_features_dump")
    );
    eprintln!("  --street               flop / turn / river");
    eprintln!("  --output               final features_<street>.bin path");
    eprintln!("  --warmup-file          on-disk OCHS warmup postflop-hist JSON for BLAKE3 chain");
    eprintln!("  --n-clusters N         opp cluster count (default 8 / 16 for river)");
    eprintln!("  --n-rivers M           postflop-hist warmup n_rivers (default 1000)");
    eprintln!("  --threads N            rayon thread pool size (default: detected)");
    eprintln!("  --chunk-size N         canonical ids per part file (default 200000)");
    eprintln!("  --n-canonical-override smoke testing: cap N at this value (default: full)");
    eprintln!("  --keep-parts           don't delete part files after concat");
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
    let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);

    // 1. 读 warmup file → BLAKE3
    let warmup_bytes = match std::fs::read(&opts.warmup_file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "error: failed to read --warmup-file {}: {e}",
                opts.warmup_file.display()
            );
            return ExitCode::from(1);
        }
    };
    let warmup_blake3 = blake3::hash(&warmup_bytes);
    eprintln!(
        "[bucket_features_dump] warmup file {} ({} B) blake3 = {}",
        opts.warmup_file.display(),
        warmup_bytes.len(),
        warmup_blake3.to_hex()
    );

    // 2. 计算 in-memory OCHS warmup table
    let t_warmup = Instant::now();
    let warmup: OchsPostflopWarmupDump =
        dump_ochs_warmup_postflop_hist(opts.n_clusters, opts.n_rivers, Arc::clone(&evaluator));
    eprintln!(
        "[bucket_features_dump] in-memory warmup n_clusters={} n_rivers={} wall={:?}",
        opts.n_clusters,
        opts.n_rivers,
        t_warmup.elapsed()
    );

    // 3. n_canonical
    let n_full = n_canonical_observation(opts.street);
    let n_canonical = opts.n_canonical_override.unwrap_or(n_full).min(n_full);
    eprintln!(
        "[bucket_features_dump] street={:?} n_canonical={} (full={})",
        opts.street, n_canonical, n_full
    );

    // 4. 写每个 part
    let n_chunks = n_canonical.div_ceil(opts.chunk_size).max(1);
    eprintln!(
        "[bucket_features_dump] chunk_size={} n_chunks={}",
        opts.chunk_size, n_chunks
    );

    for chunk_idx in 0..n_chunks {
        let start = chunk_idx * opts.chunk_size;
        let end = (start + opts.chunk_size).min(n_canonical);
        let part_path = part_file_path(&opts.output, chunk_idx);
        let expected_size = ((end - start) as usize) * BYTES_PER_SAMPLE;
        if part_exists_with_size(&part_path, expected_size) {
            eprintln!(
                "[bucket_features_dump] chunk {}/{} [{}..{}) skip (part exists, {} B)",
                chunk_idx + 1,
                n_chunks,
                start,
                end,
                expected_size
            );
            continue;
        }
        let t_chunk = Instant::now();
        let chunk_bytes = compute_chunk(
            opts.street,
            start,
            end,
            &warmup.classes_per_cluster,
            &*evaluator,
        );
        debug_assert_eq!(chunk_bytes.len(), expected_size);
        if let Err(e) = write_part_atomic(&part_path, &chunk_bytes) {
            eprintln!("error: failed to write part {}: {e}", part_path.display());
            return ExitCode::from(1);
        }
        eprintln!(
            "[bucket_features_dump] chunk {}/{} [{}..{}) wrote {} B wall={:?}",
            chunk_idx + 1,
            n_chunks,
            start,
            end,
            expected_size,
            t_chunk.elapsed()
        );
    }

    // 5. 拼成最终文件 + header + trailer
    if let Err(e) = assemble_final(&opts, &warmup_blake3, n_canonical, n_chunks) {
        eprintln!(
            "error: failed to assemble final {}: {e}",
            opts.output.display()
        );
        return ExitCode::from(1);
    }

    // 6. cleanup parts
    if !opts.keep_parts {
        for chunk_idx in 0..n_chunks {
            let part_path = part_file_path(&opts.output, chunk_idx);
            let _ = std::fs::remove_file(&part_path);
        }
    }

    eprintln!(
        "[bucket_features_dump] done street={:?} output={} total_wall={:?}",
        opts.street,
        opts.output.display(),
        t_start.elapsed()
    );
    ExitCode::from(0)
}

// =============================================================================
// chunk 计算
// =============================================================================

fn compute_chunk(
    street: StreetTag,
    start: u32,
    end: u32,
    classes_per_cluster: &[Vec<u8>],
    evaluator: &dyn HandEvaluator,
) -> Vec<u8> {
    let len = (end - start) as usize;
    // 并行算各 sample 的 16-dim feature；按 canonical_id 顺序合并。
    let rows: Vec<[f32; 16]> = (start..end)
        .into_par_iter()
        .map(|cid| feature_for_canonical(street, cid, classes_per_cluster, evaluator))
        .collect();
    debug_assert_eq!(rows.len(), len);
    let mut out: Vec<u8> = Vec::with_capacity(len * BYTES_PER_SAMPLE);
    for row in &rows {
        for v in row {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

fn feature_for_canonical(
    street: StreetTag,
    canonical_id: u32,
    classes_per_cluster: &[Vec<u8>],
    evaluator: &dyn HandEvaluator,
) -> [f32; 16] {
    let (board, hole): (Vec<Card>, [Card; 2]) = nth_canonical_form(street, canonical_id);
    match street {
        StreetTag::Flop | StreetTag::Turn => {
            let hist = equity_hist_8(hole, &board, evaluator);
            let ochs = ochs_n_combo(hole, &board, evaluator, classes_per_cluster);
            debug_assert_eq!(ochs.len(), 8);
            let mut out = [0.0_f32; 16];
            for i in 0..8 {
                out[i] = hist[i] as f32;
            }
            for i in 0..8 {
                out[8 + i] = ochs[i] as f32;
            }
            out
        }
        StreetTag::River => {
            let ochs = ochs_n_combo(hole, &board, evaluator, classes_per_cluster);
            debug_assert_eq!(ochs.len(), 16);
            let mut out = [0.0_f32; 16];
            for i in 0..16 {
                out[i] = ochs[i] as f32;
            }
            out
        }
        StreetTag::Preflop => unreachable!("preflop excluded by parse_args"),
    }
}

// =============================================================================
// part 文件
// =============================================================================

fn part_file_path(output: &Path, chunk_idx: u32) -> PathBuf {
    let mut s = output.as_os_str().to_owned();
    s.push(format!(".part{:06}", chunk_idx));
    PathBuf::from(s)
}

fn part_exists_with_size(path: &Path, expected: usize) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && (m.len() as usize) == expected,
        Err(_) => false,
    }
}

fn write_part_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp_path = path.with_extension("part.tmp");
    {
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        let mut w = BufWriter::new(f);
        w.write_all(data)?;
        w.flush()?;
        w.into_inner()
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

// =============================================================================
// 拼装最终文件（header + concat parts + trailer）
// =============================================================================

fn assemble_final(
    opts: &Opts,
    warmup_blake3: &blake3::Hash,
    n_canonical: u32,
    n_chunks: u32,
) -> std::io::Result<()> {
    let tmp_path = opts.output.with_extension("bin.tmp");
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp_path)?;
    let mut w = BufWriter::new(f);
    let mut hasher = blake3::Hasher::new();

    // header
    let header = build_header(opts, warmup_blake3, n_canonical);
    debug_assert_eq!(header.len(), HEADER_LEN);
    w.write_all(&header)?;
    hasher.update(&header);

    // body：依序追加 parts
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB scratch
    for chunk_idx in 0..n_chunks {
        let part_path = part_file_path(&opts.output, chunk_idx);
        let mut f = File::open(&part_path)?;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
        }
    }

    // trailer: BLAKE3 of header + body
    let digest = hasher.finalize();
    let trailer = digest.as_bytes();
    debug_assert_eq!(trailer.len(), TRAILER_LEN);
    w.write_all(trailer)?;
    w.flush()?;
    w.into_inner()
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .sync_all()?;

    std::fs::rename(&tmp_path, &opts.output)?;
    Ok(())
}

fn build_header(opts: &Opts, warmup_blake3: &blake3::Hash, n_canonical: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER_LEN);
    h.extend_from_slice(MAGIC);
    h.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    h.extend_from_slice(&street_code(opts.street).to_le_bytes());
    h.extend_from_slice(&n_canonical.to_le_bytes());
    h.extend_from_slice(&N_DIMS.to_le_bytes());
    h.extend_from_slice(&DTYPE_F32_LE.to_le_bytes());
    h.extend_from_slice(&feature_layout_for(opts.street).to_le_bytes());
    h.extend_from_slice(warmup_blake3.as_bytes());
    h.extend_from_slice(&opts.n_rivers.to_le_bytes());
    h.extend_from_slice(&opts.n_clusters.to_le_bytes());
    h.extend_from_slice(&[0u8; 8]); // pad
    debug_assert_eq!(h.len(), HEADER_LEN);
    h
}

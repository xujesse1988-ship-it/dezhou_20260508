//! Stage 3 F3 \[报告\] 一次性 BLAKE3 byte-equal anchor instrumentation（D-362）。
//!
//! `pluribus_stage3_workflow.md` §步骤 F3 字面 deliverables 含：
//!
//! ```text
//! 简化 NLHE 100M update D-362 BLAKE3 anchor
//! ```
//!
//! 用户授权 F3 \[报告\] 推进时降标 100M 单 run → 10M × 3 run 检查（详见 stage 3
//! workflow §修订历史 F3 段落）。本工具实现 10M × 3 anchor：
//!
//! 1. 加载 v3 production bucket table artifact（content_hash 锁 v3 ground truth）；
//! 2. 在 fixed_seed 下跑 3 次独立 `EsMccfrTrainer::step` × `--updates`（默认 10M）；
//! 3. 每次 run 结束后对 deterministic-path 上抽样 InfoSet 的 `average_strategy`
//!    做 BLAKE3 snapshot（与 `tests/cfr_simplified_nlhe.rs::blake3_avg_strategy_snapshot`
//!    同型 hashing：probe_id LE + strategy.len() LE + f64 LE bytes pure-byte mixing）；
//! 4. 断言 3 个 BLAKE3 byte-equal（D-362 重复确定性）；
//! 5. 打印每个 run 的 wall time + throughput 供 stage 3 报告引用。
//!
//! 用法：
//!
//! ```text
//! # 默认 10M update × 3（F3 [报告] 用户授权降标 baseline）
//! cargo run --release --bin nlhe_blake3_anchor -- \
//!     --artifact artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin
//!
//! # 自定义 update 数（例如本地 dev box smoke 1M × 3）
//! cargo run --release --bin nlhe_blake3_anchor -- \
//!     --artifact artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin \
//!     --updates 1000000
//! ```
//!
//! 角色边界（F3 \[报告\]）：本工具是一次性 instrumentation（继承 stage 2
//! `tools/bucket_quality_dump.rs` 模式），不修改产品代码 / 测试代码 / API
//! surface。仅 `Cargo.toml` 追加 \[\[bin\]\] entry 让 cargo 识别（继承
//! `train_bucket_table` / `bucket_quality_dump` / `train_cfr` 同型 entry）。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use blake3::Hasher;
use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::training::sampling::sample_discrete;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, ChaCha20Rng, InfoSetId};

/// v3 production artifact body BLAKE3 ground truth（CLAUDE.md "当前 artifact 基线"
/// 字面）。
const V3_BODY_BLAKE3_HEX: &str = "67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd";

/// fixed master seed — F3 anchor 与 stage 3 C1 \[测试\] test_5 共享 seed namespace
/// 但 ASCII 不同避免与 cfr_simplified_nlhe.rs FIXED_SEED 重合。
const FIXED_SEED: u64 = 0x46_33_5F_4E_4C_48_45_5F; // ASCII "F3_NLHE_"

/// 默认 update 数 = 10M（用户授权 F3 降标 baseline）。
const DEFAULT_UPDATES: u64 = 10_000_000;

/// 默认重复次数 = 3（D-362 NLHE 3× BLAKE3 byte-equal）。
const REPEAT_COUNT: usize = 3;

/// snapshot probe 上限（与 cfr_simplified_nlhe.rs::SNAPSHOT_PROBE_LIMIT 同型）。
const SNAPSHOT_PROBE_LIMIT: usize = 4_096;

fn parse_args() -> Result<(PathBuf, u64, Option<PathBuf>), String> {
    let mut artifact: Option<PathBuf> = None;
    let mut updates: u64 = DEFAULT_UPDATES;
    let mut save_checkpoint: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--artifact" => {
                artifact = Some(PathBuf::from(
                    args.next().ok_or_else(|| "--artifact 需要值".to_string())?,
                ));
            }
            "--updates" => {
                let s = args.next().ok_or_else(|| "--updates 需要值".to_string())?;
                updates = s
                    .parse::<u64>()
                    .map_err(|e| format!("--updates 解析失败：{e}"))?;
            }
            "--save-checkpoint" => {
                save_checkpoint = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--save-checkpoint 需要值".to_string())?,
                ));
            }
            "--help" | "-h" => {
                eprintln!(
                    "用法：cargo run --release --bin nlhe_blake3_anchor -- \\\n\
                     \t--artifact <path>      (默认 artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin)\n\
                     \t--updates <n>          (默认 {DEFAULT_UPDATES} = 10M)\n\
                     \t--save-checkpoint <path>  (可选) — F3 [报告] milestone checkpoint artifact 输出路径；\n\
                     \t                           设置后仅跑 1 run × N updates 然后 trainer.save_checkpoint 写入 PATH\n\
                     \t                           （不跑 3-run anchor 验证；用于 GitHub Release artifact 上传）\n"
                );
                std::process::exit(0);
            }
            other => return Err(format!("未知参数：{other}")),
        }
    }
    let artifact = artifact.unwrap_or_else(|| {
        PathBuf::from("artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin")
    });
    Ok((artifact, updates, save_checkpoint))
}

fn blake3_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn collect_snapshot_probes(game: &SimplifiedNlheGame) -> Vec<InfoSetId> {
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut state: SimplifiedNlheState = game.root(&mut rng);
    let mut probes = Vec::with_capacity(SNAPSHOT_PROBE_LIMIT);
    for _ in 0..SNAPSHOT_PROBE_LIMIT {
        match SimplifiedNlheGame::current(&state) {
            NodeKind::Terminal => break,
            NodeKind::Chance => {
                let dist = SimplifiedNlheGame::chance_distribution(&state);
                let action = sample_discrete(&dist, &mut rng);
                state = SimplifiedNlheGame::next(state, action, &mut rng);
            }
            NodeKind::Player(actor) => {
                let info = SimplifiedNlheGame::info_set(&state, actor);
                probes.push(info);
                let actions = SimplifiedNlheGame::legal_actions(&state);
                if actions.is_empty() {
                    break;
                }
                let next_action = actions[0];
                state = SimplifiedNlheGame::next(state, next_action, &mut rng);
            }
        }
    }
    probes
}

fn blake3_avg_strategy_snapshot(
    trainer: &EsMccfrTrainer<SimplifiedNlheGame>,
    probes: &[InfoSetId],
) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(&trainer.update_count().to_le_bytes());
    hasher.update(&(probes.len() as u64).to_le_bytes());
    for info in probes {
        let strategy = trainer.average_strategy(info);
        hasher.update(&info.raw().to_le_bytes());
        hasher.update(&(strategy.len() as u32).to_le_bytes());
        for &p in &strategy {
            hasher.update(&p.to_le_bytes());
        }
    }
    hasher.finalize().into()
}

fn run_one(
    table: Arc<BucketTable>,
    updates: u64,
    save_checkpoint: Option<&Path>,
) -> Result<([u8; 32], f64), String> {
    let game = SimplifiedNlheGame::new(table)
        .map_err(|e| format!("SimplifiedNlheGame::new 失败：{e:?}"))?;
    let probes = collect_snapshot_probes(&game);
    let mut trainer = EsMccfrTrainer::new(game, FIXED_SEED);
    let mut rng = ChaCha20Rng::from_seed(FIXED_SEED);
    let t0 = Instant::now();
    let log_interval = updates.max(1) / 10;
    let mut next_log = log_interval;
    for i in 0..updates {
        trainer
            .step(&mut rng)
            .map_err(|e| format!("step #{i} 失败：{e:?}"))?;
        if log_interval > 0 && trainer.update_count() == next_log {
            let elapsed = t0.elapsed().as_secs_f64();
            let throughput = trainer.update_count() as f64 / elapsed.max(1e-9);
            eprintln!(
                "  step {} / {} elapsed={:.1}s throughput={:.0} update/s",
                trainer.update_count(),
                updates,
                elapsed,
                throughput
            );
            next_log = next_log.saturating_add(log_interval);
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let hash = blake3_avg_strategy_snapshot(&trainer, &probes);
    if let Some(path) = save_checkpoint {
        trainer
            .save_checkpoint(path)
            .map_err(|e| format!("save_checkpoint({}) 失败：{e:?}", path.display()))?;
        eprintln!(
            "  saved checkpoint -> {} ({} bytes file, trainer.update_count = {})",
            path.display(),
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            trainer.update_count()
        );
    }
    Ok((hash, elapsed))
}

fn main() -> ExitCode {
    let (artifact, updates, save_checkpoint) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[nlhe_blake3_anchor] 参数错误：{e}");
            return ExitCode::from(2);
        }
    };
    // --save-checkpoint 模式：单 run 跑完后 save_checkpoint，不跑 3-run anchor 验证
    // （anchor 已在 F3 [报告] 起步 batch 1 vultr sweep 落地，本路径用于 GitHub Release
    // milestone artifact 生成）。
    let n_runs = if save_checkpoint.is_some() {
        1
    } else {
        REPEAT_COUNT
    };
    eprintln!("[nlhe_blake3_anchor] artifact = {}", artifact.display());
    eprintln!(
        "[nlhe_blake3_anchor] updates  = {} × {} run",
        updates, n_runs
    );
    eprintln!("[nlhe_blake3_anchor] seed     = 0x{FIXED_SEED:016x}");
    if let Some(p) = &save_checkpoint {
        eprintln!(
            "[nlhe_blake3_anchor] save_checkpoint = {} (single-run mode)",
            p.display()
        );
    }

    let table = match BucketTable::open(&artifact) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[nlhe_blake3_anchor] BucketTable::open 失败：{e:?}");
            return ExitCode::from(3);
        }
    };
    let body_hex = blake3_hex(&table.content_hash());
    if body_hex != V3_BODY_BLAKE3_HEX {
        eprintln!(
            "[nlhe_blake3_anchor] artifact body BLAKE3 mismatch:\n  actual   = {body_hex}\n  expected = {V3_BODY_BLAKE3_HEX}"
        );
        return ExitCode::from(3);
    }
    let shared = Arc::new(table);

    let mut hashes: Vec<[u8; 32]> = Vec::with_capacity(n_runs);
    let mut walls: Vec<f64> = Vec::with_capacity(n_runs);
    for run in 0..n_runs {
        eprintln!("[nlhe_blake3_anchor] === run #{run} ===");
        // 仅在最后一个 run 上 save_checkpoint（n_runs==1 时即 run #0；多 run 模式
        // 不 save，仅 BLAKE3 anchor 验证）。
        let save_path = if save_checkpoint.is_some() && run + 1 == n_runs {
            save_checkpoint.as_deref()
        } else {
            None
        };
        match run_one(Arc::clone(&shared), updates, save_path) {
            Ok((h, w)) => {
                eprintln!(
                    "[nlhe_blake3_anchor] run #{run} BLAKE3 = {} wall = {:.1}s",
                    blake3_hex(&h),
                    w
                );
                hashes.push(h);
                walls.push(w);
            }
            Err(e) => {
                eprintln!("[nlhe_blake3_anchor] run #{run} 失败：{e}");
                return ExitCode::from(4);
            }
        }
    }
    let first = hashes[0];
    let all_match = hashes.iter().all(|h| h == &first);
    if !all_match {
        eprintln!("[nlhe_blake3_anchor] D-362 byte-equal FAIL — runs not identical:");
        for (i, h) in hashes.iter().enumerate() {
            eprintln!("  run #{i} = {}", blake3_hex(h));
        }
        return ExitCode::from(5);
    }
    let total_updates = updates * n_runs as u64;
    let total_wall: f64 = walls.iter().sum();
    let avg_throughput = total_updates as f64 / total_wall.max(1e-9);
    let mode = if save_checkpoint.is_some() {
        "single-run milestone checkpoint mode (no 3-run anchor verification)"
    } else {
        "3-run anchor verification mode"
    };
    if n_runs >= 2 {
        println!("\nD-362 anchor PASS — {n_runs} runs BLAKE3 byte-equal ✓");
    } else {
        println!("\nMilestone checkpoint generated — 1 run completed (mode: {mode})");
    }
    println!("BLAKE3 = {}", blake3_hex(&first));
    println!("updates_per_run = {updates}");
    println!("runs = {n_runs}");
    for (i, w) in walls.iter().enumerate() {
        let tp = updates as f64 / w.max(1e-9);
        println!("  run #{i}: wall = {w:.1}s throughput = {tp:.0} update/s");
    }
    println!(
        "total_updates = {total_updates}  total_wall = {total_wall:.1}s  avg_throughput = {avg_throughput:.0} update/s"
    );
    if let Some(p) = &save_checkpoint {
        let file_size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        println!("checkpoint = {} ({} bytes)", p.display(), file_size);
    }
    ExitCode::from(0)
}

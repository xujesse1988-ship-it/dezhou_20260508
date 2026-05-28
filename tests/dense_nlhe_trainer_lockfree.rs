//! Dense **lock-free atomic** 并行路径
//! ([`DenseNlheEsMccfrTrainer::step_parallel_lockfree`]) 的回归门槛
//! （`docs/temp/nlhe_dense_parallel_merge_alternatives_2026_05_28.md` §A.9）。
//!
//! Lockfree 路径下 CAS race 顺序不定，跨 run 不再 byte-equal；不能复用
//! [`tests/dense_nlhe_trainer.rs::dense_step_parallel_byte_equal_hashmap`] 的合同。
//! 本文件给本路径自己的回归 anchor：
//!
//! - **`lockfree_smoke_no_panic`**：trainer 不 panic、update_count 正确。
//! - **`lockfree_self_consistency`**：average_strategy 每行 `Σ_a avg(a) ≈ 1.0`（CAS
//!   race 不破坏归一化语义，sum 偏 < 1e-9）。
//! - **`lockfree_avg_strategy_close_to_hashmap`**：与 HashMap deterministic 路径
//!   同 seed 跑相同总 update 数后，traverser 已访问 infoset 上 average_strategy
//!   L∞ < 5e-3（短跑 noise 宽松门槛）。
//!
//! 全部 `#[ignore]`：dense 两表 4.62 GiB 满分配 + lockfree 8 worker 并行写入需
//! ≥ 8 GiB 机器（vultr 7.7 GiB 边缘可跑短跑；HashMap 对照测试需 ≥ 10 GiB）。
//! 沿用 [`tests/dense_nlhe_trainer.rs`] 的 v4 artifact skip 兜底——v4 artifact
//! 不在机器上时 eprintln + return（不 assert）。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, ChaCha20Rng, InfoSetId, RngSource};

/// 候选 v4 artifact 路径（相对 repo root）。沿用 `dense_nlhe_trainer.rs`。
const V3_ARTIFACT_CANDIDATES: &[&str] = &[
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin",
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v4.bin",
];

/// 驱动 trainer step 的 base seed（多 worker rng pool 每 tid 派生独立 seed）。
const RNG_SEED: u64 = 0x44_4E_4C_4B_46_5F_42_45; // "DNLKF_BE"
/// trainer master seed（仅派生 checkpoint rng_substream_seed 占位，step 不消费）。
const MASTER_SEED: u64 = 0x44_4E_4C_4B_46_5F_4D_53; // "DNLKF_MS"

/// 加载任一可用 v4 artifact；缺失 → eprintln + `None`（skip）。
fn load_bucket_table_or_skip() -> Option<Arc<BucketTable>> {
    for path in V3_ARTIFACT_CANDIDATES {
        let p = PathBuf::from(path);
        if !p.exists() {
            continue;
        }
        match BucketTable::open(&p) {
            Ok(t) => return Some(Arc::new(t)),
            Err(e) => eprintln!("skip-candidate: BucketTable::open({path}) 失败：{e:?}"),
        }
    }
    eprintln!(
        "skip: 无 v4 bucket table artifact（候选 {V3_ARTIFACT_CANDIDATES:?}）；\
         dev box / vultr / AWS host 有 artifact 时跑。"
    );
    None
}

/// 建 `n` 个独立 `ChaCha20Rng`（per-tid nonce），同 `base` 下两次调用产同 pool。
fn build_rng_pool(base: u64, n: usize) -> Vec<Box<dyn RngSource>> {
    (0..n as u64)
        .map(|tid| {
            let seeded = base.wrapping_add(0xDEAD_BEEF_u64.wrapping_mul(tid + 1));
            Box::new(ChaCha20Rng::from_seed(seeded)) as Box<dyn RngSource>
        })
        .collect()
}

/// `step_parallel_lockfree` 短跑 smoke：不 panic、update_count 等于
/// `n_threads × batch_per_worker × n_calls`，trainer 不 abort。
#[test]
#[ignore = "dense 两表满分配 4.62 GiB 虚拟空间；release --ignored 单独跑"]
fn lockfree_smoke_no_panic() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let mut trainer = DenseNlheEsMccfrTrainer::new(game, MASTER_SEED);

    let n_threads = 4;
    let batch_per_worker = 16;
    let n_calls = 10; // 4 × 16 × 10 = 640 update（smoke）
    let mut pool = build_rng_pool(RNG_SEED, n_threads);
    for _ in 0..n_calls {
        trainer
            .step_parallel_lockfree(&mut pool, n_threads, batch_per_worker)
            .expect("step_parallel_lockfree must not error");
    }
    assert_eq!(
        trainer.update_count(),
        (n_threads * batch_per_worker * n_calls) as u64,
        "update_count 应等于 n_threads × batch_per_worker × n_calls"
    );
    eprintln!(
        "[lockfree smoke] {} update OK（{} thread × B={} × {} call）",
        trainer.update_count(),
        n_threads,
        batch_per_worker,
        n_calls
    );
}

/// average_strategy 每行 `Σ_a avg(a) ≈ 1.0`：lockfree 路径 CAS race 顺序不定，
/// 但归一化语义（`avg(a) = S(a) / Σ_b S(b)`）保持——任何 traverser 访问过的行
/// 都应满足 sum 偏 ≤ 1e-9。未访问行退化均匀分布也满足。
///
/// 用 HashMap trainer 同 seed lockstep 仅作**采集 traverser 已访问 infoset key 集**
/// （dense 表是扁平数组，无 key 枚举入口）；不比较两路径数值。
#[test]
#[ignore = "dense + HashMap key 采集，峰值 ~7 GiB；release --ignored 单独跑"]
fn lockfree_self_consistency() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let game_dense = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let game_hm = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("hashmap game");

    let mut dense = DenseNlheEsMccfrTrainer::new(game_dense, MASTER_SEED);
    let mut hm: EsMccfrTrainer<SimplifiedNlheGame> = EsMccfrTrainer::new(game_hm, MASTER_SEED);

    let n_threads = 4;
    let batch_per_worker = 16;
    let n_calls = 16; // 4 × 16 × 16 = 1024 update
    let mut pool_dense = build_rng_pool(RNG_SEED, n_threads);
    let mut pool_hm = build_rng_pool(RNG_SEED, n_threads);
    for _ in 0..n_calls {
        dense
            .step_parallel_lockfree(&mut pool_dense, n_threads, batch_per_worker)
            .expect("dense step_parallel_lockfree");
        hm.step_parallel(&mut pool_hm, n_threads, batch_per_worker)
            .expect("hashmap step_parallel");
    }

    let visited: Vec<InfoSetId> = hm.strategy_sum().inner().keys().copied().collect();
    assert!(
        visited.len() > 500,
        "仅访问 {} 个 infoset，样本太少",
        visited.len()
    );
    let mut checked = 0usize;
    for &info in &visited {
        let avg = dense.average_strategy(info);
        if avg.is_empty() {
            // dense touched bit 未置位（仅作为非-traverser 路过；见
            // `DenseNlheEsMccfrTrainer::current_strategy` doc）。跳过——HashMap
            // 路径返回 uniform，dense 返回空，逻辑等价。
            continue;
        }
        let sum: f64 = avg.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1.0e-9,
            "lockfree average_strategy sum 偏 @ info {:#x}: sum={sum:.17} avg={avg:?}",
            info.raw()
        );
        for (i, &p) in avg.iter().enumerate() {
            assert!(
                (0.0..=1.0 + 1.0e-12).contains(&p),
                "lockfree average_strategy 越界 @ info {:#x} action {i}: p={p}",
                info.raw()
            );
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "至少应该校验过 1 个已 touch infoset（visited={}）",
        visited.len()
    );
    eprintln!(
        "[lockfree self-consistency] {checked}/{} visited infoset sum ≈ 1.0 ✓",
        visited.len()
    );
}

/// lockfree 路径与 HashMap deterministic step_parallel 在**相同总 update / 相同
/// rng pool seed** 下的 average_strategy L∞ 距离。CAS race 让两路径同 seed 不再
/// byte-equal（σ 读取的 cell snapshot 随机），但短跑收敛趋势应同档——L∞ < 5e-3
/// 是宽松门槛，命中说明 lockfree 不发散。
#[test]
#[ignore = "dense + HashMap 两套表 + 短跑收敛对照，峰值 ~7 GiB；release --ignored 单独跑"]
fn lockfree_avg_strategy_close_to_hashmap() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let game_dense = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let game_hm = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("hashmap game");

    let mut dense = DenseNlheEsMccfrTrainer::new(game_dense, MASTER_SEED);
    let mut hm: EsMccfrTrainer<SimplifiedNlheGame> = EsMccfrTrainer::new(game_hm, MASTER_SEED);

    let n_threads = 4;
    let batch_per_worker = 16;
    let n_calls = 30; // 4 × 16 × 30 = 1920 update（与 byte-equal test 同档）
    let mut pool_dense = build_rng_pool(RNG_SEED, n_threads);
    let mut pool_hm = build_rng_pool(RNG_SEED, n_threads);
    for _ in 0..n_calls {
        dense
            .step_parallel_lockfree(&mut pool_dense, n_threads, batch_per_worker)
            .expect("dense step_parallel_lockfree");
        hm.step_parallel(&mut pool_hm, n_threads, batch_per_worker)
            .expect("hashmap step_parallel");
    }
    assert_eq!(dense.update_count(), hm.update_count());

    let visited: Vec<InfoSetId> = hm.strategy_sum().inner().keys().copied().collect();
    assert!(
        visited.len() > 500,
        "仅访问 {} 个 infoset，样本太少",
        visited.len()
    );
    let mut worst_l_inf: f64 = 0.0;
    let mut compared = 0usize;
    for &info in &visited {
        let avg_dense = dense.average_strategy(info);
        let avg_hm = hm.average_strategy(&info);
        if avg_dense.is_empty() || avg_dense.len() != avg_hm.len() {
            continue;
        }
        let l_inf = avg_dense
            .iter()
            .zip(&avg_hm)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f64, f64::max);
        if l_inf > worst_l_inf {
            worst_l_inf = l_inf;
        }
        compared += 1;
    }
    assert!(compared > 0, "无可比较的 infoset");
    // 宽松门槛：lockfree 与 deterministic 路径短跑后 avg_strategy 不应大幅发散。
    assert!(
        worst_l_inf < 5.0e-3,
        "lockfree vs HashMap avg_strategy L∞={worst_l_inf:e} 超 5e-3（compared={compared}）"
    );
    eprintln!("[lockfree vs HashMap] {compared} visited infoset L∞={worst_l_inf:e} (< 5e-3) ✓");
}

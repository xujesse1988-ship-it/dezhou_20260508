//! Phase 2（`docs/temp/nlhe_dense_infoset_table_plan_2026_05_26.md`）：
//! `DenseNlheEsMccfrTrainer` 与 HashMap `EsMccfrTrainer<SimplifiedNlheGame>` 在
//! **同 seed 单线程短跑**下的 byte-equal 对照。
//!
//! 两个 trainer 各自从同一 master seed 起、各用一个 `ChaCha20Rng::from_seed(同 seed)`
//! 驱动 `step`，逐步 lockstep。dense 与 HashMap 的 recurse 结构 / 采样 / rng 消费完全
//! 一致 → 同一 sampled trajectory；每 infoset 的 regret / strategy_sum 累积 f64 序列
//! 一致 → `current_strategy` / `average_strategy` 逐位（`f64::to_bits`）相等。
//!
//! **内存 + 运行**：dense 两表满分配 ~4.6 GiB（当前 119.7M profile），故 `#[ignore]`。
//! 在 vultr（7.7 GiB）跑：
//! ```bash
//! cargo test --release --test dense_nlhe_trainer -- --ignored --test-threads=1
//! ```
//! 两个 scenario（vanilla / LCFR）在**单个** test fn 内顺序跑，先 drop 再建下一对，
//! 峰值只压一对（即使 test-threads 误设 >1 也不会叠加）。
//!
//! **artifact**：走真实存在的 `..._schemav3.bin`（注意：`cfr_simplified_nlhe.rs` 的
//! 常量指向不存在的 `..._v3.bin`，会 skip——本文件特意用 schemav3 以确保真跑）。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, ChaCha20Rng, InfoSetId};

/// 候选 v3 artifact 路径（相对 repo root）。优先真实存在的 `schemav3`。
const V3_ARTIFACT_CANDIDATES: &[&str] = &[
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav3.bin",
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin",
];

/// 外部 rng seed（驱动 `step` 的 randomness 来源；两 trainer 各起一个同 seed rng）。
const RNG_SEED: u64 = 0x44_4E_53_5F_42_45_51_00; // "DNS_BEQ\0"

/// trainer master seed（仅派生 checkpoint rng_substream_seed 占位，step 不消费）。
const MASTER_SEED: u64 = 0x44_4E_53_5F_4D_53_54_00; // "DNS_MST\0"

/// 加载任一可用 v3 artifact；缺失 / 打不开 / schema 不符 → eprintln + `None`（skip）。
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
        "skip: 无 v3 bucket table artifact（候选 {V3_ARTIFACT_CANDIDATES:?}）；\
         本地 dev box / vultr / AWS host 有 artifact 时跑。"
    );
    None
}

/// f64 slice 逐位相等（`to_bits`）。长度不等直接 false。
fn bits_eq(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// 跑一个 scenario：建 dense + HashMap 两 trainer（同 seed），lockstep `updates` 步，
/// 对所有 HashMap 已访问 infoset 比较 `average_strategy` / `current_strategy` byte-equal。
/// 返回 (已访问 infoset 数, 比较通过数)。trainer 在函数返回时 drop（释放 ~4.6 GiB）。
fn run_scenario(bucket_table: &Arc<BucketTable>, lcfr_period: Option<u64>, updates: u64) -> usize {
    let game_dense = SimplifiedNlheGame::new(Arc::clone(bucket_table)).expect("v3 game (dense)");
    let game_hm = SimplifiedNlheGame::new(Arc::clone(bucket_table)).expect("v3 game (hashmap)");

    let mut dense = DenseNlheEsMccfrTrainer::new(game_dense, MASTER_SEED);
    let mut hm: EsMccfrTrainer<SimplifiedNlheGame> = EsMccfrTrainer::new(game_hm, MASTER_SEED);
    if let Some(p) = lcfr_period {
        dense = dense.with_lcfr_period(p);
        hm = hm.with_lcfr_period(p);
    }

    let mut rng_dense = ChaCha20Rng::from_seed(RNG_SEED);
    let mut rng_hm = ChaCha20Rng::from_seed(RNG_SEED);
    for _ in 0..updates {
        dense.step(&mut rng_dense).expect("dense step");
        hm.step(&mut rng_hm).expect("hashmap step");
    }
    assert_eq!(dense.update_count(), updates);
    assert_eq!(hm.update_count(), updates);

    // 遍历 HashMap 已访问 infoset（strategy_sum keys），逐位对照。
    let visited: Vec<InfoSetId> = hm.strategy_sum().inner().keys().copied().collect();
    assert!(
        visited.len() > 500,
        "scenario(lcfr={lcfr_period:?}) 仅访问 {} 个 infoset，样本太少不足以验证",
        visited.len()
    );
    for &info in &visited {
        let avg_hm = hm.average_strategy(&info);
        let avg_dense = dense.average_strategy(info);
        assert!(
            bits_eq(&avg_hm, &avg_dense),
            "average_strategy byte mismatch @ info {:#x} (lcfr={lcfr_period:?}): hm={avg_hm:?} dense={avg_dense:?}",
            info.raw()
        );
        let cur_hm = hm.current_strategy(&info);
        let cur_dense = dense.current_strategy(info);
        assert!(
            bits_eq(&cur_hm, &cur_dense),
            "current_strategy byte mismatch @ info {:#x} (lcfr={lcfr_period:?})",
            info.raw()
        );
    }

    visited.len()
}

/// 核心断言：dense vs HashMap ES-MCCFR 在 vanilla 与 LCFR 两 scenario 下 byte-equal。
/// 顺序跑两 scenario，峰值只压一对 dense 表。
#[test]
#[ignore = "dense 两表满分配 ~4.6 GiB；release + --ignored + --test-threads=1 在 vultr 跑"]
fn dense_es_mccfr_byte_equal_hashmap() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };

    // scenario 1：vanilla ES-MCCFR（无 LCFR）。
    let n1 = run_scenario(&bucket_table, None, 5_000);
    eprintln!("[dense byte-equal] vanilla: {n1} infoset byte-equal ✓");

    // scenario 2：LCFR period=1000，5000 update 跨 5 个 period boundary（regret +
    // strategy_sum 双 rescale），验证 rescale 路径也 byte-equal。
    let n2 = run_scenario(&bucket_table, Some(1_000), 5_000);
    eprintln!("[dense byte-equal] LCFR(period=1000): {n2} infoset byte-equal ✓");
}

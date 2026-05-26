//! Phase 2（`docs/temp/nlhe_dense_infoset_table_plan_2026_05_26.md`）：
//! `DenseNlheEsMccfrTrainer` 与 HashMap `EsMccfrTrainer<SimplifiedNlheGame>` 在
//! **同 seed 单线程短跑**下的 byte-equal 对照。
//!
//! 两个 trainer 各自从同一 master seed 起、各用一个 `ChaCha20Rng::from_seed(同 seed)`
//! 驱动 `step`，逐步 lockstep。dense 与 HashMap 的 recurse 结构 / 采样 / rng 消费完全
//! 一致 → 同一 sampled trajectory；每 infoset 的 regret / strategy_sum 累积 f64 序列
//! 一致 → `current_strategy` / `average_strategy` 逐位（`f64::to_bits`）相等。
//!
//! **内存 + 运行**：dense 两表 *满分配* 4.62 GiB（当前 119.7M profile）虚拟空间，但
//! `vec![0.0; N]` 走 `calloc` 惰性提交——RSS 只随**真正写过的 slot** 增长：
//! - **vanilla**（`rescale_all` 不触发）：dense 只提交访问过的 slot（稀疏，但
//!   ES-MCCFR traverser 节点全 fan-out 会铺开大量 infoset）→ 实测峰值 **~3.8 GiB**。
//!   vultr（7.7 GiB）充裕，是 dense recurse byte-equal 的主验证。
//! - **LCFR**：`rescale_all` 扫**整张表** → 提交全部 4.62 GiB dense；再叠 HashMap
//!   对照表（traverser fan-out 下很快饱和到 ~2 GiB，**与 update 数几乎无关**——1000
//!   与 5000 update 实测峰值都是 ~7.33 GiB）+ bucket 0.55 GiB → 峰值 **~7.33 GiB**。
//!   这逼近 vultr 7.7 GiB 上限（idle 时实测 0 swap 通过，但无余量）。dense 与 HashMap
//!   对照表必须同时在场才能比，glibc 又不把 HashMap 释放的 arena 还给 OS，**没法靠
//!   先 drop 再分配压下来**——这条路属 **≥ ~10 GiB 机器**，不是 vultr-safe。
//!
//! 拆成两个 `#[ignore]` test，**各自单独一次 `cargo test` 调用**跑（同进程顺序跑会
//! 因 glibc 不及时还内存叠加）：
//! ```bash
//! # vultr 安全：
//! cargo test --release --test dense_nlhe_trainer dense_es_mccfr_byte_equal_hashmap_vanilla -- --ignored
//! # 需 ≥ ~10 GiB 机器（vultr idle 勉强过，无余量）：
//! cargo test --release --test dense_nlhe_trainer dense_es_mccfr_byte_equal_hashmap_lcfr     -- --ignored
//! ```
//! **目标扩张 profile（359.6M / 两表 13.48 GiB）vultr 装不下**，需 32–64 GB 机器
//! （见 plan §验证门槛）。LCFR rescale byte-equal 本身已由 Phase 1 `nlhe_dense` 单测
//! （`rescale_all` vs HashMap，合成 delta）覆盖；本 LCFR 集成 test 额外验的是 trainer
//! 把 `maybe_lcfr_rescale` 接在正确 boundary 上。
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

/// vanilla ES-MCCFR（无 LCFR）byte-equal。RSS 只随访问过的 slot 增长（稀疏，
/// 实测 ~1.5 GiB），vultr 充裕。
#[test]
#[ignore = "dense 两表满分配 4.62 GiB 虚拟空间（vanilla RSS ~1.5 GiB）；release --ignored 单独跑"]
fn dense_es_mccfr_byte_equal_hashmap_vanilla() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let n = run_scenario(&bucket_table, None, 5_000);
    eprintln!("[dense byte-equal] vanilla: {n} infoset byte-equal ✓");
}

/// LCFR period=1000 byte-equal：5000 update 跨 5 个 period boundary，regret +
/// strategy_sum 双 rescale。**峰值 ~7.33 GiB**（full-dense rescale 全提交 + HashMap
/// 对照表 ~2 GiB + bucket，与 update 数几乎无关），需 ≥ ~10 GiB 机器；vultr idle
/// 勉强过、无余量。**单独一次 cargo test 调用跑**。
#[test]
#[ignore = "LCFR：峰值 ~7.33 GiB（>vultr 余量），需 ≥ ~10 GiB 机器；release --ignored 单独跑"]
fn dense_es_mccfr_byte_equal_hashmap_lcfr() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let n = run_scenario(&bucket_table, Some(1_000), 5_000);
    eprintln!("[dense byte-equal] LCFR(period=1000): {n} infoset byte-equal ✓");
}

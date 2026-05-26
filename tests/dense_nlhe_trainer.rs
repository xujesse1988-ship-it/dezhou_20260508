//! Phase 2（`docs/temp/nlhe_dense_infoset_table_plan_2026_05_26.md`）：
//! `DenseNlheEsMccfrTrainer` 与 HashMap `EsMccfrTrainer<SimplifiedNlheGame>` 在
//! **同 seed 单线程短跑**下的 byte-equal 对照。
//!
//! 两个 trainer 各自从同一 master seed 起、各用一个 `ChaCha20Rng::from_seed(同 seed)`
//! 驱动 `step`，逐步 lockstep。dense 与 HashMap 的 recurse 结构 / 采样 / rng 消费完全
//! 一致 → 同一 sampled trajectory；每 infoset 的 regret / strategy_sum 累积 f64 序列
//! 一致 → `current_strategy` / `average_strategy` 逐位（`f64::to_bits`）相等。
//!
//! **byte-equal 对照的覆盖范围 = `hm.strategy_sum().inner().keys()`（traverser 访问过的
//! infoset）**。这结构上**排除**了「仅作为非-traverser 访问过、从未作为 traverser 遍历」
//! 的 infoset：那个集合上 dense 的 public query 与 HashMap 有已知偏离（HashMap 经
//! get_or_init 返回 uniform、dense 因 touched 未置位返回空 `Vec`，见
//! `DenseNlheEsMccfrTrainer::current_strategy` doc）。这些是零信息节点（regret/strategy_sum
//! 恒 0），训练值数组两路径仍逐位相同，故不影响本对照的结论；但本测试**不**断言该集合。
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
//! **artifact**：走 `..._schemav4.bin`（v4 = v3 layout + shape-major canonical id
//! 编号）。旧 v3 artifact 已失效；v4 artifact 重算出来前本文件 skip（无可加载表）。

use std::path::PathBuf;
use std::sync::Arc;

use poker::training::nlhe::SimplifiedNlheGame;
use poker::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;
use poker::training::{EsMccfrTrainer, Trainer};
use poker::{BucketTable, ChaCha20Rng, InfoSetId, RngSource};

/// 候选 v4 artifact 路径（相对 repo root）。
const V3_ARTIFACT_CANDIDATES: &[&str] = &[
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin",
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v4.bin",
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

// ===========================================================================
// Phase 3：并行 step_parallel byte-equal 对照
// ===========================================================================

/// 建 `n` 个独立 `ChaCha20Rng`（per-tid nonce），同 `base` 下两次调用产同 pool
/// （让 dense / HashMap 两路径吃完全相同的 randomness）。
fn build_rng_pool(base: u64, n: usize) -> Vec<Box<dyn RngSource>> {
    (0..n as u64)
        .map(|tid| {
            let seeded = base.wrapping_add(0xDEAD_BEEF_u64.wrapping_mul(tid + 1));
            Box::new(ChaCha20Rng::from_seed(seeded)) as Box<dyn RngSource>
        })
        .collect()
}

/// dense `step_parallel` 与 HashMap `EsMccfrTrainer::step_parallel` 在**同 rng pool /
/// 同 n_threads / 同 batch_per_worker** 下逐位相等（Phase 3 §并行语义 最强对照）。
///
/// 两路径结构一一对应：worker 读 pre-dispatch shared regret snapshot → 同 σ → 同
/// trajectory（同 rng）→ 同 delta；merge 按 tid 升序 × push 顺序 → 每个 cell 的 f64
/// 加法序列完全一致 → strategy snapshot byte-equal。**峰值 ~7 GiB**（dense + HashMap
/// 两套表并存），需 ≥ ~10 GiB 机器。
#[test]
#[ignore = "dense + HashMap 并行两套表并存峰值 ~7 GiB，需 ≥ ~10 GiB 机器；release --ignored 单独跑"]
fn dense_step_parallel_byte_equal_hashmap() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let game_dense = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let game_hm = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("hashmap game");
    let mut dense = DenseNlheEsMccfrTrainer::new(game_dense, MASTER_SEED);
    let mut hm: EsMccfrTrainer<SimplifiedNlheGame> = EsMccfrTrainer::new(game_hm, MASTER_SEED);

    let n_threads = 4;
    let batch_per_worker = 8;
    let n_calls = 60; // 4 × 8 × 60 = 1920 update
    let mut pool_dense = build_rng_pool(RNG_SEED, n_threads);
    let mut pool_hm = build_rng_pool(RNG_SEED, n_threads);
    for _ in 0..n_calls {
        dense
            .step_parallel(&mut pool_dense, n_threads, batch_per_worker)
            .expect("dense step_parallel");
        hm.step_parallel(&mut pool_hm, n_threads, batch_per_worker)
            .expect("hashmap step_parallel");
    }
    assert_eq!(dense.update_count(), hm.update_count());
    assert_eq!(
        dense.update_count(),
        (n_threads * batch_per_worker * n_calls) as u64
    );

    let visited: Vec<InfoSetId> = hm.strategy_sum().inner().keys().copied().collect();
    assert!(
        visited.len() > 500,
        "并行短跑仅访问 {} 个 infoset，样本太少",
        visited.len()
    );
    for &info in &visited {
        assert!(
            bits_eq(&hm.average_strategy(&info), &dense.average_strategy(info)),
            "parallel average_strategy byte mismatch @ info {:#x}",
            info.raw()
        );
        assert!(
            bits_eq(&hm.current_strategy(&info), &dense.current_strategy(info)),
            "parallel current_strategy byte mismatch @ info {:#x}",
            info.raw()
        );
    }
    eprintln!(
        "[dense parallel byte-equal] {} infoset byte-equal ✓（{} update）",
        visited.len(),
        dense.update_count()
    );
}

// ===========================================================================
// Phase 4：dense checkpoint v3 集成
// ===========================================================================

/// 唯一临时文件路径（同 checkpoint_round_trip.rs 风格，避免依赖 tempfile crate 在
/// 集成 test crate 的可见性）。
fn unique_temp_path(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "poker_dense_ckpt_{label}_{}_{nanos}.bin",
        std::process::id()
    ));
    p
}

fn cleanup(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
}

/// dense save → load roundtrip：load 后 `update_count` + 所有已访问 infoset 的
/// `average_strategy` / `current_strategy` 与原 trainer byte-equal。**LCFR 元数据**
/// 也随 checkpoint 恢复（period=500，2000 update 跨 4 个 boundary）。
///
/// 用并行 HashMap lockstep 仅为**采集已访问 infoset key 集**（dense 表是扁平数组，
/// 无 key 枚举入口）——Phase 2 已证 dense 与 HashMap 同 seed 访问同一批 infoset。
/// 峰值 ~9 GiB（采集后 drop HashMap，再 load 第二个 dense）。
#[test]
#[ignore = "dense roundtrip：2 套 dense 表 + HashMap key 采集，峰值 ~9 GiB；release --ignored 单独跑"]
fn dense_checkpoint_roundtrip_preserves_strategy() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let updates = 2_000u64;
    let lcfr_period = 500u64;

    let game_dense = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let game_hm = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("hashmap game");
    let mut dense =
        DenseNlheEsMccfrTrainer::new(game_dense, MASTER_SEED).with_lcfr_period(lcfr_period);
    let mut hm: EsMccfrTrainer<SimplifiedNlheGame> =
        EsMccfrTrainer::new(game_hm, MASTER_SEED).with_lcfr_period(lcfr_period);
    let mut rng_dense = ChaCha20Rng::from_seed(RNG_SEED);
    let mut rng_hm = ChaCha20Rng::from_seed(RNG_SEED);
    for _ in 0..updates {
        dense.step(&mut rng_dense).expect("dense step");
        hm.step(&mut rng_hm).expect("hashmap step");
    }
    // 采集 key 集后释放 HashMap（降峰值），保留 dense 做 save。
    let visited: Vec<InfoSetId> = hm.strategy_sum().inner().keys().copied().collect();
    assert!(visited.len() > 500, "仅访问 {} 个 infoset", visited.len());
    drop(hm);

    let path = unique_temp_path("roundtrip");
    dense.save_checkpoint(&path).expect("dense save_checkpoint");

    let game_load = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("load game");
    let loaded =
        DenseNlheEsMccfrTrainer::load_checkpoint(&path, game_load).expect("dense load_checkpoint");
    assert_eq!(
        loaded.update_count(),
        dense.update_count(),
        "update_count roundtrip"
    );

    for &info in &visited {
        assert!(
            bits_eq(
                &dense.average_strategy(info),
                &loaded.average_strategy(info)
            ),
            "roundtrip average_strategy byte mismatch @ info {:#x}",
            info.raw()
        );
        assert!(
            bits_eq(
                &dense.current_strategy(info),
                &loaded.current_strategy(info)
            ),
            "roundtrip current_strategy byte mismatch @ info {:#x}",
            info.raw()
        );
    }
    cleanup(&path);
    eprintln!(
        "[dense ckpt roundtrip] {} infoset byte-equal ✓（{} update, lcfr={lcfr_period}）",
        visited.len(),
        loaded.update_count()
    );
}

/// 旧 HashMap v2 checkpoint → dense 单向加载 byte-equal：HashMap 跑 2000 update →
/// `save_checkpoint`（v2）→ `from_hashmap_checkpoint` 填 dense → 对所有已访问 infoset
/// `average_strategy` / `current_strategy` 与 HashMap trainer byte-equal。验证
/// plan §Checkpoint 兼容策略「HashMap → dense」无损。
#[test]
#[ignore = "hashmap→dense：dense 满分配 4.62 GiB + HashMap ~2 GiB，需 ≥ ~8 GiB 机器；release --ignored 单独跑"]
fn hashmap_checkpoint_to_dense_byte_equal() {
    let Some(bucket_table) = load_bucket_table_or_skip() else {
        return;
    };
    let game = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("hashmap game");
    let mut hm: EsMccfrTrainer<SimplifiedNlheGame> = EsMccfrTrainer::new(game, MASTER_SEED);
    let mut rng = ChaCha20Rng::from_seed(RNG_SEED);
    for _ in 0..2_000 {
        hm.step(&mut rng).expect("hashmap step");
    }
    let visited: Vec<InfoSetId> = hm.strategy_sum().inner().keys().copied().collect();
    assert!(visited.len() > 500, "仅访问 {} 个 infoset", visited.len());

    let path = unique_temp_path("hm_v2");
    hm.save_checkpoint(&path)
        .expect("HashMap v2 save_checkpoint");

    let game_dense = SimplifiedNlheGame::new(Arc::clone(&bucket_table)).expect("dense game");
    let dense = DenseNlheEsMccfrTrainer::from_hashmap_checkpoint(&path, game_dense)
        .expect("from_hashmap_checkpoint");
    assert_eq!(dense.update_count(), hm.update_count(), "update_count 一致");

    for &info in &visited {
        assert!(
            bits_eq(&hm.average_strategy(&info), &dense.average_strategy(info)),
            "hashmap→dense average_strategy byte mismatch @ info {:#x}",
            info.raw()
        );
        assert!(
            bits_eq(&hm.current_strategy(&info), &dense.current_strategy(info)),
            "hashmap→dense current_strategy byte mismatch @ info {:#x}",
            info.raw()
        );
    }
    cleanup(&path);
    eprintln!(
        "[hashmap→dense] {} infoset byte-equal ✓（{} update）",
        visited.len(),
        dense.update_count()
    );
}

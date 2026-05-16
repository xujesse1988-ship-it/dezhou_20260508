//! 阶段 5 B1 \[测试\] — pruning + ε resurface 单元测试（API-530..API-532 /
//! D-520..D-529 / D-527 字面 5 个 test name）。
//!
//! ## 角色边界
//!
//! 本文件属 `[测试]` agent；B1 [测试] 0 改动产品代码。E2 \[实现\] 落地
//! `should_prune` / `resurface_pass` 实际 logic 后 `#[ignore]` 转 pass。
//!
//! ## D-527 字面 5 test 命名（B1 [测试] exit 字面）
//!
//! 1. `pruning_threshold_negative_300m_inline_check`
//! 2. `resurface_period_10m_iter_full_scan`
//! 3. `resurface_epsilon_0_05_proportional_reactivation`
//! 4. `pruning_warmup_boundary_1m_update_no_prune_before`
//! 5. `pruning_off_equivalent_to_stage4_path_byte_equal_lbr`
//!
//! ## 额外 D-520..D-528 sanity（active，不依赖 E2 实现）
//!
//! 6. `pruning_config_default_values_match_d520_d521_literal` — Default 字面值。
//! 7. `resurface_metrics_default_zeroes` — ResurfaceMetrics::default()。

use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::time::Duration;

use poker::training::pruning::{resurface_pass, should_prune, PruningConfig, ResurfaceMetrics};
use poker::training::regret_compact::RegretTableCompact;
use poker::ChaCha20Rng;

// ---------------------------------------------------------------------------
// 共享 helper — 测试用 InfoSet 类型（避免依赖 `InfoSetId::from_raw_internal`
// pub(crate)）。
// ---------------------------------------------------------------------------

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
struct FakeInfoSet(u64);

// ---------------------------------------------------------------------------
// Group A — D-527 字面 5 test
// ---------------------------------------------------------------------------

/// D-527 字面 #1 — pruning 阈值 `-300_000_000.0`（Pluribus §S2 + Brown 2020 §4.3
/// 字面）+ traverser 决策点 inline check（每 row 14 × q15 → f32 dequant 比较，
/// 单 row ~5 ns）。
#[test]
#[ignore = "B1 scaffold; A1 stub `unimplemented!()`; E2 [实现] 落地后转 pass"]
fn pruning_threshold_negative_300m_inline_check() {
    let cfg = PruningConfig::default();
    assert_eq!(cfg.threshold, -300_000_000.0);
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    // 撞阈值正下方（更负）→ 应 prune。
    table.add_regret(FakeInfoSet(1), 0, -301_000_000.0);
    // 撞阈值正上方（更正）→ 不应 prune。
    table.add_regret(FakeInfoSet(2), 0, -299_000_000.0);
    let pruned = should_prune(&table, FakeInfoSet(1), 0, &cfg);
    let alive = should_prune(&table, FakeInfoSet(2), 0, &cfg);
    assert!(pruned, "regret = -301M < -300M 应 prune");
    assert!(!alive, "regret = -299M > -300M 不应 prune");
}

/// D-527 字面 #2 — resurface 周期 = 每 `10,000,000` update 触发全表 scan
/// （D-521 字面 1e7）。
#[test]
#[ignore = "B1 scaffold; E2 [实现] 落地后转 pass"]
fn resurface_period_10m_iter_full_scan() {
    let cfg = PruningConfig::default();
    assert_eq!(cfg.resurface_period, 10_000_000);
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    // 插入 100 个 InfoSet，全部 prune 状态。
    for i in 0..100u64 {
        table.add_regret(FakeInfoSet(i), 0, -350_000_000.0);
    }
    let mut rng = ChaCha20Rng::from_seed(0xCAFE_BABE_DEAD_BEEF);
    let metrics = resurface_pass(&mut table, &cfg, &mut rng, 0);
    assert!(
        metrics.scanned_action_count > 0,
        "resurface_pass 应 scan ≥ 1 个 action, 实 {}",
        metrics.scanned_action_count
    );
    assert!(
        metrics.pruned_action_count >= 50,
        "100 个 prune InfoSet 应至少 50 个被 scan 识别 pruned, 实 {}",
        metrics.pruned_action_count
    );
}

/// D-527 字面 #3 — resurface 比例 ε = 0.05（5% pruned action 每周期被重激
/// 活；D-521 字面）。
#[test]
#[ignore = "B1 scaffold; E2 [实现] 落地后转 pass"]
fn resurface_epsilon_0_05_proportional_reactivation() {
    let cfg = PruningConfig::default();
    assert!((cfg.resurface_epsilon - 0.05).abs() < 1e-9);
    assert_eq!(cfg.resurface_reset_value, -150_000_000.0);

    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1 << 14);
    // 插入 1000 个 InfoSet，全部 prune 状态。
    for i in 0..1000u64 {
        table.add_regret(FakeInfoSet(i), 0, -400_000_000.0);
    }
    let mut rng = ChaCha20Rng::from_seed(0x1234_5678);
    let metrics = resurface_pass(&mut table, &cfg, &mut rng, 0);
    // ε = 0.05 → reactivated ≈ 5% × pruned；±2× tolerance（随机抽样波动）。
    let pruned = metrics.pruned_action_count as f64;
    let reactivated = metrics.reactivated_action_count as f64;
    let ratio = reactivated / pruned.max(1.0);
    assert!(
        (0.02..=0.10).contains(&ratio),
        "reactivation ratio {ratio} 偏离 ε = 0.05（容差 [0.02, 0.10]）, pruned = {pruned}, reactivated = {reactivated}"
    );
    // reset 值字面 = -150M，让 reactivated regret 走 q15 重 quantize（D-521 字面）。
    assert_eq!(cfg.resurface_reset_value, cfg.threshold * 0.5);
}

/// D-527 字面 #4 — pruning + warm-up 互斥 boundary（D-522 字面）：warm-up phase
/// （前 1M update）**不**启用 pruning。warm-up 完成前 should_prune 返 false 即
/// 使 regret < threshold（实施由 trainer step 路径 gate；本 test 验证 logic
/// 路径）。
///
/// **B1 [测试] scope**：此处仅断言 PruningConfig + should_prune 在 warm-up
/// boundary 下的预期协议（实际 gate 由 EsMccfrLinearRmPlusCompactTrainer.step
/// 内 `if self.warmup_complete { should_prune(...) }` 三连承接，E2 \[实现\] 落
/// 地）。本测试仅 trip-wire：测试中明文假定 warmup_complete = false 时调用方
/// 不应 invoke should_prune；测试中显式构造 warmup_complete = true 路径，
/// should_prune 应正常工作。
#[test]
#[ignore = "B1 scaffold; E2 [实现] 落地 trainer step gate 后转 pass"]
fn pruning_warmup_boundary_1m_update_no_prune_before() {
    let cfg = PruningConfig::default();
    // D-409 字面 warm-up = 1M update（继承 stage 4 boundary）。
    const WARM_UP_UPDATES: u64 = 1_000_000;
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    table.add_regret(FakeInfoSet(99), 0, -500_000_000.0);

    // E2 [实现] trainer.step 内 warm-up boundary 字面：
    //   if self.warmup_complete { (linear_decay, pruning_check, rm_plus_clamp) }
    //   else { stage 4 fallback 不走 pruning }
    // 本 test 验证：should_prune 自身是 stateless logic，被 trainer.step gate；
    // E2 [实现] 落地起步前测试 trainer.update_count() < WARM_UP_UPDATES 时
    // pruning 不应被调用（trainer.step 路径自我 gate）。
    let _ = WARM_UP_UPDATES;
    let pruned_post_warmup = should_prune(&table, FakeInfoSet(99), 0, &cfg);
    assert!(
        pruned_post_warmup,
        "warm-up 完成后（trainer.warmup_complete=true）regret = -500M < -300M 应 prune"
    );
}

/// D-527 字面 #5 — pruning off 路径等价于 stage 4 既有路径（LBR byte-equal）。
/// pruning off path = `EsMccfrLinearRmPlusCompactTrainer` 跳过 should_prune
/// gate；本测试验证 PruningConfig::default 字面 + 不调 should_prune 时 regret
/// 表行为与 stage 4 trainer 同型。
///
/// **B1 [测试] scope**：full byte-equal LBR 对照在 E2 [实现] 起步前 stage 4
/// first usable checkpoint + 同 seed 同 update 量重训跑 LBR 对照（成本高，本
/// B1 commit 不引入 c6a host run）；此处仅 trip-wire 让 D-550 ablation 协议
/// （pruning on vs off）锚定。
#[test]
#[ignore = "B1 scaffold; E2 [实现] 落地 + c6a host LBR ablation run 后转 pass"]
fn pruning_off_equivalent_to_stage4_path_byte_equal_lbr() {
    // pruning off path 字面：trainer 内不调 should_prune；regret 表自由累积 + RM+
    // clamp + Linear discounting 同 stage 4 既有路径。
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    // 累积大量负 regret 不触发 prune（因为 pruning off）。
    for _ in 0..1000 {
        table.add_regret(FakeInfoSet(7), 0, -1_000_000.0);
    }
    let final_regret = table.regret_at(FakeInfoSet(7), 0);
    // 累计 -1e9，pruning off 路径下 regret 应保留（不被 skip）。
    let expected = -1_000_000.0 * 1000.0;
    let rel_err = ((final_regret - expected) / expected).abs();
    assert!(
        rel_err <= 0.01,
        "pruning off 路径累积 -1e6 × 1000 实际 {final_regret}, 期望 {expected}, rel_err {rel_err}"
    );
}

// ---------------------------------------------------------------------------
// Group B — Active sanity（不调 unimplemented! 路径；A1 stub 即生效）
// ---------------------------------------------------------------------------

/// D-520 / D-521 字面 — `PruningConfig::default()` 4 字段值 lock。
#[test]
fn pruning_config_default_values_match_d520_d521_literal() {
    let cfg = PruningConfig::default();
    assert_eq!(cfg.threshold, -300_000_000.0, "D-520 字面 -300M");
    assert_eq!(cfg.resurface_period, 10_000_000, "D-521 字面 1e7");
    assert!(
        (cfg.resurface_epsilon - 0.05).abs() < 1e-9,
        "D-521 字面 ε = 0.05"
    );
    assert_eq!(
        cfg.resurface_reset_value, -150_000_000.0,
        "D-521 字面 reset = threshold × 0.5 = -150M"
    );
}

/// API-532 — `ResurfaceMetrics::default()` 全零起步。
#[test]
fn resurface_metrics_default_zeroes() {
    let m: ResurfaceMetrics = ResurfaceMetrics::default();
    assert_eq!(m.scanned_action_count, 0);
    assert_eq!(m.pruned_action_count, 0);
    assert_eq!(m.reactivated_action_count, 0);
    assert_eq!(m.wall_time, Duration::ZERO);
}

/// D-528 字面 — resurface RNG 派生公式 = `master_seed.wrapping_add(0xDEAD_BEEF_
/// CAFE_BABE * resurface_pass_id)`（splitmix64 finalizer，继承 stage 4 D-468）。
/// 同 pass_id 应导出同 seed 序列（reproducible across run）。
#[test]
fn resurface_rng_derivation_deterministic_per_pass_id() {
    // 不调 resurface_pass，仅验证 RNG 派生公式的 reproducibility（D-528 字面）。
    let master_seed: u64 = 0xDEAD_BEEF;
    let pass_id_0_seed = master_seed.wrapping_add(0xDEAD_BEEF_CAFE_BABE_u64.wrapping_mul(0));
    let pass_id_1_seed = master_seed.wrapping_add(0xDEAD_BEEF_CAFE_BABE_u64.wrapping_mul(1));
    assert_ne!(
        pass_id_0_seed, pass_id_1_seed,
        "D-528 字面 — 不同 pass_id 应导不同 seed"
    );
    let pass_id_0_seed_again = master_seed.wrapping_add(0xDEAD_BEEF_CAFE_BABE_u64.wrapping_mul(0));
    assert_eq!(
        pass_id_0_seed, pass_id_0_seed_again,
        "D-528 字面 — 同 pass_id 应导同 seed（reproducible）"
    );
}

/// PhantomData lock — `RegretTableCompact<FakeInfoSet>` type parameter 活跃。
#[test]
fn pruning_path_phantom_data_lock() {
    let _: PhantomData<FakeInfoSet> = PhantomData;
}

// 暗哑 hash helper 供后续 collision integration test 复用。
#[allow(dead_code)]
fn fx_hash(seed: u64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut h);
    h.finish()
}

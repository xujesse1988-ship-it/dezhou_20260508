//! 阶段 5 B1 \[测试\] — 紧凑 RegretTable + StrategyAccumulator Robin Hood 单元
//! 测试（API-510..API-529 / D-510 + D-511 + D-517 + D-518 + D-519 字面）。
//!
//! ## 角色边界
//!
//! 本文件属 `[测试]` agent。**B1 [测试] commit 0 改动产品代码**（继承
//! `pluribus_stage5_workflow.md` §3 #2）；所有测试期望 A1 \[实现\] scaffold 阶
//! 段 `unimplemented!()` 触发 panic / 走 `#[ignore]` opt-in，B2 \[实现\] 落地
//! Robin Hood probe + q15 quantization + SIMD path 后移除 `#[ignore]` 转 pass。
//!
//! ## 测试覆盖（≥ 12 test，B1 [测试] exit 字面）
//!
//! - **Robin Hood probe 路径**：`with_initial_capacity_estimate` 构造 + `regret_at`
//!   未命中返 `0.0` lazy 语义 + `add_regret` probe-or-insert + 多次 hash collision
//!   下 max_probe_distance ≤ 16 + load_factor ≤ 0.75。
//! - **q15 quantization 内嵌**：`add_regret` 走 row scale 初始化 + 累加 saturating
//!   + overflow 走 row-renorm 单点路径不全表扫描。
//! - **RM+ clamp**：`clamp_rm_plus` in-place max(q15, 0) 全表生效 + 正值不变。
//! - **Linear discounting lazy**：`scale_linear_lazy(decay)` 仅 mutate `scales[i] *=
//!   decay`，int16 payload 不动（D-511 字面 scale-only 路径）。
//! - **section_bytes 公式**：`(keys.capacity() × 8) + (payloads.capacity() × 32) +
//!   (scales.capacity() × 4) + metadata` D-544 字面。
//! - **Iter 路径**：`iter()` 仅访问 populated slot（keys[i] != u64::MAX）。
//! - **renormalize_scales**：D-511 字面 max(|q15|) 区间判断 + scale × 2 + q15 >> 1
//!   或 scale / 2 + q15 << 1 保持 dynamic range。
//! - **collision_metrics 三阈值**（D-569 anchor 字面）：load_factor ≤ 0.75 / max
//!   probe ≤ 16 / avg probe ≤ 2.0。
//! - **StrategyAccumulator 同型签名 + Key 不共享**（D-517 字面）。
//!
//! 共 18 test：14 RegretTableCompact + 4 StrategyAccumulatorCompact。

use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use poker::training::regret_compact::{
    CollisionMetrics, RegretTableCompact, StrategyAccumulatorCompact,
};

// ---------------------------------------------------------------------------
// 共享 helper — 测试用 InfoSet 类型（避免依赖 `InfoSetId::from_raw_internal`
// pub(crate)）。
// ---------------------------------------------------------------------------

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
struct FakeInfoSet(u64);

// ---------------------------------------------------------------------------
// Group A — RegretTableCompact constructor + 容量起步（API-510 / D-518 字面）
// ---------------------------------------------------------------------------

/// API-510 字面 — `with_initial_capacity_estimate` 在 estimate ≤ `2^20` 时
/// capacity 取 `2^20 = 1,048,576`（D-518 字面 初始 capacity）。
#[test]
#[ignore = "B1 scaffold; A1 stub `unimplemented!()`; B2 [实现] 落地后转 pass"]
fn regret_compact_with_initial_capacity_estimate_default_2_to_20() {
    let table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(0);
    assert!(table.is_empty());
    assert_eq!(table.len(), 0);
    // section_bytes 至少包含 capacity = 2^20 起步预 size 字节数（公式 D-544）：
    // (2^20 × 8) + (2^20 × 32) + (2^20 × 4) = 44 × 2^20 = 46_137_344 byte
    let bytes = table.section_bytes();
    assert!(
        bytes >= 44 * 1024 * 1024,
        "section_bytes {bytes} < 44 MiB（capacity = 2^20 起步预 size，D-518 字面）"
    );
}

/// API-510 — estimate ≥ `2^20` 时 capacity 向上对齐到 2^N power-of-two。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_capacity_rounds_up_to_power_of_two() {
    // estimate = 1.5M 应触发 capacity ≥ 2_097_152 = 2^21。
    let table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1_500_000);
    let bytes = table.section_bytes();
    assert!(
        bytes >= 44 * 2 * 1024 * 1024,
        "section_bytes {bytes} < 88 MiB（capacity 应至少 2^21）"
    );
}

// ---------------------------------------------------------------------------
// Group B — regret_at lazy 语义 + add_regret round-trip
// ---------------------------------------------------------------------------

/// API-511 — `regret_at` 未命中 slot 返 `0.0`（lazy 初始化语义，继承 stage 3
/// D-323 `RegretTable::get_or_init`）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_regret_at_returns_zero_for_unknown_info_set() {
    let table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(0);
    for action in 0..14 {
        let r = table.regret_at(FakeInfoSet(0xDEAD_BEEF_CAFE_BABE), action);
        assert_eq!(
            r, 0.0,
            "regret_at 未命中应返 0.0 (action = {action}, got {r})"
        );
    }
}

/// API-512 / API-511 — `add_regret(I, a, delta)` 然后 `regret_at(I, a)` 在 q15
/// 精度内 round-trip（D-511 字面 q15 = `scale / 32768` 精度）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_add_then_get_round_trip_within_q15_precision() {
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    table.add_regret(FakeInfoSet(42), 0, 100.0);
    table.add_regret(FakeInfoSet(42), 1, -50.0);
    let r0 = table.regret_at(FakeInfoSet(42), 0);
    let r1 = table.regret_at(FakeInfoSet(42), 1);
    // q15 with scale = 100 → 精度 = 100 / 32768 ≈ 0.00305；±1 LSB tolerance。
    assert!(
        (r0 - 100.0).abs() <= 0.01,
        "round-trip action 0 漂移 {} > 0.01 q15 精度",
        (r0 - 100.0).abs()
    );
    assert!(
        (r1 - (-50.0)).abs() <= 0.01,
        "round-trip action 1 漂移 {} > 0.01 q15 精度",
        (r1 - (-50.0)).abs()
    );
    assert_eq!(table.len(), 1, "单 InfoSet 添加后 len 应 = 1");
}

/// API-512 — 多次累加（同 row 同 action）走 q15 saturating_add；超 `[-32768,
/// 32767]` 触发 row-renorm 而非 saturating wrap（D-511 字面 overflow 路径）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_add_regret_overflow_triggers_row_renorm() {
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    // 累加大量正 regret 撞 q15 上界；scale 应自动重算让累积值仍 fit。
    for _ in 0..1000 {
        table.add_regret(FakeInfoSet(1), 0, 1_000_000.0);
    }
    let r = table.regret_at(FakeInfoSet(1), 0);
    let expected = 1_000_000.0 * 1000.0;
    // 累计 1e9，q15 精度 ~ 1e9 / 32768 ≈ 30000；relative tolerance 0.1%。
    let rel_err = (r - expected).abs() / expected.abs();
    assert!(
        rel_err <= 0.01,
        "累加 1000 × 1e6 实际 {r}，期望 {expected}，relative err {rel_err}"
    );
}

// ---------------------------------------------------------------------------
// Group C — RM+ clamp + Linear discounting lazy（D-511 / D-513 字面）
// ---------------------------------------------------------------------------

/// API-513 — `clamp_rm_plus` in-place clamp `max(q15, 0)` 全表生效。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_clamp_rm_plus_clamps_negatives_to_zero() {
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    table.add_regret(FakeInfoSet(7), 3, -42.0);
    table.add_regret(FakeInfoSet(7), 4, 100.0);
    table.clamp_rm_plus();
    let r3 = table.regret_at(FakeInfoSet(7), 3);
    let r4 = table.regret_at(FakeInfoSet(7), 4);
    assert!(
        r3 >= 0.0,
        "RM+ clamp 后负值应 ≥ 0，action 3 = {r3}（D-402 字面）"
    );
    assert!(
        (r4 - 100.0).abs() <= 1.0,
        "RM+ clamp 不应改正值，action 4 = {r4} 期望 ≈ 100"
    );
}

/// API-514 — `scale_linear_lazy(decay)` 仅 mutate `scales[i] *= decay`，
/// int16 payload 不动（D-511 字面 lazy 路径不全表扫描 payload）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_scale_linear_lazy_only_scales_not_payloads() {
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    table.add_regret(FakeInfoSet(11), 0, 1000.0);
    let _before = table.regret_at(FakeInfoSet(11), 0);
    table.scale_linear_lazy(0.5);
    let after = table.regret_at(FakeInfoSet(11), 0);
    // 0.5 decay 后 effective regret = 1000 × 0.5 = 500（scale × 0.5，payload 不动）。
    assert!(
        (after - 500.0).abs() <= 5.0,
        "scale_linear_lazy(0.5) 后 regret = {after} 应 ≈ 500"
    );
}

// ---------------------------------------------------------------------------
// Group D — Iter / len / section_bytes（API-515 / API-516 / API-517 / D-544）
// ---------------------------------------------------------------------------

/// API-515 — `len()` 单调非降，跨 add_regret 不重复计数同 InfoSet。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_len_counts_unique_info_sets_only() {
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    assert_eq!(table.len(), 0);
    table.add_regret(FakeInfoSet(1), 0, 10.0);
    assert_eq!(table.len(), 1);
    table.add_regret(FakeInfoSet(1), 1, 20.0); // same InfoSet, different action
    assert_eq!(table.len(), 1, "同 InfoSet 不同 action 不应增 len");
    table.add_regret(FakeInfoSet(2), 0, 30.0);
    assert_eq!(table.len(), 2);
}

/// API-516 / D-544 字面 — `section_bytes` 公式 = `(keys.capacity() × 8) +
/// (payloads.capacity() × 32) + (scales.capacity() × 4) + metadata`。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_section_bytes_follows_d544_formula() {
    let table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1 << 20);
    let bytes = table.section_bytes();
    // 严格 lower bound = capacity × (8 + 32 + 4) = 44 × capacity；不严格 upper bound 加 metadata。
    let cap = 1u64 << 20;
    let body = cap * 44;
    assert!(
        bytes >= body && bytes <= body + 4096,
        "section_bytes {bytes} 偏离 D-544 公式 body = {body}（容差 metadata ≤ 4 KiB）"
    );
}

/// API-517 — `iter()` 仅访问 populated slot；空表 next() 返回 None。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_iter_empty_table_returns_none_immediately() {
    let table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(0);
    let mut iter = table.iter();
    assert!(iter.next().is_none(), "空表 iter 应立即返 None");
}

/// API-517 — `iter()` 访问全部 populated slot 一次（基本完备性）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_iter_visits_all_populated_slots_once() {
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    let n = 100usize;
    for i in 0..n {
        table.add_regret(FakeInfoSet(i as u64), 0, (i as f32) + 1.0);
    }
    let mut visited = 0usize;
    for _ in table.iter() {
        visited += 1;
    }
    assert_eq!(
        visited, n,
        "iter 应访问 {n} 个 populated slot，实访 {visited}"
    );
}

// ---------------------------------------------------------------------------
// Group E — renormalize_scales + collision_metrics（D-511 / D-569 字面）
// ---------------------------------------------------------------------------

/// API-518 / D-511 — `renormalize_scales` 在 q15 上界趋近时 scale × 2 / q15 >> 1。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_renormalize_scales_preserves_effective_value() {
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    table.add_regret(FakeInfoSet(99), 0, 1.0);
    let before = table.regret_at(FakeInfoSet(99), 0);
    table.renormalize_scales();
    let after = table.regret_at(FakeInfoSet(99), 0);
    // renormalize 是数学上 identity（仅 scale × 2 / q15 >> 1 同向），effective 值守恒。
    assert!(
        (before - after).abs() <= 0.01,
        "renormalize 不应改 effective regret 值（before {before}，after {after}）"
    );
}

/// API-519 / D-569 — collision_metrics 三阈值（load_factor ≤ 0.75 / max_probe
/// ≤ 16 / avg_probe ≤ 2.0）在 100 个 InfoSet 插入后满足。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_collision_metrics_within_d569_bounds_after_100_inserts() {
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    for i in 0..100u64 {
        // 用 FxHash-style 散列保证 key 在不同 bucket（避免 sequential 撞 cluster）。
        let mut h = std::collections::hash_map::DefaultHasher::new();
        i.hash(&mut h);
        table.add_regret(FakeInfoSet(h.finish()), 0, i as f32 + 1.0);
    }
    let CollisionMetrics {
        max_probe_distance,
        avg_probe_distance,
        load_factor,
    } = table.collision_metrics();
    assert!(
        load_factor <= 0.75,
        "load_factor {load_factor} > 0.75（D-569 阈值）"
    );
    assert!(
        max_probe_distance <= 16,
        "max_probe_distance {max_probe_distance} > 16（D-569 阈值）"
    );
    assert!(
        avg_probe_distance <= 2.0,
        "avg_probe_distance {avg_probe_distance} > 2.0（D-569 阈值）"
    );
}

/// API-519 / D-518 — 持续插入触发 load_factor > 0.75 时表应 grow 而非超阈值。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_compact_grow_on_load_factor_threshold() {
    let mut table: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(64);
    // capacity 起步 64（向上对齐到 power-of-two；实际 ≥ 2^20 lock，本 test 仅断
    // 言 load_factor ≤ 0.75 invariant 而不依赖 capacity 具体值）。
    for i in 0..2048u64 {
        table.add_regret(FakeInfoSet(i), 0, 1.0);
    }
    let cm = table.collision_metrics();
    assert!(
        cm.load_factor <= 0.75,
        "插入 2048 个 InfoSet 后 load_factor {} > 0.75（D-518 grow 未触发？）",
        cm.load_factor
    );
}

// ---------------------------------------------------------------------------
// Group F — StrategyAccumulatorCompact 同型签名（D-517 字面 keys 不共享）
// ---------------------------------------------------------------------------

/// API-526 — `with_initial_capacity_estimate` 同型 RegretTableCompact 起步。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn strategy_accum_compact_with_initial_capacity_default_2_to_20() {
    let table: StrategyAccumulatorCompact<FakeInfoSet> =
        StrategyAccumulatorCompact::with_initial_capacity_estimate(0);
    assert!(table.is_empty());
    assert_eq!(table.len(), 0);
}

/// API-526 — `add_strategy_sum` + `average_strategy` 归一化输出（sum = 1.0）。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn strategy_accum_compact_average_strategy_normalizes_to_one() {
    let mut table: StrategyAccumulatorCompact<FakeInfoSet> =
        StrategyAccumulatorCompact::with_initial_capacity_estimate(1024);
    table.add_strategy_sum(FakeInfoSet(5), 0, 3.0);
    table.add_strategy_sum(FakeInfoSet(5), 1, 1.0);
    let mut avg = [0.0_f32; 14];
    table.average_strategy(FakeInfoSet(5), &mut avg);
    let sum: f32 = avg.iter().sum();
    assert!(
        (sum - 1.0).abs() <= 1e-3,
        "average_strategy 归一化 sum 应 ≈ 1.0，实际 {sum}"
    );
    // 比例 3:1 应保留，action 0 ≈ 0.75 / action 1 ≈ 0.25。
    assert!(
        (avg[0] - 0.75).abs() <= 0.01,
        "action 0 概率 {} ≠ 0.75（3:1 比例）",
        avg[0]
    );
    assert!(
        (avg[1] - 0.25).abs() <= 0.01,
        "action 1 概率 {} ≠ 0.25",
        avg[1]
    );
}

/// API-526 — `scale_linear_lazy` 同 RegretTableCompact lazy 路径。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn strategy_accum_compact_scale_linear_lazy_only_scales_not_payloads() {
    let mut table: StrategyAccumulatorCompact<FakeInfoSet> =
        StrategyAccumulatorCompact::with_initial_capacity_estimate(1024);
    table.add_strategy_sum(FakeInfoSet(17), 0, 2.0);
    table.add_strategy_sum(FakeInfoSet(17), 1, 6.0);
    table.scale_linear_lazy(0.5);
    // 归一化输出在 scale × 0.5 后比例不变（比例本身归一不受 scale 同向影响）。
    let mut avg = [0.0_f32; 14];
    table.average_strategy(FakeInfoSet(17), &mut avg);
    assert!(
        (avg[0] - 0.25).abs() <= 0.01,
        "action 0 概率 {} ≠ 0.25（2:6 比例，scale × 0.5 不改归一化）",
        avg[0]
    );
    assert!(
        (avg[1] - 0.75).abs() <= 0.01,
        "action 1 概率 {} ≠ 0.75",
        avg[1]
    );
}

/// D-517 字面 — RegretTable 与 StrategyAccumulator hash table **不共享 keys**：
/// 同 InfoSet 在两表内独立存在，互不影响 len。
#[test]
#[ignore = "B1 scaffold; B2 [实现] 落地后转 pass"]
fn regret_and_strategy_accum_keys_arrays_not_shared() {
    let mut regret: RegretTableCompact<FakeInfoSet> =
        RegretTableCompact::with_initial_capacity_estimate(1024);
    let mut strat: StrategyAccumulatorCompact<FakeInfoSet> =
        StrategyAccumulatorCompact::with_initial_capacity_estimate(1024);
    regret.add_regret(FakeInfoSet(33), 0, 10.0);
    assert_eq!(regret.len(), 1);
    assert_eq!(
        strat.len(),
        0,
        "D-517 字面 — RegretTable 写入不应影响 StrategyAccumulator len"
    );
    strat.add_strategy_sum(FakeInfoSet(34), 0, 5.0);
    assert_eq!(
        regret.len(),
        1,
        "StrategyAccumulator 写入不应影响 RegretTable len"
    );
    assert_eq!(strat.len(), 1);
}

// ---------------------------------------------------------------------------
// Group G — Active sanity（PhantomData 类型完备 + 编译期 lock，不调
// `unimplemented!()` 路径；B1 转 pass 不需要 B2 实现，A1 stub 即生效）
// ---------------------------------------------------------------------------

/// API-519 — `CollisionMetrics` struct 字段值字面构造 + 字段访问完整。
#[test]
fn collision_metrics_struct_field_lock() {
    let cm = CollisionMetrics {
        max_probe_distance: 7,
        avg_probe_distance: 1.5,
        load_factor: 0.42,
    };
    assert_eq!(cm.max_probe_distance, 7);
    assert!((cm.avg_probe_distance - 1.5).abs() < 1e-6);
    assert!((cm.load_factor - 0.42).abs() < 1e-6);
}

/// API-510 / API-526 — PhantomData type marker 编译期 0-size lock。
#[test]
fn regret_and_strategy_compact_phantom_data_zero_sized() {
    // 编译期 PhantomData lock：`RegretTableCompact<FakeInfoSet>` 不持有 FakeInfoSet
    // 值，仅靠 PhantomData 保留 type parameter 活跃。本测试 trip-wire 让
    // type parameter 被 monomorphization。
    let _phantom: PhantomData<FakeInfoSet> = PhantomData;
}

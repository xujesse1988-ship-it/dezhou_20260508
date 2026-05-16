//! 阶段 5 B1 \[测试\] — 紧凑 RegretTable Robin Hood collision metrics anchor
//! integration crate（API-598 / D-569 字面）。
//!
//! ## 角色边界
//!
//! 本文件属 `[测试]` agent；B1 [测试] 0 改动产品代码。E1 \[测试\] / E2
//! \[实现\] 落地紧凑 RegretTable + Robin Hood probe + FxHash 路径后 c6a host
//! 实测 opt-in 转 pass。
//!
//! ## D-569 字面 — collision metrics anchor
//!
//! 紧凑 RegretTable Robin Hood probing 健康检查：
//! - `load_factor ≤ 0.75`
//! - `max_probe_distance ≤ 16`
//! - `avg_probe_distance ≤ 2.0`
//!
//! ## D-569 字面 — measurement protocol
//!
//! 在 1M update warm-up 后 + 10M update steady-state 后两次 snapshot 全表
//! `collision_metrics(traverser)`（API-519 字面 getter）。违反任一阈值 abort
//! + alarm dispatch（继承 stage 4 D-477 alarm variant pattern 扩展）。
//!
//! ## 测试覆盖
//!
//! - 3 阈值常量字面 lock（active，A1 scaffold 即生效）。
//! - 1M warm-up snapshot anchor（`#[ignore]`，E2 [实现] 落地后转 pass）。
//! - 10M steady-state snapshot anchor（`#[ignore]`，E2 [实现] 落地后转 pass）。
//! - 6 traverser 全套 snapshot（`#[ignore]`，E2 [实现] 落地后转 pass）。

use poker::training::regret_compact::CollisionMetrics;

// ---------------------------------------------------------------------------
// D-569 字面阈值常量（active sanity；A1 scaffold 即生效）
// ---------------------------------------------------------------------------

/// D-569 字面阈值 — load_factor 上界。
const D569_LOAD_FACTOR_THRESHOLD: f32 = 0.75;

/// D-569 字面阈值 — max_probe_distance 上界。
const D569_MAX_PROBE_DISTANCE_THRESHOLD: usize = 16;

/// D-569 字面阈值 — avg_probe_distance 上界。
const D569_AVG_PROBE_DISTANCE_THRESHOLD: f32 = 2.0;

/// 1M warm-up snapshot wall（D-409 字面 stage 4 boundary，stage 5 D-522 维持）。
const WARM_UP_UPDATES: u64 = 1_000_000;

/// 10M steady-state snapshot wall。
const STEADY_STATE_UPDATES: u64 = 10_000_000;

// ---------------------------------------------------------------------------
// Group A — D-569 阈值常量字面 lock（active）
// ---------------------------------------------------------------------------

/// D-569 字面 — 3 阈值常量锁。
#[test]
fn d569_collision_thresholds_match_literal_values() {
    assert!(
        (D569_LOAD_FACTOR_THRESHOLD - 0.75).abs() < 1e-6,
        "load_factor 阈值字面 0.75"
    );
    assert_eq!(
        D569_MAX_PROBE_DISTANCE_THRESHOLD, 16,
        "max_probe_distance 阈值字面 16"
    );
    assert!(
        (D569_AVG_PROBE_DISTANCE_THRESHOLD - 2.0).abs() < 1e-6,
        "avg_probe_distance 阈值字面 2.0"
    );
}

/// D-569 字面 — `CollisionMetrics` struct 字段集字面 lock。
#[test]
fn collision_metrics_field_layout_lock() {
    // 显式构造让字段集字面顺序锁定。
    let cm = CollisionMetrics {
        max_probe_distance: 0,
        avg_probe_distance: 0.0,
        load_factor: 0.0,
    };
    let _: usize = cm.max_probe_distance;
    let _: f32 = cm.avg_probe_distance;
    let _: f32 = cm.load_factor;
}

/// `CollisionMetrics` 默认值（max_probe_distance=0 / avg=0.0 / load_factor=0.0）
/// 视为"空表 sentinel"满足全部 D-569 阈值。
#[test]
fn empty_table_collision_metrics_within_d569_bounds() {
    let empty = CollisionMetrics {
        max_probe_distance: 0,
        avg_probe_distance: 0.0,
        load_factor: 0.0,
    };
    assert!(empty.load_factor <= D569_LOAD_FACTOR_THRESHOLD);
    assert!(empty.max_probe_distance <= D569_MAX_PROBE_DISTANCE_THRESHOLD);
    assert!(empty.avg_probe_distance <= D569_AVG_PROBE_DISTANCE_THRESHOLD);
}

// ---------------------------------------------------------------------------
// Group B — D-569 anchor 1M / 10M snapshot（`#[ignore]` opt-in）
// ---------------------------------------------------------------------------

/// D-569 字面 anchor — 1M warm-up snapshot 全表 collision_metrics 满足 3 阈值。
///
/// **B1 [测试] 状态**：E2 [实现] 落地 + c6a host run 实测后 opt-in 转 pass。
/// 路径：
/// 1. 构造 `EsMccfrLinearRmPlusCompactTrainer<NlheGame6>`（v3 bucket table 528 MiB）。
/// 2. 跑 1M update（warm-up boundary，D-522）。
/// 3. 对 6 traverser 各取 `collision_metrics(traverser)` snapshot。
/// 4. 全 6 traverser 均满足 load_factor ≤ 0.75 / max_probe ≤ 16 / avg_probe ≤ 2.0。
#[test]
#[ignore = "B1 scaffold; E2 [实现] 落地 + c6a host run 1M warm-up 实测后 opt-in 转 pass"]
fn collision_metrics_after_1m_warmup_within_d569_bounds() {
    let _ = WARM_UP_UPDATES;
    panic!(
        "D-569 字面 1M warm-up snapshot — E2 [实现] 起步前 stub。路径：\
         EsMccfrLinearRmPlusCompactTrainer + step × 1_000_000 → \
         collision_metrics(0..6) snapshot → 6 traverser 全套阈值检查。\
         违反阈值 abort + alarm dispatch（D-477 variant 扩展）。"
    );
}

/// D-569 字面 anchor — 10M steady-state snapshot 全表 collision_metrics 满足
/// 3 阈值（与 1M 同协议但 wall 长 10×）。
#[test]
#[ignore = "B1 scaffold; E2 [实现] 落地 + c6a host run 10M steady-state 实测后 opt-in 转 pass"]
fn collision_metrics_after_10m_steady_state_within_d569_bounds() {
    let _ = STEADY_STATE_UPDATES;
    panic!(
        "D-569 字面 10M steady-state snapshot — E2 [实现] 起步前 stub。路径：\
         继 1M warm-up 后 +9M update → collision_metrics(0..6) snapshot → 6 traverser 全套阈值检查。"
    );
}

// ---------------------------------------------------------------------------
// Group C — 6 traverser 全套 collision_metrics（D-412 字面 per-traverser 独立表）
// ---------------------------------------------------------------------------

/// D-412 + D-569 — 6 traverser 各自的 RegretTableCompact 全套 collision_metrics
/// 满足 3 阈值（per-traverser 独立 hash table，D-517 字面 RegretTable +
/// StrategyAccumulator hash table 不共享）。
#[test]
#[ignore = "B1 scaffold; E2 [实现] 落地后 opt-in 转 pass"]
fn collision_metrics_six_traverser_independent_tables() {
    panic!(
        "D-412 + D-569 — 6 traverser 独立表各自 collision_metrics snapshot \
         全套满足阈值（per-traverser 独立 hash table）。\
         E2 [实现] 落地起步前 stub。"
    );
}

/// D-512 + D-569 — 256 shard 分片路径下 collision_metrics 仍满足 3 阈值
/// （production 10¹¹ path；first usable path 不消费 ShardLoader）。
#[test]
#[ignore = "B1 scaffold; E2 [实现] 落地 production 10¹¹ shard 路径后 opt-in 转 pass"]
fn collision_metrics_with_shard_loader_production_path() {
    panic!(
        "D-512 + D-569 — 256 shard 分片路径下 collision_metrics 满足 D-569 阈值。\
         production 10¹¹ path 触发条件 = D-441-rev0 用户授权（~$214 × 7 days c6a）。\
         E2 [实现] 落地后 opt-in 转 pass。"
    );
}

/// D-569 字面 alarm dispatch — 违反任一阈值 → abort + alarm（继承 stage 4
/// D-477 alarm variant pattern 扩展，stage 5 新增 variant 落地由 E2 [实现] +
/// metrics.jsonl alarm 字段联动）。
#[test]
#[ignore = "B1 scaffold; E2 [实现] 落地 alarm variant 扩展后转 pass"]
fn collision_metrics_threshold_violation_dispatches_alarm() {
    panic!(
        "D-569 字面 alarm dispatch — violated threshold → trainer alarm + metrics.jsonl 写入。\
         继承 stage 4 D-477 alarm variant pattern 扩展（5-variant alarm + 1 stage 5 新增）。\
         E2 [实现] 落地后 opt-in 转 pass。"
    );
}

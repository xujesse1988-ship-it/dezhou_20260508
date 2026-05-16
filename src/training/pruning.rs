//! 阶段 5 极负 regret pruning + 周期性 ε resurface（API-530..API-539 / D-520..D-529）。
//!
//! ## Pluribus 论文 §S2 字面阈值
//!
//! - **阈值** = `regret_f32 < -300_000_000.0`（D-520，与 Brown 2020 PhD 论文 §4.3 一致）
//! - **resurface 周期** = 每 `10_000_000` update（D-521）
//! - **resurface 比例 ε** = `0.05`（5% pruned action 重激活）
//! - **resurface reset 值** = `threshold × 0.5 = -150_000_000.0`（D-521）
//!
//! ## warm-up 互斥（D-522）
//!
//! warm-up phase（前 1M update）**不** 启用 pruning（regret scale 小 + 全 action
//! 都需要充分 explore）。warm-up 完成后**同步切** Linear MCCFR + RM+ 和 pruning
//! （D-409 single boundary，禁止双切点漂移）。
//!
//! ## 数学正确性（D-523）
//!
//! 跳过 pruned action 子树等价于"该 action 的 cfv 估计未更新 + regret delta
//! 不累加"。Linear MCCFR + RM+ 数学允许这种 lazy update（Brown 2020 PhD §4.3
//! sublinear regret growth 保留）。**不**额外加补偿项。
//!
//! ## A1 \[实现\] 状态
//!
//! `PruningConfig` 字段集 + `Default` impl 落地真实值；`should_prune` /
//! `resurface_pass` 走 `unimplemented!()` 占位。E2 \[实现\] 落地 step 路径接入。

#![allow(clippy::needless_pass_by_value)]

use std::hash::Hash;
use std::time::Duration;

use crate::core::rng::RngSource;
use crate::training::regret_compact::RegretTableCompact;

/// API-530 — pruning 阈值 + ε resurface 配置。
///
/// 默认值字面对应 D-520 / D-521 Pluribus 论文 §S2：
/// - `threshold = -300_000_000.0`
/// - `resurface_period = 10_000_000`
/// - `resurface_epsilon = 0.05`
/// - `resurface_reset_value = -150_000_000.0`（= threshold × 0.5）
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PruningConfig {
    /// regret 绝对阈值；低于该值的 action 在 traverser 决策点 inline check 时
    /// 被 skip 整个递归子树（D-520 字面 -300M）。
    pub threshold: f32,
    /// 每多少 update 触发一次全表 resurface scan（D-521 字面 1e7）。
    pub resurface_period: u64,
    /// 5% pruned action 每周期被重激活（D-521 字面 0.05）。
    pub resurface_epsilon: f32,
    /// 重激活时 q15 reset 到 threshold × 0.5 = -150M（D-521 字面留 50% 上升
    /// 空间到 0，让其有充分机会被下次 traverser 访问 + 不立即触发 RM+ clamp）。
    pub resurface_reset_value: f32,
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            threshold: -300_000_000.0,
            resurface_period: 10_000_000,
            resurface_epsilon: 0.05,
            resurface_reset_value: -150_000_000.0,
        }
    }
}

/// API-531 — inline 单次 pruning 判定。
///
/// `regret_at(info_set, action) < cfg.threshold` 即 prune。q15 路径下 dequant
/// 走 [`crate::training::quantize::dequantize_action`]（D-520 字面 q15 等价
/// 阈值 `q15 < (-300M / current_scale × 32768)`）。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位。E2 \[实现\] 落地。
pub fn should_prune<I: Eq + Hash + Clone>(
    table: &RegretTableCompact<I>,
    info_set: I,
    action: usize,
    cfg: &PruningConfig,
) -> bool {
    let _ = (table, info_set, action, cfg);
    unimplemented!("stage 5 A1 scaffold — pruning::should_prune 落地于 E2 [实现]")
}

/// API-532 — 全表 ε resurface scan。
///
/// 全表 scan → 每 pruned action（`q15 < quantized_threshold`） → `rng
/// .next_uniform_f32() < ε` → `q15 ← quantize(reset_value, scale)`。
///
/// **RNG 派生**（D-528 字面）：`master_seed.wrapping_add(0xDEAD_BEEF_CAFE_BABE *
/// resurface_pass_id)`（splitmix64 finalizer，继承 stage 4 D-468 同型派生）。
/// `resurface_pass_id` 从 0 单调递增，确保跨 run reproducible。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位。E2 \[实现\] 落地。
pub fn resurface_pass<I: Eq + Hash + Clone>(
    table: &mut RegretTableCompact<I>,
    cfg: &PruningConfig,
    rng: &mut dyn RngSource,
    resurface_pass_id: u64,
) -> ResurfaceMetrics {
    let _ = (table, cfg, rng, resurface_pass_id);
    unimplemented!("stage 5 A1 scaffold — pruning::resurface_pass 落地于 E2 [实现]")
}

/// API-532 — `resurface_pass` 返回值。
///
/// metrics 字段进 metrics.jsonl（D-526 字面 4 字段 `pruned_action_count` /
/// `pruned_action_ratio` / `resurface_event_count` / `resurface_reactivated_count`）。
#[derive(Clone, Copy, Debug, Default)]
pub struct ResurfaceMetrics {
    /// 本次 scan 扫描的总 (info_set, action) 数。
    pub scanned_action_count: u64,
    /// 当前满足 pruning 条件（`q15 < threshold_q15`）的 action 数。
    pub pruned_action_count: u64,
    /// 本次 scan 期间被 ε 选中重激活的 action 数。
    pub reactivated_action_count: u64,
    /// 本次 scan 墙钟开销。
    pub wall_time: Duration,
}

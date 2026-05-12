//! `derive_substream_seed` + `sample_discrete` + 6 个 `op_id` 常量（API-330 / API-331）。
//!
//! RNG sub-stream 派生继承 stage 1 D-228 + stage 2 [`crate::abstraction::cluster::rng_substream`]
//! 模式（SplitMix64 finalizer），差异：stage 3 返回 32 byte ChaCha20Rng seed（D-335），
//! 而 stage 2 派生函数返回 `u64`（按需 `ChaCha20Rng::from_seed` 升维）。
//!
//! `sample_discrete` D-336 自实现 CDF binary search，不走 `rand::distributions::
//! WeightedIndex`（外部 crate 浮点行为跨版本可能漂移破 byte-equal；继承 stage 2
//! D-250 自实现 k-means 同型政策）。
//!
//! A1 \[实现\] 阶段所有方法体 `unimplemented!()`；B2 \[实现\] 落地（Kuhn / Leduc
//! deck shuffle 走 chance sampling + opponent action sampling）。

use crate::core::rng::RngSource;

/// Kuhn deck deal sub-stream op id（D-335）。
///
/// 命名空间 `0x03_xx` 与 stage 1 D-228（`0x01_xx`）+ stage 2 D-228（`0x02_xx`）
/// 隔离，避免跨 stage 派生 seed 撞车。
pub const OP_KUHN_DEAL: u64 = 0x03_00;

/// Leduc deck deal sub-stream op id（D-335）。
pub const OP_LEDUC_DEAL: u64 = 0x03_01;

/// 简化 NLHE deal hole / board sub-stream op id（D-335）。
pub const OP_NLHE_DEAL: u64 = 0x03_02;

/// Opponent action sampling sub-stream op id（D-337）。
///
/// 按 [`crate::training::Trainer::current_strategy`] 返回的分布采样 1 action
/// （D-309 algorithm + D-337 implementation）。
pub const OP_OPP_ACTION_SAMPLE: u64 = 0x03_10;

/// Chance node sampling sub-stream op id（D-336）。
///
/// 调用 [`sample_discrete`] 在 [`crate::training::Game::chance_distribution`] 返回
/// 的分布上采样 1 outcome（D-308 sample-1 路径）。
pub const OP_CHANCE_SAMPLE: u64 = 0x03_11;

/// Traverser tie-break sub-stream op id（D-307）。
///
/// 当 multiple traverser 选项等概率时用作 tie-break（ES-MCCFR 单 traverser
/// 选择路径在 D-307 锁定，本 op_id 留给 B2 / C2 \[实现\] 评估具体需求）。
pub const OP_TRAVERSER_TIE: u64 = 0x03_20;

/// 从 `(master_seed, op_id, iter)` 三元组派生 32 byte ChaCha20Rng seed（API-330 / D-335）。
///
/// SplitMix64 finalizer × 4 → 32 byte（继承 stage 1 D-228 + stage 2
/// `cluster::rng_substream::derive_substream_seed` 同型 mix 函数）。
///
/// **不变量**（B2 \[实现\] 落地后由 `tests/cfr_kuhn.rs::kuhn_vanilla_cfr_fixed_seed_repeat_1000_times_blake3_identical`
/// 等回归断言）：
/// - 同 `(master_seed, op_id, iter)` 三元组 → 同 32 byte 输出（pure function）
/// - 跨 host / 跨架构 byte-equal（D-347 跨 host 不变量）
/// - 不同 `op_id` 在同 `(master_seed, iter)` 下输出统计独立（low correlation）
pub fn derive_substream_seed(_master_seed: u64, _op_id: u64, _iter: u64) -> [u8; 32] {
    unimplemented!("stage 3 A1 scaffold: derive_substream_seed (B2 实现)")
}

/// 在 `(action, probability)` 列表上采样 1 个 outcome（API-331 / D-336）。
///
/// 实现路径：
/// 1. `rng.next_u64()` → 归一化到 `[0, 1)`（除以 `2^64`）
/// 2. 在 CDF（累积 probability）上 binary search 找最小 `i` s.t. `cdf[i] >= u`
/// 3. 返回 `distribution[i].0`
///
/// **不变量**：
/// - Σ probability == `1.0 ± 1e-12`（D-336 sum_check；不达 panic）
/// - 所有 probability > `0.0`（零概率 outcome 应从分布中剔除）
/// - rng 消费 exactly 1 次 `next_u64`（不消费多次）
pub fn sample_discrete<A: Copy>(_distribution: &[(A, f64)], _rng: &mut dyn RngSource) -> A {
    unimplemented!("stage 3 A1 scaffold: sample_discrete (B2 实现)")
}

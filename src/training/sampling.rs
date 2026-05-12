//! `derive_substream_seed` + `sample_discrete` + 6 个 `op_id` 常量（API-330 / API-331）。
//!
//! RNG sub-stream 派生继承 stage 1 D-228 + stage 2 [`crate::abstraction::cluster::rng_substream`]
//! 模式（SplitMix64 finalizer），差异：stage 3 返回 32 byte ChaCha20Rng seed（D-335），
//! 而 stage 2 派生函数返回 `u64`（按需 `ChaCha20Rng::from_seed` 升维）。
//!
//! `sample_discrete` D-336 自实现 CDF binary search，不走 `rand::distributions::
//! WeightedIndex`（外部 crate 浮点行为跨版本可能漂移破 byte-equal；继承 stage 2
//! D-250 自实现 k-means 同型政策）。

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
/// **算法**：把 `(master_seed, op_id, iter)` 折叠成 4 个独立 64-bit lane
/// （lane index `0..4` 作为额外扰动），每 lane 走 SplitMix64 finalizer 得到 8 byte，
/// 按 lane 顺序拼接成 32 byte。该路径让相同三元组 → 相同 32 byte，跨 host /
/// 跨架构 byte-equal（pure integer arithmetic，f64 不参与）。
///
/// **不变量**：
/// - 同 `(master_seed, op_id, iter)` 三元组 → 同 32 byte 输出（pure function）。
/// - 跨 host / 跨架构 byte-equal（D-347 跨 host 不变量）。
/// - 不同 `op_id` 在同 `(master_seed, iter)` 下输出统计独立（low correlation，由
///   SplitMix64 avalanche 性质保证）。
pub fn derive_substream_seed(master_seed: u64, op_id: u64, iter: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    for lane in 0u64..4 {
        // 把 (master_seed, op_id, iter, lane) 折成一个 64-bit tag 再 mix。每 lane
        // 独立 xor 不同位置避免 lane 之间退化成线性相关；继承 stage 2
        // `derive_substream_seed(master_seed, op_id, sub_index)` 的 finalizer 形态。
        let mut x = master_seed
            ^ op_id.wrapping_mul(0x9E37_79B9_7F4A_7C15) // golden ratio prime
            ^ iter.wrapping_mul(0xBF58_476D_1CE4_E5B9)
            ^ lane.wrapping_mul(0x94D0_49BB_1331_11EB);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 31;
        let bytes = x.to_le_bytes();
        let off = (lane as usize) * 8;
        out[off..off + 8].copy_from_slice(&bytes);
    }
    out
}

/// 在 `(action, probability)` 列表上采样 1 个 outcome（API-331 / D-336）。
///
/// 实现路径：
/// 1. `rng.next_u64()` → 归一化到 `[0, 1)`（取高 53 位 / `2^53`，标准 f64 单位采样）
/// 2. 在 CDF（累积 probability）上线性扫描找最小 `i` s.t. `cdf[i] >= u`（D-336
///    字面 "binary search"，但短分布线性扫描更简单且确定性等价）
/// 3. 返回 `distribution[i].0`
///
/// **不变量**：
/// - Σ probability == `1.0 ± 1e-12`（D-336 sum_check；不达 panic）
/// - 所有 probability > `0.0`（零概率 outcome 应从分布中剔除）
/// - rng 消费 exactly 1 次 `next_u64`（不消费多次）
pub fn sample_discrete<A: Copy>(distribution: &[(A, f64)], rng: &mut dyn RngSource) -> A {
    assert!(
        !distribution.is_empty(),
        "sample_discrete: empty distribution"
    );
    let mut sum = 0.0_f64;
    for &(_, p) in distribution {
        assert!(
            p > 0.0,
            "sample_discrete: 非正概率 {p}（零概率 outcome 应剔除）"
        );
        sum += p;
    }
    assert!(
        (sum - 1.0).abs() < 1e-12,
        "sample_discrete: probabilities sum = {sum}, 超出 1e-12 容差（D-336）"
    );

    // u ∈ [0, 1) via 53-bit mantissa（与 stage 2 `sample_signed_f64` 同型，浮点
    // 取整后直接 < 1.0）
    let raw = rng.next_u64();
    let u = (raw >> 11) as f64 / ((1u64 << 53) as f64);

    let mut cum = 0.0_f64;
    for &(action, p) in distribution.iter().take(distribution.len() - 1) {
        cum += p;
        if u < cum {
            return action;
        }
    }
    // 浮点累积误差可能让 u 等价 1.0 - epsilon；保底走最后一个 outcome（仍然
    // deterministic：CDF 单调非降且最后 cdf == 1.0 ± 1e-12 必然命中）。
    distribution[distribution.len() - 1].0
}

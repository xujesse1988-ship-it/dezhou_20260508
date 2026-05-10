//! k-means / EMD 聚类（D-230..D-238）。
//!
//! 模块私有 surface（D-254 内部子模块隔离，不在 `lib.rs` 顶层 re-export），
//! 仅由 `tools/train_bucket_table.rs` CLI 引用。允许使用浮点（D-273）。
//!
//! **例外**：[`rng_substream`] 子模块作为 D-228 公开 contract 在 `lib.rs` 顶层
//! re-export，便于 `tests/clustering_determinism.rs` 等 \[测试\] 独立构造
//! sub-stream 验证 byte-equal。
//!
//! C2 \[实现\]：填充 k-means++ 初始化（D-231）/ k-means + L2（D-230）/ EMD
//! 1D（D-234）/ 空 cluster split（D-236）/ EHS 中位数重编号（D-236b）/ centroid
//! u8 量化（D-241）/ k-means 量化抽样（D-235 SCALE=2^40）。

use crate::core::rng::{ChaCha20Rng, RngSource};

pub mod rng_substream {
    //! D-228 RngSource sub-stream 派生协议（公开 contract）。
    //!
    //! `derive_substream_seed(master_seed, op_id, sub_index) -> u64` 走 SplitMix64
    //! finalizer：
    //!
    //! ```text
    //! let tag = ((op_id as u64) << 32) | (sub_index as u64);
    //! let mut x = master_seed ^ tag;
    //! x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    //! x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    //! x ^ (x >> 31)
    //! ```
    //!
    //! op_id 高 16 位 = 类别，低 16 位 = 街 / 子操作；新增 op_id 必须走 D-228-revM
    //! 流程并 bump `BucketTable.schema_version`（违反会让相同 `(training_seed,
    //! BucketConfig)` 输出不同 BLAKE3 trailer，破坏 D-237 byte-equal 不变量）。
    //!
    //! sub_seed 的标准用法：`ChaCha20Rng::from_seed(sub_seed)`（继承 stage 1
    //! D-028 RNG 实例化），不允许直接 `next_u64()` master 后用其 raw bits 当
    //! sub_seed。

    /// SplitMix64 finalizer-based sub-stream seed derivation（D-228）。
    ///
    /// 入参：`master_seed`（caller 持有的 training-time 主 seed）+
    /// `op_id`（本表内常量之一）+ `sub_index`（caller 在 op_id 命名空间内的
    /// 线性整数：iter / outer-enum-index / split-attempt-index）。
    ///
    /// 输出：64-bit 派生 seed，可直接喂给 `ChaCha20Rng::from_seed`。
    pub fn derive_substream_seed(master_seed: u64, op_id: u32, sub_index: u32) -> u64 {
        let tag = ((op_id as u64) << 32) | (sub_index as u64);
        let mut x = master_seed ^ tag;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        x ^ (x >> 31)
    }

    // ===========================================================================
    // op_id 表（D-228）。任何修改必须走 D-228-revM 并 bump
    // BucketTable.schema_version。
    // ===========================================================================

    /// OCHS opponent cluster 暖启动（D-228）。
    pub const OCHS_WARMUP: u32 = 0x0001_0000;

    /// k-means 主聚类 fork（D-228）；街区分以低 16 位标记。
    pub const CLUSTER_MAIN_FLOP: u32 = 0x0002_0001;
    pub const CLUSTER_MAIN_TURN: u32 = 0x0002_0002;
    pub const CLUSTER_MAIN_RIVER: u32 = 0x0002_0003;

    /// k-means++ 初始化采样（D-228）。
    pub const KMEANS_PP_INIT_FLOP: u32 = 0x0003_0001;
    pub const KMEANS_PP_INIT_TURN: u32 = 0x0003_0002;
    pub const KMEANS_PP_INIT_RIVER: u32 = 0x0003_0003;

    /// 空 cluster 切分回退采样（D-236）。
    pub const EMPTY_CLUSTER_SPLIT_FLOP: u32 = 0x0004_0001;
    pub const EMPTY_CLUSTER_SPLIT_TURN: u32 = 0x0004_0002;
    pub const EMPTY_CLUSTER_SPLIT_RIVER: u32 = 0x0004_0003;

    /// `EquityCalculator::equity_vs_hand` preflop Monte Carlo（D-220 / D-220a-rev1）。
    pub const EQUITY_MONTE_CARLO: u32 = 0x0005_0000;

    /// EHS² inner equity Monte Carlo（D-227）；街区分以低 16 位标记。
    pub const EHS2_INNER_EQUITY_FLOP: u32 = 0x0006_0001;
    pub const EHS2_INNER_EQUITY_TURN: u32 = 0x0006_0002;
    pub const EHS2_INNER_EQUITY_RIVER: u32 = 0x0006_0003;

    /// OCHS feature 计算的 inner equity 采样（D-222 / D-228）。
    pub const OCHS_FEATURE_INNER: u32 = 0x0007_0000;
}

// ============================================================================
// 1D EMD（D-234）
// ============================================================================

/// 1D EMD（1-Wasserstein 距离）在 [0, 1] 区间。
///
/// 数学定义：`W_1(P, Q) = ∫_0^1 |F_P(x) - F_Q(x)| dx`，其中 `F_P` / `F_Q` 是
/// 经验 CDF（每个 sample 贡献 1/n 阶跃，n 为各自样本数）。
///
/// 实现分两条路径：
///
/// - **等长**：保持原实现 `Σ|a[i] - b[i]| / n`（与步函数 CDF 积分数学等价于此特例）。
///   等长是 D-234 训练时 cluster 内 EHS 分布比较的主路径；保留此路径让历史
///   byte-equal trace 不漂移。
/// - **不等长**：合并 `a ∪ b` 排序后扫一遍 step CDF，逐段累加 `|F_a - F_b| · Δx`。
///   不等长是 §C-rev2 §5a 修正的核心路径——cluster size 不均时 bucket-quality 验收
///   的相邻 EMD 阈值需要正确反映 long-tail 分布质量，旧 `acc / min(len_a, len_b)`
///   会丢弃尾部样本，系统性低估距离。
///
/// 假设：`samples_a` 与 `samples_b` 元素位于 `[0, 1]`（EHS / equity 输出范围）。
/// 超出范围不会 panic，但 `prev_x = 0.0` 起步、`tail to 1.0` 收尾的积分语义只在
/// 单位区间上定义良好。
pub fn emd_1d_unit_interval(samples_a: &[f64], samples_b: &[f64]) -> f64 {
    let mut a: Vec<f64> = samples_a.to_vec();
    let mut b: Vec<f64> = samples_b.to_vec();
    a.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    b.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a.len() == b.len() {
        let n = a.len();
        let mut acc = 0.0_f64;
        for i in 0..n {
            acc += (a[i] - b[i]).abs();
        }
        return acc / n as f64;
    }
    emd_step_cdf_integral(&a, &b)
}

/// 步函数 CDF 积分（不等长样本路径）。`a` / `b` 已升序排序、非空。
fn emd_step_cdf_integral(a: &[f64], b: &[f64]) -> f64 {
    let inc_a = 1.0_f64 / a.len() as f64;
    let inc_b = 1.0_f64 / b.len() as f64;
    let mut i = 0usize;
    let mut j = 0usize;
    let mut prev_x = 0.0_f64;
    let mut ca = 0.0_f64;
    let mut cb = 0.0_f64;
    let mut acc = 0.0_f64;
    while i < a.len() || j < b.len() {
        let x = match (i < a.len(), j < b.len()) {
            (true, true) => a[i].min(b[j]),
            (true, false) => a[i],
            (false, true) => b[j],
            (false, false) => unreachable!(),
        };
        acc += (ca - cb).abs() * (x - prev_x);
        while i < a.len() && a[i] == x {
            ca += inc_a;
            i += 1;
        }
        while j < b.len() && b[j] == x {
            cb += inc_b;
            j += 1;
        }
        prev_x = x;
    }
    // tail to 1.0：理论上 ca == cb == 1.0，浮点 round-off 残留量级 < 1e-15。
    acc += (ca - cb).abs() * (1.0_f64 - prev_x).max(0.0);
    acc
}

// ============================================================================
// k-means + L2（D-230 / D-231 / D-232 / D-235 / D-236）
// ============================================================================

/// k-means 收敛参数（D-232）。
#[derive(Copy, Clone, Debug)]
pub struct KMeansConfig {
    /// 簇数 K（≤ 候选点数量）。
    pub k: u32,
    /// 上限迭代数（D-232 `max_iter = 100`）。
    pub max_iter: u32,
    /// centroid_shift_l_inf 收敛阈值（D-232 `1e-4`）。
    pub centroid_shift_tol: f64,
}

impl KMeansConfig {
    pub const fn default_d232(k: u32) -> KMeansConfig {
        KMeansConfig {
            k,
            max_iter: 100,
            centroid_shift_tol: 1e-4,
        }
    }
}

/// k-means 输出。
#[derive(Clone, Debug)]
pub struct KMeansResult {
    /// `centroids[c][d]` = cluster c 在维度 d 的中心值；`centroids.len() == k`。
    pub centroids: Vec<Vec<f64>>,
    /// `assignments[i]` = sample i 所属 cluster id ∈ 0..k。
    pub assignments: Vec<u32>,
}

/// D-235 k-means++ 抽样的浮点距离平方量化方案：`d2[i] ∈ [0, D2_MAX]` 量化到
/// `[0, 2^40]` 的 u64，累积和最大 N × 2^40，N ≤ 2_000_000 时 sum ≤ 2^61 安全在 u64 内。
const D2_MAX: f64 = 9.0;
const D2_QUANT_SCALE: u64 = 1u64 << 40;
const KMEANS_N_MAX: usize = 2_000_000;

/// k-means + k-means++ 初始化（D-230 / D-231 / D-232 / D-235）。
///
/// `features[i]` = 第 i 个 sample 的特征向量（所有 sample 等维）。
///
/// `op_id_init` 用于 k-means++ 初始化的 RngSource sub-stream（D-228，传入对应街
/// 的 `KMEANS_PP_INIT_*` 常量）；`op_id_split` 用于空 cluster split（同上，传入
/// `EMPTY_CLUSTER_SPLIT_*`）。
///
/// `master_seed` = `BucketTable.metadata.training_seed`，全程经 D-228
/// SplitMix64 finalizer 派生 sub-stream（不直接复用）。
///
/// 收敛判据：D-232 max_iter 与 centroid_shift_l_inf 的 OR。tie-break：D-235
/// "数据点到多个 cluster 距离严格相等时取小 cluster id"。
#[allow(clippy::needless_range_loop)] // index-style 循环用 cluster id 双层访问 centroids[c][d] 更清晰
pub fn kmeans_fit(
    features: &[Vec<f64>],
    cfg: KMeansConfig,
    master_seed: u64,
    op_id_init: u32,
    op_id_split: u32,
) -> KMeansResult {
    let n = features.len();
    let k = cfg.k as usize;
    assert!(n >= k, "kmeans_fit: n={n} < k={k}");
    assert!(n <= KMEANS_N_MAX, "kmeans_fit: n={n} > KMEANS_N_MAX");
    let dim = features.first().map(|v| v.len()).unwrap_or(0);
    assert!(dim > 0, "kmeans_fit: feature vectors empty");

    // 1. k-means++ 初始化（D-231）。
    let mut centroids = kmeans_pp_init(features, k, master_seed, op_id_init);

    // 2. k-means 主迭代（D-230 / D-232）。
    let mut assignments: Vec<u32> = vec![0u32; n];
    let mut split_sub_index: u32 = 0;
    for _iter in 0..cfg.max_iter {
        // 2a. assignment：每个 sample 分配到最近 centroid（L2² 距离，tie-break 小 id）。
        for (i, sample) in features.iter().enumerate() {
            let mut best_c: u32 = 0;
            let mut best_d2 = l2_sq(sample, &centroids[0]);
            for c in 1..k {
                let d2 = l2_sq(sample, &centroids[c]);
                if d2 < best_d2 {
                    best_d2 = d2;
                    best_c = c as u32;
                }
            }
            assignments[i] = best_c;
        }

        // 2b. centroid 更新：对每个 cluster 重新计算中心；记录 max 位移。
        let mut new_centroids: Vec<Vec<f64>> = vec![vec![0.0; dim]; k];
        let mut counts: Vec<u64> = vec![0; k];
        for (i, sample) in features.iter().enumerate() {
            let c = assignments[i] as usize;
            counts[c] += 1;
            for d in 0..dim {
                new_centroids[c][d] += sample[d];
            }
        }
        for c in 0..k {
            if counts[c] > 0 {
                let inv = 1.0 / counts[c] as f64;
                for d in 0..dim {
                    new_centroids[c][d] *= inv;
                }
            } else {
                // 空 cluster：D-236 split——从最大 cluster 中取距离最远点切出。
                split_empty_cluster(
                    features,
                    &assignments,
                    &mut new_centroids,
                    &counts,
                    c,
                    master_seed,
                    op_id_split,
                    split_sub_index,
                );
                split_sub_index += 1;
            }
        }

        // 2c. 收敛判定：max over centroids of max over dims of |c_new - c_old|。
        let mut shift_inf: f64 = 0.0;
        for c in 0..k {
            for d in 0..dim {
                let s = (new_centroids[c][d] - centroids[c][d]).abs();
                if s > shift_inf {
                    shift_inf = s;
                }
            }
        }
        centroids = new_centroids;
        if shift_inf <= cfg.centroid_shift_tol {
            break;
        }
    }

    // 3. 最终 assignment（centroid 已更新，重算一次保证一致）。
    for (i, sample) in features.iter().enumerate() {
        let mut best_c: u32 = 0;
        let mut best_d2 = l2_sq(sample, &centroids[0]);
        for c in 1..k {
            let d2 = l2_sq(sample, &centroids[c]);
            if d2 < best_d2 {
                best_d2 = d2;
                best_c = c as u32;
            }
        }
        assignments[i] = best_c;
    }

    KMeansResult {
        centroids,
        assignments,
    }
}

/// k-means++ 初始化（D-231 / D-235）。第一个 centroid 直接取 sample 0；后续每个
/// centroid 按到最近已选 centroid 的距离平方加权概率抽样。RngSource sub-stream
/// 经 `derive_substream_seed(master_seed, op_id_init, sub_index)` 派生。
fn kmeans_pp_init(
    features: &[Vec<f64>],
    k: usize,
    master_seed: u64,
    op_id_init: u32,
) -> Vec<Vec<f64>> {
    let n = features.len();
    let dim = features[0].len();
    let mut centroids: Vec<Vec<f64>> = Vec::with_capacity(k);

    // c0：取 sample 0（确定性；与 stage 1 D-027 显式 RngSource 同型——首个样本不
    // 消耗 RngSource，但每次 init RngSource 派生独立 sub_seed 保证后续抽样 byte-equal）。
    let sub_seed = rng_substream::derive_substream_seed(master_seed, op_id_init, 0);
    let mut rng = ChaCha20Rng::from_seed(sub_seed);

    centroids.push(features[0].clone());

    // 后续 c1..k-1：D-235 量化抽样路径。
    let mut min_d2: Vec<f64> = vec![f64::INFINITY; n];
    // 初始化 min_d2[i] = ||features[i] - c0||²
    for i in 0..n {
        min_d2[i] = l2_sq(&features[i], &centroids[0]);
    }

    for _c in 1..k {
        // D-235 ① 量化：d2_q[i] = (clamp(d2[i], 0, D2_MAX) / D2_MAX * 2^40) as u64
        let mut d2_q: Vec<u64> = Vec::with_capacity(n);
        for &d2 in min_d2.iter() {
            let clamped = d2.clamp(0.0, D2_MAX);
            let scaled = clamped / D2_MAX * (D2_QUANT_SCALE as f64);
            d2_q.push(scaled as u64);
        }

        // D-235 ② 累积 cum_q[i] = sum_{j ≤ i} d2_q[j]（u64 安全：N ≤ 2_000_000 时
        // sum ≤ 2_000_000 × 2^40 ≈ 2^61 < u64::MAX）。
        let mut cum_q: Vec<u64> = Vec::with_capacity(n);
        let mut acc: u64 = 0;
        for &q in d2_q.iter() {
            acc = acc.saturating_add(q);
            cum_q.push(acc);
        }

        // D-235 ③ 零和 fallback：若 cum_q[N-1] == 0，取最小 index 的未选点。
        let next_idx: usize;
        if acc == 0 {
            // 找未被作为 centroid 的最小 index（即 min_d2[i] > 0 表示未选；然而
            // 可能所有 sample 与已选 centroid 重合，此时取 0 即可保证确定性）。
            next_idx = 0;
        } else {
            // D-235 ④ sample：r = next_u64() % cum_q[N-1]；二分查找最小 i 使得 cum_q[i] > r。
            let r = rng.next_u64() % acc;
            // 二分：left = lower bound where cum_q[left] > r。
            let mut lo: usize = 0;
            let mut hi: usize = n;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if cum_q[mid] > r {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            next_idx = lo;
        }

        let new_centroid = features[next_idx].clone();
        // 更新 min_d2：对每个 sample，取与新 centroid 距离平方与原 min_d2 的较小值。
        for i in 0..n {
            let d2 = l2_sq(&features[i], &new_centroid);
            if d2 < min_d2[i] {
                min_d2[i] = d2;
            }
        }
        centroids.push(new_centroid);
        // dim 仅用于断言；保持参考避免编译器警告。
        debug_assert_eq!(centroids.last().unwrap().len(), dim);
    }

    centroids
}

/// 空 cluster split（D-236）：从样本数最多的 cluster 中找到距离该 cluster
/// centroid 最远的 sample，复制为新 centroid（cluster id = `empty_idx`）。
/// tie-break：距离严格相等时取最小 sample id（D-235 / D-236 RngSource tie-break
/// 的退化路径，本路径不抽样，无需 RNG 消费）。
#[allow(clippy::too_many_arguments)] // 8 个参数对应 D-236 完整 split 协议入参
#[allow(clippy::needless_range_loop)]
fn split_empty_cluster(
    features: &[Vec<f64>],
    assignments: &[u32],
    centroids: &mut [Vec<f64>],
    counts: &[u64],
    empty_idx: usize,
    _master_seed: u64,
    _op_id_split: u32,
    _sub_index: u32,
) {
    let n = features.len();
    // 找 counts 最大的 cluster id（tie-break 取最小）。
    let mut best_c = 0usize;
    let mut best_count = counts[0];
    for c in 1..counts.len() {
        if counts[c] > best_count {
            best_count = counts[c];
            best_c = c;
        }
    }
    // 计算最远 sample 在 best_c 内。
    let target_centroid = &centroids[best_c];
    let mut farthest_idx = 0usize;
    let mut farthest_d2: f64 = -1.0;
    for i in 0..n {
        if assignments[i] as usize != best_c {
            continue;
        }
        let d2 = l2_sq(&features[i], target_centroid);
        if d2 > farthest_d2 {
            farthest_d2 = d2;
            farthest_idx = i;
        }
    }
    // 把 farthest_idx 的特征作为 empty_idx cluster 的 centroid。
    centroids[empty_idx] = features[farthest_idx].clone();
}

fn l2_sq(a: &[f64], b: &[f64]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = 0.0_f64;
    for i in 0..a.len() {
        let d = a[i] - b[i];
        acc += d * d;
    }
    acc
}

// ============================================================================
// D-236b 重编号：训练完成后按 bucket 内 EHS 中位数升序重排 cluster id。
// ============================================================================

/// 输入：原始 cluster centroid + 原始 assignments + 每个 sample 的 EHS。
/// 输出：重排后的 (centroids, assignments)，bucket id 0 = 最弱 / k-1 = 最强。
///
/// tie-break（D-236b）：① EHS 中位数严格相等时按 centroid 向量字典序（u8 量化后
/// 字节序）；② centroid 字节序也相等时按旧 cluster id 升序。
pub fn reorder_by_ehs_median(
    centroids: Vec<Vec<f64>>,
    assignments: Vec<u32>,
    ehs_per_sample: &[f64],
) -> (Vec<Vec<f64>>, Vec<u32>) {
    let k = centroids.len();
    let n = assignments.len();
    debug_assert_eq!(ehs_per_sample.len(), n);

    // 1. 按 cluster 收集 EHS samples。
    let mut by_cluster: Vec<Vec<f64>> = vec![Vec::new(); k];
    for i in 0..n {
        let c = assignments[i] as usize;
        by_cluster[c].push(ehs_per_sample[i]);
    }
    // 2. 计算每个 cluster 的 EHS 中位数；空 cluster 用 NaN 占位（D-236 已保证 0 空）。
    let medians: Vec<f64> = by_cluster.iter().map(|v| median(v)).collect();
    // 3. 计算每个 cluster 的 centroid u8 字节序（D-241 量化）。
    let bytes: Vec<Vec<u8>> = quantize_centroids_simple_bytes(&centroids);
    // 4. 排序：按 (median, byte_seq, old_cluster_id) 升序。
    let mut order: Vec<usize> = (0..k).collect();
    order.sort_by(|&a, &b| {
        match medians[a]
            .partial_cmp(&medians[b])
            .unwrap_or(std::cmp::Ordering::Equal)
        {
            std::cmp::Ordering::Equal => match bytes[a].cmp(&bytes[b]) {
                std::cmp::Ordering::Equal => a.cmp(&b),
                other => other,
            },
            other => other,
        }
    });
    // 5. 构造 old → new mapping：old cluster `order[new]` → new id `new`。
    let mut old_to_new: Vec<u32> = vec![0u32; k];
    for (new_id, &old_id) in order.iter().enumerate() {
        old_to_new[old_id] = new_id as u32;
    }
    // 6. 重排 centroids 与 assignments。
    let new_centroids: Vec<Vec<f64>> = order.iter().map(|&old| centroids[old].clone()).collect();
    let new_assignments: Vec<u32> = assignments
        .iter()
        .map(|&c| old_to_new[c as usize])
        .collect();
    (new_centroids, new_assignments)
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        0.5 * (sorted[n / 2 - 1] + sorted[n / 2])
    }
}

/// D-241 quick u8 量化（仅用于 D-236b tie-break；与 `quantize_centroids_u8`
/// 共享算法）。每维独立 min/max 到 [0, 255]。空 dim 不应出现，dim > 0 由
/// `kmeans_fit` 入口断言保证。
fn quantize_centroids_simple_bytes(centroids: &[Vec<f64>]) -> Vec<Vec<u8>> {
    let k = centroids.len();
    if k == 0 {
        return Vec::new();
    }
    let dim = centroids[0].len();
    let mut min_per_dim: Vec<f64> = vec![f64::INFINITY; dim];
    let mut max_per_dim: Vec<f64> = vec![f64::NEG_INFINITY; dim];
    for c in centroids {
        for d in 0..dim {
            if c[d] < min_per_dim[d] {
                min_per_dim[d] = c[d];
            }
            if c[d] > max_per_dim[d] {
                max_per_dim[d] = c[d];
            }
        }
    }
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(k);
    for c in centroids {
        let mut row = Vec::with_capacity(dim);
        for d in 0..dim {
            let q = quantize_one_dim(c[d], min_per_dim[d], max_per_dim[d]);
            row.push(q);
        }
        out.push(row);
    }
    out
}

// ============================================================================
// D-241 centroid u8 量化（公开供 BucketTable 写出使用）
// ============================================================================

/// 每条街独立量化：每维 min/max + u8 量化。返回 (quantized_data, min_per_dim, max_per_dim)。
/// `quantized_data[c][d]` ∈ [0, 255] = `((centroids[c][d] - min[d]) / (max[d] - min[d]) * 255)`，
/// 反量化：`x = min + (q / 255.0) * (max - min)`。
pub fn quantize_centroids_u8(centroids: &[Vec<f64>]) -> (Vec<Vec<u8>>, Vec<f32>, Vec<f32>) {
    let k = centroids.len();
    if k == 0 {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let dim = centroids[0].len();
    let mut min_per_dim: Vec<f64> = vec![f64::INFINITY; dim];
    let mut max_per_dim: Vec<f64> = vec![f64::NEG_INFINITY; dim];
    for c in centroids {
        for d in 0..dim {
            if c[d] < min_per_dim[d] {
                min_per_dim[d] = c[d];
            }
            if c[d] > max_per_dim[d] {
                max_per_dim[d] = c[d];
            }
        }
    }
    let mut quantized: Vec<Vec<u8>> = Vec::with_capacity(k);
    for c in centroids {
        let mut row: Vec<u8> = Vec::with_capacity(dim);
        for d in 0..dim {
            row.push(quantize_one_dim(c[d], min_per_dim[d], max_per_dim[d]));
        }
        quantized.push(row);
    }
    let min_f32: Vec<f32> = min_per_dim.iter().map(|x| *x as f32).collect();
    let max_f32: Vec<f32> = max_per_dim.iter().map(|x| *x as f32).collect();
    (quantized, min_f32, max_f32)
}

fn quantize_one_dim(value: f64, min: f64, max: f64) -> u8 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() {
        return 0;
    }
    if (max - min).abs() < 1e-12 {
        return 0;
    }
    let normed = ((value - min) / (max - min)).clamp(0.0, 1.0);
    (normed * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emd_identical_zero() {
        let a = [0.1, 0.5, 0.9];
        let b = [0.1, 0.5, 0.9];
        assert!(emd_1d_unit_interval(&a, &b).abs() < 1e-12);
    }

    #[test]
    fn emd_disjoint_extremes_one() {
        let a = [0.0, 0.0, 0.0];
        let b = [1.0, 1.0, 1.0];
        assert!((emd_1d_unit_interval(&a, &b) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn emd_unequal_length_uses_full_distribution() {
        // §C-rev2 §5a 防回归：旧 `acc / min(len_a, len_b)` 截断会丢弃 a[1]=0.9，
        // 算出 |0.8-0.5| = 0.3；步函数 CDF 积分用全部样本，正确值 0.35：
        //   F_a: 0 below 0.8, 0.5 in [0.8, 0.9), 1.0 above 0.9
        //   F_b: 0 below 0.5, 1.0 above 0.5
        //   ∫|F_a - F_b| = 1.0·(0.8-0.5) + 0.5·(0.9-0.8) = 0.3 + 0.05 = 0.35
        let a = [0.8, 0.9];
        let b = [0.5];
        let emd = emd_1d_unit_interval(&a, &b);
        assert!(
            (emd - 0.35).abs() < 1e-12,
            "step-CDF integral expected 0.35, got {emd}"
        );
    }

    #[test]
    fn emd_unequal_length_same_distribution_near_zero() {
        // 同分布不同样本数 → EMD 应远小于"明显差异"（如上面 0.35）。两组
        // deterministic 均匀样本：100 等距 vs 1000 等距。理论上对应的步函数 CDF
        // 都接近真实 uniform[0,1]，EMD 应 < 0.02。
        let a: Vec<f64> = (0..100).map(|i| (i as f64 + 0.5) / 100.0).collect();
        let b: Vec<f64> = (0..1000).map(|i| (i as f64 + 0.5) / 1000.0).collect();
        let emd = emd_1d_unit_interval(&a, &b);
        assert!(emd < 0.02, "1000 vs 100 同分布 EMD 应 < 0.02，got {emd}");
    }

    #[test]
    fn kmeans_two_well_separated_clusters() {
        // 8 个 1-d 特征点：4 个在 0.1 附近，4 个在 0.9 附近。k=2 应分清。
        let features: Vec<Vec<f64>> = vec![
            vec![0.1],
            vec![0.12],
            vec![0.08],
            vec![0.11],
            vec![0.9],
            vec![0.92],
            vec![0.88],
            vec![0.91],
        ];
        let cfg = KMeansConfig::default_d232(2);
        let res = kmeans_fit(&features, cfg, 0xCAFE_BABE, 0x0003_0001, 0x0004_0001);
        assert_eq!(res.assignments.len(), 8);
        // 前 4 应同 cluster，后 4 应同 cluster，但不一定 0 / 1。
        let c_first = res.assignments[0];
        for i in 1..4 {
            assert_eq!(
                res.assignments[i], c_first,
                "first 4 should share cluster, got {:?}",
                res.assignments
            );
        }
        let c_last = res.assignments[4];
        assert_ne!(c_first, c_last);
        for i in 5..8 {
            assert_eq!(res.assignments[i], c_last);
        }
    }

    #[test]
    fn kmeans_deterministic_same_seed() {
        let features: Vec<Vec<f64>> = (0..50)
            .map(|i| vec![(i as f64) / 50.0, ((i * 7) % 13) as f64 / 13.0])
            .collect();
        let cfg = KMeansConfig::default_d232(5);
        let r1 = kmeans_fit(&features, cfg, 0xDEAD_BEEF, 0x0003_0001, 0x0004_0001);
        let r2 = kmeans_fit(&features, cfg, 0xDEAD_BEEF, 0x0003_0001, 0x0004_0001);
        assert_eq!(r1.assignments, r2.assignments);
        for c in 0..5 {
            for d in 0..2 {
                assert!((r1.centroids[c][d] - r2.centroids[c][d]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn reorder_monotonic_by_ehs_median() {
        // 3 cluster：ehs medians = [0.5, 0.2, 0.8] → 重编号后 [old=1 → new=0,
        // old=0 → new=1, old=2 → new=2]。
        let centroids = vec![vec![0.5], vec![0.2], vec![0.8]];
        let assignments: Vec<u32> = vec![0, 0, 1, 1, 2, 2];
        let ehs = vec![0.5, 0.5, 0.2, 0.2, 0.8, 0.8];
        let (new_centroids, new_assignments) = reorder_by_ehs_median(centroids, assignments, &ehs);
        // new_centroids 顺序：weakest (0.2) → strongest (0.8)
        assert!((new_centroids[0][0] - 0.2).abs() < 1e-12);
        assert!((new_centroids[1][0] - 0.5).abs() < 1e-12);
        assert!((new_centroids[2][0] - 0.8).abs() < 1e-12);
        // 原先 cluster 1 的 sample (id 2, 3) 应该被重排到 new cluster 0。
        assert_eq!(new_assignments[2], 0);
        assert_eq!(new_assignments[3], 0);
        assert_eq!(new_assignments[0], 1);
        assert_eq!(new_assignments[4], 2);
    }

    #[test]
    fn quantize_u8_round_trip_within_tol() {
        let centroids = vec![vec![0.0, 0.5, 1.0], vec![0.25, 0.75, 0.0]];
        let (q, min, max) = quantize_centroids_u8(&centroids);
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].len(), 3);
        // 反量化检查：dim 0 min=0, max=0.25 → q[0][0] = 0, q[1][0] = 255。
        assert_eq!(q[0][0], 0);
        assert_eq!(q[1][0], 255);
        assert!((min[0] - 0.0).abs() < 1e-6);
        assert!((max[0] - 0.25).abs() < 1e-6);
    }
}

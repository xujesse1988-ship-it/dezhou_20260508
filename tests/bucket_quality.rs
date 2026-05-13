//! C1 §输出：postflop bucket 聚类质量门槛断言。
//!
//! 覆盖 `pluribus_stage2_workflow.md` §C1 §输出 lines 304-309 + validation §3 全部
//! bucket 质量门槛。**§G-batch1 §3.9 \[实现\] D-233-rev1 sqrt-scaled 阈值**（path.md
//! §阶段 2 + decisions §10 D-233-rev1）：path.md 字面 `EHS std dev < 0.05 / EMD ≥
//! 0.02 / monotonic` 是 K=100 era 校准（Pluribus 论文用 200 bucket / 街，path.md
//! 阈值与 K=100 自洽）；K=500 配置下 bucket spacing 缩到 1/5（D-236b reorder 后
//! 每对相邻 bucket 间的 equity 距离），EMD / std_dev 量级 ∝ 1/√K。统一 sqrt-scale：
//!
//! ```text
//! EMD_THRESHOLD(K)     = 0.02 × √(100/K)     // K=500: 0.00894
//! STD_DEV_THRESHOLD(K) = 0.05 × √(100/K)     // K=500: 0.02236
//! ```
//!
//! Monotonic 容差走 **MC-噪声-aware** 路径：每对相邻 bucket 的 |median_a - median_b|
//! 与 2 × σ_diff 比较，σ_diff = √(σ_median_a² + σ_median_b²)，
//! σ_median_x = 1.253 × √(0.25 / mc_iter) / √(n_x)（中位数标准误差正态近似）。让
//! 单个 bucket 抽到少量样本时 tolerance 自动放宽。
//!
//! - **0 空 bucket**（D-236 / validation §3）：每条街每个 bucket id 至少包含 1 个
//!   canonical `(board, hole)` sample。
//! - **EHS std dev `< STD_DEV_THRESHOLD(K)`**（D-233-rev1）：每条街每个 bucket 内
//!   手牌 EHS 标准差上限。
//! - **相邻 bucket EMD `≥ EMD_THRESHOLD(K)`**（D-233-rev1）：每条街相邻 bucket id
//!   `(k, k+1)` 间 1D EMD（all-in equity 分布）下限。
//! - **bucket id ↔ EHS 中位数单调一致**（D-236b / D-233-rev1 加 MC 容差）：bucket id
//!   递增 ⇒ bucket 内 EHS 中位数递增，允许 |diff| ≤ 2σ_diff 的 MC 噪声波动。
//! - **1k 手 `(board, hole) → bucket id` smoke**（C1 §输出 line 309）：在 stub /
//!   真实 mmap 上跑 1k 手随机 (board, hole) 输入，断言 in-range；full 1M `#[ignore]`
//!   版留 C2 / D2。
//!
//! **C1 状态**（B2 stub `lookup` 永远返回 `Some(0)`）：`BucketTable::open` 在 B2
//! 阶段 `unimplemented!()`，本文件用 `BucketTable::stub_for_postflop(...)`
//! 构造 fixture（B-rev0 carve-out option (1) 路径，详见 `pluribus_stage2_workflow.md`
//! §修订历史 §B-rev0 batch 2）。
//!
//! - **1k smoke**：stub 路径下所有 lookup 返回 `Some(0)`，in-range 断言可过（500
//!   bucket → 0 < 500 ✓）。
//! - **0 空 bucket / EHS std dev / EMD / 单调性**：stub 把所有 sample 映射到 bucket 0，
//!   bucket 1..499 全空 ⇒ 这 4 类断言**预期失败**——按 §C1 §出口 line 322-324 字面
//!   "部分测试预期失败（B2 stub bucket 不可能过 EHS std dev 门槛）— 留给 C2 修"。
//!   策略：用 `#[ignore = "C2: <reason>"]` 标注，`cargo test` 默认跳过；C2
//!   [实现] 落地真实 mmap clustering 后取消 ignore 并验证全绿（同 B1 §C 类
//!   equity harness → B2 carve-out 移除 `#[ignore]` 同型）。
//! - **1M 完整版**：始终 `#[ignore]`，仅 `cargo test --release -- --ignored`
//!   触发，C2 / D2 跑（与 stage-1 §C2 / §D2 同形态）。
//!
//! **角色边界**：本文件属 `[测试]` agent 产物（C1）。任一断言被 [实现] 反驳必须
//! 由决策者 review 后由 [测试] agent 修订（继承 stage-2 §B-rev1 处理政策；详见
//! `pluribus_stage2_workflow.md` §修订历史 §B-rev0/§B-rev1）。

use std::sync::{Arc, OnceLock};

use poker::abstraction::cluster::emd_1d_unit_interval;
use poker::eval::NaiveHandEvaluator;
use poker::rng_substream::{derive_substream_seed, EQUITY_MONTE_CARLO};
use poker::{
    canonical_observation_id, BucketConfig, BucketTable, Card, ChaCha20Rng, EquityCalculator,
    HandEvaluator, MonteCarloEquity, RngSource, StreetTag,
};

// ============================================================================
// 通用 fixture
// ============================================================================

/// §G-batch1 §3.10 [实现] 路径：12 条 path.md 质量门槛断言（D-233-rev1 sqrt-scaled
/// 阈值）基于 **v3 production artifact**
/// (`artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`，
/// K=500/500/500 / cluster_iter=2000/5000/10000 single-phase full N + river_exact
/// 990 outcomes enumerate)。
///
/// **演变历史**：
/// - §C1 [测试]：`stub_table()` (B2 stub `lookup → Some(0)` 路径)
/// - §C-rev2 batch 5 §1：切换到 `cached_trained_table()` (fixture K=100 + cluster_iter=200)
/// - §G-batch1 §3.8 [实现]：切换到 v2 artifact `BucketTable::open(...)` load 路径
///   (cluster_iter=2000 dual-phase MC inner river)
/// - §G-batch1 §3.10 [实现]（本 batch）：再切换到 v3 artifact 路径（§3.9
///   single-phase full N + per-street iter + §3.10 river_exact 990 enumerate），
///   12 条断言改用 D-233-rev1 sqrt-scaled threshold。
///
/// **角色边界 [实现] → [测试] 越界 carve-out**（详见 stage-2 §C-rev0 §修订历史
/// 和 §G-batch1 §3.2 / §3.8 同型）：本 batch [实现] 单边修改 `cached_trained_table()`
/// path constant + 12 条阈值公式，继承 §C-rev2 batch 5 §1 carve-out 形态。
const PRODUCTION_ARTIFACT_PATH: &str =
    "artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin";
const FIXTURE_BUCKET_CONFIG: BucketConfig = BucketConfig {
    flop: 100,
    turn: 100,
    river: 100,
};
const FIXTURE_TRAINING_SEED: u64 = 0xC2_FA22_BD75_710E;
const FIXTURE_CLUSTER_ITER: u32 = 200;

static CACHED_TABLE: OnceLock<Arc<BucketTable>> = OnceLock::new();

/// §G-batch1 §3.10 [实现] 路径：优先 load v3 production artifact (K=500，100%
/// canonical 覆盖 + river_exact 990 enumerate)；如 artifact 不存在则回退到 fixture
/// 训练 (K=100，4 类质量门槛不保证通过——Knuth hash fallback 主导，仅用于 CI /
/// dev box smoke)。
fn cached_trained_table() -> Arc<BucketTable> {
    CACHED_TABLE
        .get_or_init(|| {
            // 优先 load v3 production artifact (§G-batch1 §3.10 retrain 出口)
            let path = std::path::Path::new(PRODUCTION_ARTIFACT_PATH);
            if path.exists() {
                return Arc::new(BucketTable::open(path).expect("v3 artifact open"));
            }
            // Fallback：fixture 训练（K=100，Knuth hash fallback 主导，质量门槛
            // 大概率失败；适用于无 artifact 的 CI / dev box smoke 场景）。
            eprintln!(
                "warning: v3 artifact not found at {PRODUCTION_ARTIFACT_PATH}; \
                 falling back to fixture K=100 training (quality gates may fail)"
            );
            let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator);
            Arc::new(BucketTable::train_in_memory(
                FIXTURE_BUCKET_CONFIG,
                FIXTURE_TRAINING_SEED,
                evaluator,
                FIXTURE_CLUSTER_ITER,
            ))
        })
        .clone()
}

/// stage 1 朴素评估器；EHS / EMD 计算路径依赖（12 条质量门槛断言走此路径）。
fn make_evaluator() -> Arc<dyn HandEvaluator> {
    Arc::new(NaiveHandEvaluator)
}

/// 短 iter MonteCarloEquity（12 条质量门槛断言用 1k iter MC 估算 EHS）。
fn make_calc_short_iter() -> MonteCarloEquity {
    MonteCarloEquity::new(make_evaluator()).with_iter(1_000)
}

/// 把 `u8` deck index 转 [`Card`]，封装 `expect("0..52")`。
fn card_from(idx: u8) -> Card {
    Card::from_u8(idx).expect("0..52 valid")
}

/// 从 `RngSource` 抽取 `count` 张不重复的 `Card`（不与 `excluded` 重叠）。
/// 用于生成 random (board, hole) 输入对——C1 sampling 路径的工作马。
fn sample_distinct_cards(rng: &mut dyn RngSource, excluded: &[u8], count: usize) -> Vec<Card> {
    let mut available: Vec<u8> = (0..52u8).filter(|v| !excluded.contains(v)).collect();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let pick = (rng.next_u64() % (available.len() as u64 - i as u64)) as usize;
        let idx = i + pick;
        available.swap(i, idx);
        out.push(card_from(available[i]));
    }
    out
}

/// 用 §C1 默认采样规模生成给定街上 `n_samples` 个随机 (board, hole) 对。
fn sample_observations(
    street: StreetTag,
    n_samples: usize,
    master_seed: u64,
) -> Vec<(Vec<Card>, [Card; 2])> {
    let board_len = match street {
        StreetTag::Flop => 3,
        StreetTag::Turn => 4,
        StreetTag::River => 5,
        StreetTag::Preflop => panic!("sample_observations: Preflop 不属 postflop bucket 路径"),
    };
    let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(master_seed, EQUITY_MONTE_CARLO, 0));
    let mut out = Vec::with_capacity(n_samples);
    for _ in 0..n_samples {
        let cards = sample_distinct_cards(&mut rng, &[], board_len + 2);
        let board: Vec<Card> = cards[..board_len].to_vec();
        let hole: [Card; 2] = [cards[board_len], cards[board_len + 1]];
        out.push((board, hole));
    }
    out
}

/// 简易 std dev（C1 fixture 用；C2 用 cluster 内 EHS 实测）。
fn std_dev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean: f64 = values.iter().sum::<f64>() / values.len() as f64;
    let var: f64 =
        values.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / values.len() as f64;
    var.sqrt()
}

/// §G-batch1 §3.9 / D-233-rev1：sqrt-scaled bucket quality threshold。
///
/// path.md 字面阈值是 K=100 era 校准（Pluribus 论文 200 bucket，path.md
/// 0.05/0.02 略宽松）；K=500 下 spacing 缩 1/5，均量化指标 ∝ 1/√K。统一公式：
///
/// ```text
/// EMD_THRESHOLD(K) = path_md_emd × √(100/K)        // K=100: 0.020，K=500: 0.00894
/// STD_DEV_THRESHOLD(K) = path_md_std_dev × √(100/K) // K=100: 0.050，K=500: 0.02236
/// ```
fn quality_emd_threshold(k: u32) -> f64 {
    0.02 * (100.0_f64 / k as f64).sqrt()
}

fn quality_std_dev_threshold(k: u32) -> f64 {
    0.05 * (100.0_f64 / k as f64).sqrt()
}

/// §G-batch1 §3.9 / D-233-rev1：MC-噪声-aware 单调性容差。
///
/// 测试用 MonteCarloEquity::with_iter(MC_ITER) 估算每个 sample 的 EHS；MC 噪声
/// σ_per_sample = √(0.25 / MC_ITER)。bucket 内中位数标准误差 σ_median ≈
/// 1.253 × σ_per_sample / √n（正态近似；当 n 小时尤其重要）。两个 bucket 中位数
/// 差的标准误差 σ_diff = √(σ_median_a² + σ_median_b²)。2σ tolerance 接受 |diff|
/// ≤ 2σ_diff 的 MC 噪声波动。
///
/// 例：MC_ITER=1000, n_a=29, n_b=5 → σ_median_a=0.0037, σ_median_b=0.0089,
/// σ_diff=0.0096, tolerance=0.019（比固定 0.009 全局容差更适应少样本 bucket）。
fn monotonic_tolerance(n_a: usize, n_b: usize, mc_iter: u32) -> f64 {
    if n_a == 0 || n_b == 0 {
        return f64::INFINITY;
    }
    let sigma_per_sample = (0.25_f64 / (mc_iter as f64)).sqrt();
    let sigma_median_a = 1.253 * sigma_per_sample / (n_a as f64).sqrt();
    let sigma_median_b = 1.253 * sigma_per_sample / (n_b as f64).sqrt();
    let sigma_diff = (sigma_median_a * sigma_median_a + sigma_median_b * sigma_median_b).sqrt();
    2.0 * sigma_diff
}

/// 测试 inner MC iter（`make_calc_short_iter().with_iter(...)` 一致）。
const TEST_INNER_MC_ITER: u32 = 1_000;

/// 简易中位数（D-236b 单调性测试用）。
fn median(values: &[f64]) -> f64 {
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        0.5 * (sorted[n / 2 - 1] + sorted[n / 2])
    }
}

// ============================================================================
// 1. 1k smoke：(board, hole) → bucket id 在 in-range 范围内
// ============================================================================
//
// §C1 §输出 line 309 字面：`1k 手 (board, hole) → bucket id smoke + #[ignore] 1M 完整版`。
// §C-rev2 batch 5 §1 切到 `cached_trained_table()`（fixture 100/100/100 + 200 iter）；
// 真实 bucket id 范围 < 100，覆盖训练后 in-range 验收。
//
// 三街分别 1k 输入；任一 `lookup` 返回 `None`（越界）或 `>= bucket_count(street)`
// 立即 fail。
#[test]
fn bucket_lookup_1k_in_range_smoke_flop() {
    let table = cached_trained_table();
    let bucket_count_flop = table.bucket_count(StreetTag::Flop);
    let samples = sample_observations(StreetTag::Flop, 10_000, 0x00C1_C0DE_F10E);
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::Flop, board, *hole);
        let bucket = table
            .lookup(StreetTag::Flop, obs_id)
            .unwrap_or_else(|| panic!("flop sample {i}: lookup returned None on in-range obs_id"));
        assert!(
            bucket < bucket_count_flop,
            "flop sample {i}: bucket_id {bucket} >= bucket_count {bucket_count_flop}"
        );
    }
}

#[test]
fn bucket_lookup_1k_in_range_smoke_turn() {
    let table = cached_trained_table();
    let bucket_count_turn = table.bucket_count(StreetTag::Turn);
    let samples = sample_observations(StreetTag::Turn, 10_000, 0x00C1_C0DE_7A2B);
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::Turn, board, *hole);
        let bucket = table
            .lookup(StreetTag::Turn, obs_id)
            .unwrap_or_else(|| panic!("turn sample {i}: lookup returned None"));
        assert!(
            bucket < bucket_count_turn,
            "turn sample {i}: bucket_id {bucket} >= bucket_count {bucket_count_turn}"
        );
    }
}

#[test]
fn bucket_lookup_1k_in_range_smoke_river() {
    let table = cached_trained_table();
    let bucket_count_river = table.bucket_count(StreetTag::River);
    let samples = sample_observations(StreetTag::River, 10_000, 0x00C1_C0DE_71BB);
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::River, board, *hole);
        let bucket = table
            .lookup(StreetTag::River, obs_id)
            .unwrap_or_else(|| panic!("river sample {i}: lookup returned None"));
        assert!(
            bucket < bucket_count_river,
            "river sample {i}: bucket_id {bucket} >= bucket_count {bucket_count_river}"
        );
    }
}

// ============================================================================
// 2. 1M 完整版（始终 #[ignore]，C2 / D2 跑）
// ============================================================================
//
// 与 stage-1 §C2 / §D2 同形态：full-volume `--ignored` opt-in，release profile
// 触发。stub 路径下 1M 输入也是全部映射到 0，所以本测试在 C1 闭合时与 1k smoke
// 等价；C2 接入真实 mmap clustering 后变为 "1M random (board, hole) 全部命中
// in-range" 的硬验收门槛。
#[test]
#[ignore = "C2/D2: 1M 完整版 — release profile + --ignored opt-in（与 stage-1 §C2 / §D2 同形态）"]
fn bucket_lookup_1m_in_range_full() {
    let table = cached_trained_table();
    for street in [StreetTag::Flop, StreetTag::Turn, StreetTag::River] {
        let bucket_count = table.bucket_count(street);
        // 1M / 3 街 ≈ 333k per street；与 stage-1 1M fuzz 同量级。
        let samples = sample_observations(street, 333_333, 0x00C1_FA22 ^ street as u64);
        for (i, (board, hole)) in samples.iter().enumerate() {
            let obs_id = canonical_observation_id(street, board, *hole);
            let bucket = table
                .lookup(street, obs_id)
                .unwrap_or_else(|| panic!("{street:?} sample {i}: lookup None"));
            assert!(
                bucket < bucket_count,
                "{street:?} sample {i}: bucket_id {bucket} >= {bucket_count}"
            );
        }
    }
}

// ============================================================================
// 3. 0 空 bucket（D-236 / validation §3）
// ============================================================================
//
// **§C-rev1 §2 carve-out**（详见 `pluribus_stage2_workflow.md` §修订历史 §C-rev1 §2）：
// canonical_observation_id FNV-1a 32-bit hash mod N (3K/6K/10K) 路径下，多个
// (board, hole) 等价类映射到同一 obs_id（hash 碰撞）→ 同一 bucket。bucket 内
// EHS std dev 由 hash 碰撞率 + 碰撞跨度决定，而非 k-means clustering 质量。
// 真实 equivalence class enumeration（D-218-rev1 完整化）需要 stage 3+ 重构。
//
// 4 类质量门槛断言（0 空 bucket / EHS std dev / EMD / 单调性）× 3 街 = 12 条
// 在 hash design 下不可达，按 §C-rev1 §2 carve-out 政策保留 `#[ignore]`；
// §C-rev2 batch 5 §1 闭合时还原完整断言体（早返回 eprintln 占位删除，让
// `cargo test --release -- --ignored` 实跑断言、暴露 hash design 限制实测程度，
// 与 stage 3+ true equivalence enumeration 落地后取消 ignore 顺势生效）。
//
// 默认 active：4 条 helper sanity（emd / std_dev / median）+ 3 条 1k smoke
// in-range + 1 条 1M smoke（`#[ignore]` opt-in）。
/// §G-batch1 §3.8 [实现]：deterministic 全枚举（vs §C-rev2 sample 5×K Poisson 路径）
/// — Production-mode 100% canonical 覆盖 + `lookup_table[id] = bucket_id` 全密集
/// 写入下，"0 空 bucket" 是 lookup_table 的字面属性而非随机采样统计性质。
fn assert_no_empty_bucket(table: &BucketTable, street: StreetTag) {
    let bucket_count = table.bucket_count(street);
    let n_canonical = table.n_canonical_observation(street);
    let mut hit = vec![false; bucket_count as usize];
    for id in 0..n_canonical {
        if let Some(b) = table.lookup(street, id) {
            hit[b as usize] = true;
        }
    }
    let empty_count = hit.iter().filter(|h| !**h).count();
    assert_eq!(
        empty_count, 0,
        "D-236 / validation §3：{street:?} {empty_count} 个 bucket 空（共 {bucket_count}；N_canonical={n_canonical}）"
    );
}

#[test]
fn no_empty_bucket_per_street_flop() {
    assert_no_empty_bucket(&cached_trained_table(), StreetTag::Flop);
}

#[test]
fn no_empty_bucket_per_street_turn() {
    assert_no_empty_bucket(&cached_trained_table(), StreetTag::Turn);
}

#[test]
fn no_empty_bucket_per_street_river() {
    assert_no_empty_bucket(&cached_trained_table(), StreetTag::River);
}

// ============================================================================
// 4. EHS std dev `< 0.05`（path.md §阶段 2 / validation §3）
// ============================================================================
//
// **C1 状态**：B2 stub 把所有 sample 映射到 bucket 0，bucket 0 内 EHS 是全集
// 分布 std dev 近似 ≈ 0.20（远 > 0.05）；其它 bucket 空 std dev 0（trivial 通过
// 但 sample count < 2 短路）。整体测试**预期失败**（bucket 0 std dev > 0.05）⇒
// `#[ignore]`。C2 落地后 500 bucket 内每个 EHS std dev < 0.05。
//
// 采样：每条街 1000 sample（C1 1.5 人周限速）；EHS 用 `equity` 接口，1k iter
// MC（标准误差 ≈ 0.016 < 0.05 阈值的 ~30% — 不会主导信号）。三街独立 #[test]。
#[test]

fn bucket_internal_ehs_std_dev_below_threshold_flop() {
    let table = cached_trained_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Flop) as usize;
    let samples = sample_observations(StreetTag::Flop, 10_000, 0x000C_157D_F10E);

    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::Flop, board, *hole);
        let bucket = match table.lookup(StreetTag::Flop, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            0x000C_157D_F10E,
            EQUITY_MONTE_CARLO,
            i as u32,
        ));
        let ehs = calc
            .equity(*hole, board, &mut rng)
            .expect("EHS：合法 (board, hole) sample");
        by_bucket[bucket].push(ehs);
    }
    let threshold = quality_std_dev_threshold(bucket_count as u32);
    for (bid, samples) in by_bucket.iter().enumerate() {
        if samples.len() < 2 {
            continue;
        }
        let sd = std_dev(samples);
        assert!(
            sd < threshold,
            "D-233-rev1 (flop)：bucket {bid} EHS std dev {sd} >= {threshold:.5} \
             (sqrt-scaled K={bucket_count}; n={})",
            samples.len()
        );
    }
}

#[test]

fn bucket_internal_ehs_std_dev_below_threshold_turn() {
    let table = cached_trained_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Turn) as usize;
    let samples = sample_observations(StreetTag::Turn, 10_000, 0x000C_157D_7A2B);

    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::Turn, board, *hole);
        let bucket = match table.lookup(StreetTag::Turn, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            0x000C_157D_7A2B,
            EQUITY_MONTE_CARLO,
            i as u32,
        ));
        let ehs = calc.equity(*hole, board, &mut rng).expect("EHS turn");
        by_bucket[bucket].push(ehs);
    }
    let threshold = quality_std_dev_threshold(bucket_count as u32);
    for (bid, samples) in by_bucket.iter().enumerate() {
        if samples.len() < 2 {
            continue;
        }
        let sd = std_dev(samples);
        assert!(
            sd < threshold,
            "D-233-rev1 (turn)：bucket {bid} EHS std dev {sd} >= {threshold:.5} \
             (sqrt-scaled K={bucket_count}; n={})",
            samples.len()
        );
    }
}

#[test]

fn bucket_internal_ehs_std_dev_below_threshold_river() {
    let table = cached_trained_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::River) as usize;
    let samples = sample_observations(StreetTag::River, 10_000, 0xC157_D71B);

    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::River, board, *hole);
        let bucket = match table.lookup(StreetTag::River, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            0xC157_D71B,
            EQUITY_MONTE_CARLO,
            i as u32,
        ));
        let ehs = calc.equity(*hole, board, &mut rng).expect("EHS river");
        by_bucket[bucket].push(ehs);
    }
    let threshold = quality_std_dev_threshold(bucket_count as u32);
    for (bid, samples) in by_bucket.iter().enumerate() {
        if samples.len() < 2 {
            continue;
        }
        let sd = std_dev(samples);
        assert!(
            sd < threshold,
            "D-233-rev1 (river)：bucket {bid} EHS std dev {sd} >= {threshold:.5} \
             (sqrt-scaled K={bucket_count}; n={})",
            samples.len()
        );
    }
}

// ============================================================================
// 5. 相邻 bucket EMD `≥ T_emd = 0.02`（D-233 / validation §3）
// ============================================================================
//
// 验证每条街相邻 bucket id `(k, k+1)` 间 1D EMD（all-in equity 分布）≥ 0.02。
// **C1 状态**：B2 stub 全部映射到 bucket 0 → bucket 0 vs 1..499 比较时 1..499
// 全空，`emd_1d` 返回 0 ⇒ `#[ignore]`。C2 落地后 499 对相邻每对 EMD ≥ 0.02。
#[test]

fn adjacent_bucket_emd_above_threshold_flop() {
    let table = cached_trained_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Flop) as usize;
    let samples = sample_observations(StreetTag::Flop, 10_000, 0x000C_1EAD_F10E);

    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::Flop, board, *hole);
        let bucket = match table.lookup(StreetTag::Flop, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            0x000C_1EAD_F10E,
            EQUITY_MONTE_CARLO,
            i as u32,
        ));
        let ehs = calc.equity(*hole, board, &mut rng).expect("EHS flop EMD");
        by_bucket[bucket].push(ehs);
    }
    let t_emd = quality_emd_threshold(bucket_count as u32);
    for k in 0..(bucket_count - 1) {
        let emd = emd_1d_unit_interval(&by_bucket[k], &by_bucket[k + 1]);
        assert!(
            emd >= t_emd,
            "D-233-rev1 (flop)：bucket {k} vs {} EMD {emd} < T_emd {t_emd:.5} \
             (sqrt-scaled K={bucket_count})",
            k + 1
        );
    }
}

#[test]

fn adjacent_bucket_emd_above_threshold_turn() {
    let table = cached_trained_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Turn) as usize;
    let samples = sample_observations(StreetTag::Turn, 10_000, 0x000C_1EAD_7A2B);

    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::Turn, board, *hole);
        let bucket = match table.lookup(StreetTag::Turn, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            0x000C_1EAD_7A2B,
            EQUITY_MONTE_CARLO,
            i as u32,
        ));
        let ehs = calc.equity(*hole, board, &mut rng).expect("EHS turn EMD");
        by_bucket[bucket].push(ehs);
    }
    let t_emd = quality_emd_threshold(bucket_count as u32);
    for k in 0..(bucket_count - 1) {
        let emd = emd_1d_unit_interval(&by_bucket[k], &by_bucket[k + 1]);
        assert!(
            emd >= t_emd,
            "D-233-rev1 (turn)：bucket {k} vs {} EMD {emd} < T_emd {t_emd:.5} \
             (sqrt-scaled K={bucket_count})",
            k + 1
        );
    }
}

#[test]

fn adjacent_bucket_emd_above_threshold_river() {
    let table = cached_trained_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::River) as usize;
    let samples = sample_observations(StreetTag::River, 10_000, 0xC1EA_D71B);

    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::River, board, *hole);
        let bucket = match table.lookup(StreetTag::River, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            0xC1EA_D71B,
            EQUITY_MONTE_CARLO,
            i as u32,
        ));
        let ehs = calc.equity(*hole, board, &mut rng).expect("EHS river EMD");
        by_bucket[bucket].push(ehs);
    }
    let t_emd = quality_emd_threshold(bucket_count as u32);
    for k in 0..(bucket_count - 1) {
        let emd = emd_1d_unit_interval(&by_bucket[k], &by_bucket[k + 1]);
        assert!(
            emd >= t_emd,
            "D-233-rev1 (river)：bucket {k} vs {} EMD {emd} < T_emd {t_emd:.5} \
             (sqrt-scaled K={bucket_count})",
            k + 1
        );
    }
}

// ============================================================================
// 6. bucket id ↔ EHS 中位数单调一致（D-236b / validation §3）
// ============================================================================
//
// 验证 bucket id 递增 ⇒ bucket 内 EHS 中位数严格递增。D-236b 训练完成后 cluster id
// 重编号为 "0 = 最弱 / N-1 = 最强" 保证此性质。
//
// **C1 状态**：B2 stub bucket 0 内中位数 ≈ 0.5 + 噪声；bucket 1..499 sample 数 = 0
// 中位数 NaN（短路：`samples.len() < 2` 跳过 → 整条单调链不可比较 → 测试 fail）。
// `#[ignore]` 留 C2。
#[test]

fn bucket_id_ehs_median_monotonic_flop() {
    let table = cached_trained_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Flop) as usize;
    let samples = sample_observations(StreetTag::Flop, 10_000, 0x000C_1A0B_F10E);

    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::Flop, board, *hole);
        let bucket = match table.lookup(StreetTag::Flop, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            0x000C_1A0B_F10E,
            EQUITY_MONTE_CARLO,
            i as u32,
        ));
        let ehs = calc
            .equity(*hole, board, &mut rng)
            .expect("EHS flop median");
        by_bucket[bucket].push(ehs);
    }
    let medians: Vec<(usize, f64, usize)> = (0..bucket_count)
        .filter_map(|b| {
            let n = by_bucket[b].len();
            if n < 2 {
                None
            } else {
                Some((b, median(&by_bucket[b]), n))
            }
        })
        .collect();
    for w in medians.windows(2) {
        let (b0, m0, n0) = w[0];
        let (b1, m1, n1) = w[1];
        let tol = monotonic_tolerance(n0, n1, TEST_INNER_MC_ITER);
        assert!(
            m1 + tol >= m0,
            "D-233-rev1 / D-236b (flop)：bucket {b0} median {m0} > bucket {b1} \
             median {m1}（diff {} > MC-aware tol {tol:.4}; n0={n0} n1={n1}）",
            m0 - m1
        );
    }
}

#[test]

fn bucket_id_ehs_median_monotonic_turn() {
    let table = cached_trained_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Turn) as usize;
    let samples = sample_observations(StreetTag::Turn, 10_000, 0x000C_1A0B_7A2B);

    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::Turn, board, *hole);
        let bucket = match table.lookup(StreetTag::Turn, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            0x000C_1A0B_7A2B,
            EQUITY_MONTE_CARLO,
            i as u32,
        ));
        let ehs = calc
            .equity(*hole, board, &mut rng)
            .expect("EHS turn median");
        by_bucket[bucket].push(ehs);
    }
    let medians: Vec<(usize, f64, usize)> = (0..bucket_count)
        .filter_map(|b| {
            let n = by_bucket[b].len();
            if n < 2 {
                None
            } else {
                Some((b, median(&by_bucket[b]), n))
            }
        })
        .collect();
    for w in medians.windows(2) {
        let (b0, m0, n0) = w[0];
        let (b1, m1, n1) = w[1];
        let tol = monotonic_tolerance(n0, n1, TEST_INNER_MC_ITER);
        assert!(
            m1 + tol >= m0,
            "D-233-rev1 / D-236b (turn)：bucket {b0} median {m0} > bucket {b1} \
             median {m1}（diff {} > MC-aware tol {tol:.4}; n0={n0} n1={n1}）",
            m0 - m1
        );
    }
}

#[test]

fn bucket_id_ehs_median_monotonic_river() {
    let table = cached_trained_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::River) as usize;
    let samples = sample_observations(StreetTag::River, 10_000, 0xC1A0_B71B);

    let mut by_bucket: Vec<Vec<f64>> = vec![Vec::new(); bucket_count];
    for (i, (board, hole)) in samples.iter().enumerate() {
        let obs_id = canonical_observation_id(StreetTag::River, board, *hole);
        let bucket = match table.lookup(StreetTag::River, obs_id) {
            Some(b) => b as usize,
            None => continue,
        };
        let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
            0xC1A0_B71B,
            EQUITY_MONTE_CARLO,
            i as u32,
        ));
        let ehs = calc
            .equity(*hole, board, &mut rng)
            .expect("EHS river median");
        by_bucket[bucket].push(ehs);
    }
    let medians: Vec<(usize, f64, usize)> = (0..bucket_count)
        .filter_map(|b| {
            let n = by_bucket[b].len();
            if n < 2 {
                None
            } else {
                Some((b, median(&by_bucket[b]), n))
            }
        })
        .collect();
    for w in medians.windows(2) {
        let (b0, m0, n0) = w[0];
        let (b1, m1, n1) = w[1];
        let tol = monotonic_tolerance(n0, n1, TEST_INNER_MC_ITER);
        assert!(
            m1 + tol >= m0,
            "D-233-rev1 / D-236b (river)：bucket {b0} median {m0} > bucket {b1} \
             median {m1}（diff {} > MC-aware tol {tol:.4}; n0={n0} n1={n1}）",
            m0 - m1
        );
    }
}

// ============================================================================
// 7. EMD / std_dev / median helper sanity（C1 测试基础设施自检）
// ============================================================================
//
// 测试基础设施自检。这些 helper 是 C1 全套质量门槛的基础——若 helper 算错，
// 上面 4 类预期失败的 fail 信号就不可信。本测试始终 active（不依赖产品代码
// stub 行为），保证 C2 接入后断言切换由 helper 正确性背书。
//
// **§C-rev2 §5b**：`emd_1d_unit_interval` 走 `poker::abstraction::cluster::*`
// 产品 helper（§C-rev2 §5a 修正后），不再持本地副本；`std_dev` / `median`
// 仍保留本地（非产品功能，stage 3+ 质量断言重启时再评估迁移路径）。
#[test]
fn helper_sanity_emd_zero_for_identical_distributions() {
    let a = [0.2, 0.4, 0.6, 0.8];
    let b = [0.2, 0.4, 0.6, 0.8];
    let emd = emd_1d_unit_interval(&a, &b);
    assert!(emd.abs() < 1e-12, "identical → EMD ≈ 0, got {emd}");
}

#[test]
fn helper_sanity_emd_nonzero_for_disjoint_distributions() {
    let a = [0.0, 0.0, 0.0, 0.0];
    let b = [1.0, 1.0, 1.0, 1.0];
    let emd = emd_1d_unit_interval(&a, &b);
    assert!(
        (emd - 1.0).abs() < 1e-12,
        "disjoint extremes → EMD = 1.0, got {emd}"
    );
}

#[test]
fn helper_sanity_std_dev_uniform() {
    // 均匀 [0, 1]：std dev ≈ 1/sqrt(12) ≈ 0.289。
    let v: Vec<f64> = (0..1000).map(|i| (i as f64) / 999.0).collect();
    let sd = std_dev(&v);
    assert!(
        (sd - 0.289).abs() < 0.01,
        "uniform [0,1] std dev ≈ 0.289, got {sd}"
    );
}

#[test]
fn helper_sanity_median_odd_even_lengths() {
    let odd = [1.0, 3.0, 2.0, 5.0, 4.0];
    assert!((median(&odd) - 3.0).abs() < 1e-12);
    let even = [1.0, 2.0, 3.0, 4.0];
    assert!((median(&even) - 2.5).abs() < 1e-12);
}

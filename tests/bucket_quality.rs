//! C1 §输出：postflop bucket 聚类质量门槛断言。
//!
//! 覆盖 `pluribus_stage2_workflow.md` §C1 §输出 lines 304-309 + validation §3 全部
//! bucket 质量门槛：
//!
//! - **0 空 bucket**（D-236 / validation §3）：每条街每个 bucket id 至少包含 1 个
//!   canonical `(board, hole)` sample。
//! - **EHS std dev `< 0.05`**（path.md §阶段 2 字面 / validation §3）：每条街每个
//!   bucket 内手牌 EHS 标准差上限。
//! - **相邻 bucket EMD `≥ T_emd = 0.02`**（D-233 / validation §3）：每条街相邻
//!   bucket id `(k, k+1)` 间 1D EMD（all-in equity 分布）下限；500 bucket → 499
//!   对相邻；任一对 EMD `< 0.02` 视为聚类质量不足。
//! - **bucket id ↔ EHS 中位数单调一致**（D-236b / validation §3）：bucket id 递增
//!   ⇒ bucket 内 EHS 中位数递增。D-236b 训练完成后重编号保证。
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

use std::sync::Arc;

use poker::eval::NaiveHandEvaluator;
use poker::rng_substream::{derive_substream_seed, EQUITY_MONTE_CARLO};
use poker::{
    canonical_observation_id, BucketConfig, BucketTable, Card, ChaCha20Rng, EquityCalculator,
    HandEvaluator, MonteCarloEquity, RngSource, StreetTag,
};

// ============================================================================
// 通用 fixture
// ============================================================================

/// 默认 500/500/500 配置 + B2 stub BucketTable。
fn stub_table() -> BucketTable {
    BucketTable::stub_for_postflop(BucketConfig::default_500_500_500())
}

/// stage 1 朴素评估器；EHS / EMD 计算路径依赖。
fn make_evaluator() -> Arc<dyn HandEvaluator> {
    Arc::new(NaiveHandEvaluator)
}

/// 短 iter MonteCarloEquity（C1 sampling 路径要在 1.5 人周内跑完，用 1k iter 即可
/// 满足 EHS std dev / EMD 估计的 "noisy 0.02 容差" 数量级——SE ≈ sqrt(0.25/1000)
/// ≈ 0.016 < 0.05 / 0.02 阈值的合理倍数）。C2 实跑用默认 10k iter。
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

/// 把 `Vec<f64>` 排序后用 1D EMD 公式计算两分布距离（D-234）：
///
/// ```text
/// EMD_1D = sum_{i} |F_a(x_i) - F_b(x_i)| · Δx
/// ```
///
/// 等价于排序后 sample 的逐项差分绝对值平均。给定两 sample 集合（已 ∈ [0, 1]）：
fn emd_1d_unit_interval(samples_a: &[f64], samples_b: &[f64]) -> f64 {
    // 等长简单实现（D-234 sorted CDF 差分积分）；不等长时按各自 size 等距插值。
    let mut a: Vec<f64> = samples_a.to_vec();
    let mut b: Vec<f64> = samples_b.to_vec();
    a.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    b.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let n = a.len().min(b.len()) as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mut acc = 0.0_f64;
    for i in 0..(a.len().min(b.len())) {
        acc += (a[i] - b[i]).abs();
    }
    acc / n
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
// stub 路径下所有 lookup 返回 `Some(0)`，in-range 断言（< 500）总可过。本测试是
// C1 唯一**默认 active 通过**的项；其它 4 类聚类质量断言因 stub 行为均 `#[ignore]`
// 留 C2。
//
// 三街分别 1k 输入；任一 `lookup` 返回 `None`（越界）或 `>= bucket_count(street)`
// 立即 fail。
#[test]
fn bucket_lookup_1k_in_range_smoke_flop() {
    let table = stub_table();
    let bucket_count_flop = table.bucket_count(StreetTag::Flop);
    let samples = sample_observations(StreetTag::Flop, 1_000, 0x00C1_C0DE_F10E);
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
    let table = stub_table();
    let bucket_count_turn = table.bucket_count(StreetTag::Turn);
    let samples = sample_observations(StreetTag::Turn, 1_000, 0x00C1_C0DE_7A2B);
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
    let table = stub_table();
    let bucket_count_river = table.bucket_count(StreetTag::River);
    let samples = sample_observations(StreetTag::River, 1_000, 0x00C1_C0DE_71BB);
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
    let table = stub_table();
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
// **C1 状态**：B2 stub `lookup` 永远返回 `Some(0)`，bucket 1..499 全空。本测试
// **预期失败** ⇒ `#[ignore]` 标注（§C1 §出口 line 323 字面 "部分测试预期失败 —
// 留给 C2 修"）。C2 [实现] 落地真实 mmap clustering 后取消 ignore，断言：每条街
// `bucket_count(street)` 个 bucket 上每个至少有 1 个 canonical sample 命中。
//
// 采样规模：每条街 5 × `bucket_count`（500 → 2500 sample）保证统计意义上 0 空
// bucket（C2 落地后）；C1 stub 下 5 × 500 = 2500 个 sample 全部映射到 bucket 0
// → 1..499 空 ⇒ fail（按设计）。
#[test]
#[ignore = "C2: B2 stub lookup 永远返回 Some(0)，bucket 1..499 全空 — 预期失败"]
fn no_empty_bucket_per_street_flop() {
    let table = stub_table();
    let bucket_count = table.bucket_count(StreetTag::Flop);
    let samples = sample_observations(StreetTag::Flop, 5 * bucket_count as usize, 0x000C_1EBA_F10E);
    let mut hit = vec![false; bucket_count as usize];
    for (board, hole) in &samples {
        let obs_id = canonical_observation_id(StreetTag::Flop, board, *hole);
        if let Some(b) = table.lookup(StreetTag::Flop, obs_id) {
            hit[b as usize] = true;
        }
    }
    let empty_count = hit.iter().filter(|h| !**h).count();
    assert_eq!(
        empty_count, 0,
        "D-236 / validation §3：flop {empty_count} 个 bucket 空（共 {bucket_count}）"
    );
}

#[test]
#[ignore = "C2: B2 stub lookup 永远返回 Some(0)，bucket 1..499 全空 — 预期失败"]
fn no_empty_bucket_per_street_turn() {
    let table = stub_table();
    let bucket_count = table.bucket_count(StreetTag::Turn);
    let samples = sample_observations(StreetTag::Turn, 5 * bucket_count as usize, 0x000C_1EBA_7A2B);
    let mut hit = vec![false; bucket_count as usize];
    for (board, hole) in &samples {
        let obs_id = canonical_observation_id(StreetTag::Turn, board, *hole);
        if let Some(b) = table.lookup(StreetTag::Turn, obs_id) {
            hit[b as usize] = true;
        }
    }
    let empty_count = hit.iter().filter(|h| !**h).count();
    assert_eq!(
        empty_count, 0,
        "D-236 / validation §3：turn {empty_count} 个 bucket 空"
    );
}

#[test]
#[ignore = "C2: B2 stub lookup 永远返回 Some(0)，bucket 1..499 全空 — 预期失败"]
fn no_empty_bucket_per_street_river() {
    let table = stub_table();
    let bucket_count = table.bucket_count(StreetTag::River);
    let samples = sample_observations(StreetTag::River, 5 * bucket_count as usize, 0xC1EB_A71B);
    let mut hit = vec![false; bucket_count as usize];
    for (board, hole) in &samples {
        let obs_id = canonical_observation_id(StreetTag::River, board, *hole);
        if let Some(b) = table.lookup(StreetTag::River, obs_id) {
            hit[b as usize] = true;
        }
    }
    let empty_count = hit.iter().filter(|h| !**h).count();
    assert_eq!(
        empty_count, 0,
        "D-236 / validation §3：river {empty_count} 个 bucket 空"
    );
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
#[ignore = "C2: B2 stub 全部映射到 bucket 0 — bucket 0 std dev ≈ 0.20 > 0.05 — 预期失败"]
fn bucket_internal_ehs_std_dev_below_threshold_flop() {
    let table = stub_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Flop) as usize;
    let samples = sample_observations(StreetTag::Flop, 1_000, 0x000C_157D_F10E);

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
    for (bid, samples) in by_bucket.iter().enumerate() {
        if samples.len() < 2 {
            continue;
        }
        let sd = std_dev(samples);
        assert!(
            sd < 0.05,
            "validation §3 (flop)：bucket {bid} EHS std dev {sd} >= 0.05（n={}）",
            samples.len()
        );
    }
}

#[test]
#[ignore = "C2: B2 stub 全部映射到 bucket 0 — bucket 0 std dev > 0.05 — 预期失败"]
fn bucket_internal_ehs_std_dev_below_threshold_turn() {
    let table = stub_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Turn) as usize;
    let samples = sample_observations(StreetTag::Turn, 1_000, 0x000C_157D_7A2B);

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
    for (bid, samples) in by_bucket.iter().enumerate() {
        if samples.len() < 2 {
            continue;
        }
        let sd = std_dev(samples);
        assert!(
            sd < 0.05,
            "validation §3 (turn)：bucket {bid} EHS std dev {sd} >= 0.05"
        );
    }
}

#[test]
#[ignore = "C2: B2 stub 全部映射到 bucket 0 — bucket 0 std dev > 0.05 — 预期失败"]
fn bucket_internal_ehs_std_dev_below_threshold_river() {
    let table = stub_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::River) as usize;
    let samples = sample_observations(StreetTag::River, 1_000, 0xC157_D71B);

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
    for (bid, samples) in by_bucket.iter().enumerate() {
        if samples.len() < 2 {
            continue;
        }
        let sd = std_dev(samples);
        assert!(
            sd < 0.05,
            "validation §3 (river)：bucket {bid} EHS std dev {sd} >= 0.05"
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
#[ignore = "C2: B2 stub bucket 1..499 全空，相邻 EMD = 0 — 预期失败"]
fn adjacent_bucket_emd_above_threshold_flop() {
    let table = stub_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Flop) as usize;
    let samples = sample_observations(StreetTag::Flop, 1_000, 0x000C_1EAD_F10E);

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
    let t_emd: f64 = 0.02;
    for k in 0..(bucket_count - 1) {
        let emd = emd_1d_unit_interval(&by_bucket[k], &by_bucket[k + 1]);
        assert!(
            emd >= t_emd,
            "D-233 (flop)：bucket {k} vs {} EMD {emd} < T_emd {t_emd}",
            k + 1
        );
    }
}

#[test]
#[ignore = "C2: B2 stub bucket 1..499 全空，相邻 EMD = 0 — 预期失败"]
fn adjacent_bucket_emd_above_threshold_turn() {
    let table = stub_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Turn) as usize;
    let samples = sample_observations(StreetTag::Turn, 1_000, 0x000C_1EAD_7A2B);

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
    let t_emd: f64 = 0.02;
    for k in 0..(bucket_count - 1) {
        let emd = emd_1d_unit_interval(&by_bucket[k], &by_bucket[k + 1]);
        assert!(
            emd >= t_emd,
            "D-233 (turn)：bucket {k} vs {} EMD {emd} < T_emd",
            k + 1
        );
    }
}

#[test]
#[ignore = "C2: B2 stub bucket 1..499 全空，相邻 EMD = 0 — 预期失败"]
fn adjacent_bucket_emd_above_threshold_river() {
    let table = stub_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::River) as usize;
    let samples = sample_observations(StreetTag::River, 1_000, 0xC1EA_D71B);

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
    let t_emd: f64 = 0.02;
    for k in 0..(bucket_count - 1) {
        let emd = emd_1d_unit_interval(&by_bucket[k], &by_bucket[k + 1]);
        assert!(
            emd >= t_emd,
            "D-233 (river)：bucket {k} vs {} EMD {emd} < T_emd",
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
#[ignore = "C2: B2 stub bucket 1..499 全空 — 单调链断 — 预期失败"]
fn bucket_id_ehs_median_monotonic_flop() {
    let table = stub_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Flop) as usize;
    let samples = sample_observations(StreetTag::Flop, 2_000, 0x000C_1A0B_F10E);

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
    let medians: Vec<(usize, f64)> = (0..bucket_count)
        .filter_map(|b| {
            if by_bucket[b].len() < 2 {
                None
            } else {
                Some((b, median(&by_bucket[b])))
            }
        })
        .collect();
    for w in medians.windows(2) {
        let (b0, m0) = w[0];
        let (b1, m1) = w[1];
        assert!(
            m1 >= m0,
            "D-236b (flop)：bucket {b0} median {m0} > bucket {b1} median {m1}（单调违反）"
        );
    }
}

#[test]
#[ignore = "C2: B2 stub — 单调链断 — 预期失败"]
fn bucket_id_ehs_median_monotonic_turn() {
    let table = stub_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::Turn) as usize;
    let samples = sample_observations(StreetTag::Turn, 2_000, 0x000C_1A0B_7A2B);

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
    let medians: Vec<(usize, f64)> = (0..bucket_count)
        .filter_map(|b| {
            if by_bucket[b].len() < 2 {
                None
            } else {
                Some((b, median(&by_bucket[b])))
            }
        })
        .collect();
    for w in medians.windows(2) {
        let (b0, m0) = w[0];
        let (b1, m1) = w[1];
        assert!(
            m1 >= m0,
            "D-236b (turn)：bucket {b0} median {m0} > {b1} median {m1}"
        );
    }
}

#[test]
#[ignore = "C2: B2 stub — 单调链断 — 预期失败"]
fn bucket_id_ehs_median_monotonic_river() {
    let table = stub_table();
    let calc = make_calc_short_iter();
    let bucket_count = table.bucket_count(StreetTag::River) as usize;
    let samples = sample_observations(StreetTag::River, 2_000, 0xC1A0_B71B);

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
    let medians: Vec<(usize, f64)> = (0..bucket_count)
        .filter_map(|b| {
            if by_bucket[b].len() < 2 {
                None
            } else {
                Some((b, median(&by_bucket[b])))
            }
        })
        .collect();
    for w in medians.windows(2) {
        let (b0, m0) = w[0];
        let (b1, m1) = w[1];
        assert!(
            m1 >= m0,
            "D-236b (river)：bucket {b0} median {m0} > {b1} median {m1}"
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

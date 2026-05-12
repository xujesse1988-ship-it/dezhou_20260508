//! 阶段 3 B1 \[测试\]：regret matching 数值不变量（D-303 + D-306 + D-330 + D-331）。
//!
//! 覆盖 path.md §阶段 3 字面 `|Σ_a σ(I, a) - 1| < 1e-9` 容差 + 退化局面回退均匀
//! 分布 + 负 regret `max(R, 0)` 钳位三条 sanity，是 B2 \[实现\]
//! [`poker::training::RegretTable::current_strategy`] 的回归 trip-wire。任一
//! 数值不变量被 [实现] 错改（如把 `max(R, 0)` 改成裸 `R`、把退化分支阈值改成
//! `> 1e-9`、把容差判等放宽到 `1e-6`），本文件首条 active test 立即 fail。
//!
//! B1 \[测试\] 角色边界：本文件不修改 `src/training/`；如某条断言落在 [实现] 边界
//! 错误的产品代码上，filed issue 移交 B2 \[实现\]，不在测试内 patch 产品逻辑。

use blake3::Hasher;
use poker::training::kuhn::{KuhnHistory, KuhnInfoSet};
use poker::training::RegretTable;
use poker::{ChaCha20Rng, RngSource};

/// 12 KuhnInfoSet 全集枚举（D-310 Kuhn 规则下每 player 6 个 × 2 player = 12）。
///
/// 顺序固定（actor 升序 × private_card 升序 × history 升序），用于：
/// (a) 让本文件多条测试共享同一 InfoSet 序列；
/// (b) BLAKE3 hash 同形态测试（后续 B2 \[实现\] 落地的 cfr_kuhn 测试可复用）。
fn enumerate_kuhn_info_sets() -> Vec<KuhnInfoSet> {
    let mut out = Vec::with_capacity(12);
    for actor in 0u8..2 {
        // P1 (actor=0) 走 {Empty, CheckBet}；P2 (actor=1) 走 {Check, Bet}。
        let histories: [KuhnHistory; 2] = if actor == 0 {
            [KuhnHistory::Empty, KuhnHistory::CheckBet]
        } else {
            [KuhnHistory::Check, KuhnHistory::Bet]
        };
        for &history in &histories {
            for private_card in [11u8, 12, 13] {
                out.push(KuhnInfoSet {
                    actor,
                    private_card,
                    history,
                });
            }
        }
    }
    assert_eq!(out.len(), 12, "Kuhn InfoSet 全集 12 条（D-310）");
    out
}

/// D-330 + D-331 联合不变量：任意 regret 累积后，`current_strategy` 输出向量
/// 必须 sum 到 `1.0 ± 1e-9`（path.md §阶段 3 字面）。
///
/// 1M 随机输入覆盖：
/// - 全正 regret（typical case，`max(R, 0)` 钳位无效，sum = 1.0 严格）
/// - 全负 regret（D-331 退化局面，回退均匀分布 `1 / n_actions`，sum 严格 1.0）
/// - 混合 regret（部分正、部分负，`max(R, 0)` 钳位生效，sum 由浮点除法误差决定）
///
/// `#[ignore]` opt-in（release `~10 s`）：1M iter × `accumulate + current_strategy`
/// 重复，dev box default-profile 跑会拖到分钟级。
#[test]
#[ignore = "release/--ignored opt-in（~10 s release × 1M iter `accumulate + current_strategy`）"]
fn regret_matching_probability_sum_within_1e_minus_9_tolerance_1m_random_inputs() {
    let info_sets = enumerate_kuhn_info_sets();
    let mut rng = ChaCha20Rng::from_seed(0xCAFE_BABE_DEAD_BEEF);
    let mut max_err = 0.0_f64;

    for iter in 0..1_000_000u64 {
        let mut table: RegretTable<KuhnInfoSet> = RegretTable::new();
        let info = info_sets[(iter as usize) % info_sets.len()].clone();
        // 2-action InfoSet（Kuhn 全部 InfoSet 都是 2-action）；随机 regret ∈
        // [-1e6, +1e6]，覆盖大幅度负值 + 大幅度正值混合区间。
        let r0 = sample_signed_f64(&mut rng, 1.0e6);
        let r1 = sample_signed_f64(&mut rng, 1.0e6);
        table.accumulate(info.clone(), &[r0, r1]);
        let strategy = table.current_strategy(&info, 2);
        assert_eq!(strategy.len(), 2);
        let sum: f64 = strategy.iter().sum();
        let err = (sum - 1.0).abs();
        if err > max_err {
            max_err = err;
        }
        assert!(
            err < 1e-9,
            "iter={iter} info={info:?} regrets=[{r0}, {r1}] strategy={strategy:?} sum={sum} err={err}"
        );
    }
    eprintln!(
        "regret_matching sum tolerance over 1M random inputs: max_err = {max_err:.3e} (< 1e-9 OK)"
    );
}

/// D-331 退化局面：所有 regret ≤ 0 时回退均匀分布 `1 / n_actions`。
///
/// 直接测试 `RegretTable::new()` 之未访问 InfoSet（regrets 全 0 初始）应得均匀
/// 分布；以及显式 accumulate 全 0 / 全负值后同样退化为均匀分布。
#[test]
fn regret_matching_all_zero_regrets_returns_uniform_distribution() {
    let info_sets = enumerate_kuhn_info_sets();

    // Case 1：fresh table，未访问 InfoSet。D-323 lazy 初始化下 regret 视作全 0。
    {
        let table: RegretTable<KuhnInfoSet> = RegretTable::new();
        for info in &info_sets {
            let strategy = table.current_strategy(info, 2);
            assert_eq!(strategy.len(), 2);
            assert!(
                (strategy[0] - 0.5).abs() < 1e-12 && (strategy[1] - 0.5).abs() < 1e-12,
                "fresh table 未访问 InfoSet 应均匀分布 [0.5, 0.5]，得 {strategy:?}"
            );
        }
    }

    // Case 2：显式 accumulate 全 0 delta。
    {
        let mut table: RegretTable<KuhnInfoSet> = RegretTable::new();
        for info in &info_sets {
            table.accumulate(info.clone(), &[0.0, 0.0]);
        }
        for info in &info_sets {
            let strategy = table.current_strategy(info, 2);
            assert!(
                (strategy[0] - 0.5).abs() < 1e-12 && (strategy[1] - 0.5).abs() < 1e-12,
                "全 0 regret InfoSet 应均匀分布 [0.5, 0.5]，得 {strategy:?}"
            );
        }
    }

    // Case 3：accumulate 全负值（混合大小幅度），D-331 退化分支应触发。
    {
        let mut table: RegretTable<KuhnInfoSet> = RegretTable::new();
        for info in &info_sets {
            table.accumulate(info.clone(), &[-1.5, -0.25]);
        }
        for info in &info_sets {
            let strategy = table.current_strategy(info, 2);
            assert!(
                (strategy[0] - 0.5).abs() < 1e-12 && (strategy[1] - 0.5).abs() < 1e-12,
                "全负 regret InfoSet 应退化为均匀分布 [0.5, 0.5]，得 {strategy:?}"
            );
        }
    }

    // Case 4：n_actions = 3 / 4 / 5 sanity（非 Kuhn 具体 InfoSet，仅校验 D-331
    // 公式 `1 / n_actions` 跨 arity 一致性）。复用 KuhnInfoSet 仅做 key。
    for n_actions in 2usize..=5 {
        let table: RegretTable<KuhnInfoSet> = RegretTable::new();
        let strategy = table.current_strategy(&info_sets[0], n_actions);
        assert_eq!(strategy.len(), n_actions);
        let expected = 1.0 / n_actions as f64;
        for (i, p) in strategy.iter().enumerate() {
            assert!(
                (p - expected).abs() < 1e-12,
                "n_actions={n_actions} 退化均匀分布 p[{i}] = {p} 期望 {expected}"
            );
        }
        let sum: f64 = strategy.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1e-12,
            "n_actions={n_actions} 退化分布 sum = {sum}"
        );
    }
}

/// D-303 + D-306 标准公式：`σ(I, a) = max(R(I, a), 0) / Σ_b max(R(I, b), 0)`。
///
/// 验证 `max(R, 0)` 钳位语义：混合正负 regret 时，负值不进入分母 / 分子，对应
/// action 概率严格 = 0。这条 trip-wire 是为了防止 [实现] 误把 `max(R, 0)` 写成
/// 裸 R（会让负 regret 的 action 拿到负概率或抢占正 regret 的占比）。
#[test]
fn regret_matching_handles_negative_regrets_via_max_zero() {
    let info_sets = enumerate_kuhn_info_sets();
    let info = info_sets[0].clone();

    // Case 1：[3.0, -1.0] → R⁺ = [3.0, 0.0]，sum = 3.0，σ = [1.0, 0.0]。
    {
        let mut table: RegretTable<KuhnInfoSet> = RegretTable::new();
        table.accumulate(info.clone(), &[3.0, -1.0]);
        let strategy = table.current_strategy(&info, 2);
        assert!(
            (strategy[0] - 1.0).abs() < 1e-12 && strategy[1].abs() < 1e-12,
            "regrets=[3.0, -1.0] 期望 σ=[1.0, 0.0]，得 {strategy:?}"
        );
    }

    // Case 2：[2.0, 6.0, -100.0] → R⁺ = [2.0, 6.0, 0.0]，sum = 8.0，σ = [0.25, 0.75, 0.0]。
    {
        let mut table: RegretTable<KuhnInfoSet> = RegretTable::new();
        table.accumulate(info.clone(), &[2.0, 6.0, -100.0]);
        let strategy = table.current_strategy(&info, 3);
        let expected = [0.25, 0.75, 0.0];
        for (i, (got, exp)) in strategy.iter().zip(expected.iter()).enumerate() {
            assert!(
                (got - exp).abs() < 1e-12,
                "regrets=[2.0, 6.0, -100.0] σ[{i}] = {got} 期望 {exp}"
            );
        }
        let sum: f64 = strategy.iter().sum();
        assert!((sum - 1.0).abs() < 1e-12, "sum = {sum}，σ 必须 sum 到 1.0");
    }

    // Case 3：[-5.0, 2.0] → R⁺ = [0.0, 2.0]，σ = [0.0, 1.0]（负 regret 不让出概率）。
    {
        let mut table: RegretTable<KuhnInfoSet> = RegretTable::new();
        table.accumulate(info.clone(), &[-5.0, 2.0]);
        let strategy = table.current_strategy(&info, 2);
        assert!(
            strategy[0].abs() < 1e-12 && (strategy[1] - 1.0).abs() < 1e-12,
            "regrets=[-5.0, 2.0] 期望 σ=[0.0, 1.0]，得 {strategy:?}"
        );
    }

    // Case 4：[1.0, 1.0, 1.0] → 均匀 1/3 each（正 regret 全相等，平分概率）。
    {
        let mut table: RegretTable<KuhnInfoSet> = RegretTable::new();
        table.accumulate(info.clone(), &[1.0, 1.0, 1.0]);
        let strategy = table.current_strategy(&info, 3);
        let expected = 1.0 / 3.0;
        for (i, p) in strategy.iter().enumerate() {
            assert!(
                (p - expected).abs() < 1e-12,
                "regrets 全相等正值 σ[{i}] = {p} 期望 {expected}"
            );
        }
    }

    // Case 5：多次 accumulate 同 InfoSet（D-305 累积语义）：[1.0, 0.0] + [0.0, 2.0]
    // = [1.0, 2.0] → σ = [1/3, 2/3]。
    {
        let mut table: RegretTable<KuhnInfoSet> = RegretTable::new();
        table.accumulate(info.clone(), &[1.0, 0.0]);
        table.accumulate(info.clone(), &[0.0, 2.0]);
        let strategy = table.current_strategy(&info, 2);
        assert!(
            (strategy[0] - 1.0 / 3.0).abs() < 1e-12 && (strategy[1] - 2.0 / 3.0).abs() < 1e-12,
            "累积后 R=[1.0, 2.0] σ 期望 [1/3, 2/3]，得 {strategy:?}"
        );
    }
}

// ===========================================================================
// 辅助函数
// ===========================================================================

/// 在 `[-bound, +bound]` 均匀采样 f64（用于随机化测试输入）。
fn sample_signed_f64(rng: &mut dyn RngSource, bound: f64) -> f64 {
    let u = rng.next_u64();
    // 64 位 → `[0, 1)`，再线性映到 `[-bound, +bound]`。
    let unit = (u >> 11) as f64 / ((1u64 << 53) as f64);
    bound * (2.0 * unit - 1.0)
}

/// BLAKE3 helper：用 stable 字节序列 hash 一段 RegretTable strategy snapshot；
/// 本文件未直接使用，由 cfr_kuhn / cfr_leduc 可共享同型 helper（如未来 [测试]
/// 抽取公共 module 时）。保留以方便后续 [测试] 复用，避免重复实现。
#[allow(dead_code)]
fn blake3_strategy_snapshot(strategies: &[Vec<f64>]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    for s in strategies {
        for &p in s {
            hasher.update(&p.to_le_bytes());
        }
    }
    hasher.finalize().into()
}

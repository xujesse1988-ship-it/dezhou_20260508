//! 阶段 3 B1 \[测试\]：regret matching 数值不变量（D-303 + D-306 + D-330 + D-331）
//! + 阶段 4 B1 \[测试\] 扩展：Linear weighted regret / RM+ clamp 数值容差
//!   （D-401 / D-402 / D-403 / D-330 字面 1e-9 容差）。
//!
//! **stage 3 核心 3 条**（B1 \[测试\] 既有，落地于 stage 3 commit）：
//! 覆盖 path.md §阶段 3 字面 `|Σ_a σ(I, a) - 1| < 1e-9` 容差 + 退化局面回退均匀
//! 分布 + 负 regret `max(R, 0)` 钳位三条 sanity，是 B2 \[实现\]
//! [`poker::training::RegretTable::current_strategy`] 的回归 trip-wire。任一
//! 数值不变量被 [实现] 错改（如把 `max(R, 0)` 改成裸 `R`、把退化分支阈值改成
//! `> 1e-9`、把容差判等放宽到 `1e-6`），本文件首条 active test 立即 fail。
//!
//! **stage 4 扩展 5 条**（B1 \[测试\] 追加，2026-05-15 stage 4 commit；继承
//! stage 3 D-330 1e-9 容差，扩展到 Linear weighted regret + RM+ clamp 路径）：
//! - `linear_weighted_strategy_sum_within_1e_minus_9_tolerance_after_100_steps`
//!   — D-403 + D-330 字面 Linear weighted `S_t(I, a) = S_{t-1}(I, a) + t × σ_t(I, a)`
//!   在 t=100 步累积后 σ̄ sum tolerance 不破。
//! - `rm_plus_in_place_clamp_strict_non_negative_via_checkpoint_at_t10`
//!   — D-402 字面 in-place clamp 后 RegretTable raw R 严格 `>= 0`（通过
//!   `save_checkpoint + Checkpoint::open + bincode::deserialize` 间接读 raw R）。
//! - `linear_rm_plus_current_strategy_probability_sum_within_1e_minus_9_at_t100`
//!   — D-330 + D-402 字面：Linear+RM+ 路径下 current_strategy σ sum tolerance
//!   不破。
//! - `linear_weighting_at_t1_byte_equal_standard_within_1e_minus_12`
//!   — D-401 字面 t=1 边界：`R̃_1 = (1/2) × R̃_0 + r_1 = r_1`，Linear+RM+ 路径
//!   在 t=1 处 σ 应与 stage 3 standard CFR 路径 byte-equal within 1e-12（注：
//!   B2 \[实现\] 落地前两路径恒等，这条 sanity 持续通过；B2 错误把 decay 应用
//!   在 t=1 引入 (1/2) × 0 = 0 提前 cancel r_1 会立即 fail）。
//! - `linear_weighted_strategy_sum_t_weighted_oversample_diverges_at_t100`
//!   — D-403 字面 t-weighted 累积让 σ̄ 在 late t (t=100) 与 standard CFR 累积
//!   显著不同（at least 1 InfoSet diff > 1e-9）。
//!
//! B1 \[测试\] 角色边界：本文件不修改 `src/training/`；如某条断言落在 [实现] 边界
//! 错误的产品代码上，filed issue 移交 B2 \[实现\]，不在测试内 patch 产品逻辑。

use blake3::Hasher;
use poker::training::kuhn::{KuhnGame, KuhnHistory, KuhnInfoSet};
use poker::training::{Checkpoint, EsMccfrTrainer, RegretTable, Trainer};
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

// ===========================================================================
// 阶段 4 B1 \[测试\] 扩展：Linear weighted regret / RM+ clamp 数值容差 5 条
// ===========================================================================

/// stage 4 B1 \[测试\] 扩展 5 条共享 fixed master seed（ASCII "STG4_B1\xRM"）。
const STAGE4_FIXED_SEED: u64 = 0x53_54_47_34_5F_42_31_52; // ASCII "STG4_B1" + 0x52 (R)

/// stage 4 D-330 容差（与 stage 3 字面继承）。
const TOLERANCE_1E_MINUS_9: f64 = 1e-9;

/// 跑 `n_steps` 个 [`Trainer::step`] 调用于 KuhnGame trainer。
fn run_kuhn_trainer_steps(
    trainer: &mut EsMccfrTrainer<KuhnGame>,
    rng: &mut ChaCha20Rng,
    n_steps: u64,
) {
    for i in 0..n_steps {
        trainer
            .step(rng)
            .unwrap_or_else(|e| panic!("KuhnGame trainer.step #{i} 失败：{e:?}"));
    }
}

/// 跑 Kuhn trainer 后存 checkpoint，读回 RegretTable raw 累积值
/// （D-327 `encode_table` 反路径：bincode 1.x deserialize `Vec<(I, Vec<f64>)>`）。
fn dump_kuhn_regret_table_raw(
    trainer: &EsMccfrTrainer<KuhnGame>,
    label: &str,
) -> Vec<(KuhnInfoSet, Vec<f64>)> {
    let tmpdir = tempfile::tempdir().expect("tempfile::tempdir 失败");
    let path = tmpdir.path().join(format!("{label}.ckpt"));
    trainer
        .save_checkpoint(&path)
        .unwrap_or_else(|e| panic!("{label} save_checkpoint 失败：{e:?}"));
    let ckpt =
        Checkpoint::open(&path).unwrap_or_else(|e| panic!("{label} Checkpoint::open 失败：{e:?}"));
    bincode::deserialize::<Vec<(KuhnInfoSet, Vec<f64>)>>(&ckpt.regret_table_bytes)
        .unwrap_or_else(|e| panic!("{label} bincode::deserialize regret_table_bytes 失败：{e:?}"))
}

/// 收集 Kuhn 12 InfoSet 上的 strategy / avg_strategy（按需切换）。
fn kuhn_collect_strategy_map(
    trainer: &EsMccfrTrainer<KuhnGame>,
    use_average: bool,
) -> std::collections::HashMap<KuhnInfoSet, Vec<f64>> {
    let mut out = std::collections::HashMap::new();
    for info in enumerate_kuhn_info_sets() {
        let sigma = if use_average {
            trainer.average_strategy(&info)
        } else {
            trainer.current_strategy(&info)
        };
        if !sigma.is_empty() {
            out.insert(info, sigma);
        }
    }
    out
}

/// Test 4 — D-403 + D-330 字面：Linear weighted strategy sum 在 t=100 累积后
/// σ̄ sum to 1.0 ± 1e-9（每 touched InfoSet）。Linear weighted `S_t(I, a) =
/// S_{t-1}(I, a) + t × σ_t(I, a)` 归一化后 σ̄ sum 仍严格 1.0；浮点累积容差
/// 1e-9 是 D-330 字面继承（stage 3 D-330 → stage 4 D-403 字面同型 1e-9）。
///
/// **持续通过 sanity anchor**：B2 \[实现\] 落地前后均通过（stage 3 standard
/// CFR 路径下 σ̄ sum 1.0 严格满足；B2 落地 Linear weighting 不破坏归一化）。
#[test]
fn linear_weighted_strategy_sum_within_1e_minus_9_tolerance_after_100_steps() {
    let mut trainer = EsMccfrTrainer::new(KuhnGame, STAGE4_FIXED_SEED).with_linear_rm_plus(0);
    let mut rng = ChaCha20Rng::from_seed(STAGE4_FIXED_SEED);
    run_kuhn_trainer_steps(&mut trainer, &mut rng, 100);
    let strategies = kuhn_collect_strategy_map(&trainer, true);
    assert!(
        !strategies.is_empty(),
        "Kuhn 100 step DFS 后 touched InfoSet 数为 0，trainer.step 异常"
    );
    let mut max_err = 0.0_f64;
    let mut worst: Option<KuhnInfoSet> = None;
    for (info, sigma) in &strategies {
        assert_eq!(sigma.len(), 2, "Kuhn n_actions=2 (D-310)");
        let sum: f64 = sigma.iter().sum();
        let err = (sum - 1.0).abs();
        if err > max_err {
            max_err = err;
            worst = Some(info.clone());
        }
        assert!(
            err < TOLERANCE_1E_MINUS_9,
            "D-403 + D-330 容差超 1e-9：info={info:?} σ̄={sigma:?} sum={sum} err={err:.3e}"
        );
    }
    eprintln!(
        "Linear weighted strategy sum tolerance after 100 step ✓ max_err={max_err:.3e} \
         worst={:?}",
        worst
    );
}

/// Test 5 — D-402 字面：Linear+RM+ in-place clamp 让 RegretTable raw R 严格
/// `>= 0`（通过 save_checkpoint + Checkpoint::open + bincode::deserialize 间接
/// 读 raw R；外部 test 无法直接访问 trainer.regret pub(crate) 字段）。
///
/// **B2 \[实现\] 落地前 panic-fail**：trainer.step 不应用 clamp → R 可累积负值
/// → 首条负值 entry 上 assertion fail。**B2 落地后转绿**：R 全表 `>= 0`。
#[test]
fn rm_plus_in_place_clamp_strict_non_negative_via_checkpoint_at_t10() {
    // baseline 路径（区分度 anchor）：标准 CFR 让 R 可累积负值
    let baseline_entries = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, STAGE4_FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(STAGE4_FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, 10);
        dump_kuhn_regret_table_raw(&trainer, "regret_numeric_baseline_t10")
    };
    let baseline_min: f64 = baseline_entries
        .iter()
        .flat_map(|(_, rs)| rs.iter().copied())
        .fold(f64::INFINITY, f64::min);
    assert!(
        baseline_min < 0.0,
        "测试区分度 broken：stage 3 baseline 跑 10 step 后 R min = {baseline_min} >= 0 \
         （应有负 R 累积；调整 seed / step 数让 baseline 触达负 regret 区）"
    );

    // stage 4 Linear+RM+ 路径
    let stage4_entries = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, STAGE4_FIXED_SEED).with_linear_rm_plus(0);
        let mut rng = ChaCha20Rng::from_seed(STAGE4_FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, 10);
        dump_kuhn_regret_table_raw(&trainer, "regret_numeric_stage4_t10")
    };
    for (info, rs) in &stage4_entries {
        for (i, &r) in rs.iter().enumerate() {
            assert!(
                r >= 0.0,
                "D-402 RM+ in-place clamp 失效：info={info:?} R[{i}]={r} < 0\n\
                 baseline 区分度 anchor min R = {baseline_min} 证明本测试有区分度。"
            );
            assert!(r.is_finite(), "info={info:?} R[{i}]={r} 非 finite");
        }
    }
    eprintln!(
        "RM+ in-place clamp at t=10 ✓ baseline_min_R={baseline_min:.6e} \
         stage4_entries={}",
        stage4_entries.len()
    );
}

/// Test 6 — D-330 + D-402 字面：Linear+RM+ 路径下 current_strategy σ 严格 sum
/// to 1.0 ± 1e-9（touched InfoSet 全集）。
///
/// **持续通过 sanity anchor**：B2 \[实现\] 落地前 stage 3 路径 σ sum 1.0；B2
/// 落地后 RM+ clamp 不破坏归一化 → σ sum 仍 1.0。任一漂移（如 normalize 分母
/// `1 / (Σ + ε)` 误改让 σ sum < 1.0）立即 fail。
#[test]
fn linear_rm_plus_current_strategy_probability_sum_within_1e_minus_9_at_t100() {
    let mut trainer = EsMccfrTrainer::new(KuhnGame, STAGE4_FIXED_SEED).with_linear_rm_plus(0);
    let mut rng = ChaCha20Rng::from_seed(STAGE4_FIXED_SEED);
    run_kuhn_trainer_steps(&mut trainer, &mut rng, 100);
    let strategies = kuhn_collect_strategy_map(&trainer, false);
    assert!(
        !strategies.is_empty(),
        "Kuhn 100 step 后 touched InfoSet 数为 0"
    );
    let mut max_err = 0.0_f64;
    for (info, sigma) in &strategies {
        let sum: f64 = sigma.iter().sum();
        let err = (sum - 1.0).abs();
        if err > max_err {
            max_err = err;
        }
        assert!(
            err < TOLERANCE_1E_MINUS_9,
            "D-330 + D-402 容差超 1e-9：info={info:?} σ={sigma:?} sum={sum} err={err:.3e}"
        );
    }
    eprintln!("Linear+RM+ current_strategy σ sum tolerance after 100 step ✓ max_err={max_err:.3e}");
}

/// Test 7 — D-401 字面 t=1 边界：`R̃_1 = (1/2) × R̃_0 + r_1 = (1/2) × 0 + r_1
/// = r_1`，即 t=1 处 Linear weighting 不改变 r_1 累积。Linear+RM+ 路径在 step 1
/// 后的 σ 应当与 stage 3 standard CFR 路径**byte-equal within 1e-12**（数值
/// 噪声边界），任何 B2 \[实现\] 错误把 decay 应用让 R_1 ≠ r_1 立即 fail。
///
/// **持续通过 sanity anchor**：B2 \[实现\] 落地前两路径恒等（trainer.step 路径
/// 完全相同）；B2 落地后 Linear weighting 在 t=1 处不破坏 R_1 = r_1（D-401
/// 字面公式 t=1 系数 `1 / (1+1) = 0.5` 乘 0 退化为 r_1 单项）。
#[test]
fn linear_weighting_at_t1_byte_equal_standard_within_1e_minus_12() {
    let standard_strategies = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, STAGE4_FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(STAGE4_FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, 1);
        kuhn_collect_strategy_map(&trainer, false)
    };
    let linear_strategies = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, STAGE4_FIXED_SEED).with_linear_rm_plus(0);
        let mut rng = ChaCha20Rng::from_seed(STAGE4_FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, 1);
        kuhn_collect_strategy_map(&trainer, false)
    };
    assert_eq!(
        standard_strategies.len(),
        linear_strategies.len(),
        "t=1 step 后 touched InfoSet 数应 byte-equal: standard={} linear={}",
        standard_strategies.len(),
        linear_strategies.len()
    );
    for (info, s) in &standard_strategies {
        let l = linear_strategies.get(info).unwrap_or_else(|| {
            panic!(
                "D-401 t=1 边界：info={info:?} 在 standard 路径 touched 但 \
                                       linear 路径未 touched（trainer.step 决定性破坏）"
            )
        });
        assert_eq!(s.len(), l.len(), "info={info:?} σ.len 不一致");
        for (i, (si, li)) in s.iter().zip(l).enumerate() {
            let d = (si - li).abs();
            assert!(
                d < 1e-12,
                "D-401 t=1 字面 R̃_1 = r_1：linear 路径 σ 与 standard 路径 σ 在 t=1 必 byte-equal\n\
                 info={info:?} idx={i} standard={si} linear={li} diff={d:.3e}\n\
                 B2 \\[实现\\] 错误把 decay 应用让 R_1 ≠ r_1 → σ 漂移 → fail"
            );
        }
    }
    eprintln!(
        "D-401 t=1 byte-equal sanity anchor ✓ {} touched InfoSet",
        standard_strategies.len()
    );
}

/// Test 8 — D-403 字面 t-weighted strategy sum oversample 让 σ̄ 在 late t 处
/// 与 standard CFR 显著不同：D-403 累积 `S_t(I, a) = S_{t-1}(I, a) + t × σ_t`
/// 给晚期 σ 加权 t 倍（vs stage 3 D-304 unweighted by t 累积 `S_t += σ_t`）。
///
/// 100 step 后，Linear+RM+ 路径与 stage 3 baseline 路径 σ̄ 在 Kuhn 12 InfoSet
/// 全集中至少 1 个 InfoSet diff > 1e-9。
///
/// **B2 \[实现\] 落地前 panic-fail**：trainer.step 不应用 Linear weighted
/// strategy sum → 两路径 σ̄ 恒等 → max_diff = 0 → fail。**B2 落地后转绿**：
/// σ̄ 显著不同。
#[test]
fn linear_weighted_strategy_sum_t_weighted_oversample_diverges_at_t100() {
    let standard_avg = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, STAGE4_FIXED_SEED);
        let mut rng = ChaCha20Rng::from_seed(STAGE4_FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, 100);
        kuhn_collect_strategy_map(&trainer, true)
    };
    let linear_avg = {
        let mut trainer = EsMccfrTrainer::new(KuhnGame, STAGE4_FIXED_SEED).with_linear_rm_plus(0);
        let mut rng = ChaCha20Rng::from_seed(STAGE4_FIXED_SEED);
        run_kuhn_trainer_steps(&mut trainer, &mut rng, 100);
        kuhn_collect_strategy_map(&trainer, true)
    };
    let mut max_diff = 0.0_f64;
    let mut max_diff_info: Option<KuhnInfoSet> = None;
    for info in enumerate_kuhn_info_sets() {
        let s = standard_avg.get(&info).cloned().unwrap_or_default();
        let l = linear_avg.get(&info).cloned().unwrap_or_default();
        if s.len() != l.len() {
            continue;
        }
        for (si, li) in s.iter().zip(&l) {
            let d = (si - li).abs();
            if d > max_diff {
                max_diff = d;
                max_diff_info = Some(info.clone());
            }
        }
    }
    assert!(
        max_diff > TOLERANCE_1E_MINUS_9,
        "D-403 Linear weighted strategy sum 未应用：100 step 后 σ̄ 在 Kuhn 12 InfoSet 全集 \
         与 standard CFR 完全 byte-equal（max_diff={max_diff:.3e} < 1e-9）。\
         B2 \\[实现\\] 起步前 trainer 未路由 Linear weighted accumulate 路径。"
    );
    eprintln!(
        "D-403 Linear weighted strategy sum oversample at t=100 ✓ max_diff={max_diff:.6e} \
         at info={:?}",
        max_diff_info
    );
}

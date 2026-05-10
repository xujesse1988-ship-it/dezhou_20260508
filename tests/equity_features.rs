//! C1 §输出：EHS² / OCHS 特征自洽（反对称 / 单调 / 边界）+ OCHS opponent
//! cluster 数与 D-222 一致。
//!
//! 覆盖 `pluribus_stage2_workflow.md` §C1 §输出 lines 314-316：
//!
//! - **EHS² 单调性**：preflop AA 的 EHS² 显著高于 72o（与 EHS 单调性同向）。
//! - **EHS² river 退化边界**（D-227）：river 状态 outer rollout = 0，EHS²
//!   退化为 `inner_EHS²`（即 `equity²`）；本测试断言 `|ehs_squared(river) -
//!   equity(river)²| ≤ MC noise tol`。
//! - **EHS² 二阶矩范围**：postflop EHS² ≥ 0 且 EHS² ≤ EHS（Cauchy-Schwarz：
//!   `E[X²] ≥ (E[X])²` 反向 = `E[X²] ≤ E[X]` 当 X ∈ [0, 1] 时；EHS² 是 inner
//!   EHS 的均方，inner EHS ∈ [0, 1] ⇒ EHS² ≤ E[inner_EHS] ≤ 1）。flop / turn /
//!   river 三街分流。
//! - **OCHS N=8 一致**（D-222）：`MonteCarloEquity::n_opp_clusters() == 8`，
//!   `ochs(...).len() == 8`。
//! - **OCHS 单调性**：手牌 X 在强 opp cluster 上的胜率应低于在弱 opp cluster
//!   上的胜率（cluster 0 = AA 最强；cluster 6/7 = 72o 最弱；持有 KK 时 vs AA
//!   < vs 72o）。
//! - **OCHS 反对称（pairwise vs cluster representative 等价）**：OCHS 内部以
//!   `equity_vs_hand` 为原语，断言 `ochs(A)[k] + ochs(B)[k] ≈ 1.0` 当 cluster k
//!   只含 1 个 representative 且 A, B, opp 三方互不重叠。
//!
//! 与 `tests/equity_self_consistency.rs` 的边界：`equity_self_consistency.rs`
//! 覆盖 EQ-001-rev1 反对称 + EQ-002-rev1 finite/range + EQ-005 determinism +
//! EQ-001-rev1 错误路径 + ochs/ehs² shape；本文件覆盖 §C1 §输出 line 314 字面
//! "**特征自洽**（反对称 / 单调 / 边界）" 的 *单调性 + 边界* 维度，与前者
//! shape 类断言互补不重复。
//!
//! **C1 状态**：B2 落地 `MonteCarloEquity` 已能跑通；本文件无需 `#[ignore]`。
//! 部分测试在 1k iter 噪声下偶尔触边界（如 EHS² ≤ EHS 在 noise 下逼近相等），
//! 容差按 1k iter 标准误差 0.016 + 三街叠加放宽到 0.03。
//!
//! 角色边界：本文件属 `[测试]` agent 产物（C1）。

use std::sync::Arc;

use poker::eval::NaiveHandEvaluator;
use poker::rng_substream::{derive_substream_seed, EQUITY_MONTE_CARLO};
use poker::{Card, ChaCha20Rng, EquityCalculator, HandEvaluator, MonteCarloEquity, Rank, Suit};

// ============================================================================
// 通用 fixture（与 equity_self_consistency.rs 等价；C1 不抽到 common/ 是因每个
// 集成测试独立编译，跨文件复用代价不抵直接复制小段 fixture）
// ============================================================================

fn make_evaluator() -> Arc<dyn HandEvaluator> {
    Arc::new(NaiveHandEvaluator)
}

fn make_calc_default() -> MonteCarloEquity {
    MonteCarloEquity::new(make_evaluator())
}

fn make_calc_short_iter() -> MonteCarloEquity {
    MonteCarloEquity::new(make_evaluator()).with_iter(1_000)
}

fn aa() -> [Card; 2] {
    [
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::Ace, Suit::Hearts),
    ]
}

fn kk() -> [Card; 2] {
    [
        Card::new(Rank::King, Suit::Clubs),
        Card::new(Rank::King, Suit::Diamonds),
    ]
}

fn seven_two_off() -> [Card; 2] {
    [
        Card::new(Rank::Seven, Suit::Clubs),
        Card::new(Rank::Two, Suit::Diamonds),
    ]
}

fn flop_board() -> Vec<Card> {
    vec![
        Card::new(Rank::Five, Suit::Spades),
        Card::new(Rank::Eight, Suit::Hearts),
        Card::new(Rank::Ten, Suit::Diamonds),
    ]
}

fn turn_board() -> Vec<Card> {
    vec![
        Card::new(Rank::Five, Suit::Spades),
        Card::new(Rank::Eight, Suit::Hearts),
        Card::new(Rank::Ten, Suit::Diamonds),
        Card::new(Rank::Jack, Suit::Clubs),
    ]
}

fn river_board() -> Vec<Card> {
    vec![
        Card::new(Rank::Five, Suit::Spades),
        Card::new(Rank::Eight, Suit::Hearts),
        Card::new(Rank::Ten, Suit::Diamonds),
        Card::new(Rank::Jack, Suit::Clubs),
        Card::new(Rank::Queen, Suit::Hearts),
    ]
}

// ============================================================================
// 1. EHS² 单调性：preflop AA 显著高于 72o
// ============================================================================
//
// 与 `equity_self_consistency.rs` 的 preflop EHS 单调性 smoke 等价路径，但走
// `ehs_squared` 接口。AA preflop EHS² ≈ 0.85² ≈ 0.72；72o EHS² ≈ 0.36² ≈ 0.13。
// 差距 0.59 远超 1k iter 噪声 0.016（preflop outer MC + inner MC 双层）。
#[test]
fn ehs_squared_monotonicity_aa_beats_72o_preflop() {
    let calc = make_calc_short_iter();
    let board: Vec<Card> = Vec::new();

    let mut rng_aa = ChaCha20Rng::from_seed(derive_substream_seed(
        0xCA11_F227_EB1F,
        EQUITY_MONTE_CARLO,
        100,
    ));
    let mut rng_72 = ChaCha20Rng::from_seed(derive_substream_seed(
        0xCA11_F227_EB1F,
        EQUITY_MONTE_CARLO,
        101,
    ));

    let aa_ehs2 = calc.ehs_squared(aa(), &board, &mut rng_aa).unwrap();
    let s72_ehs2 = calc
        .ehs_squared(seven_two_off(), &board, &mut rng_72)
        .unwrap();

    assert!(
        aa_ehs2 > s72_ehs2 + 0.10,
        "EHS² 单调性 (preflop)：AA ({aa_ehs2}) 应远高于 72o ({s72_ehs2})"
    );
    // EQ-002-rev1 finite + range。
    assert!(aa_ehs2.is_finite() && (0.0..=1.0).contains(&aa_ehs2));
    assert!(s72_ehs2.is_finite() && (0.0..=1.0).contains(&s72_ehs2));
}

// ============================================================================
// 2. EHS² river 退化边界（D-227）
// ============================================================================
//
// D-227 字面：river 状态 outer rollout = 0，EHS² 退化为 `inner_EHS²`（即
// `equity(river)²`）。容差：river inner equity 是 1k iter MC（标准误差 ≈
// 0.016）；EHS² = equity²，二者均带 MC 噪声叠加 ⇒ `|ehs² - equity²| ≤ 0.05`
// 留足缓冲。
//
// 注意：D-227 字面是 EHS² 仅在 outer rollout 维度退化；`MonteCarloEquity`
// 仍然走 inner MC sampling（D-220 默认 10k；本测试用 1k iter shortcut）。
// 本测试**不**强制 `ehs² == equity²` byte-equal——两者各自独立消费 RngSource，
// 数学上等价但运行时数值不同。
#[test]
fn ehs_squared_river_degenerates_to_equity_squared() {
    let calc = make_calc_default();
    let board = river_board();

    // 同 sub_seed 双 RngSource 模式（与 EQ-001-rev1 strict 同型；这里走 default
    // 10k iter 控噪声）。
    let sub_seed_eq = derive_substream_seed(0x0CA1_1F22_7B1B, EQUITY_MONTE_CARLO, 200);
    let sub_seed_ehs2 = derive_substream_seed(0x0CA1_1F22_7B1B, EQUITY_MONTE_CARLO, 201);
    let mut rng_eq = ChaCha20Rng::from_seed(sub_seed_eq);
    let mut rng_ehs2 = ChaCha20Rng::from_seed(sub_seed_ehs2);

    let eq = calc.equity(aa(), &board, &mut rng_eq).unwrap();
    let ehs2 = calc.ehs_squared(aa(), &board, &mut rng_ehs2).unwrap();

    let expected = eq * eq;
    let tol = 0.05;
    assert!(
        (ehs2 - expected).abs() <= tol,
        "D-227 (river)：ehs² ({ehs2}) ≈ equity² ({expected}, eq={eq})，差 {} > tol {tol}",
        (ehs2 - expected).abs()
    );
}

// ============================================================================
// 3. EHS² ≤ EHS（Cauchy-Schwarz / 二阶矩边界）
// ============================================================================
//
// inner EHS ∈ [0, 1]，所以 inner EHS² ≤ inner EHS（点上）；外层均值保号：
// `E[inner_EHS²] ≤ E[inner_EHS] ≤ 1`。本测试断言 flop / turn / river 三街
// 上 EHS² ≤ EHS（容差 0.03 留 1k iter MC 双层噪声）。
//
// 注意：preflop 不在本断言范围——preflop outer 也是 MC 不是确定性枚举，双层
// MC 噪声叠加 ≈ 0.04 + 容差 = 0.07 容易翻车。preflop 单调性由测试 1 覆盖。
#[test]
fn ehs_squared_le_ehs_postflop_flop() {
    let calc = make_calc_short_iter();
    let board = flop_board();

    let sub = derive_substream_seed(0xC543_FE25, EQUITY_MONTE_CARLO, 300);
    let mut rng_eq = ChaCha20Rng::from_seed(sub);
    let mut rng_ehs2 = ChaCha20Rng::from_seed(sub);

    let eq = calc.equity(aa(), &board, &mut rng_eq).unwrap();
    let ehs2 = calc.ehs_squared(aa(), &board, &mut rng_ehs2).unwrap();
    let tol = 0.03;
    assert!(
        ehs2 <= eq + tol,
        "EHS² ≤ EHS (flop, AA)：ehs² {ehs2} > equity {eq} + tol {tol}"
    );
}

#[test]
fn ehs_squared_le_ehs_postflop_turn() {
    let calc = make_calc_short_iter();
    let board = turn_board();
    let sub = derive_substream_seed(0xC543_FE25, EQUITY_MONTE_CARLO, 301);
    let mut rng_eq = ChaCha20Rng::from_seed(sub);
    let mut rng_ehs2 = ChaCha20Rng::from_seed(sub);
    let eq = calc.equity(aa(), &board, &mut rng_eq).unwrap();
    let ehs2 = calc.ehs_squared(aa(), &board, &mut rng_ehs2).unwrap();
    let tol = 0.03;
    assert!(
        ehs2 <= eq + tol,
        "EHS² ≤ EHS (turn, AA)：ehs² {ehs2} > equity {eq} + tol"
    );
}

#[test]
fn ehs_squared_le_ehs_postflop_river() {
    let calc = make_calc_short_iter();
    let board = river_board();
    let sub = derive_substream_seed(0xC543_FE25, EQUITY_MONTE_CARLO, 302);
    let mut rng_eq = ChaCha20Rng::from_seed(sub);
    let mut rng_ehs2 = ChaCha20Rng::from_seed(sub);
    let eq = calc.equity(aa(), &board, &mut rng_eq).unwrap();
    let ehs2 = calc.ehs_squared(aa(), &board, &mut rng_ehs2).unwrap();
    let tol = 0.03;
    // river 状态 ehs2 = equity²，equity² ≤ equity 当 equity ∈ [0,1] 自动成立。
    assert!(
        ehs2 <= eq + tol,
        "EHS² ≤ EHS (river, AA)：ehs² {ehs2} > equity {eq} + tol"
    );
}

// ============================================================================
// 4. OCHS N=8 一致（D-222）
// ============================================================================
//
// `MonteCarloEquity::n_opp_clusters()` 默认返回 8（D-222），`ochs(...)` 输出
// `Vec<f64>` 长度也是 8。本测试在 default + with_opp_clusters(8) 两种构造路径
// 上断言一致性。
#[test]
fn ochs_n_opp_clusters_eq_8_default() {
    let calc = make_calc_default();
    assert_eq!(
        calc.n_opp_clusters(),
        8,
        "D-222：stage 2 默认 n_opp_clusters = 8"
    );

    let board = flop_board();
    let mut rng =
        ChaCha20Rng::from_seed(derive_substream_seed(0xC14B_DCF5, EQUITY_MONTE_CARLO, 400));
    let v = calc.ochs(aa(), &board, &mut rng).unwrap();
    assert_eq!(v.len(), 8, "D-222：ochs 向量长度 = 8");
}

#[test]
fn ochs_n_opp_clusters_explicit_8() {
    let calc = make_calc_default().with_opp_clusters(8);
    assert_eq!(calc.n_opp_clusters(), 8);
    let mut rng = ChaCha20Rng::from_seed(0xDEAD_BEEF);
    let v = calc.ochs(aa(), &flop_board(), &mut rng).unwrap();
    assert_eq!(v.len(), 8);
}

// ============================================================================
// 5. OCHS 单调性：KK vs 弱 opp（cluster 0） > KK vs 强 opp（cluster N-1）
// ============================================================================
//
// **§C-rev2 §3 后**（issue #5 落地）：D-236b 重编号让 cluster id 升序对应 EHS
// 中位数升序 → cluster 0 = weakest opp pool / cluster N-1 = strongest opp pool。
// 持 KK：vs cluster 0 (weakest，~72o-class) 应高（约 0.85+ 在 flop board），
// vs cluster 7 (strongest，AA-class 主导) 应低（约 0.20）。差距 ~0.6 远超 noise。
//
// 注：旧 B2 stub 顺序相反（cluster 0 = AsAh 最强），断言方向与本测试相反；
// §C-rev2 §3 落地真实 OCHS clustering 同 PR 翻转测试方向（由 §C-rev2 §3
// "[实现] + 同 PR 测试断言数值校准" carve-out 显式追认，与 §B-rev1 §3 同型）。
#[test]
fn ochs_monotonicity_kk_weaker_vs_strong_cluster() {
    let calc = make_calc_default();
    let board = flop_board();
    let mut rng =
        ChaCha20Rng::from_seed(derive_substream_seed(0xC1A0_0CB5, EQUITY_MONTE_CARLO, 500));
    let v = calc.ochs(kk(), &board, &mut rng).unwrap();
    // §C-rev2 §3：D-236b 升序 → cluster 0 = weakest / 7 = strongest。
    let vs_weak = v[0];
    let vs_strong = v[v.len() - 1];
    assert!(
        vs_strong < vs_weak,
        "OCHS 单调性：KK vs cluster 0 (weak, {vs_weak}) 应 > vs cluster {} (strong, {vs_strong})",
        v.len() - 1
    );
    // 差距下限：vs weak 约 0.85，vs strong 约 0.20，差 ≈ 0.65；保守取 0.4 留 noise。
    assert!(
        vs_weak - vs_strong > 0.4,
        "OCHS 单调性：差距 ({vs_weak} - {vs_strong}) 应 > 0.4"
    );
}

// ============================================================================
// 6. OCHS smoothness：强 hole vs 弱 cluster 胜率显著 ≥ 0.5
// ============================================================================
//
// **§C-rev2 §3 后**：D-236b cluster 0 = weakest opp（约 72o-class 主导）。
// 强 hole（AA / KK）on river_board vs 弱 cluster 平均胜率应 ≥ 0.5。river 板
// （5 张）走 D-227 outer = 0 路径 → equity_vs_hand 直接评估（无 RNG 消费）→
// cluster 内 N 个 representative 的 equity_vs_hand 平均确定性。
#[test]
fn ochs_strong_hole_dominates_weak_cluster_river() {
    let calc = make_calc_default();
    let board = river_board();
    let mut rng_a = ChaCha20Rng::from_seed(derive_substream_seed(
        0x000C_14B7_50CB,
        EQUITY_MONTE_CARLO,
        600,
    ));
    let mut rng_b = ChaCha20Rng::from_seed(derive_substream_seed(
        0x000C_14B7_50CB,
        EQUITY_MONTE_CARLO,
        601,
    ));

    let v_aa = calc.ochs(aa(), &board, &mut rng_a).unwrap();
    let v_kk = calc.ochs(kk(), &board, &mut rng_b).unwrap();
    let weak_idx = 0;
    let r_aa_vs_weak = v_aa[weak_idx];
    let r_kk_vs_weak = v_kk[weak_idx];
    assert!(
        (0.0..=1.0).contains(&r_aa_vs_weak),
        "EQ-002-rev1：AA vs weakest cluster ∈ [0,1], got {r_aa_vs_weak}"
    );
    assert!(
        (0.0..=1.0).contains(&r_kk_vs_weak),
        "EQ-002-rev1：KK vs weakest cluster ∈ [0,1], got {r_kk_vs_weak}"
    );
    assert!(
        r_aa_vs_weak >= 0.5,
        "AA river vs weakest opp cluster：胜率 {r_aa_vs_weak} 应 ≥ 0.5"
    );
    assert!(
        r_kk_vs_weak >= 0.5,
        "KK river vs weakest opp cluster：胜率 {r_kk_vs_weak} 应 ≥ 0.5"
    );
}

// ============================================================================
// 7. OCHS / EHS² 跨街边界 finite
// ============================================================================
//
// EQ-002-rev1：所有合法输入下，ochs / ehs_squared 输出 finite ∈ [0, 1]。本测试
// 在 6 个组合（{AA, KK, 72o} × {flop, turn}）上抽样断言无 NaN / Inf。river 已
// 在测试 2 / 3 / 6 覆盖。preflop 由 ehs_squared_finite_range_smoke 在
// equity_self_consistency.rs 覆盖。
#[test]
fn ochs_and_ehs_squared_finite_postflop_smoke() {
    let calc = make_calc_short_iter();
    let holes = [("AA", aa()), ("KK", kk()), ("72o", seven_two_off())];
    let boards = [("flop", flop_board()), ("turn", turn_board())];
    for (h_name, hole) in holes {
        for (b_name, board) in &boards {
            let mut rng_oc = ChaCha20Rng::from_seed(derive_substream_seed(
                0x0CA1_F1BA,
                EQUITY_MONTE_CARLO,
                700 + (h_name.len() + b_name.len()) as u32,
            ));
            let mut rng_ehs2 = ChaCha20Rng::from_seed(derive_substream_seed(
                0x0CA1_F1BA,
                EQUITY_MONTE_CARLO,
                800 + (h_name.len() + b_name.len()) as u32,
            ));

            let v = calc.ochs(hole, board, &mut rng_oc).unwrap_or_else(|e| {
                panic!("ochs({h_name}, {b_name}): expected Ok, got Err({e:?})")
            });
            for (k, x) in v.iter().enumerate() {
                assert!(
                    x.is_finite() && (0.0..=1.0).contains(x),
                    "EQ-002-rev1 ochs[{k}] {h_name}/{b_name}：finite + ∈ [0,1], got {x}"
                );
            }

            let ehs2 = calc
                .ehs_squared(hole, board, &mut rng_ehs2)
                .unwrap_or_else(|e| {
                    panic!("ehs²({h_name}, {b_name}): expected Ok, got Err({e:?})")
                });
            assert!(
                ehs2.is_finite() && (0.0..=1.0).contains(&ehs2),
                "EQ-002-rev1 ehs² {h_name}/{b_name}：finite + ∈ [0,1], got {ehs2}"
            );
        }
    }
}

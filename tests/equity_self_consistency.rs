//! B1 §C 类：Equity Monte Carlo 自洽性 harness。
//!
//! 覆盖 `pluribus_stage2_workflow.md` §B1 §输出 C 类清单：
//!
//! - **反对称**（D-220a-rev1 / EQ-001-rev1）容差按街分流：postflop 1e-9 严格 /
//!   preflop strict 1e-9（双 RngSource 同 sub_seed）/ preflop noisy 0.005
//!   （10k iter）/ 0.02（1k iter）。
//! - **preflop 169 EHS 单调性 smoke**：AA 最高 / 72o 最低（vs uniform random）。
//! - 阶段 2 不接入外部参考；自洽即可。
//!
//! **B1 状态**：A1 阶段 `MonteCarloEquity::*` / `EquityCalculator` trait 全部
//! `unimplemented!()`，本文件中的 `#[test]` 在第一次调用时 panic。**所有测试
//! `#[ignore]`**——按 §B1 §出口标准 line 250 "C / D / E 类 harness 能跑出占位
//! 结果或断言失败，流程不 panic"，默认 `cargo test` 不触发；B2 [实现] 落地
//! `MonteCarloEquity` 后取消 `#[ignore]`。
//!
//! **B-rev0 carve-out（继承 stage-1 §B-rev1 §3 同型）**：B1 §出口 line 250
//! "harness 流程不 panic" 与 [实现] agent "禁修测试代码" 规则在本文件硬冲突
//! ——A1 全 `unimplemented!()` 状态下只有 `#[ignore]` 才能避免默认 panic。
//! 解决方案：B2 [实现] 闭合 commit 同 commit **取消 C 类 equity `#[ignore]`**
//! （[测试] 角色越界，由 §B-rev1 同型 carve-out 追认）。详见
//! `pluribus_stage2_workflow.md` §修订历史 §B-rev0 carve-out 段落。
//!
//! 容差源：D-220a-rev1（`pluribus_stage2_decisions.md` §3 / API §9
//! EQ-001-rev1）。容差由 [决策] 锁定，[测试] 不自己拍数。
//!
//! 角色边界：本文件属 `[测试]` agent 产物。

use std::sync::Arc;

use poker::eval::NaiveHandEvaluator;
use poker::rng_substream::{derive_substream_seed, EQUITY_MONTE_CARLO};
use poker::{Card, ChaCha20Rng, EquityCalculator, HandEvaluator, MonteCarloEquity, Rank, Suit};

// ============================================================================
// 通用 fixture
// ============================================================================

/// stage 1 朴素评估器（B2 [实现] 起步阶段 MonteCarloEquity 必须能用 stage 1
/// `HandEvaluator`，不依赖 stage 2 自带 evaluator）。
fn make_evaluator() -> Arc<dyn HandEvaluator> {
    Arc::new(NaiveHandEvaluator)
}

/// 默认 10k iter MonteCarloEquity（D-220）。
fn make_calc_default() -> MonteCarloEquity {
    MonteCarloEquity::new(make_evaluator())
}

/// 短 1k iter MonteCarloEquity（CI 短测试 / preflop noisy 0.02 容差路径）。
fn make_calc_1k_iter() -> MonteCarloEquity {
    MonteCarloEquity::new(make_evaluator()).with_iter(1_000)
}

/// hand fixture：A♠A♥（最强 pocket pair）。
fn aa() -> [Card; 2] {
    [
        Card::new(Rank::Ace, Suit::Spades),
        Card::new(Rank::Ace, Suit::Hearts),
    ]
}

/// hand fixture：7♣2♦（最弱 offsuit hand 之一）。
fn seven_two_off() -> [Card; 2] {
    [
        Card::new(Rank::Seven, Suit::Clubs),
        Card::new(Rank::Two, Suit::Diamonds),
    ]
}

/// hand fixture：K♣K♦（次强 pocket pair）。
fn kk() -> [Card; 2] {
    [
        Card::new(Rank::King, Suit::Clubs),
        Card::new(Rank::King, Suit::Diamonds),
    ]
}

/// 全空 board（preflop）。
fn preflop_board() -> Vec<Card> {
    Vec::new()
}

/// 固定 flop board（避开 hand fixtures 的牌）：5♠ 8♥ T♦。
fn flop_board() -> Vec<Card> {
    vec![
        Card::new(Rank::Five, Suit::Spades),
        Card::new(Rank::Eight, Suit::Hearts),
        Card::new(Rank::Ten, Suit::Diamonds),
    ]
}

/// 固定 turn board：5♠ 8♥ T♦ J♣。
fn turn_board() -> Vec<Card> {
    vec![
        Card::new(Rank::Five, Suit::Spades),
        Card::new(Rank::Eight, Suit::Hearts),
        Card::new(Rank::Ten, Suit::Diamonds),
        Card::new(Rank::Jack, Suit::Clubs),
    ]
}

/// 固定 river board：5♠ 8♥ T♦ J♣ Q♥。
fn river_board() -> Vec<Card> {
    vec![
        Card::new(Rank::Five, Suit::Spades),
        Card::new(Rank::Eight, Suit::Hearts),
        Card::new(Rank::Ten, Suit::Diamonds),
        Card::new(Rank::Jack, Suit::Clubs),
        Card::new(Rank::Queen, Suit::Hearts),
    ]
}

/// 从 D-228 `derive_substream_seed` 派生独立 RngSource（EQ-001-rev1 严格反对称
/// 标准模式）。两次调用返回同 seed 的两个独立 RngSource 实例。
fn fresh_rng_pair(master_seed: u64, sub_index: u32) -> (ChaCha20Rng, ChaCha20Rng) {
    let sub_seed = derive_substream_seed(master_seed, EQUITY_MONTE_CARLO, sub_index);
    (
        ChaCha20Rng::from_seed(sub_seed),
        ChaCha20Rng::from_seed(sub_seed),
    )
}

// ============================================================================
// 1. EQ-001-rev1 反对称（postflop river）：1e-9 严格容差
// ============================================================================
//
// river state 确定性枚举（无 RNG 消费），对称性 IEEE-754 reorder 容忍 ≤ 1e-9。
#[test]
fn equity_vs_hand_antisymmetry_river_strict() {
    let calc = make_calc_default();
    let board = river_board();
    let (mut rng_ab, mut rng_ba) = fresh_rng_pair(0xA0B1_C2D3_E4F5_0617, 0);

    let r1 = calc
        .equity_vs_hand(aa(), kk(), &board, &mut rng_ab)
        .expect("river: 合法输入");
    let r2 = calc
        .equity_vs_hand(kk(), aa(), &board, &mut rng_ba)
        .expect("river: 合法输入");

    assert!(
        (r1 + r2 - 1.0).abs() <= 1e-9,
        "EQ-001-rev1 postflop strict (river)：|r1+r2-1| ≤ 1e-9, 得到 {r1}+{r2}={}",
        r1 + r2
    );
    // EQ-002-rev1 finite invariant：Ok(x) 时 x ∈ [0.0, 1.0] 且 finite。
    assert!(r1.is_finite() && (0.0..=1.0).contains(&r1));
    assert!(r2.is_finite() && (0.0..=1.0).contains(&r2));
}

// ============================================================================
// 2. EQ-001-rev1 反对称（postflop turn / flop）
// ============================================================================
//
// turn / flop 确定性枚举，反对称严格 1e-9 容差。
#[test]
fn equity_vs_hand_antisymmetry_turn_strict() {
    let calc = make_calc_default();
    let board = turn_board();
    let (mut rng_ab, mut rng_ba) = fresh_rng_pair(0xA0B1_C2D3_E4F5_0617, 1);

    let r1 = calc
        .equity_vs_hand(aa(), kk(), &board, &mut rng_ab)
        .unwrap();
    let r2 = calc
        .equity_vs_hand(kk(), aa(), &board, &mut rng_ba)
        .unwrap();
    assert!(
        (r1 + r2 - 1.0).abs() <= 1e-9,
        "EQ-001-rev1 postflop strict (turn)：|r1+r2-1| ≤ 1e-9"
    );
}

#[test]
fn equity_vs_hand_antisymmetry_flop_strict() {
    let calc = make_calc_default();
    let board = flop_board();
    let (mut rng_ab, mut rng_ba) = fresh_rng_pair(0xA0B1_C2D3_E4F5_0617, 2);

    let r1 = calc
        .equity_vs_hand(aa(), kk(), &board, &mut rng_ab)
        .unwrap();
    let r2 = calc
        .equity_vs_hand(kk(), aa(), &board, &mut rng_ba)
        .unwrap();
    assert!(
        (r1 + r2 - 1.0).abs() <= 1e-9,
        "EQ-001-rev1 postflop strict (flop)：|r1+r2-1| ≤ 1e-9"
    );
}

// ============================================================================
// 3. EQ-001-rev1 反对称（preflop strict）：双 RngSource 同 sub_seed，1e-9 容差
// ============================================================================
//
// EQ-001-rev1 字面要求：preflop 严格反对称必须用**两个独立 RngSource，从同一
// `derive_substream_seed(master_seed, EQUITY_MONTE_CARLO, sub_index)` 构造**。
// 顺序复用同一 `&mut rng` 是 EQ-001-rev1 显式列出的禁止模式（detail 见
// `pluribus_stage2_api.md` §9 EQ-001-rev1）。
#[test]
fn equity_vs_hand_antisymmetry_preflop_strict_dual_rng_same_seed() {
    let calc = make_calc_default();
    let board = preflop_board();

    // EQ-001-rev1 标准模式：同 sub_seed → 两个独立 RngSource。
    let (mut rng_ab, mut rng_ba) = fresh_rng_pair(0xA0B1_C2D3_E4F5_0617, 0);

    let r1 = calc
        .equity_vs_hand(aa(), kk(), &board, &mut rng_ab)
        .unwrap();
    let r2 = calc
        .equity_vs_hand(kk(), aa(), &board, &mut rng_ba)
        .unwrap();

    assert!(
        (r1 + r2 - 1.0).abs() <= 1e-9,
        "EQ-001-rev1 preflop strict (双 RngSource 同 sub_seed)：|r1+r2-1| ≤ 1e-9, \
         得到 {r1}+{r2}={}",
        r1 + r2
    );
}

// ============================================================================
// 4. EQ-001-rev1 反对称（preflop noisy 10k iter）：0.005 容差
// ============================================================================
//
// 不同 sub_seed 下 Monte Carlo 噪声反对称容忍。10k iter 标准误差近似
// `sqrt(0.25 / 10000) ≈ 0.005`。
#[test]
fn equity_vs_hand_antisymmetry_preflop_noisy_10k() {
    let calc = make_calc_default();
    let board = preflop_board();

    // 不同 sub_seed 构造（noisy 路径）。
    let sub_seed_ab = derive_substream_seed(0xA0B1_C2D3_E4F5_0617, EQUITY_MONTE_CARLO, 100);
    let sub_seed_ba = derive_substream_seed(0xA0B1_C2D3_E4F5_0617, EQUITY_MONTE_CARLO, 101);
    let mut rng_ab = ChaCha20Rng::from_seed(sub_seed_ab);
    let mut rng_ba = ChaCha20Rng::from_seed(sub_seed_ba);

    let r1 = calc
        .equity_vs_hand(aa(), kk(), &board, &mut rng_ab)
        .unwrap();
    let r2 = calc
        .equity_vs_hand(kk(), aa(), &board, &mut rng_ba)
        .unwrap();

    assert!(
        (r1 + r2 - 1.0).abs() <= 0.005,
        "EQ-001-rev1 preflop noisy (10k iter)：|r1+r2-1| ≤ 0.005, 得到 {}",
        (r1 + r2 - 1.0).abs()
    );
}

// ============================================================================
// 5. EQ-001-rev1 反对称（preflop noisy 1k iter）：0.02 容差
// ============================================================================
//
// 1k iter（CI 短测试）标准误差近似 `sqrt(0.25 / 1000) ≈ 0.016`，容忍 0.02。
#[test]
fn equity_vs_hand_antisymmetry_preflop_noisy_1k() {
    let calc = make_calc_1k_iter();
    let board = preflop_board();

    let sub_seed_ab = derive_substream_seed(0xA0B1_C2D3_E4F5_0617, EQUITY_MONTE_CARLO, 200);
    let sub_seed_ba = derive_substream_seed(0xA0B1_C2D3_E4F5_0617, EQUITY_MONTE_CARLO, 201);
    let mut rng_ab = ChaCha20Rng::from_seed(sub_seed_ab);
    let mut rng_ba = ChaCha20Rng::from_seed(sub_seed_ba);

    let r1 = calc
        .equity_vs_hand(aa(), kk(), &board, &mut rng_ab)
        .unwrap();
    let r2 = calc
        .equity_vs_hand(kk(), aa(), &board, &mut rng_ba)
        .unwrap();

    assert!(
        (r1 + r2 - 1.0).abs() <= 0.02,
        "EQ-001-rev1 preflop noisy (1k iter)：|r1+r2-1| ≤ 0.02, 得到 {}",
        (r1 + r2 - 1.0).abs()
    );
}

// ============================================================================
// 6. preflop 169 EHS 单调性 smoke：AA 最高 / 72o 最低
// ============================================================================
//
// `EquityCalculator::equity` (vs uniform random opponent，EHS 路径) 在 preflop
// 上 AA > 72o。容差走 noisy 10k iter（标准误差 ≈ 0.005）。
//
// 注意：本测试用 `equity` 接口（hand-vs-uniform-random-hole），**不**用
// `equity_vs_hand`——后者要求显式对手 hole；EHS 单调性是在 random-opp 上的
// 性质。
#[test]
fn preflop_ehs_monotonicity_aa_beats_72o_smoke() {
    let calc = make_calc_default();
    let board = preflop_board();

    let sub_seed_aa = derive_substream_seed(0xA0B1_C2D3_E4F5_0617, EQUITY_MONTE_CARLO, 300);
    let sub_seed_72 = derive_substream_seed(0xA0B1_C2D3_E4F5_0617, EQUITY_MONTE_CARLO, 301);
    let mut rng_aa = ChaCha20Rng::from_seed(sub_seed_aa);
    let mut rng_72 = ChaCha20Rng::from_seed(sub_seed_72);

    let eq_aa = calc.equity(aa(), &board, &mut rng_aa).unwrap();
    let eq_72 = calc.equity(seven_two_off(), &board, &mut rng_72).unwrap();

    // AA 公认 preflop 最强；72o 公认 preflop 最弱（offsuit 段最低）。
    // 实测 AA EHS ≈ 0.85，72o EHS ≈ 0.36。差距足够大不被 10k 噪声吞噬。
    assert!(
        eq_aa > eq_72 + 0.10,
        "preflop EHS 单调性 smoke：AA ({eq_aa}) 应远高于 72o ({eq_72})"
    );
    // EQ-002-rev1 finite invariant。
    assert!(eq_aa.is_finite() && (0.0..=1.0).contains(&eq_aa));
    assert!(eq_72.is_finite() && (0.0..=1.0).contains(&eq_72));
}

// ============================================================================
// 7. EQ-005 deterministic：同 (hole, board, rng_seed, iter) 重复 byte-equal
// ============================================================================
//
// 同 sub_seed → 同 RngSource state → equity 输出 byte-equal。1k 次重复验证
// （B1 smoke；full 1M 留 D1）。
#[test]
fn equity_determinism_repeat_1k_smoke() {
    let calc = make_calc_default();
    let board = preflop_board();

    // baseline
    let sub_seed = derive_substream_seed(0xA0B1_C2D3_E4F5_0617, EQUITY_MONTE_CARLO, 400);
    let mut rng_baseline = ChaCha20Rng::from_seed(sub_seed);
    let baseline = calc.equity(aa(), &board, &mut rng_baseline).unwrap();
    assert!(baseline.is_finite());

    // 重复 1k 次，每次 fresh RngSource → 同结果。
    for i in 0..1_000 {
        let mut rng = ChaCha20Rng::from_seed(sub_seed);
        let other = calc.equity(aa(), &board, &mut rng).unwrap();
        assert_eq!(
            baseline.to_bits(),
            other.to_bits(),
            "EQ-005 iter {i}: byte-equal (f64::to_bits)"
        );
    }
}

// ============================================================================
// 8. EquityError invalid input：重叠 / 板长非法
// ============================================================================
//
// EquityCalculator-rev1：合法输入返回 Ok，无效输入返回 Err。本测试验证 4 类
// 错误路径全部命中。
#[test]
fn equity_invalid_input_returns_err() {
    use poker::EquityError;

    let calc = make_calc_default();
    let mut rng = ChaCha20Rng::from_seed(0xDEAD_BEEF);

    // OverlapBoard：hole 与 board 重叠。
    let bad_board_overlap = vec![
        Card::new(Rank::Ace, Suit::Spades), // 与 aa() 第一张冲突
        Card::new(Rank::Eight, Suit::Hearts),
        Card::new(Rank::Ten, Suit::Diamonds),
    ];
    let r1 = calc.equity(aa(), &bad_board_overlap, &mut rng);
    assert!(
        matches!(r1, Err(EquityError::OverlapBoard { .. })),
        "EQ overlap board → Err(OverlapBoard)"
    );

    // InvalidBoardLen：board.len() ∉ {0, 3, 4, 5}。
    let board_2_cards = vec![
        Card::new(Rank::Five, Suit::Spades),
        Card::new(Rank::Eight, Suit::Hearts),
    ];
    let r2 = calc.equity(aa(), &board_2_cards, &mut rng);
    assert!(
        matches!(r2, Err(EquityError::InvalidBoardLen { got: 2 })),
        "EQ invalid board len → Err(InvalidBoardLen)"
    );

    // OverlapHole：opp_hole 与 hole 重叠（同张牌）。
    let bad_opp = aa(); // 与 aa() 完全重叠
    let r3 = calc.equity_vs_hand(aa(), bad_opp, &[], &mut rng);
    assert!(
        matches!(r3, Err(EquityError::OverlapHole { .. })),
        "EQ overlap hole → Err(OverlapHole)"
    );
}

// ============================================================================
// 9. EquityError::IterTooLow（API §3 EquityCalculator 5 类错误之一）
// ============================================================================
//
// `MonteCarloEquity::with_iter(0)` 后调用 `equity` / `equity_vs_hand` /
// `ehs_squared` / `ochs` 任一接口必须返回 `Err(EquityError::IterTooLow {
// got: 0 })`。D-220 默认 10_000 不触发，stage 4 消融 / 错配置时触发。
//
// 当前测试 8 仅覆盖 OverlapBoard / InvalidBoardLen / OverlapHole 3 类，
// IterTooLow 未覆盖；本测试补完。
#[test]
fn equity_iter_too_low_returns_err() {
    use poker::EquityError;

    let calc = make_calc_default().with_iter(0);
    let mut rng = ChaCha20Rng::from_seed(0xDEAD_BEEF);
    let board = preflop_board();

    let r = calc.equity(aa(), &board, &mut rng);
    assert!(
        matches!(r, Err(EquityError::IterTooLow { got: 0 })),
        "iter=0 → Err(IterTooLow {{ got: 0 }})，got {r:?}"
    );

    // equity_vs_hand 同样路径。
    let r2 = calc.equity_vs_hand(aa(), kk(), &board, &mut rng);
    assert!(
        matches!(r2, Err(EquityError::IterTooLow { got: 0 })),
        "iter=0 / equity_vs_hand → Err(IterTooLow)，got {r2:?}"
    );
}

// ============================================================================
// 10. OCHS shape + finite + range invariant（EQ-002-rev1 / D-222）
// ============================================================================
//
// API §3 / EQ-002-rev1（line 473）字面：`ochs` 返回 `Ok(v)` 时 `v.len() ==
// n_opp_clusters` 且每维 `∈ [0.0, 1.0]` 且 finite。任一 NaN / Inf / 越界是 P0
// 阻塞 bug。
//
// **stage-2 默认 N=8**（D-222），本测试断言 v.len() == 8、每维 finite、每维
// `∈ [0.0, 1.0]`。stage 4 消融如果改了 `with_opp_clusters` 配置，本测试自动
// 通过 `n_opp_clusters()` getter 读到的实际 N 校验 shape。
#[test]
fn ochs_shape_finite_range_smoke() {
    let calc = make_calc_default();
    let board = flop_board();
    let mut rng = ChaCha20Rng::from_seed(derive_substream_seed(
        0xCAFE_BABE_F00D,
        EQUITY_MONTE_CARLO,
        500,
    ));

    let v = calc
        .ochs(aa(), &board, &mut rng)
        .expect("flop OCHS：合法输入");

    // EQ-002-rev1 shape：v.len() == n_opp_clusters。
    let n = calc.n_opp_clusters() as usize;
    assert_eq!(
        v.len(),
        n,
        "EQ-002-rev1：ochs.len() ({}) == n_opp_clusters ({})",
        v.len(),
        n
    );
    assert_eq!(n, 8, "D-222：stage 2 默认 n_opp_clusters = 8");

    // EQ-002-rev1 finite + range：每维 finite ∧ ∈ [0.0, 1.0]。
    for (idx, x) in v.iter().enumerate() {
        assert!(
            x.is_finite(),
            "EQ-002-rev1：ochs[{idx}] 必须 finite，got {x}"
        );
        assert!(
            (0.0..=1.0).contains(x),
            "EQ-002-rev1：ochs[{idx}] ∈ [0.0, 1.0]，got {x}"
        );
    }
}

// ============================================================================
// 11. ehs_squared finite + range invariant（EQ-002-rev1）
// ============================================================================
//
// EQ-002-rev1 字面：`ehs_squared` 返回 `Ok(x)` 时 `x ∈ [0.0, 1.0]` 且 finite。
// river 状态退化为 `equity²`（D-227 rollout=0）。本测试断言 flop / turn / river
// 三街的 `ehs_squared` 输出落在 EQ-002-rev1 不变量内。
#[test]
fn ehs_squared_finite_range_smoke() {
    let calc = make_calc_default();
    let mut rng_flop = ChaCha20Rng::from_seed(derive_substream_seed(
        0xCAFE_BABE_F00D,
        EQUITY_MONTE_CARLO,
        600,
    ));
    let mut rng_turn = ChaCha20Rng::from_seed(derive_substream_seed(
        0xCAFE_BABE_F00D,
        EQUITY_MONTE_CARLO,
        601,
    ));
    let mut rng_river = ChaCha20Rng::from_seed(derive_substream_seed(
        0xCAFE_BABE_F00D,
        EQUITY_MONTE_CARLO,
        602,
    ));

    for (board, rng, label) in [
        (flop_board(), &mut rng_flop, "flop"),
        (turn_board(), &mut rng_turn, "turn"),
        (river_board(), &mut rng_river, "river"),
    ] {
        let x = calc
            .ehs_squared(aa(), &board, rng)
            .unwrap_or_else(|e| panic!("{label} ehs²: 合法输入返 Ok, got Err({e:?})"));
        assert!(x.is_finite(), "EQ-002-rev1 ehs² {label}：finite, got {x}");
        assert!(
            (0.0..=1.0).contains(&x),
            "EQ-002-rev1 ehs² {label}：∈ [0.0, 1.0], got {x}"
        );
    }
}

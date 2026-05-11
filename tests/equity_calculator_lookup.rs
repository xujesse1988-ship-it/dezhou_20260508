//! F1：equity calculator iter 边界测试（workflow §F1 第 4 件套）。
//!
//! 验收门槛（workflow §F1 §输出 第 4 行）：
//!
//! > `iter=0` / `iter=1` / `iter=u32::MAX` 边界（与阶段 1 `evaluator_lookup.rs`
//! > 同形态）
//!
//! 与 stage-1 `tests/evaluator_lookup.rs` 同形态（无 fallible constructor + 1k
//! 随机输入 no-panic + 完备性扫描）但聚焦 `MonteCarloEquity` 的 iter 边界。
//!
//! ## iter=0 路径分流（D-220 / EQ-005）
//!
//! - `equity(any board)` → 一律 `Err(IterTooLow { got: 0 })`
//!   （`src/abstraction/equity.rs:170` 无条件 early return）
//! - `equity_vs_hand(river)` → `Ok(0.0 / 0.5 / 1.0)`（确定性，无 iter 依赖）
//! - `equity_vs_hand(turn)` → `Ok(...)`（44 unseen river 枚举，确定性）
//! - `equity_vs_hand(flop)` → `Ok(...)`（C(45,2)=990 (turn, river) 枚举，确定性）
//! - `equity_vs_hand(preflop)` → `Err(IterTooLow)`（line 280 — preflop 唯一走 RNG
//!   消费的分支）
//! - `ehs_squared(any)` → `Err(IterTooLow)`（line 304）
//! - `ochs(any)` → `Err(IterTooLow)`（line 367）
//!
//! ## iter=1 路径
//!
//! 单 iter 仍走完整 MC 流程，输出 ratio ∈ [0.0, 1.0] 且 finite（D-224 / EQ-002-rev1）。
//! 一次抽样 → ratio ∈ {0.0, 0.5, 1.0} discrete（一手对一手 + ties）但本测试
//! 只断 ∈ [0,1] + finite 形态，不锁定具体值（输入和 RNG 共变）。
//!
//! ## iter=u32::MAX 路径
//!
//! 4.3G iter × 10 μs/iter ≈ 30 hours 单 flop equity 调用。`#[ignore]` 走 `--ignored`
//! 完全 opt-in。本测试不实际跑到底，而是 **smoke** 验证：(a) `iter()` getter
//! 回传 `u32::MAX` 不溢出（与 stage-1 1M determinism `#[ignore]` 同形态）；
//! (b) `with_iter(u32::MAX)` 构造路径不 panic、不需要任何 IO；(c) MonteCarloEquity
//! `with_iter` 链式调用接收 u32::MAX 不破坏其他字段（n_opp_clusters / evaluator
//! 透传）。
//!
//! 角色边界：[测试]，不修改产品代码。

use poker::eval::NaiveHandEvaluator;
use poker::{Card, ChaCha20Rng, EquityCalculator, EquityError, HandEvaluator, MonteCarloEquity};
use std::sync::Arc;

// ============================================================================
// 通用 fixture
// ============================================================================

fn make_evaluator() -> Arc<dyn HandEvaluator> {
    Arc::new(NaiveHandEvaluator)
}

fn calc_with(iter: u32) -> MonteCarloEquity {
    MonteCarloEquity::new(make_evaluator()).with_iter(iter)
}

fn rng() -> ChaCha20Rng {
    ChaCha20Rng::from_seed(0xF1EA_AB17_0757_0004)
}

/// 4 不重叠 cards：As Kd Qh Jc → 简单 fixture（与 evaluator_lookup.rs 同形态）。
const HOLE_A: [u8; 2] = [12 << 2, (11 << 2) | 1]; // A♣ K♦
const HOLE_B: [u8; 2] = [(10 << 2) | 2, (9 << 2) | 3]; // Q♥ J♠

fn hole_a() -> [Card; 2] {
    [
        Card::from_u8(HOLE_A[0]).unwrap(),
        Card::from_u8(HOLE_A[1]).unwrap(),
    ]
}

fn hole_b() -> [Card; 2] {
    [
        Card::from_u8(HOLE_B[0]).unwrap(),
        Card::from_u8(HOLE_B[1]).unwrap(),
    ]
}

fn flop_board() -> [Card; 3] {
    // 2♣ 3♦ 4♥（与 hole_a / hole_b 不冲突）
    [
        Card::from_u8(0).unwrap(),
        Card::from_u8(5).unwrap(),
        Card::from_u8(10).unwrap(),
    ]
}

fn turn_board() -> [Card; 4] {
    [
        Card::from_u8(0).unwrap(),
        Card::from_u8(5).unwrap(),
        Card::from_u8(10).unwrap(),
        Card::from_u8(15).unwrap(),
    ]
}

fn river_board() -> [Card; 5] {
    [
        Card::from_u8(0).unwrap(),
        Card::from_u8(5).unwrap(),
        Card::from_u8(10).unwrap(),
        Card::from_u8(15).unwrap(),
        Card::from_u8(20).unwrap(),
    ]
}

// ============================================================================
// (A) 结构性断言：构造器签名 + chain 链式语义（与 evaluator_lookup.rs (A) 同型）
// ============================================================================

#[test]
fn calculator_chain_setters_preserve_other_fields() {
    let c = MonteCarloEquity::new(make_evaluator());
    // 默认值（D-220 / D-222）
    assert_eq!(c.iter(), 10_000, "默认 iter = 10_000 (D-220)");
    assert_eq!(c.n_opp_clusters(), 8, "默认 n_opp_clusters = 8 (D-222)");

    // chain：with_iter 不动 n_opp_clusters
    let c1 = MonteCarloEquity::new(make_evaluator()).with_iter(42);
    assert_eq!(c1.iter(), 42);
    assert_eq!(c1.n_opp_clusters(), 8, "with_iter 不应触碰 n_opp_clusters");

    // chain：with_opp_clusters 不动 iter
    let c2 = MonteCarloEquity::new(make_evaluator()).with_opp_clusters(4);
    assert_eq!(c2.iter(), 10_000, "with_opp_clusters 不应触碰 iter");
    assert_eq!(c2.n_opp_clusters(), 4);

    // chain：双链
    let c3 = MonteCarloEquity::new(make_evaluator())
        .with_iter(7)
        .with_opp_clusters(2);
    assert_eq!(c3.iter(), 7);
    assert_eq!(c3.n_opp_clusters(), 2);

    // u32::MAX 构造不 panic
    let c_max = MonteCarloEquity::new(make_evaluator()).with_iter(u32::MAX);
    assert_eq!(c_max.iter(), u32::MAX);
}

// ============================================================================
// (B) iter=0 边界
// ============================================================================

#[test]
fn iter_zero_equity_returns_iter_too_low_on_all_boards() {
    let c = calc_with(0);
    let mut r = rng();

    for (label, board) in [
        ("preflop", &[] as &[Card]),
        ("flop", &flop_board()[..]),
        ("turn", &turn_board()[..]),
        ("river", &river_board()[..]),
    ] {
        let err = c
            .equity(hole_a(), board, &mut r)
            .err()
            .unwrap_or_else(|| panic!("iter=0 equity({label}) 必须 Err"));
        match err {
            EquityError::IterTooLow { got } => assert_eq!(got, 0),
            other => panic!("expected IterTooLow, got {other:?} on {label}"),
        }
    }
}

#[test]
fn iter_zero_ehs_squared_returns_iter_too_low_on_all_boards() {
    let c = calc_with(0);
    let mut r = rng();

    for (label, board) in [
        ("preflop", &[] as &[Card]),
        ("flop", &flop_board()[..]),
        ("turn", &turn_board()[..]),
        ("river", &river_board()[..]),
    ] {
        let err = c
            .ehs_squared(hole_a(), board, &mut r)
            .err()
            .unwrap_or_else(|| panic!("iter=0 ehs_squared({label}) 必须 Err"));
        match err {
            EquityError::IterTooLow { got } => assert_eq!(got, 0),
            other => panic!("expected IterTooLow, got {other:?} on {label}"),
        }
    }
}

#[test]
fn iter_zero_ochs_returns_iter_too_low_on_all_boards() {
    let c = calc_with(0);
    let mut r = rng();

    for (label, board) in [
        ("preflop", &[] as &[Card]),
        ("flop", &flop_board()[..]),
        ("turn", &turn_board()[..]),
        ("river", &river_board()[..]),
    ] {
        let err = c
            .ochs(hole_a(), board, &mut r)
            .err()
            .unwrap_or_else(|| panic!("iter=0 ochs({label}) 必须 Err"));
        match err {
            EquityError::IterTooLow { got } => assert_eq!(got, 0),
            other => panic!("expected IterTooLow, got {other:?} on {label}"),
        }
    }
}

#[test]
fn iter_zero_equity_vs_hand_only_preflop_returns_iter_too_low() {
    // river / turn / flop 是确定性 enum，iter=0 也成功（无 RNG 消费）
    let c = calc_with(0);
    let mut r = rng();

    // river — 1.0/0.5/0.0 离散
    let v_river = c
        .equity_vs_hand(hole_a(), hole_b(), &river_board(), &mut r)
        .expect("iter=0 equity_vs_hand(river) 应 Ok 确定性");
    assert!(
        v_river == 0.0 || v_river == 0.5 || v_river == 1.0,
        "river equity_vs_hand 必为 0.0/0.5/1.0，实际 {v_river}"
    );

    // turn / flop — Ok finite ∈ [0,1]
    for (label, board) in [("turn", &turn_board()[..]), ("flop", &flop_board()[..])] {
        let v = c
            .equity_vs_hand(hole_a(), hole_b(), board, &mut r)
            .unwrap_or_else(|_| panic!("iter=0 equity_vs_hand({label}) 应 Ok 确定性"));
        assert!(
            v.is_finite() && (0.0..=1.0).contains(&v),
            "{label}: equity_vs_hand 必 finite ∈ [0,1]，实际 {v}"
        );
    }

    // preflop — 唯一 RNG 消费分支，iter=0 必 Err
    let err = c
        .equity_vs_hand(hole_a(), hole_b(), &[], &mut r)
        .expect_err("iter=0 equity_vs_hand(preflop) 必须 Err");
    match err {
        EquityError::IterTooLow { got } => assert_eq!(got, 0),
        other => panic!("expected IterTooLow on preflop equity_vs_hand, got {other:?}"),
    }
}

// ============================================================================
// (C) iter=1 边界
// ============================================================================

#[test]
fn iter_one_equity_yields_finite_ratio_in_unit_interval_all_boards() {
    let c = calc_with(1);
    let mut r = rng();

    for (label, board) in [
        ("preflop", &[] as &[Card]),
        ("flop", &flop_board()[..]),
        ("turn", &turn_board()[..]),
        ("river", &river_board()[..]),
    ] {
        let v = c
            .equity(hole_a(), board, &mut r)
            .unwrap_or_else(|_| panic!("iter=1 equity({label}) 应 Ok"));
        assert!(
            v.is_finite() && (0.0..=1.0).contains(&v),
            "{label}: equity finite ∈ [0,1] (D-224)，实际 {v}"
        );
    }
}

#[test]
fn iter_one_ehs_squared_yields_finite_ratio_in_unit_interval_all_boards() {
    let c = calc_with(1);
    let mut r = rng();

    for (label, board) in [
        ("preflop", &[] as &[Card]),
        ("flop", &flop_board()[..]),
        ("turn", &turn_board()[..]),
        ("river", &river_board()[..]),
    ] {
        let v = c
            .ehs_squared(hole_a(), board, &mut r)
            .unwrap_or_else(|_| panic!("iter=1 ehs_squared({label}) 应 Ok"));
        assert!(
            v.is_finite() && (0.0..=1.0).contains(&v),
            "{label}: ehs_squared finite ∈ [0,1]，实际 {v}"
        );
    }
}

#[test]
fn iter_one_ochs_yields_correct_length_and_finite_components() {
    let c = calc_with(1); // 默认 n_opp_clusters=8
    let mut r = rng();

    for (label, board) in [
        ("preflop", &[] as &[Card]),
        ("flop", &flop_board()[..]),
        ("turn", &turn_board()[..]),
        ("river", &river_board()[..]),
    ] {
        let v = c
            .ochs(hole_a(), board, &mut r)
            .unwrap_or_else(|_| panic!("iter=1 ochs({label}) 应 Ok"));
        assert_eq!(v.len(), 8, "{label}: ochs 长度应 = n_opp_clusters=8");
        for (i, comp) in v.iter().enumerate() {
            assert!(
                comp.is_finite() && (0.0..=1.0).contains(comp),
                "{label}[{i}]: ochs comp finite ∈ [0,1]，实际 {comp}"
            );
        }
    }
}

#[test]
fn iter_one_equity_vs_hand_yields_finite_in_unit_interval_all_boards() {
    let c = calc_with(1);
    let mut r = rng();

    for (label, board) in [
        ("preflop", &[] as &[Card]),
        ("flop", &flop_board()[..]),
        ("turn", &turn_board()[..]),
        ("river", &river_board()[..]),
    ] {
        let v = c
            .equity_vs_hand(hole_a(), hole_b(), board, &mut r)
            .unwrap_or_else(|_| panic!("iter=1 equity_vs_hand({label}) 应 Ok"));
        assert!(
            v.is_finite() && (0.0..=1.0).contains(&v),
            "{label}: equity_vs_hand finite ∈ [0,1]，实际 {v}"
        );
    }
}

// ============================================================================
// (D) iter=u32::MAX 边界 — 构造侧 smoke + 完整跑 #[ignore]
// ============================================================================

#[test]
fn iter_u32_max_construction_does_not_panic_or_overflow() {
    let c = calc_with(u32::MAX);
    assert_eq!(c.iter(), u32::MAX, "u32::MAX 透传，无 wrap");
    assert_eq!(c.n_opp_clusters(), 8, "其他字段不动");

    // arithmetic preview：iter as f64 不 panic / 不 Inf
    let f = u32::MAX as f64;
    assert!(f.is_finite(), "u32::MAX as f64 应 finite");

    // chain 后再 with_iter 回小值，下游 equity 调用应正常工作（确认 with_iter
    // 不持久污染状态）
    let c2 = c.with_iter(1);
    let mut r = rng();
    let v = c2
        .equity(hole_a(), &river_board(), &mut r)
        .expect("with_iter(1) 后 equity(river) 应 Ok");
    assert!(v.is_finite() && (0.0..=1.0).contains(&v));
}

/// river `equity_vs_hand` 不消耗 iter（line 224-237 确定性 1.0/0.5/0.0 enum），
/// 故 iter=u32::MAX + river equity_vs_hand 实际成本与 iter=1 同。这是 u32::MAX
/// 路径唯一可在合理时间内跑 default test 的口子。
#[test]
fn iter_u32_max_equity_vs_hand_river_succeeds_without_iter_cost() {
    let c = calc_with(u32::MAX);
    let mut r = rng();
    let v = c
        .equity_vs_hand(hole_a(), hole_b(), &river_board(), &mut r)
        .expect("iter=u32::MAX equity_vs_hand(river) 不应触 RNG，应 Ok");
    assert!(
        v == 0.0 || v == 0.5 || v == 1.0,
        "river equity_vs_hand ∈ {{0.0, 0.5, 1.0}}，实际 {v}"
    );
}

#[test]
#[ignore = "F1 full: iter=u32::MAX 单 flop equity ≈ 30 hours release（4.3G iter × 10 μs/iter）。\
            opt-in via --ignored；F3 [报告] 不要求实跑此 case，仅留接口验证。"]
fn iter_u32_max_equity_flop_full_run() {
    let c = calc_with(u32::MAX);
    let mut r = rng();
    let v = c
        .equity(hole_a(), &flop_board(), &mut r)
        .expect("iter=u32::MAX equity(flop) 跑到底应 Ok（耗时 ~30 hours）");
    assert!(v.is_finite() && (0.0..=1.0).contains(&v));
}

// ============================================================================
// (E) EquityError 5 类变体 exhaustive match（与 history_corruption 同形态 trip-wire）
// ============================================================================

#[test]
fn equity_error_has_exactly_five_variants() {
    fn _exhaustive_match(err: EquityError) -> &'static str {
        match err {
            EquityError::OverlapHole { .. } => "OverlapHole",
            EquityError::OverlapBoard { .. } => "OverlapBoard",
            EquityError::InvalidBoardLen { .. } => "InvalidBoardLen",
            EquityError::IterTooLow { .. } => "IterTooLow",
            EquityError::Internal(_) => "Internal",
        }
    }
    let dummy_card = Card::from_u8(0).unwrap();
    let _ = _exhaustive_match(EquityError::OverlapHole { card: dummy_card });
    let _ = _exhaustive_match(EquityError::OverlapBoard { card: dummy_card });
    let _ = _exhaustive_match(EquityError::InvalidBoardLen { got: 2 });
    let _ = _exhaustive_match(EquityError::IterTooLow { got: 0 });
    let _ = _exhaustive_match(EquityError::Internal("stub".into()));
}

// ============================================================================
// (F) 边界完备性 — InvalidBoardLen 触发条件
// ============================================================================

#[test]
fn invalid_board_len_2_returns_invalid_board_len_error() {
    let c = calc_with(1);
    let mut r = rng();
    let bad_board = [Card::from_u8(0).unwrap(), Card::from_u8(5).unwrap()];
    let err = c
        .equity(hole_a(), &bad_board, &mut r)
        .expect_err("board.len()=2 必须 Err InvalidBoardLen");
    match err {
        EquityError::InvalidBoardLen { got } => assert_eq!(got, 2),
        other => panic!("expected InvalidBoardLen, got {other:?}"),
    }
}

#[test]
fn invalid_board_len_6_returns_invalid_board_len_error() {
    let c = calc_with(1);
    let mut r = rng();
    let bad_board: Vec<Card> = (0u8..6).map(|v| Card::from_u8(v).unwrap()).collect();
    let err = c
        .equity(hole_a(), &bad_board, &mut r)
        .expect_err("board.len()=6 必须 Err InvalidBoardLen");
    match err {
        EquityError::InvalidBoardLen { got } => assert_eq!(got, 6),
        other => panic!("expected InvalidBoardLen, got {other:?}"),
    }
}

// ============================================================================
// (G) overlap 路径 — OverlapBoard / OverlapHole
// ============================================================================

#[test]
fn equity_with_hole_overlapping_board_returns_overlap_board() {
    let c = calc_with(1);
    let mut r = rng();
    // 把 hole_a 的第 0 张牌也放到 board → overlap
    let board: [Card; 3] = [
        Card::from_u8(HOLE_A[0]).unwrap(),
        Card::from_u8(5).unwrap(),
        Card::from_u8(10).unwrap(),
    ];
    let err = c
        .equity(hole_a(), &board, &mut r)
        .expect_err("hole/board overlap 必须 Err");
    match err {
        EquityError::OverlapBoard { .. } => { /* ok */ }
        other => panic!("expected OverlapBoard, got {other:?}"),
    }
}

#[test]
fn equity_vs_hand_with_overlapping_holes_returns_overlap_hole() {
    let c = calc_with(1);
    let mut r = rng();
    // opp_hole 第 0 张 = hole_a 第 0 张
    let opp_hole: [Card; 2] = [
        Card::from_u8(HOLE_A[0]).unwrap(),
        Card::from_u8(HOLE_B[1]).unwrap(),
    ];
    let err = c
        .equity_vs_hand(hole_a(), opp_hole, &flop_board(), &mut r)
        .expect_err("hole/opp_hole overlap 必须 Err");
    match err {
        EquityError::OverlapHole { .. } => { /* ok */ }
        other => panic!("expected OverlapHole, got {other:?}"),
    }
}

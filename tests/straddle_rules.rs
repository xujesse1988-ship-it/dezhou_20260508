//! UTG 强制 straddle 规则回归（`TableConfig::straddle` ≠ 0）。
//!
//! 语义：发完 ante / 盲注后，大盲下家（UTG；3 座桌即按钮）强制交 `straddle`（不足额则 all-in），
//! 算本轮下注。翻前从 straddle 下家开始说话、straddle 座最后说话，没人加注时可过牌；翻前跟注额 =
//! straddle、最小加注额 = straddle（加注到 2× straddle）；翻后最小下注回到大盲、从 SB 开始说话。
//! PokerKit 跨验证见 `tests/cross_validation.rs` 的 `cross_validation_pokerkit_straddle_*`；本文件
//! 用手算期望值锁死几个边角。

mod common;

use poker::{
    Action, ChipAmount, GameState, HandHistory, PlayerStatus, SeatId, Street, TableConfig,
};

use common::{chips, expected_total_chips, seat, Invariants};

/// 1/2/4 (1) 桌按 solver 单位（BB = 100）放大：SB 50 / BB 100 / straddle 200 / 每座 ante 50，
/// 6 座 100BB，按钮座 0（SB = 1 / BB = 2 / straddle = 3 / 首个行动者 = 4）。
fn cfg_6max_1_2_4_ante1() -> TableConfig {
    TableConfig::default_6max_100bb()
        .with_ante(chips(50))
        .with_straddle(chips(200))
}

fn drive(state: &mut GameState, total: u64, plan: &[(u8, Action)]) {
    for (i, &(want, action)) in plan.iter().enumerate() {
        assert_eq!(state.current_player(), Some(seat(want)), "step {i}");
        state
            .apply(action)
            .unwrap_or_else(|e| panic!("step {i}: apply({action:?}) failed: {e}"));
        Invariants::check_all(state, total).unwrap_or_else(|e| panic!("step {i}: {e}"));
    }
}

fn net(state: &GameState, s: u8) -> i64 {
    state
        .payouts()
        .expect("terminal")
        .iter()
        .find(|(id, _)| *id == SeatId(s))
        .map(|&(_, v)| v)
        .expect("seat in payouts")
}

/// 开局：底池 = 6×50 + 50 + 100 + 200 = 650；straddle 进 `committed_this_round`；首个行动者是
/// straddle 下家（座 4），跟注额 200、最小加注到 400。
#[test]
fn straddle_posted_as_bet_and_utg1_acts_first() {
    let cfg = cfg_6max_1_2_4_ante1();
    assert_eq!(cfg.straddle_seat(), Some(seat(3)));
    let state = GameState::new(&cfg, 1);
    Invariants::check_all(&state, expected_total_chips(&cfg)).expect("invariants");

    assert_eq!(state.pot(), chips(650));
    let want = [
        // (stack, committed_total, committed_this_round)
        (9_950, 50, 0),    // BTN
        (9_900, 100, 50),  // SB
        (9_850, 150, 100), // BB
        (9_750, 250, 200), // straddle
        (9_950, 50, 0),
        (9_950, 50, 0),
    ];
    for (i, (p, &(stack, total, round))) in state.players().iter().zip(&want).enumerate() {
        assert_eq!(p.stack, chips(stack), "seat {i} stack");
        assert_eq!(p.committed_total, chips(total), "seat {i} committed_total");
        assert_eq!(
            p.committed_this_round,
            chips(round),
            "seat {i} committed_this_round"
        );
        assert_eq!(p.status, PlayerStatus::Active, "seat {i} status");
    }

    assert_eq!(state.current_player(), Some(seat(4)));
    let la = state.legal_actions();
    assert!(la.fold && !la.check);
    assert_eq!(la.call, Some(chips(200)));
    assert_eq!(la.raise_range.map(|(min, _)| min), Some(chips(400)));
}

/// 全弃到 straddle：straddle 收走全部 ante + SB + BB → 净 +400（= 5×50 + 50 + 100）；
/// SB −100、BB −150、其余 −50。
#[test]
fn straddle_walk_collects_blinds_and_antes() {
    let cfg = cfg_6max_1_2_4_ante1();
    let mut state = GameState::new(&cfg, 2);
    let total = expected_total_chips(&cfg);
    drive(
        &mut state,
        total,
        &[
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::Fold),
            (2, Action::Fold),
        ],
    );
    assert!(state.is_terminal());
    assert_eq!(net(&state, 3), 400);
    assert_eq!(net(&state, 1), -100);
    assert_eq!(net(&state, 2), -150);
    for s in [0, 4, 5] {
        assert_eq!(net(&state, s), -50, "seat {s}");
    }
}

/// 没人加注：SB / BB 补到 straddle（补 150 / 100），straddle 座有选择权（可过牌、可加注到 400）；
/// 过牌后进翻牌，SB 先说话、最小下注 = 大盲 100。
#[test]
fn straddle_has_option_and_postflop_min_bet_is_big_blind() {
    let cfg = cfg_6max_1_2_4_ante1();
    let mut state = GameState::new(&cfg, 3);
    let total = expected_total_chips(&cfg);
    drive(
        &mut state,
        total,
        &[(4, Action::Call), (5, Action::Fold), (0, Action::Call)],
    );
    assert_eq!(state.legal_actions().call, Some(chips(200)), "SB 补到 200");
    drive(&mut state, total, &[(1, Action::Call)]);
    assert_eq!(state.legal_actions().call, Some(chips(200)), "BB 补到 200");
    drive(&mut state, total, &[(2, Action::Call)]);

    assert_eq!(state.street(), Street::Preflop);
    assert_eq!(state.current_player(), Some(seat(3)));
    let la = state.legal_actions();
    assert!(la.check && la.call.is_none(), "straddle 的选择权：可过牌");
    assert_eq!(la.raise_range.map(|(min, _)| min), Some(chips(400)));

    drive(&mut state, total, &[(3, Action::Check)]);
    assert_eq!(state.street(), Street::Flop);
    assert_eq!(state.pot(), chips(300 + 5 * 200));
    assert_eq!(state.current_player(), Some(seat(1)));
    assert_eq!(
        state.legal_actions().bet_range.map(|(min, _)| min),
        Some(chips(100)),
        "翻后最小下注回到大盲"
    );
}

/// 翻前加注后最小再加注按加注增量算：straddle 200 → 座 4 加到 500（增量 300）→ 下一个最小
/// 再加注到 800。
#[test]
fn reraise_min_follows_last_raise_increment() {
    let cfg = cfg_6max_1_2_4_ante1();
    let mut state = GameState::new(&cfg, 4);
    let total = expected_total_chips(&cfg);
    drive(&mut state, total, &[(4, Action::Raise { to: chips(500) })]);
    assert_eq!(state.current_player(), Some(seat(5)));
    assert_eq!(
        state.legal_actions().raise_range.map(|(min, _)| min),
        Some(chips(800))
    );
    assert!(
        state
            .clone()
            .apply(Action::Raise { to: chips(700) })
            .is_err(),
        "低于最小再加注须拒"
    );
}

/// 3 座桌：straddle 座 = 按钮，翻前 SB 先说话、按钮最后（选择权）。
#[test]
fn three_handed_button_straddles() {
    let cfg = TableConfig::n_max_at_bb(3, 100).with_straddle(chips(200));
    assert_eq!(cfg.straddle_seat(), Some(seat(0)));
    let mut state = GameState::new(&cfg, 5);
    let total = expected_total_chips(&cfg);
    assert_eq!(state.pot(), chips(350));
    drive(&mut state, total, &[(1, Action::Call), (2, Action::Call)]);
    assert_eq!(state.current_player(), Some(seat(0)));
    assert!(state.legal_actions().check);
    drive(&mut state, total, &[(0, Action::Check)]);
    assert_eq!(state.street(), Street::Flop);
    assert_eq!(state.current_player(), Some(seat(1)));
}

/// straddle 座栈不够：交多少算多少、即 all-in；跟注额 = 它实际交的 150，最小加注仍按 straddle
/// 额（到 150 + 200 = 350）。其余弃到 BB、BB 跟 150 → 只剩一个能行动的人，直接发完到终局。
#[test]
fn short_straddle_posts_all_in() {
    let mut cfg = cfg_6max_1_2_4_ante1();
    cfg.starting_stacks[3] = chips(200); // ante 50 后只剩 150
    let mut state = GameState::new(&cfg, 6);
    let total = expected_total_chips(&cfg);
    Invariants::check_all(&state, total).expect("invariants");
    let s3 = &state.players()[3];
    assert_eq!(s3.status, PlayerStatus::AllIn);
    assert_eq!(s3.committed_this_round, chips(150));
    assert_eq!(state.current_player(), Some(seat(4)));
    let la = state.legal_actions();
    assert_eq!(la.call, Some(chips(150)));
    assert_eq!(la.raise_range.map(|(min, _)| min), Some(chips(350)));
    drive(
        &mut state,
        total,
        &[
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::Fold),
            (2, Action::Call),
        ],
    );
    assert!(state.is_terminal());
    let sum: i64 = (0..6).map(|s| net(&state, s)).sum();
    assert_eq!(sum, 0, "守恒");
}

/// history 往返：straddle 写进 proto（tag 9）、读回逐字段相同，replay 到同一终局。
#[test]
fn history_roundtrip_keeps_straddle() {
    let cfg = cfg_6max_1_2_4_ante1();
    let mut state = GameState::new(&cfg, 7);
    let total = expected_total_chips(&cfg);
    drive(
        &mut state,
        total,
        &[
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::Fold),
            (2, Action::Fold),
        ],
    );
    let bytes = state.hand_history().to_proto();
    let back = HandHistory::from_proto(&bytes).expect("decode");
    assert_eq!(back.config.straddle, chips(200));
    let replayed = back.replay().expect("replay");
    assert_eq!(replayed.payouts(), state.payouts());

    // straddle = 0 时不编码：与不带 straddle 字段的旧字节相同。
    let plain = GameState::new(&TableConfig::default_6max_100bb(), 7);
    let mut cfg0 = TableConfig::default_6max_100bb();
    cfg0.straddle = ChipAmount::ZERO;
    let plain0 = GameState::new(&cfg0, 7);
    assert_eq!(
        plain.hand_history().to_proto(),
        plain0.hand_history().to_proto()
    );
}

/// HU 不能有 straddle；straddle 小于大盲也拒。
#[test]
#[should_panic(expected = "straddle")]
fn heads_up_straddle_rejected() {
    let cfg = TableConfig::default_hu_200bb().with_straddle(chips(200));
    let _ = GameState::new(&cfg, 8);
}

#[test]
#[should_panic(expected = "straddle")]
fn straddle_below_big_blind_rejected() {
    let cfg = TableConfig::default_6max_100bb().with_straddle(chips(50));
    let _ = GameState::new(&cfg, 9);
}

/// 补盲不能标 straddle 座。
#[test]
#[should_panic(expected = "live_posts")]
fn live_post_on_straddle_seat_rejected() {
    let mut cfg = cfg_6max_1_2_4_ante1();
    cfg.live_posts = 1 << 3;
    let _ = GameState::new(&cfg, 10);
}

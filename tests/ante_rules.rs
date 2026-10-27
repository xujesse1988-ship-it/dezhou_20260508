//! ante 规则回归（`TableConfig::ante` ≠ 0）。
//!
//! 语义：发盲注前每座交 `ante`（不足额则 all-in），计入底池 / `committed_total`，但**不算**
//! 本轮下注（`committed_this_round`），故跟注额 / 最小加注只看盲注。side pot 按每人总投入分层，
//! ante 不足额 all-in 的玩家只能从每个对手处赢回自己交的那么多。PokerKit 跨验证见
//! `tests/cross_validation.rs` 的 `cross_validation_pokerkit_ante_*`；本文件用手算期望值
//! 锁死几个边角。

mod common;

use std::collections::HashSet;

use poker::{Action, ChipAmount, GameState, PlayerStatus, SeatId, TableConfig};

use common::{
    build_dealing_order, card, chips, expected_total_chips, pick_unused_padding, seat, Invariants,
    StackedDeckRng,
};

/// 6-max 100BB、每座 ante 10 chips（0.1BB），按钮座 0（SB = 1 / BB = 2 / UTG = 3）。
fn cfg_6max_ante10() -> TableConfig {
    TableConfig::default_6max_100bb().with_ante(chips(10))
}

/// 让 `winner_seat` 在公共牌上拿 AAAA 必胜的牌序（同 `tests/side_pots.rs` 模板 A）。
fn winner_deck(cfg: &TableConfig, winner_seat: u8) -> StackedDeckRng {
    let n = cfg.n_seats as usize;
    let (win_a, win_b) = (card(12, 3), card(12, 2)); // As Ah
    let flop = [card(12, 0), card(12, 1), card(7, 2)]; // Ac Ad 9h
    let (turn, river) = (card(8, 2), card(9, 2)); // Th Jh
    let fixed: HashSet<u8> = [win_a, win_b, flop[0], flop[1], flop[2], turn, river]
        .iter()
        .map(|c| c.to_u8())
        .collect();
    let mut pool = pick_unused_padding(&fixed, 52 - 7);
    // holes[k] = 按钮左手第 k+1 个座位（发牌起点 SB）的底牌，与 build_dealing_order 契约一致。
    let holes: Vec<_> = (0..n)
        .map(|k| {
            if (cfg.button_seat.0 as usize + 1 + k) % n == winner_seat as usize {
                (win_a, win_b)
            } else {
                (pool.remove(0), pool.remove(0))
            }
        })
        .collect();
    let used: HashSet<u8> = holes
        .iter()
        .flat_map(|(a, b)| [a.to_u8(), b.to_u8()])
        .chain(fixed.iter().copied())
        .collect();
    let padding = pick_unused_padding(&used, 52 - 2 * n - 5);
    StackedDeckRng::from_target_cards(build_dealing_order(n, &holes, flop, turn, river, &padding))
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

/// 每座先交 ante 再发盲注：底池 = 6×10 + 50 + 100 = 210；ante 不进 `committed_this_round`，
/// UTG 跟注额仍是 100、最小加注到 200（与无 ante 相同）。
#[test]
fn ante_posted_into_pot_but_not_as_bet() {
    let cfg = cfg_6max_ante10();
    let state = GameState::new(&cfg, 1);
    Invariants::check_all(&state, expected_total_chips(&cfg)).expect("invariants");

    assert_eq!(state.pot(), chips(210));
    let want = [
        // (stack, committed_total, committed_this_round)
        (9_990, 10, 0),    // BTN
        (9_940, 60, 50),   // SB
        (9_890, 110, 100), // BB
        (9_990, 10, 0),    // UTG
        (9_990, 10, 0),
        (9_990, 10, 0),
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

    assert_eq!(state.current_player(), Some(seat(3)));
    let la = state.legal_actions();
    assert_eq!(la.call, Some(chips(100)));
    assert_eq!(la.raise_range.map(|(min, _)| min), Some(chips(200)));
}

/// 全弃到 BB：BB 收走全部 ante + SB → 净 +100（= 5×10 + 50）；SB −60；其余 −10。
#[test]
fn ante_walk_bb_collects_all_antes() {
    let cfg = cfg_6max_ante10();
    let mut state = GameState::new(&cfg, 2);
    let total = expected_total_chips(&cfg);
    drive(
        &mut state,
        total,
        &[
            (3, Action::Fold),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::Fold),
        ],
    );
    assert!(state.is_terminal());
    assert_eq!(net(&state, 2), 100);
    assert_eq!(net(&state, 1), -60);
    for s in [0, 3, 4, 5] {
        assert_eq!(net(&state, s), -10, "seat {s}");
    }
}

/// 栈 4 < ante 10：交 4 即 all-in、不再行动。它拿必胜牌也只能赢主池 6×4 = 24（净 +20）；
/// 每人多交的 6 chip ante 进 side pot，归仍在争的 SB / BB。弃牌者净 −10。
#[test]
fn short_stack_all_in_on_ante_wins_only_capped_main_pot() {
    let mut cfg = cfg_6max_ante10();
    cfg.starting_stacks[3] = chips(4);
    let total = expected_total_chips(&cfg);
    let mut deck = winner_deck(&cfg, 3);
    let mut state = GameState::with_rng(&cfg, 3, &mut deck);
    Invariants::check_all(&state, total).expect("initial invariants");

    let utg = &state.players()[3];
    assert_eq!(utg.status, PlayerStatus::AllIn);
    assert_eq!(utg.committed_total, chips(4));
    assert_eq!(utg.committed_this_round, ChipAmount::ZERO);
    assert_eq!(state.pot(), chips(5 * 10 + 4 + 150));

    // UTG 已 all-in，首个行动者是座 4；SB 补齐、BB 过牌，翻后 SB / BB 一路过牌到摊牌。
    let mut plan = vec![
        (4, Action::Fold),
        (5, Action::Fold),
        (0, Action::Fold),
        (1, Action::Call),
        (2, Action::Check),
    ];
    for _ in 0..3 {
        plan.extend([(1, Action::Check), (2, Action::Check)]);
    }
    drive(&mut state, total, &plan);
    assert!(state.is_terminal());

    assert_eq!(net(&state, 3), 6 * 4 - 4);
    for s in [0, 4, 5] {
        assert_eq!(net(&state, s), -10, "seat {s}");
    }
    let sum: i64 = (0..6).map(|s| net(&state, s)).sum();
    assert_eq!(sum, 0);
}

/// BB 栈 50：先交 ante 10，剩 40 全部作盲注 → all-in；ante 仍不算本轮下注。
#[test]
fn ante_then_partial_big_blind_all_in() {
    let mut cfg = cfg_6max_ante10();
    cfg.starting_stacks[2] = chips(50);
    let state = GameState::new(&cfg, 4);
    Invariants::check_all(&state, expected_total_chips(&cfg)).expect("invariants");

    let bb = &state.players()[2];
    assert_eq!(bb.status, PlayerStatus::AllIn);
    assert_eq!(bb.stack, ChipAmount::ZERO);
    assert_eq!(bb.committed_total, chips(50));
    assert_eq!(bb.committed_this_round, chips(40));
    assert_eq!(state.pot(), chips(5 * 10 + 50 + 50));
}

/// `ante_exempt`（实盘短桌幻影座用）：座 4、5 不交 ante → 底池 = 4×10 + 150 = 190，豁免座栈不动。
#[test]
fn exempt_seats_post_no_ante() {
    let mut cfg = cfg_6max_ante10();
    cfg.ante_exempt = 0b11_0000;
    let state = GameState::new(&cfg, 5);
    Invariants::check_all(&state, expected_total_chips(&cfg)).expect("invariants");
    assert_eq!(state.pot(), chips(190));
    for s in [4usize, 5] {
        assert_eq!(state.players()[s].stack, chips(10_000), "seat {s}");
        assert_eq!(
            state.players()[s].committed_total,
            ChipAmount::ZERO,
            "seat {s}"
        );
    }
    assert_eq!(state.players()[3].committed_total, chips(10));
}

/// BB 豁免 ante、全弃到 BB：BB 退回没人跟的 50 后总投入 50，低于弃牌的 SB（60）——60 那层只有
/// 弃牌者，这份死钱归仍在场的 BB。BB 净 +100（5×10 + 50），SB −60，其余 −10。PokerKit 对这种层
/// 未定义（见 `cross_validation_pokerkit_ante_exempt_200_hands`）。
#[test]
fn exempt_bb_walk_collects_dead_antes() {
    let mut cfg = cfg_6max_ante10();
    cfg.ante_exempt = 1 << 2;
    let mut state = GameState::new(&cfg, 6);
    let total = expected_total_chips(&cfg);
    drive(
        &mut state,
        total,
        &[
            (3, Action::Fold),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::Fold),
        ],
    );
    assert!(state.is_terminal());
    assert_eq!(net(&state, 2), 100);
    assert_eq!(net(&state, 1), -60);
    for s in [0, 3, 4, 5] {
        assert_eq!(net(&state, s), -10, "seat {s}");
    }
}

/// 3 座等栈 1000、ante 100、BTN 豁免：BTN 全下 1000，SB / BB 交完 ante 只够全下到 900 跟注 →
/// BTN 多下的 100 没人跟得起、退回（ante 是死钱、不算跟注）。SB 拿必胜牌：赢 900×3 + BB 多投的
/// 100×2 = 2900，净 +1900；BTN 只输 900，BB 输 1000。
#[test]
fn exempt_player_excess_over_callers_is_returned() {
    let mut cfg = TableConfig::n_max_at_bb(3, 10).with_ante(chips(100));
    cfg.ante_exempt = 1 << 0;
    let total = expected_total_chips(&cfg);
    let mut deck = winner_deck(&cfg, 1);
    let mut state = GameState::with_rng(&cfg, 7, &mut deck);
    drive(
        &mut state,
        total,
        &[(0, Action::AllIn), (1, Action::Call), (2, Action::Call)],
    );
    assert!(state.is_terminal());
    assert_eq!(net(&state, 1), 1_900);
    assert_eq!(net(&state, 0), -900);
    assert_eq!(net(&state, 2), -1_000);
}

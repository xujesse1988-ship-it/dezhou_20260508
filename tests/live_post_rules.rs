//! 补盲规则回归（`TableConfig::live_posts` ≠ 0）。
//!
//! 语义（wopo 实盘新进场 / 回座玩家补盲，2026-10-28 用户确认）：非盲注座开局额外补一个
//! `big_blind`，算**本轮下注**（live：跟注额照扣），行动顺序不变（不是 straddle，UTG 照常先说话），
//! 没人加注时轮到他可以过牌（同大盲的选择权）。PokerKit 只能把 BB 之后的强制注当 straddle
//! （改变行动顺序），跨验证不了，本文件用手算期望值和一手实盘牌锁死。

mod common;

use std::collections::HashSet;

use poker::{Action, ChipAmount, GameState, HandHistory, SeatId, TableConfig};

use common::{
    build_dealing_order, card, chips, expected_total_chips, pick_unused_padding, seat, Invariants,
    StackedDeckRng,
};

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

/// 6-max 100BB 无 ante，按钮座 0（SB = 1 / BB = 2 / UTG = 3），座 4 补盲。
fn cfg_6max_post4() -> TableConfig {
    let mut cfg = TableConfig::default_6max_100bb();
    cfg.live_posts = 1 << 4;
    cfg
}

/// 补的盲注进底池且算本轮下注；UTG 仍先说话，跟注额 100、最小加注到 200（与无补盲相同）。
#[test]
fn post_is_live_bet_and_order_unchanged() {
    let cfg = cfg_6max_post4();
    let state = GameState::new(&cfg, 1);
    Invariants::check_all(&state, expected_total_chips(&cfg)).expect("invariants");
    assert_eq!(state.pot(), chips(250));
    let p4 = &state.players()[4];
    assert_eq!(p4.stack, chips(9_900));
    assert_eq!(p4.committed_this_round, chips(100));
    assert_eq!(p4.committed_total, chips(100));
    assert_eq!(state.current_player(), Some(seat(3)));
    let la = state.legal_actions();
    assert_eq!(la.call, Some(chips(100)));
    assert_eq!(la.raise_range.map(|(lo, _)| lo), Some(chips(200)));
}

/// 没人加注轮到补盲者：可以过牌（不能跟注）、可以加注；他过牌后 BB 仍有选择权，过牌进翻牌。
#[test]
fn poster_has_option_when_unraised() {
    let cfg = cfg_6max_post4();
    let total = expected_total_chips(&cfg);
    let mut state = GameState::new(&cfg, 2);
    drive(&mut state, total, &[(3, Action::Call)]);
    assert_eq!(state.current_player(), Some(seat(4)));
    let la = state.legal_actions();
    assert!(la.check, "{la:?}");
    assert_eq!(la.call, None, "{la:?}");
    assert_eq!(la.raise_range.map(|(lo, _)| lo), Some(chips(200)));
    drive(
        &mut state,
        total,
        &[
            (4, Action::Check),
            (5, Action::Fold),
            (0, Action::Call),
            (1, Action::Call),
        ],
    );
    assert_eq!(state.current_player(), Some(seat(2)));
    assert!(state.legal_actions().check);
    drive(&mut state, total, &[(2, Action::Check)]);
    assert_eq!(state.street(), poker::Street::Flop);
    assert_eq!(state.pot(), chips(500));
    // 翻牌从 SB 起。
    assert_eq!(state.current_player(), Some(seat(1)));
}

/// 面对加注补盲者只需补差额（加注到 300 → 跟 200）；弃牌则那 100 是死钱，归赢家。
#[test]
fn poster_calls_difference_or_leaves_dead_money() {
    let cfg = cfg_6max_post4();
    let total = expected_total_chips(&cfg);
    let mut state = GameState::new(&cfg, 3);
    drive(&mut state, total, &[(3, Action::Raise { to: chips(300) })]);
    assert_eq!(state.current_player(), Some(seat(4)));
    assert_eq!(state.legal_actions().call, Some(chips(300)));
    let before = state.players()[4].stack;
    drive(&mut state, total, &[(4, Action::Call)]);
    assert_eq!(before - state.players()[4].stack, chips(200));

    let mut state = GameState::new(&cfg, 4);
    drive(
        &mut state,
        total,
        &[
            (3, Action::Raise { to: chips(300) }),
            (4, Action::Fold),
            (5, Action::Fold),
            (0, Action::Fold),
            (1, Action::Fold),
            (2, Action::Fold),
        ],
    );
    assert!(state.is_terminal());
    // UTG 没人跟的 200 退回，赢 SB 50 + BB 100 + 补盲 100。
    assert_eq!(net(&state, 3), 250);
    assert_eq!(net(&state, 4), -100);
    assert_eq!(net(&state, 1), -50);
    assert_eq!(net(&state, 2), -100);
}

/// 补盲栈不足一个大盲 → 补多少算多少、直接全下（同盲注）。
#[test]
fn short_poster_goes_all_in() {
    let mut cfg = cfg_6max_post4();
    cfg.starting_stacks[4] = chips(60);
    let state = GameState::new(&cfg, 5);
    assert_eq!(state.players()[4].stack, ChipAmount::ZERO);
    assert_eq!(state.players()[4].committed_this_round, chips(60));
    assert_eq!(state.players()[4].status, poker::PlayerStatus::AllIn);
}

#[test]
#[should_panic(expected = "live_posts")]
fn post_on_blind_seat_rejected() {
    let mut cfg = TableConfig::default_6max_100bb();
    cfg.live_posts = 1 << 2; // BB
    let _ = GameState::new(&cfg, 6);
}

/// history 往返：live_posts 编进 proto（tag 8）、回放逐字相同；0 不编码（旧字节不变）；
/// 标盲注座的 proto 解码报错而不是 panic。
#[test]
fn history_roundtrip_and_corruption() {
    let cfg = cfg_6max_post4();
    let total = expected_total_chips(&cfg);
    let mut state = GameState::new(&cfg, 7);
    drive(
        &mut state,
        total,
        &[(3, Action::Call), (4, Action::Raise { to: chips(400) })],
    );
    let bytes = state.hand_history().to_proto();
    let back = HandHistory::from_proto(&bytes).expect("decode");
    assert_eq!(back.config.live_posts, 1 << 4);
    let replayed = back.replay().expect("replay");
    assert_eq!(replayed.pot(), state.pot());
    assert_eq!(back.to_proto(), bytes);

    let plain = GameState::new(&TableConfig::default_6max_100bb(), 7);
    let mut cfg_zero = TableConfig::default_6max_100bb();
    cfg_zero.live_posts = 0;
    assert_eq!(
        GameState::new(&cfg_zero, 7).hand_history().to_proto(),
        plain.hand_history().to_proto()
    );

    let mut bad = state.hand_history().clone();
    bad.config.live_posts = 1 << 1; // SB
    assert!(HandHistory::from_proto(&bad.to_proto()).is_err());
}

/// wopo 实盘 `ante-table:hand:000132`（2026-10-28，8 座、SB 50 / BB 100 / ante 100，按钮座 6，
/// 座 4 新进场补盲）整手重放：每条街底池与服务端 `pot` 一致，结算与 `hand_result` 一致
/// （座 3 Ac4c 同花赢座 5 8d6c 顺子，河牌座 3 全下多出的 10193 没人跟、退回）。
#[test]
fn wopo_hand_132_replays_exactly() {
    let mut cfg = TableConfig::n_max_at_bb(8, 100).with_ante(chips(100));
    cfg.button_seat = SeatId(6);
    cfg.live_posts = 1 << 4;
    // 开局真栈 = 第一个 table_state 的 stack + bet + ante。
    cfg.starting_stacks = [
        22_170, 48_350, 28_375, 60_193, 30_000, 50_000, 28_416, 88_708,
    ]
    .into_iter()
    .map(chips)
    .collect();
    let total = expected_total_chips(&cfg);

    // 底牌：座 0 7h3s、座 3 Ac4c、座 5 8d6c，其余随便；公共牌 3c 9c 7c / 9s / 5d。
    // card(rank, suit)：rank 0 = 2 … 12 = A；suit 0 = c、1 = d、2 = h、3 = s。
    let flop = [card(1, 0), card(7, 0), card(5, 0)];
    let (turn, river) = (card(7, 3), card(3, 1));
    let known: [(u8, (poker::Card, poker::Card)); 3] = [
        (0, (card(5, 2), card(1, 3))),
        (3, (card(12, 0), card(2, 0))),
        (5, (card(6, 1), card(4, 0))),
    ];
    let mut fixed: HashSet<u8> = [flop[0], flop[1], flop[2], turn, river]
        .iter()
        .map(|c| c.to_u8())
        .collect();
    for (_, (a, b)) in known {
        fixed.insert(a.to_u8());
        fixed.insert(b.to_u8());
    }
    let mut pool = pick_unused_padding(&fixed, 52 - fixed.len());
    let n = 8usize;
    let holes: Vec<_> = (0..n)
        .map(|k| {
            let s = ((6 + 1 + k) % n) as u8;
            known
                .iter()
                .find(|(ks, _)| *ks == s)
                .map(|&(_, h)| h)
                .unwrap_or_else(|| (pool.remove(0), pool.remove(0)))
        })
        .collect();
    let used: HashSet<u8> = holes
        .iter()
        .flat_map(|(a, b)| [a.to_u8(), b.to_u8()])
        .chain(fixed.iter().copied())
        .collect();
    let padding = pick_unused_padding(&used, 52 - 2 * n - 5);
    let mut deck = StackedDeckRng::from_target_cards(build_dealing_order(
        n, &holes, flop, turn, river, &padding,
    ));
    let mut state = GameState::with_rng(&cfg, 132, &mut deck);

    assert_eq!(state.pot(), chips(1_050));
    drive(
        &mut state,
        total,
        &[
            (1, Action::Fold),
            (2, Action::Fold),
            (3, Action::Raise { to: chips(960) }),
            (4, Action::Fold),
            (5, Action::Call),
            (6, Action::Fold),
            (7, Action::Call),
            (0, Action::Fold),
        ],
    );
    assert_eq!(state.street(), poker::Street::Flop);
    assert_eq!(state.pot(), chips(3_880));
    drive(
        &mut state,
        total,
        &[
            (7, Action::Check),
            (3, Action::Check),
            (5, Action::Bet { to: chips(2_600) }),
            (7, Action::Raise { to: chips(8_700) }),
            (3, Action::Call),
            (5, Action::Raise { to: chips(45_000) }),
            (7, Action::Fold),
            (3, Action::Call),
        ],
    );
    assert_eq!(state.street(), poker::Street::Turn);
    assert_eq!(state.pot(), chips(102_580));
    drive(&mut state, total, &[(3, Action::Check), (5, Action::Check)]);
    assert_eq!(state.pot(), chips(102_580));
    drive(&mut state, total, &[(3, Action::AllIn), (5, Action::AllIn)]);
    assert!(state.is_terminal());
    // hand_result.final_stacks − 开局真栈。
    let want = [
        (0, -200),
        (1, -100),
        (2, -100),
        (3, 60_460),
        (4, -200),
        (5, -50_000),
        (6, -100),
        (7, -9_760),
    ];
    for (s, v) in want {
        assert_eq!(net(&state, s), v, "seat {s}");
    }
}

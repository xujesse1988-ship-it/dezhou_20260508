//! AIVAT runout / showdown 值与 stage-1 规则引擎结算**逐 completion 一致**（`docs/aivat_eval.md`
//! §4.4）。证明 `runout_ev` 闭式 `m·(2eq−1)` == GameState `compute_payouts` 对补全取平均，
//! `showdown_net` == 单局摊牌 net payout——把"用 eval7 自己算" 钉死到规则引擎的 side-pot /
//! 退注 / odd-chip 结算口径上，零漂移。

mod common;

use std::collections::HashSet;

use common::{build_dealing_order, pick_unused_padding, StackedDeckRng};
use poker::training::aivat_nlhe::{runout_ev, showdown_net};
use poker::training::nlhe_replay::parse_card;
use poker::{Action, Card, GameState, TableConfig};

fn c(s: &str) -> Card {
    parse_card(s).unwrap()
}

/// 构造一手 HU 200BB preflop 全下到摊牌的终局：双方 all-in（matched 20000），跑满
/// `board`。返回 `(终局 state, 我方座位, m)`。
fn allin_terminal(our: [Card; 2], opp: [Card; 2], board: [Card; 5]) -> (GameState, usize, f64) {
    let used: HashSet<u8> = our
        .iter()
        .chain(opp.iter())
        .chain(board.iter())
        .map(|x| x.to_u8())
        .collect();
    let padding = pick_unused_padding(&used, 52 - 4 - 5);
    let deck = build_dealing_order(
        2,
        &[(our[0], our[1]), (opp[0], opp[1])],
        [board[0], board[1], board[2]],
        board[3],
        board[4],
        &padding,
    );
    let mut rng = StackedDeckRng::from_target_cards(deck);
    let cfg = TableConfig::default_hu_200bb();
    let mut gs = GameState::with_rng(&cfg, 0, &mut rng);
    // 先动方 all-in，对方 call → 双方 all-in → runout → 摊牌终局。
    gs.apply(Action::AllIn).expect("first all-in legal");
    gs.apply(Action::Call).expect("call all-in legal");
    assert!(gs.is_terminal(), "all-in + call 后应终局");

    let our_set: HashSet<u8> = [our[0].to_u8(), our[1].to_u8()].into_iter().collect();
    let mut our_seat = None;
    for (i, p) in gs.players().iter().enumerate() {
        if let Some(h) = p.hole_cards {
            let s: HashSet<u8> = [h[0].to_u8(), h[1].to_u8()].into_iter().collect();
            if s == our_set {
                our_seat = Some(i);
            }
        }
    }
    let our_seat = our_seat.expect("我方底牌应在某座位");
    let m = gs
        .players()
        .iter()
        .map(|p| p.committed_total.as_u64())
        .min()
        .unwrap() as f64;
    (gs, our_seat, m)
}

fn engine_net(our: [Card; 2], opp: [Card; 2], board: [Card; 5]) -> f64 {
    let (gs, seat, _m) = allin_terminal(our, opp, board);
    let payouts = gs.payouts().expect("终局有 payouts");
    payouts
        .iter()
        .find(|(s, _)| s.0 as usize == seat)
        .unwrap()
        .1 as f64
}

fn remaining(our: [Card; 2], opp: [Card; 2], fixed: &[Card]) -> Vec<Card> {
    let used: HashSet<u8> = our
        .iter()
        .chain(opp.iter())
        .chain(fixed.iter())
        .map(|x| x.to_u8())
        .collect();
    (0u8..52)
        .filter(|i| !used.contains(i))
        .map(|i| Card::from_u8(i).unwrap())
        .collect()
}

#[test]
fn showdown_net_matches_engine_win_lose_tie() {
    let m = 20_000.0;
    // 我方两对 AK vs 对方 QQ → 我方胜。
    let our = [c("As"), c("Ks")];
    let opp = [c("Qd"), c("Qc")];
    let board = [c("Ad"), c("Kd"), c("2c"), c("7h"), c("3s")];
    assert_eq!(showdown_net(our, opp, &board, m), m);
    assert_eq!(engine_net(our, opp, board), m);

    // 我方负（对方一对 vs 我方高牌）。
    let our2 = [c("2c"), c("7d")];
    let opp2 = [c("As"), c("Ah")];
    let board2 = [c("Ad"), c("Kd"), c("9c"), c("4h"), c("3s")];
    assert_eq!(showdown_net(our2, opp2, &board2, m), -m);
    assert_eq!(engine_net(our2, opp2, board2), -m);

    // 平局：牌面皇家同花顺，两手都打 board。
    let our3 = [c("2c"), c("3d")];
    let opp3 = [c("4h"), c("8c")];
    let board3 = [c("As"), c("Ks"), c("Qs"), c("Js"), c("Ts")];
    assert_eq!(showdown_net(our3, opp3, &board3, m), 0.0);
    assert_eq!(engine_net(our3, opp3, board3), 0.0);
}

#[test]
fn runout_ev_matches_engine_river_runout() {
    // board_so_far = 4 张（turn 后全下），river 1 张 runout（44 补全）。
    let m = 20_000.0;
    let our = [c("As"), c("Kh")];
    let opp = [c("Qd"), c("Jc")];
    let board4 = [c("Ad"), c("7s"), c("2c"), c("9h")];

    let deck = remaining(our, opp, &board4);
    let mut sum = 0.0;
    let mut n = 0u64;
    for &river in &deck {
        let board5 = [board4[0], board4[1], board4[2], board4[3], river];
        sum += engine_net(our, opp, board5);
        n += 1;
    }
    let engine_avg = sum / n as f64;
    let mine = runout_ev(our, opp, &board4, m);
    assert!(
        (engine_avg - mine).abs() < 1e-6,
        "river runout: engine_avg {engine_avg} != runout_ev {mine}（n={n}）"
    );
}

#[test]
fn runout_ev_matches_engine_turn_river_runout() {
    // board_so_far = 3 张（flop 后全下），turn+river 2 张 runout（C(45,2)=990 补全）。
    let m = 20_000.0;
    let our = [c("Ts"), c("Th")];
    let opp = [c("Ac"), c("Kd")];
    let board3 = [c("Td"), c("5s"), c("2c")];

    let deck = remaining(our, opp, &board3);
    let mut sum = 0.0;
    let mut n = 0u64;
    for i in 0..deck.len() {
        for j in (i + 1)..deck.len() {
            let board5 = [board3[0], board3[1], board3[2], deck[i], deck[j]];
            sum += engine_net(our, opp, board5);
            n += 1;
        }
    }
    let engine_avg = sum / n as f64;
    let mine = runout_ev(our, opp, &board3, m);
    assert_eq!(n, 990, "C(45,2) 补全数");
    assert!(
        (engine_avg - mine).abs() < 1e-6,
        "turn+river runout: engine_avg {engine_avg} != runout_ev {mine}（n={n}）"
    );
}

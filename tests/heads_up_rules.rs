//! Heads-up NLHE profile regression tests.
//!
//! These tests lock the public 2-player 100BB profile introduced for the
//! heads-up solver target while keeping the generic `SeatId` / `TableConfig`
//! model intact.

mod common;

use common::{chips, expected_total_chips, seat, Invariants};

use poker::{Action, GameState, SeatId, Street, TableConfig};

fn drive(state: &mut GameState, expected_total: u64, plan: &[(SeatId, Action)]) {
    for (i, (want_seat, action)) in plan.iter().enumerate() {
        let cp = state
            .current_player()
            .unwrap_or_else(|| panic!("step {i}: current_player == None, expected {want_seat:?}"));
        assert_eq!(
            cp, *want_seat,
            "step {i}: current_player mismatch (expected {want_seat:?}, got {cp:?})"
        );
        state
            .apply(*action)
            .unwrap_or_else(|e| panic!("step {i}: apply({action:?}) failed: {e}"));
        Invariants::check_all(state, expected_total)
            .unwrap_or_else(|e| panic!("step {i} (after {action:?}): {e}"));
    }
}

#[test]
fn default_hu_100bb_posts_button_sb_and_bb() {
    let cfg = TableConfig::default_hu_100bb();
    assert_eq!(cfg.n_seats, 2);
    assert_eq!(cfg.starting_stacks, vec![chips(10_000); 2]);
    assert_eq!(cfg.small_blind, chips(50));
    assert_eq!(cfg.big_blind, chips(100));
    assert_eq!(cfg.ante, chips(0));
    assert_eq!(cfg.button_seat, seat(0));

    let state = GameState::new(&cfg, 0);
    assert_eq!(state.players().len(), 2);
    assert_eq!(state.pot(), chips(150));
    assert_eq!(
        state.current_player(),
        Some(seat(0)),
        "HU preflop first action should be button/SB"
    );

    let sb = &state.players()[0];
    let bb = &state.players()[1];
    assert_eq!(sb.seat, seat(0));
    assert_eq!(sb.stack, chips(9_950));
    assert_eq!(sb.committed_this_round, chips(50));
    assert_eq!(sb.committed_total, chips(50));
    assert_eq!(bb.seat, seat(1));
    assert_eq!(bb.stack, chips(9_900));
    assert_eq!(bb.committed_this_round, chips(100));
    assert_eq!(bb.committed_total, chips(100));

    let legal = state.legal_actions();
    assert!(legal.fold);
    assert!(!legal.check);
    assert_eq!(legal.call, Some(chips(100)));
    assert_eq!(legal.raise_range, Some((chips(200), chips(10_000))));
    assert_eq!(legal.all_in_amount, Some(chips(10_000)));

    Invariants::check_all(&state, expected_total_chips(&cfg))
        .expect("default HU initial state should satisfy invariants");
}

#[test]
fn heads_up_call_check_reaches_flop_with_bb_first_to_act() {
    let cfg = TableConfig::default_hu_100bb();
    let mut state = GameState::new(&cfg, 1);
    let total = expected_total_chips(&cfg);

    drive(
        &mut state,
        total,
        &[(seat(0), Action::Call), (seat(1), Action::Check)],
    );

    assert_eq!(state.street(), Street::Flop);
    assert_eq!(state.board().len(), 3);
    assert_eq!(
        state.current_player(),
        Some(seat(1)),
        "HU postflop first action should be BB/OOP"
    );
    assert_eq!(
        state.players()[0].committed_this_round,
        chips(0),
        "new betting round should reset SB committed_this_round"
    );
    assert_eq!(
        state.players()[1].committed_this_round,
        chips(0),
        "new betting round should reset BB committed_this_round"
    );

    drive(
        &mut state,
        total,
        &[
            (seat(1), Action::Check),
            (seat(0), Action::Check),
            (seat(1), Action::Check),
            (seat(0), Action::Check),
            (seat(1), Action::Check),
            (seat(0), Action::Check),
        ],
    );

    assert!(state.is_terminal());
    assert_eq!(state.street(), Street::Showdown);
    assert_eq!(state.board().len(), 5);
    let payouts = state
        .payouts()
        .expect("terminal HU showdown should have payouts");
    assert_eq!(payouts.len(), 2);
    assert_eq!(payouts.iter().map(|(_, pnl)| *pnl).sum::<i64>(), 0);
}

#[test]
fn heads_up_button_seat_controls_preflop_and_postflop_order() {
    let mut cfg = TableConfig::default_hu_100bb();
    cfg.button_seat = seat(1);
    let mut state = GameState::new(&cfg, 2);
    let total = expected_total_chips(&cfg);

    assert_eq!(
        state.current_player(),
        Some(seat(1)),
        "when button is seat 1, HU preflop first action should be seat 1/SB"
    );
    assert_eq!(state.players()[1].committed_this_round, chips(50));
    assert_eq!(state.players()[0].committed_this_round, chips(100));

    drive(
        &mut state,
        total,
        &[(seat(1), Action::Call), (seat(0), Action::Check)],
    );

    assert_eq!(state.street(), Street::Flop);
    assert_eq!(
        state.current_player(),
        Some(seat(0)),
        "when button is seat 1, HU postflop first action should be seat 0/BB"
    );
}

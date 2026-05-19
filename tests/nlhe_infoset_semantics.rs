//! H3 500M checkpoint investigation regression tests.
//!
//! These tests keep the simplified NLHE infoset key honest: the CFR table is
//! indexed by `(InfoSetId, action_index)`, so two states with the same key must
//! not assign different amount semantics to the same D-209 action slot.

use std::sync::Arc;

use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::SimplifiedNlheGame;
use poker::{AbstractAction, BucketConfig, BucketTable, ChaCha20Rng, ChipAmount, InfoSetId};

fn stub_game() -> SimplifiedNlheGame {
    let table = Arc::new(BucketTable::stub_for_postflop(
        BucketConfig::default_500_500_500(),
    ));
    SimplifiedNlheGame::new(table).expect("500/500/500 stub table should construct NLHE game")
}

fn find_call(actions: &[AbstractAction]) -> AbstractAction {
    actions
        .iter()
        .copied()
        .find(|action| matches!(action, AbstractAction::Call { .. }))
        .expect("state should offer Call")
}

fn raise_actions(actions: &[AbstractAction]) -> Vec<AbstractAction> {
    actions
        .iter()
        .copied()
        .filter(|action| matches!(action, AbstractAction::Raise { .. }))
        .collect()
}

fn action_to_amount(action: AbstractAction) -> ChipAmount {
    match action {
        AbstractAction::Call { to }
        | AbstractAction::Bet { to, .. }
        | AbstractAction::Raise { to, .. }
        | AbstractAction::AllIn { to } => to,
        AbstractAction::Fold | AbstractAction::Check => ChipAmount::ZERO,
    }
}

fn action_shape_signature(actions: &[AbstractAction]) -> Vec<(u8, u32)> {
    actions
        .iter()
        .map(|action| match action {
            AbstractAction::Fold => (0, 0),
            AbstractAction::Check => (1, 0),
            AbstractAction::Call { .. } => (2, 0),
            AbstractAction::Bet { ratio_label, .. } => (3, ratio_label.as_milli()),
            AbstractAction::Raise { ratio_label, .. } => (4, ratio_label.as_milli()),
            AbstractAction::AllIn { .. } => (5, 0),
        })
        .collect()
}

#[test]
fn simplified_nlhe_infoset_distinguishes_limp_raise_amount_semantics() {
    let game = stub_game();
    let mut rng = ChaCha20Rng::from_seed(0x4833_494E_464F_0001);
    let root = game.root(&mut rng);
    assert_eq!(SimplifiedNlheGame::current(&root), NodeKind::Player(0));

    // SB/button limps.
    let root_actions = SimplifiedNlheGame::legal_actions(&root);
    let limp = find_call(&root_actions);
    let bb_to_act = SimplifiedNlheGame::next(root, limp, &mut rng);
    assert_eq!(SimplifiedNlheGame::current(&bb_to_act), NodeKind::Player(1));

    // Compare two BB raise sizes, then SB's response nodes. Do not lock exact
    // labels here: the expanded default profile can dedup smaller ratios into
    // the same `to` amount, so the available labels are profile-dependent.
    let bb_actions = SimplifiedNlheGame::legal_actions(&bb_to_act);
    let raises = raise_actions(&bb_actions);
    assert!(
        raises.len() >= 2,
        "regression setup needs at least two BB raise sizes; actions={bb_actions:?}"
    );

    let mut chosen = None;
    for i in 0..raises.len() {
        for j in (i + 1)..raises.len() {
            if action_to_amount(raises[i]) == action_to_amount(raises[j]) {
                continue;
            }
            let facing_a = SimplifiedNlheGame::next(bb_to_act.clone(), raises[i], &mut rng);
            let facing_b = SimplifiedNlheGame::next(bb_to_act.clone(), raises[j], &mut rng);
            if SimplifiedNlheGame::current(&facing_a) != NodeKind::Player(0)
                || SimplifiedNlheGame::current(&facing_b) != NodeKind::Player(0)
            {
                continue;
            }
            let actions_a = SimplifiedNlheGame::legal_actions(&facing_a);
            let actions_b = SimplifiedNlheGame::legal_actions(&facing_b);
            if action_shape_signature(&actions_a) == action_shape_signature(&actions_b)
                && actions_a != actions_b
            {
                chosen = Some((facing_a, facing_b, actions_a, actions_b));
                break;
            }
        }
        if chosen.is_some() {
            break;
        }
    }

    let (facing_a, facing_b, actions_a, actions_b) = chosen.unwrap_or_else(|| {
        panic!(
            "regression setup must find two raise sizes with same action shape but different amount semantics; raises={raises:?}"
        )
    });

    assert_ne!(
        action_to_amount(find_call(&actions_a)),
        action_to_amount(find_call(&actions_b)),
        "response nodes should differ in call amount semantics"
    );

    let info_a: InfoSetId = SimplifiedNlheGame::info_set(&facing_a, 0);
    let info_b: InfoSetId = SimplifiedNlheGame::info_set(&facing_b, 0);
    assert_ne!(
        info_a.raw(),
        info_b.raw(),
        "SB facing distinct BB raise amounts with same action shape must not share an InfoSetId"
    );
}

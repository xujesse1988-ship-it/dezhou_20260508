//! H3 500M checkpoint investigation regression tests.
//!
//! These tests keep the simplified NLHE infoset key honest: the CFR table is
//! indexed by `(InfoSetId, action_index)`, so two states with the same key must
//! not assign different amount semantics to the same D-209 action slot.

use std::sync::Arc;

use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::SimplifiedNlheGame;
use poker::{
    AbstractAction, BetRatio, BucketConfig, BucketTable, ChaCha20Rng, ChipAmount, InfoSetId,
};

fn stub_game() -> SimplifiedNlheGame {
    let table = Arc::new(BucketTable::stub_for_postflop(
        BucketConfig::default_500_500_500(),
    ));
    SimplifiedNlheGame::new(table).expect("500/500/500 stub table should construct NLHE game")
}

fn role_mask(actions: &[AbstractAction]) -> u8 {
    let mut mask = 0u8;
    for action in actions {
        let bit = match action {
            AbstractAction::Fold => 0,
            AbstractAction::Check => 1,
            AbstractAction::Call { .. } => 2,
            AbstractAction::Bet { ratio_label, .. } | AbstractAction::Raise { ratio_label, .. } => {
                if ratio_label.as_milli() == BetRatio::HALF_POT.as_milli() {
                    3
                } else {
                    4
                }
            }
            AbstractAction::AllIn { .. } => 5,
        };
        mask |= 1 << bit;
    }
    mask
}

fn find_call(actions: &[AbstractAction]) -> AbstractAction {
    actions
        .iter()
        .copied()
        .find(|action| matches!(action, AbstractAction::Call { .. }))
        .expect("state should offer Call")
}

fn find_raise(actions: &[AbstractAction], ratio: BetRatio) -> AbstractAction {
    actions
        .iter()
        .copied()
        .find(|action| {
            matches!(
                action,
                AbstractAction::Raise { ratio_label, .. }
                    if ratio_label.as_milli() == ratio.as_milli()
            )
        })
        .expect("state should offer requested Raise ratio")
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

    // Compare BB half-pot raise vs full-pot raise, then SB's response node.
    let bb_actions = SimplifiedNlheGame::legal_actions(&bb_to_act);
    let half_pot_raise = find_raise(&bb_actions, BetRatio::HALF_POT);
    let full_pot_raise = find_raise(&bb_actions, BetRatio::FULL_POT);
    assert_eq!(action_to_amount(half_pot_raise), ChipAmount::new(200));
    assert_eq!(action_to_amount(full_pot_raise), ChipAmount::new(300));

    let facing_half = SimplifiedNlheGame::next(bb_to_act.clone(), half_pot_raise, &mut rng);
    let facing_full = SimplifiedNlheGame::next(bb_to_act, full_pot_raise, &mut rng);
    assert_eq!(
        SimplifiedNlheGame::current(&facing_half),
        NodeKind::Player(0)
    );
    assert_eq!(
        SimplifiedNlheGame::current(&facing_full),
        NodeKind::Player(0)
    );

    let half_actions = SimplifiedNlheGame::legal_actions(&facing_half);
    let full_actions = SimplifiedNlheGame::legal_actions(&facing_full);
    assert_eq!(
        role_mask(&half_actions),
        role_mask(&full_actions),
        "regression setup must keep the old 6-bit availability mask identical"
    );
    assert_ne!(
        half_actions, full_actions,
        "regression setup must differ only by action amount semantics"
    );

    assert_eq!(
        action_to_amount(find_call(&half_actions)),
        ChipAmount::new(200)
    );
    assert_eq!(
        action_to_amount(find_call(&full_actions)),
        ChipAmount::new(300)
    );
    assert_eq!(
        action_to_amount(find_raise(&half_actions, BetRatio::HALF_POT)),
        ChipAmount::new(400)
    );
    assert_eq!(
        action_to_amount(find_raise(&full_actions, BetRatio::HALF_POT)),
        ChipAmount::new(600)
    );
    assert_eq!(
        action_to_amount(find_raise(&half_actions, BetRatio::FULL_POT)),
        ChipAmount::new(600)
    );
    assert_eq!(
        action_to_amount(find_raise(&full_actions, BetRatio::FULL_POT)),
        ChipAmount::new(900)
    );

    let info_half: InfoSetId = SimplifiedNlheGame::info_set(&facing_half, 0);
    let info_full: InfoSetId = SimplifiedNlheGame::info_set(&facing_full, 0);
    assert_ne!(
        info_half.raw(),
        info_full.raw(),
        "SB facing BB half-pot raise and full-pot raise must not share an InfoSetId"
    );
}

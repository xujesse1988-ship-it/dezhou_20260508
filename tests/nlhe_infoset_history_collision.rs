//! 验证 `docs/nlhe_infoset_history_investigation.md` Step 1 假设。
//!
//! 简化 NLHE 的 `InfoSetId` 不编码跨街动作历史。两条不同的 preflop 线推进到
//! flop 同一决策点时，应当产生**同一** `InfoSetId`（collision）。本测试把这条
//! collision 钉成 regression：当 history 字段加入 `InfoSetId` 编码后，`assert_eq`
//! 会失败，提示测试需要翻转为 `assert_ne`。

use std::sync::Arc;

use poker::training::game::{Game, NodeKind};
use poker::training::nlhe::{SimplifiedNlheGame, SimplifiedNlheState};
use poker::{AbstractAction, BetRatio, BucketConfig, BucketTable, ChaCha20Rng, InfoSetId, Street};

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
        .find(|a| matches!(a, AbstractAction::Call { .. }))
        .expect("expected Call in action set")
}

fn find_raise(actions: &[AbstractAction], ratio: BetRatio) -> AbstractAction {
    actions
        .iter()
        .copied()
        .find(|a| matches!(
            a,
            AbstractAction::Raise { ratio_label, .. } if ratio_label.as_milli() == ratio.as_milli()
        ))
        .expect("expected matching Raise in action set")
}

/// 推进直到 `state.game_state.street() == Street::Flop` 第一次成立时停下，返回 BB
/// (player 1) 决策点。
fn assert_at_flop_bb_to_act(state: &SimplifiedNlheState) {
    assert_eq!(
        state.game_state.street(),
        Street::Flop,
        "branch must reach Flop"
    );
    assert_eq!(
        SimplifiedNlheGame::current(state),
        NodeKind::Player(1),
        "BB acts first on flop (out of position)"
    );
}

#[test]
fn simplified_nlhe_infoset_collapses_distinct_preflop_lines() {
    let game = stub_game();
    let seed = 0x4833_494E_464F_5F43_u64; // "H3INFO_C"

    // --- Branch A: SB raises FULL_POT preflop, BB calls -> flop, BB to act. ---
    let mut rng_a = ChaCha20Rng::from_seed(seed);
    let root_a = game.root(&mut rng_a);
    assert_eq!(SimplifiedNlheGame::current(&root_a), NodeKind::Player(0));

    let sb_actions_a = SimplifiedNlheGame::legal_actions(&root_a);
    let sb_raise = find_raise(&sb_actions_a, BetRatio::FULL_POT);
    let after_sb_raise = SimplifiedNlheGame::next(root_a, sb_raise, &mut rng_a);
    assert_eq!(
        SimplifiedNlheGame::current(&after_sb_raise),
        NodeKind::Player(1)
    );

    let bb_actions_a = SimplifiedNlheGame::legal_actions(&after_sb_raise);
    let bb_call_a = find_call(&bb_actions_a);
    let flop_a = SimplifiedNlheGame::next(after_sb_raise, bb_call_a, &mut rng_a);
    assert_at_flop_bb_to_act(&flop_a);

    // --- Branch B: SB limps, BB raises FULL_POT, SB calls -> flop, BB to act. ---
    let mut rng_b = ChaCha20Rng::from_seed(seed);
    let root_b = game.root(&mut rng_b);
    assert_eq!(SimplifiedNlheGame::current(&root_b), NodeKind::Player(0));

    let sb_actions_b = SimplifiedNlheGame::legal_actions(&root_b);
    let sb_limp = find_call(&sb_actions_b);
    let after_sb_limp = SimplifiedNlheGame::next(root_b, sb_limp, &mut rng_b);
    assert_eq!(
        SimplifiedNlheGame::current(&after_sb_limp),
        NodeKind::Player(1)
    );

    let bb_actions_b = SimplifiedNlheGame::legal_actions(&after_sb_limp);
    let bb_raise = find_raise(&bb_actions_b, BetRatio::FULL_POT);
    let after_bb_raise = SimplifiedNlheGame::next(after_sb_limp, bb_raise, &mut rng_b);
    assert_eq!(
        SimplifiedNlheGame::current(&after_bb_raise),
        NodeKind::Player(0)
    );

    let sb_response_actions = SimplifiedNlheGame::legal_actions(&after_bb_raise);
    let sb_call = find_call(&sb_response_actions);
    let flop_b = SimplifiedNlheGame::next(after_bb_raise, sb_call, &mut rng_b);
    assert_at_flop_bb_to_act(&flop_b);

    // --- Sanity: 同一 RNG seed 下两条线发牌完全一致（next 不消费 rng；root 一次性发牌）。 ---
    for seat in 0..2 {
        assert_eq!(
            flop_a.game_state.players()[seat].hole_cards,
            flop_b.game_state.players()[seat].hole_cards,
            "seat {seat} hole cards must match between branches"
        );
    }
    assert_eq!(
        flop_a.game_state.board(),
        flop_b.game_state.board(),
        "flop board must match between branches"
    );

    // --- Sanity: 两条线 action_history 实际不同，collision 不是状态退化。 ---
    assert_ne!(
        flop_a.action_history, flop_b.action_history,
        "branches must have taken distinct preflop action sequences"
    );

    // --- 核心断言：当前 `InfoSetId` 编码丢失跨街 history，两条线 collapse 到同一 key。 ---
    let info_a: InfoSetId = SimplifiedNlheGame::info_set(&flop_a, 1);
    let info_b: InfoSetId = SimplifiedNlheGame::info_set(&flop_b, 1);
    assert_eq!(
        info_a.raw(),
        info_b.raw(),
        "regression: SB-aggressor flop vs BB-aggressor flop currently share InfoSetId. \
         当 InfoSetId 加入 history 编码后翻转为 assert_ne。"
    );
}

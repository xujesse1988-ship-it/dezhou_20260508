//! 验证 `docs/nlhe_infoset_history_investigation.md` 方案 A Phase 3 行为修复。
//!
//! 历史背景（commit 60eacec）：旧 InfoSetId 编码不含跨街动作历史，本测试当时以
//! `assert_eq` 钉住了 collision——SB-aggressor 与 BB-aggressor 两条 preflop 线
//! 推进到 flop 同一决策点时返回同一 InfoSetId。
//!
//! Phase 3 起 InfoSetId v2 layout 把 PublicBettingTree node_id 写入高 26 bit，
//! 抽象动作序列单射于 node_id → 两条不同 preflop 线必产出不同 InfoSetId。
//! 断言翻转为 `assert_ne`，作为 history fix 的 regression gate。

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
fn simplified_nlhe_infoset_distinguishes_distinct_preflop_lines() {
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

    // --- Sanity: 两条线 实际走到不同 tree 节点（D-378 CFR fast path 起 root
    // 不累 `action_history`，改用 tree path-to-root 验证抽象动作序列不同）。 ---
    let path_a = game.tree().path_to_root(flop_a.current_node_id);
    let path_b = game.tree().path_to_root(flop_b.current_node_id);
    assert_ne!(
        path_a, path_b,
        "branches must have taken distinct preflop abstract-action paths"
    );

    // --- 核心断言：v2 InfoSetId 通过 node_id 区分跨街 aggressor 身份。 ---
    let info_a: InfoSetId = SimplifiedNlheGame::info_set(&flop_a, 1);
    let info_b: InfoSetId = SimplifiedNlheGame::info_set(&flop_b, 1);
    assert_ne!(
        info_a.raw(),
        info_b.raw(),
        "InfoSetId v2 must distinguish SB-aggressor flop vs BB-aggressor flop \
         via node_id; assert_eq 翻 assert_ne 是 Phase 3 行为修复的 gate。"
    );
    // 同 hand_bucket 同 street_tag → 低 38 bit 应一致；差异只能来自 node_id（bits 38..64）。
    let low_mask: u64 = (1u64 << 38) - 1;
    assert_eq!(
        info_a.raw() & low_mask,
        info_b.raw() & low_mask,
        "两条线 hand_bucket / street_tag 一致；差异应当只在 node_id 位上"
    );
    assert_ne!(
        info_a.raw() >> 38,
        info_b.raw() >> 38,
        "node_id 必须不同，否则 tree 没做单射"
    );
}

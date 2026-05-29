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

// ===========================================================================
// T1（Slumbot advisor 桥接，docs/temp/slumbot_api_bridge_plan_2026_05_29.md §7）：
// SimplifiedNlheGame::info_set_for_cards 注入路径与 Game::info_set 训练路径 byte-equal。
// advisor 用抽象影子状态的 node_id + 对面发来的真实牌查 blueprint，必须命中训练时
// 同一个 InfoSetId，否则策略查错格。
// ===========================================================================

/// 沿被动线（有 Check 走 Check，否则 Call）从 root 走到 terminal，在**每个**决策节点
/// 断言 `info_set_for_cards(current_node_id, actor 真实 hole, 真实 board)` 与
/// `Game::info_set(state, actor)` 逐位相等。被动线覆盖 preflop（SB 先动）+ flop/turn/
/// river（BB 先动 + SB 后动）两个 actor、全四街。
fn assert_info_set_for_cards_byte_equal(game: &SimplifiedNlheGame, seeds: &[u64]) {
    for &seed in seeds {
        let mut rng = ChaCha20Rng::from_seed(seed);
        let mut state = game.root(&mut rng);
        let mut decision_nodes_checked = 0usize;
        for _ in 0..64 {
            match SimplifiedNlheGame::current(&state) {
                NodeKind::Terminal => break,
                NodeKind::Chance => unreachable!("simplified NLHE has no in-game chance node"),
                NodeKind::Player(actor) => {
                    let node_id = state.current_node_id;
                    let hole = state.game_state.players()[actor as usize]
                        .hole_cards
                        .expect("decision-node actor must hold hole cards");
                    let injected = game.info_set_for_cards(node_id, hole, state.game_state.board());
                    let direct = SimplifiedNlheGame::info_set(&state, actor);
                    assert_eq!(
                        injected.raw(),
                        direct.raw(),
                        "info_set_for_cards diverged from Game::info_set at node {node_id} \
                         (street {:?}, actor {actor}, seed {seed:#x})",
                        state.game_state.street()
                    );
                    decision_nodes_checked += 1;
                    // 被动线推进：postflop 首动有 Check；preflop SB 面对 BB 无 Check 走 Call。
                    let actions = SimplifiedNlheGame::legal_actions(&state);
                    let action = actions
                        .iter()
                        .copied()
                        .find(|a| matches!(a, AbstractAction::Check))
                        .or_else(|| {
                            actions
                                .iter()
                                .copied()
                                .find(|a| matches!(a, AbstractAction::Call { .. }))
                        })
                        .expect("被动线在任何决策节点都至少有 Check 或 Call");
                    state = SimplifiedNlheGame::next(state, action, &mut rng);
                }
            }
        }
        assert!(
            decision_nodes_checked >= 4,
            "被动线 (seed {seed:#x}) 应至少跨 4 个决策节点（preflop+flop+turn+river），\
             实测 {decision_nodes_checked}——root 发牌或被动推进逻辑异常"
        );
    }
}

const T1_SEEDS: [u64; 6] = [1, 7, 42, 0xC0FFEE, 0xDEAD_BEEF, 0x4833_5f4e_4c48_455f];

/// T1（stub 表）：注入路径 byte-equal 训练路径。stub postflop bucket 恒 0，故主要锁
/// preflop hand_class + node_id/street_tag 打包 + postflop-vs-preflop 分支不混淆（若
/// postflop 误走 preflop 路径会拿到 hand_class∈0..168 ≠ stub lookup 的 0，立即 fail）。
#[test]
fn info_set_for_cards_byte_equal_to_info_set_stub() {
    let game = stub_game();
    assert_info_set_for_cards_byte_equal(&game, &T1_SEEDS);
}

/// T1（真实 cafebabe v4 表）：postflop bucket 有区分度时验证 `canonical_observation_id`
/// 接 `lookup` 注入路径——stub 恒 0 盖不住的那段。artifact 不进 git，仅 vultr 有；本机
/// 缺文件时跳过（不 fail，保持本机 build/clippy 绿）。
#[test]
fn info_set_for_cards_byte_equal_to_info_set_real_artifact() {
    let path = "artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin";
    if !std::path::Path::new(path).exists() {
        eprintln!("[T1] skip real-artifact byte-equal check: {path} absent (本机无 artifact)");
        return;
    }
    let table = Arc::new(
        BucketTable::open(std::path::Path::new(path)).expect("open cafebabe v4 bucket table"),
    );
    let game =
        SimplifiedNlheGame::new(table).expect("real cafebabe v4 table should construct NLHE game");
    assert_info_set_for_cards_byte_equal(&game, &T1_SEEDS);
}

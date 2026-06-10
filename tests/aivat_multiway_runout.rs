//! 多人 AIVAT `E_runout` 与规则引擎**逐补全**一致（缺口⑥，`aivat_multiway` 模块 doc）。
//!
//! 把估计器的补全口径（[`GameState::with_external_cards_and_runout`] + apply 最后动作 +
//! 权威 `payouts`）钉死到独立 oracle——**stacked-deck 全程重放**（每补全从发牌起重打整手）：
//! 3-max **不等栈 + all-in-for-less side pot** 在 flop 锁定，turn+river 共 C(43,2)=903 补全，
//! 两条路逐补全平均必须一致；oracle 侧同时验每补全 Σ payouts == 0（守恒）。

mod common;

use std::collections::HashSet;

use common::{build_dealing_order, pick_unused_padding, StackedDeckRng};
use poker::training::aivat_multiway::{MultiwayAivatEstimator, MultiwayHandInput};
use poker::{Action, Card, ChipAmount, GameState, PlayerStatus, SeatId, TableConfig};

fn cfg_3max_sidepot() -> TableConfig {
    TableConfig {
        n_seats: 3,
        // BTN 深、SB 短、BB 中 → flop 3-way all-in 必出 main + side pot（all-in-for-less）。
        starting_stacks: vec![
            ChipAmount::new(10_000),
            ChipAmount::new(4_000),
            ChipAmount::new(7_000),
        ],
        small_blind: ChipAmount::new(50),
        big_blind: ChipAmount::new(100),
        ante: ChipAmount::ZERO,
        button_seat: SeatId(0),
    }
}

/// 整手动作序：limp 进 flop → SB all-in（4000 总）→ BB all-in（7000 总）→ BTN call → 锁定。
fn hand_actions() -> Vec<(SeatId, Action)> {
    vec![
        (SeatId(0), Action::Call),
        (SeatId(1), Action::Call),
        (SeatId(2), Action::Check),
        (SeatId(1), Action::AllIn),
        (SeatId(2), Action::AllIn),
        (SeatId(0), Action::Call),
    ]
}

/// 标定 `build_dealing_order` 的「builder 下标 → 座位」映射（不假设 D-028 的发牌起点）：
/// 用 marker 牌发一次，读每座实际拿到哪组 → `perm[seat] = builder 下标`。
fn dealing_perm(cfg: &TableConfig) -> Vec<usize> {
    let n = cfg.n_seats as usize;
    let holes: Vec<(Card, Card)> = (0..n)
        .map(|k| {
            (
                Card::from_u8(2 * k as u8).unwrap(),
                Card::from_u8(2 * k as u8 + 1).unwrap(),
            )
        })
        .collect();
    let used: HashSet<u8> = (0..2 * n as u8).collect();
    let rest = pick_unused_padding(&used, 52 - 2 * n - 5);
    let (board5, padding) = rest.split_at(5);
    let deck = build_dealing_order(
        n,
        &holes,
        [board5[0], board5[1], board5[2]],
        board5[3],
        board5[4],
        padding,
    );
    let mut rng = StackedDeckRng::from_target_cards(deck);
    let st = GameState::with_rng(cfg, 0, &mut rng);
    (0..n)
        .map(|seat| {
            let h = st.players()[seat].hole_cards.expect("发牌后必有底牌");
            let key = (h[0].to_u8().min(h[1].to_u8()) / 2) as usize;
            assert!(key < n, "marker 底牌应成对落座");
            key
        })
        .collect()
}

#[test]
fn multiway_sidepot_runout_matches_stacked_replay_oracle() {
    let cfg = cfg_3max_sidepot();
    let actions = hand_actions();

    // —— 模拟真实一手（随机发牌；牌从 state 读回）——
    let mut sim = GameState::new(&cfg, 0x4D57_5F52_554E_4F55); // "MW_RUNOU"
    for (seat, a) in &actions {
        assert_eq!(sim.current_player(), Some(*seat), "动作序与回合不符");
        sim.apply(*a).expect("动作应合法");
    }
    assert!(sim.is_terminal(), "BTN call 后应锁定 → runout → 终局");
    let board: Vec<Card> = sim.board().to_vec();
    assert_eq!(board.len(), 5, "终局应满 5 张");
    let holes: Vec<[Card; 2]> = (0..3)
        .map(|i| sim.players()[i].hole_cards.expect("3-way 全员到摊牌"))
        .collect();
    // side pot 前置：短码 all-in-for-less（committed_total 不等）。
    let committed: Vec<u64> = sim
        .players()
        .iter()
        .map(|p| p.committed_total.as_u64())
        .collect();
    assert_eq!(committed[1], 4_000, "SB 全押 4000");
    assert!(
        committed[2] > committed[1],
        "BB 押注超过 SB（side pot 存在）"
    );
    assert!(
        sim.players()
            .iter()
            .all(|p| p.status != PlayerStatus::Folded),
        "3-way 摊牌"
    );
    let hero = 0usize; // BTN（covering caller）
    let payouts = sim.payouts().expect("终局有 payouts");
    let winnings = payouts
        .iter()
        .find(|(s, _)| s.0 as usize == hero)
        .unwrap()
        .1;

    // —— 估计器路径 ——
    let input = MultiwayHandInput {
        config: cfg.clone(),
        our_seat: SeatId(hero as u8),
        our_hole: holes[hero],
        revealed: vec![Some(holes[0]), Some(holes[1]), Some(holes[2])],
        board: board.clone(),
        actions: actions.clone(),
        winnings,
    };
    let est = MultiwayAivatEstimator::new(None);
    let r = est.estimate_hand(&input).expect("side-pot runout 手应 Ok");
    assert!(r.has_runout, "flop 锁定 → 有 runout 段");
    assert_eq!(
        r.n_runout_completions,
        43 * 42 / 2,
        "3-max 全亮：52−6−3=43 → C(43,2) 补全"
    );
    assert_eq!(r.n_unknown_folded, 0, "无弃牌座");
    let e_est = r.raw - r.c_runout; // E_runout = U − c_runout

    // —— oracle：stacked-deck 全程重放逐补全 ——
    let perm = dealing_perm(&cfg);
    let mut builder_holes = vec![(holes[0][0], holes[0][1]); 3];
    for seat in 0..3 {
        builder_holes[perm[seat]] = (holes[seat][0], holes[seat][1]);
    }
    let mut known: HashSet<u8> = holes
        .iter()
        .flatten()
        .map(|c| c.to_u8())
        .chain(board[..3].iter().map(|c| c.to_u8()))
        .collect();
    assert_eq!(known.len(), 9);
    let deck_rest: Vec<Card> = (0u8..52)
        .filter(|v| !known.contains(v))
        .map(|v| Card::from_u8(v).unwrap())
        .collect();
    let mut oracle_sum: i64 = 0;
    let mut oracle_cnt: u64 = 0;
    for i in 0..deck_rest.len() {
        for j in (i + 1)..deck_rest.len() {
            let (turn, river) = (deck_rest[i], deck_rest[j]);
            known.insert(turn.to_u8());
            known.insert(river.to_u8());
            let padding = pick_unused_padding(&known, 52 - 6 - 5);
            known.remove(&turn.to_u8());
            known.remove(&river.to_u8());
            let deck = build_dealing_order(
                3,
                &builder_holes,
                [board[0], board[1], board[2]],
                turn,
                river,
                &padding,
            );
            let mut rng = StackedDeckRng::from_target_cards(deck);
            let mut st = GameState::with_rng(&cfg, 0, &mut rng);
            for (seat, a) in &actions {
                assert_eq!(st.current_player(), Some(*seat), "oracle 重放回合不符");
                st.apply(*a).expect("oracle 重放动作合法");
            }
            assert!(st.is_terminal());
            let p = st.payouts().expect("oracle 终局 payouts");
            let total: i64 = p.iter().map(|(_, v)| *v).sum();
            assert_eq!(total, 0, "每补全 per-seat PnL Σ==0（side pot 守恒）");
            oracle_sum += p.iter().find(|(s, _)| s.0 as usize == hero).unwrap().1;
            oracle_cnt += 1;
        }
    }
    assert_eq!(oracle_cnt, r.n_runout_completions, "补全数一致");
    let e_oracle = oracle_sum as f64 / oracle_cnt as f64;
    assert!(
        (e_est - e_oracle).abs() < 1e-6,
        "E_runout 估计器 {e_est} ≠ oracle {e_oracle}（整数平均应精确一致）"
    );
}

//! KuhnGame（API-301 / D-310）。
//!
//! 标准 Kuhn Poker：3 张牌 deck `{J, Q, K}`（rank 11/12/13）/ 2 player / 各发 1
//! 张私有牌 / 各 ante `1` chip / 1 round betting / 最多 `1` voluntary bet
//! （size = `1` chip）/ player 1 先行动 `{Check, Bet}`；player 2 应对
//! `{Pass, Bet}`（Check 后 Bet 视为 1 raise，最多 1 raise 链）。Showdown 比 rank
//! `J < Q < K`。Payoff 单位 chip。
//!
//! **InfoSet 数 = 12**（每 player 6 个：3 牌 × 2 公开历史 `["", "pb"]` for
//! player 1 / `["c", "b"]` for player 2）。
//!
//! A1 \[实现\] 阶段所有方法体 `unimplemented!()`；B2 \[实现\] 落地全部规则
//! transitions + closed-form Nash 收敛锚点（player 1 EV `-1/18`）。

use crate::core::rng::RngSource;
use crate::training::game::{Game, NodeKind, PlayerId};

/// Zero-sized game token；`KuhnGame` 不持有 state（API-301）。
#[derive(Clone, Copy, Debug, Default)]
pub struct KuhnGame;

/// Kuhn action 枚举（API-301 / D-310）。
///
/// 顺序与 [`Game::legal_actions`] 返回 `Vec<KuhnAction>` 索引一一对应，作为
/// [`crate::training::RegretTable`] `Vec<f64>` 的 deterministic 索引基础
/// （D-324 action_count 全程恒定）。
#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq)]
pub enum KuhnAction {
    Check,
    Bet,
    Call,
    Fold,
}

/// Kuhn 公开历史枚举（API-301）。
///
/// 每个 player 视角下的 InfoSet 仅由 `(private_card, history)` 决定（D-317 独立
/// 编码，不走 stage 2 `InfoSetId`）。
#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq)]
pub enum KuhnHistory {
    /// P1 to act（initial 状态）。
    Empty,
    /// P2 to act after P1 check。
    Check,
    /// P2 to act after P1 bet。
    Bet,
    /// P1 to act after P1 check, P2 bet。
    CheckBet,
}

/// Kuhn 玩家视角 InfoSet（API-301）。
///
/// `private_card ∈ {11, 12, 13}` per D-310 Kuhn rules。`Eq + Hash + Clone + Debug`
/// 满足 [`Game::InfoSet`] trait bound。
#[derive(Clone, Eq, Hash, Debug, PartialEq)]
pub struct KuhnInfoSet {
    pub actor: PlayerId,
    pub private_card: u8,
    pub history: KuhnHistory,
}

/// Kuhn 完整状态（API-301）。
///
/// `cards = [P1 card, P2 card]`；`history` 顺次记录 voluntary action；
/// `terminal_payoffs = Some([p1_chip_net, p2_chip_net])` 当且仅当当前 node 是
/// terminal（D-316 chip 净收益直接当 utility）。
#[derive(Clone, Debug)]
pub struct KuhnState {
    pub cards: [u8; 2],
    pub history: KuhnHistory,
    pub terminal_payoffs: Option<[f64; 2]>,
}

impl Game for KuhnGame {
    type State = KuhnState;
    type Action = KuhnAction;
    type InfoSet = KuhnInfoSet;

    fn n_players(&self) -> usize {
        unimplemented!("stage 3 A1 scaffold: KuhnGame::n_players (B2 实现)")
    }

    fn root(&self, _rng: &mut dyn RngSource) -> KuhnState {
        unimplemented!("stage 3 A1 scaffold: KuhnGame::root (B2 实现)")
    }

    fn current(_state: &KuhnState) -> NodeKind {
        unimplemented!("stage 3 A1 scaffold: KuhnGame::current (B2 实现)")
    }

    fn info_set(_state: &KuhnState, _actor: PlayerId) -> KuhnInfoSet {
        unimplemented!("stage 3 A1 scaffold: KuhnGame::info_set (B2 实现)")
    }

    fn legal_actions(_state: &KuhnState) -> Vec<KuhnAction> {
        unimplemented!("stage 3 A1 scaffold: KuhnGame::legal_actions (B2 实现)")
    }

    fn next(_state: KuhnState, _action: KuhnAction, _rng: &mut dyn RngSource) -> KuhnState {
        unimplemented!("stage 3 A1 scaffold: KuhnGame::next (B2 实现)")
    }

    fn chance_distribution(_state: &KuhnState) -> Vec<(KuhnAction, f64)> {
        unimplemented!("stage 3 A1 scaffold: KuhnGame::chance_distribution (B2 实现)")
    }

    fn payoff(_state: &KuhnState, _player: PlayerId) -> f64 {
        unimplemented!("stage 3 A1 scaffold: KuhnGame::payoff (B2 实现)")
    }
}

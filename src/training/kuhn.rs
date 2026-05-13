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
//! 发牌走两个连续 chance node：root 状态 `cards = [0, 0]` 触发 chance 1（deal P0
//! 私有牌 3 等概率 outcome）；`cards = [c, 0]` 触发 chance 2（deal P1 私有牌 2
//! 等概率 outcome，剔除 P0 已用 rank）。`KuhnAction::DealCard(u8)` 是 B2 \[实现\]
//! 在 API-301 字面 4 变体上的**追加变体**（满足 `Copy + Eq + Hash + Debug + PartialEq`
//! trait bound 不破坏既有 surface；详见 `tests/api_signatures.rs` 仅在类型层级用
//! KuhnAction 不 pattern-match 变体的契约）。

use crate::core::rng::RngSource;
use crate::training::game::{Game, NodeKind, PlayerId};

/// Zero-sized game token；`KuhnGame` 不持有 state（API-301）。
#[derive(Clone, Copy, Debug, Default)]
pub struct KuhnGame;

/// Kuhn action 枚举（API-301 / D-310）。
///
/// 顺序与 [`Game::legal_actions`] 返回 `Vec<KuhnAction>` 索引一一对应（D-324
/// action_count 全程恒定）。`DealCard(u8)` 仅在 chance node 出现（B2 \[实现\]
/// 追加变体，让 `chance_distribution` 在 chance node 暴露具体 card outcome）。
#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum KuhnAction {
    Check,
    Bet,
    Call,
    Fold,
    /// chance 节点上的发牌结果：u8 ∈ {11=J, 12=Q, 13=K}。
    DealCard(u8),
}

/// Kuhn 公开历史枚举（API-301）。
#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum KuhnHistory {
    /// P0 to act（initial betting 状态）。
    Empty,
    /// P1 to act after P0 check。
    Check,
    /// P1 to act after P0 bet。
    Bet,
    /// P0 to act after P0 check, P1 bet。
    CheckBet,
}

/// Kuhn 玩家视角 InfoSet（API-301）。
#[derive(Clone, Eq, Hash, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KuhnInfoSet {
    pub actor: PlayerId,
    pub private_card: u8,
    pub history: KuhnHistory,
}

/// Kuhn 完整状态（API-301）。
///
/// `cards = [0, 0]` 时为初始 chance node（待发 P0 牌）；`cards = [c, 0]` 时为
/// 第二 chance node（待发 P1 牌）；`cards = [c0, c1]`（均 `> 0`）后进入 betting。
/// `terminal_payoffs = Some([p0_net, p1_net])` 当且仅当 terminal。
#[derive(Clone, Debug)]
pub struct KuhnState {
    pub cards: [u8; 2],
    pub history: KuhnHistory,
    pub terminal_payoffs: Option<[f64; 2]>,
}

impl KuhnState {
    fn deal_phase(&self) -> Option<usize> {
        if self.cards[0] == 0 {
            Some(0)
        } else if self.cards[1] == 0 {
            Some(1)
        } else {
            None
        }
    }

    fn actor_of(history: KuhnHistory) -> PlayerId {
        match history {
            KuhnHistory::Empty | KuhnHistory::CheckBet => 0,
            KuhnHistory::Check | KuhnHistory::Bet => 1,
        }
    }
}

impl Game for KuhnGame {
    type State = KuhnState;
    type Action = KuhnAction;
    type InfoSet = KuhnInfoSet;

    const VARIANT: crate::error::GameVariant = crate::error::GameVariant::Kuhn;

    fn n_players(&self) -> usize {
        2
    }

    fn root(&self, _rng: &mut dyn RngSource) -> KuhnState {
        KuhnState {
            cards: [0, 0],
            history: KuhnHistory::Empty,
            terminal_payoffs: None,
        }
    }

    fn current(state: &KuhnState) -> NodeKind {
        if state.terminal_payoffs.is_some() {
            return NodeKind::Terminal;
        }
        if state.deal_phase().is_some() {
            return NodeKind::Chance;
        }
        NodeKind::Player(KuhnState::actor_of(state.history))
    }

    fn info_set(state: &KuhnState, actor: PlayerId) -> KuhnInfoSet {
        let card = state.cards[actor as usize];
        assert!(card > 0, "info_set called on chance/undealt state");
        KuhnInfoSet {
            actor,
            private_card: card,
            history: state.history,
        }
    }

    fn legal_actions(state: &KuhnState) -> Vec<KuhnAction> {
        match state.history {
            KuhnHistory::Empty | KuhnHistory::Check => vec![KuhnAction::Check, KuhnAction::Bet],
            KuhnHistory::Bet | KuhnHistory::CheckBet => vec![KuhnAction::Fold, KuhnAction::Call],
        }
    }

    fn next(state: KuhnState, action: KuhnAction, _rng: &mut dyn RngSource) -> KuhnState {
        if let Some(slot) = state.deal_phase() {
            let card = match action {
                KuhnAction::DealCard(c) => c,
                other => panic!("KuhnGame::next at chance node expects DealCard, got {other:?}"),
            };
            let mut cards = state.cards;
            cards[slot] = card;
            return KuhnState {
                cards,
                history: state.history,
                terminal_payoffs: None,
            };
        }

        // Decision node transition：按 (history, action) 派发。
        let (next_history, terminal_payoffs) = match (state.history, action) {
            (KuhnHistory::Empty, KuhnAction::Check) => (KuhnHistory::Check, None),
            (KuhnHistory::Empty, KuhnAction::Bet) => (KuhnHistory::Bet, None),
            (KuhnHistory::Check, KuhnAction::Check) => {
                // Check/Check → showdown ±1
                (state.history, Some(showdown(state.cards, 1.0)))
            }
            (KuhnHistory::Check, KuhnAction::Bet) => (KuhnHistory::CheckBet, None),
            (KuhnHistory::Bet, KuhnAction::Fold) => {
                // P1 folds → P0 wins ante (net P0=+1, P1=-1)
                (state.history, Some([1.0, -1.0]))
            }
            (KuhnHistory::Bet, KuhnAction::Call) => {
                // showdown ±2
                (state.history, Some(showdown(state.cards, 2.0)))
            }
            (KuhnHistory::CheckBet, KuhnAction::Fold) => {
                // P0 folds → P1 wins ante (net P0=-1, P1=+1)
                (state.history, Some([-1.0, 1.0]))
            }
            (KuhnHistory::CheckBet, KuhnAction::Call) => {
                // showdown ±2
                (state.history, Some(showdown(state.cards, 2.0)))
            }
            (history, action) => {
                panic!("KuhnGame::next 非法 (history={history:?}, action={action:?})")
            }
        };

        KuhnState {
            cards: state.cards,
            history: next_history,
            terminal_payoffs,
        }
    }

    fn chance_distribution(state: &KuhnState) -> Vec<(KuhnAction, f64)> {
        match state.deal_phase() {
            Some(0) => vec![
                (KuhnAction::DealCard(11), 1.0 / 3.0),
                (KuhnAction::DealCard(12), 1.0 / 3.0),
                (KuhnAction::DealCard(13), 1.0 / 3.0),
            ],
            Some(1) => {
                let p0 = state.cards[0];
                let remaining: Vec<u8> = [11u8, 12, 13].into_iter().filter(|c| *c != p0).collect();
                remaining
                    .into_iter()
                    .map(|c| (KuhnAction::DealCard(c), 0.5))
                    .collect()
            }
            _ => panic!("KuhnGame::chance_distribution called on non-chance state"),
        }
    }

    fn payoff(state: &KuhnState, player: PlayerId) -> f64 {
        let payoffs = state
            .terminal_payoffs
            .expect("KuhnGame::payoff called on non-terminal state");
        payoffs[player as usize]
    }
}

fn showdown(cards: [u8; 2], stakes: f64) -> [f64; 2] {
    if cards[0] > cards[1] {
        [stakes, -stakes]
    } else {
        [-stakes, stakes]
    }
}

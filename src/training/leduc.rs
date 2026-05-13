//! LeducGame（API-302 / D-311）。
//!
//! 标准 Leduc Poker：6 张牌 deck `{J♠, J♥, Q♠, Q♥, K♠, K♥}`（rank 11/12/13 ×
//! suit 0/1）/ 2 player / 每人发 1 张私有牌（preflop）+ 1 张公共牌（flop 后翻
//! 开）/ 各 ante `1` chip / 2 round betting / 每 round 最多 `2` voluntary raise /
//! preflop bet size = `2` chip、postflop bet size = `4` chip / showdown：先比
//! pair（私有 rank == 公共 rank → 自动赢）再比 rank。
//!
//! Card encoding `0..=5` → `{J♠=0, J♥=1, Q♠=2, Q♥=3, K♠=4, K♥=5}`；rank = `11 +
//! card / 2`。Chance node 上的发牌结果编码为 `LeducAction::Deal0..Deal5`（unit
//! variants，让 `LeducAction as u8` cast 保持有效，与 `tests/cfr_leduc.rs` BLAKE3
//! snapshot 路径兼容）；6 个 deal variant 不会出现在 `LeducHistory`（betting 历史
//! 仅累积 `{Check, Bet, Call, Fold, Raise}`）。
//!
//! Sentinel：未发的私有牌 `cards[i] == 0xFF`；初始 `cards = [0xFF, 0xFF]`。

use crate::core::rng::RngSource;
use crate::training::game::{Game, NodeKind, PlayerId};

/// Zero-sized game token（API-302）。
#[derive(Clone, Copy, Debug, Default)]
pub struct LeducGame;

/// Leduc action 枚举（API-302 / D-311 + B2 \[实现\] 追加 6 个 Deal 变体）。
///
/// 仅 betting 5 个变体 `{Check, Bet, Call, Fold, Raise}` 出现在 `LeducHistory`；
/// `Deal0..Deal5` 仅出现在 chance node 的 `chance_distribution` / `next` 入参，
/// 表示发牌结果（card index 0..=5）。Unit variants 让 `as u8` cast 保持有效。
#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LeducAction {
    Check,
    Bet,
    Call,
    Fold,
    Raise,
    Deal0,
    Deal1,
    Deal2,
    Deal3,
    Deal4,
    Deal5,
}

impl LeducAction {
    fn from_card(card: u8) -> LeducAction {
        match card {
            0 => LeducAction::Deal0,
            1 => LeducAction::Deal1,
            2 => LeducAction::Deal2,
            3 => LeducAction::Deal3,
            4 => LeducAction::Deal4,
            5 => LeducAction::Deal5,
            other => panic!("invalid Leduc card index {other}"),
        }
    }

    fn to_card(self) -> Option<u8> {
        Some(match self {
            LeducAction::Deal0 => 0,
            LeducAction::Deal1 => 1,
            LeducAction::Deal2 => 2,
            LeducAction::Deal3 => 3,
            LeducAction::Deal4 => 4,
            LeducAction::Deal5 => 5,
            _ => return None,
        })
    }
}

/// Leduc 街阶段（API-302 / D-311）。
#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LeducStreet {
    Preflop,
    Postflop,
}

/// Leduc 公开历史（API-302）。当前街的 betting action 序列；进入下一街时清空。
pub type LeducHistory = Vec<LeducAction>;

/// Leduc 玩家视角 InfoSet（API-302）。
#[derive(Clone, Eq, Hash, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LeducInfoSet {
    pub actor: PlayerId,
    pub private_card: u8,
    pub public_card: Option<u8>,
    pub street: LeducStreet,
    pub history: LeducHistory,
}

/// Leduc 完整状态（API-302）。
///
/// `cards[i] == 0xFF` 表示 P_i 私有牌未发；`public_card == None` 在 postflop 之前
/// （preflop 整段都 None）；`terminal_payoffs = Some([p0_net, p1_net])` 当且仅当
/// terminal。
#[derive(Clone, Debug)]
pub struct LeducState {
    pub cards: [u8; 2],
    pub public_card: Option<u8>,
    pub street: LeducStreet,
    pub history: LeducHistory,
    pub committed: [u32; 2],
    pub terminal_payoffs: Option<[f64; 2]>,
}

/// Card index → rank (11/12/13)。
fn rank_of(card: u8) -> u8 {
    11 + card / 2
}

/// 当前街最多 raise 数（D-311：每 round 最多 2 voluntary raise，含 initial bet）。
const MAX_RAISES_PER_ROUND: usize = 2;

/// Preflop bet size。
const PREFLOP_BET: u32 = 2;
/// Postflop bet size。
const POSTFLOP_BET: u32 = 4;

impl LeducState {
    fn deal_phase(&self) -> Option<DealPhase> {
        if self.cards[0] == 0xFF {
            Some(DealPhase::Private(0))
        } else if self.cards[1] == 0xFF {
            Some(DealPhase::Private(1))
        } else if self.street == LeducStreet::Postflop && self.public_card.is_none() {
            Some(DealPhase::Public)
        } else {
            None
        }
    }

    /// 当前街中 voluntary raise 数（含 initial Bet）。
    fn raises_in_round(&self) -> usize {
        self.history
            .iter()
            .filter(|a| matches!(a, LeducAction::Bet | LeducAction::Raise))
            .count()
    }

    /// 当前街是否有 outstanding bet 等 call/fold/raise 响应。
    fn has_outstanding_bet(&self) -> bool {
        self.history
            .iter()
            .rev()
            .find_map(|a| match a {
                LeducAction::Bet | LeducAction::Raise => Some(true),
                LeducAction::Call | LeducAction::Check | LeducAction::Fold => Some(false),
                _ => None,
            })
            .unwrap_or(false)
    }

    /// 当前街中 voluntary action 数（用于判断 actor）。
    fn actions_in_round(&self) -> usize {
        self.history
            .iter()
            .filter(|a| {
                matches!(
                    a,
                    LeducAction::Check
                        | LeducAction::Bet
                        | LeducAction::Call
                        | LeducAction::Fold
                        | LeducAction::Raise
                )
            })
            .count()
    }

    /// 当前 actor：preflop / postflop 起手都是 P0；之后 alternate。
    fn current_actor(&self) -> PlayerId {
        (self.actions_in_round() % 2) as u8
    }

    /// 本街的 bet size（D-311 preflop=2, postflop=4）。
    fn bet_size(&self) -> u32 {
        match self.street {
            LeducStreet::Preflop => PREFLOP_BET,
            LeducStreet::Postflop => POSTFLOP_BET,
        }
    }

    /// 判断当前 round 是否结束（双方对齐：双 check / 有 bet 后 call / fold）。
    /// 返回 (round_closed, terminal_fold)：terminal_fold 表示该 round 以 fold
    /// 终止整手牌。
    fn round_status(&self) -> RoundStatus {
        let n = self.history.len();
        if n == 0 {
            return RoundStatus::Continue;
        }
        let last = self.history[n - 1];
        match last {
            LeducAction::Fold => RoundStatus::Fold,
            LeducAction::Check => {
                // 双 check（n == 2 全 Check）→ round closes。
                if n >= 2 && self.history[n - 2] == LeducAction::Check {
                    RoundStatus::Closed
                } else {
                    RoundStatus::Continue
                }
            }
            LeducAction::Call => RoundStatus::Closed,
            LeducAction::Bet | LeducAction::Raise => RoundStatus::Continue,
            _ => RoundStatus::Continue,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DealPhase {
    Private(usize),
    Public,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoundStatus {
    Continue,
    Closed,
    Fold,
}

impl Game for LeducGame {
    type State = LeducState;
    type Action = LeducAction;
    type InfoSet = LeducInfoSet;

    const VARIANT: crate::error::GameVariant = crate::error::GameVariant::Leduc;

    fn n_players(&self) -> usize {
        2
    }

    fn root(&self, _rng: &mut dyn RngSource) -> LeducState {
        LeducState {
            cards: [0xFF, 0xFF],
            public_card: None,
            street: LeducStreet::Preflop,
            history: Vec::new(),
            committed: [1, 1],
            terminal_payoffs: None,
        }
    }

    fn current(state: &LeducState) -> NodeKind {
        if state.terminal_payoffs.is_some() {
            return NodeKind::Terminal;
        }
        if state.deal_phase().is_some() {
            return NodeKind::Chance;
        }
        NodeKind::Player(state.current_actor())
    }

    fn info_set(state: &LeducState, actor: PlayerId) -> LeducInfoSet {
        let private = state.cards[actor as usize];
        assert!(private != 0xFF, "info_set called before private deal");
        LeducInfoSet {
            actor,
            private_card: private,
            public_card: state.public_card,
            street: state.street,
            history: state.history.clone(),
        }
    }

    fn legal_actions(state: &LeducState) -> Vec<LeducAction> {
        if state.has_outstanding_bet() {
            // {Fold, Call} always; Raise if raises < 2
            let mut out = vec![LeducAction::Fold, LeducAction::Call];
            if state.raises_in_round() < MAX_RAISES_PER_ROUND {
                out.push(LeducAction::Raise);
            }
            out
        } else {
            // no bet pending → {Check, Bet}
            vec![LeducAction::Check, LeducAction::Bet]
        }
    }

    fn next(mut state: LeducState, action: LeducAction, _rng: &mut dyn RngSource) -> LeducState {
        if let Some(phase) = state.deal_phase() {
            let card = action.to_card().unwrap_or_else(|| {
                panic!("LeducGame::next at chance node expects DealN, got {action:?}")
            });
            match phase {
                DealPhase::Private(idx) => {
                    state.cards[idx] = card;
                }
                DealPhase::Public => {
                    state.public_card = Some(card);
                }
            }
            return state;
        }

        // Decision node：append action，更新 committed，按 round status 派发。
        let actor = state.current_actor() as usize;
        let bet_size = state.bet_size();
        match action {
            LeducAction::Check => {}
            LeducAction::Fold => {}
            LeducAction::Bet => {
                state.committed[actor] += bet_size;
            }
            LeducAction::Call => {
                // 与对手 committed 对齐
                let opp = 1 - actor;
                state.committed[actor] = state.committed[opp];
            }
            LeducAction::Raise => {
                // Raise 把对手 committed 抬高再加一个 bet_size
                let opp = 1 - actor;
                state.committed[actor] = state.committed[opp] + bet_size;
            }
            _ => panic!("LeducGame::next decision node 不接受 deal action {action:?}"),
        }
        state.history.push(action);

        match state.round_status() {
            RoundStatus::Fold => {
                // 当前 actor fold → opponent 赢 folder 已 invest 的部分
                let folder = actor;
                let winner = 1 - folder;
                let mut payoffs = [0.0_f64; 2];
                payoffs[winner] = state.committed[folder] as f64;
                payoffs[folder] = -(state.committed[folder] as f64);
                state.terminal_payoffs = Some(payoffs);
            }
            RoundStatus::Closed => match state.street {
                LeducStreet::Preflop => {
                    // 进入 postflop：清 history，准备 public deal chance
                    state.street = LeducStreet::Postflop;
                    state.history.clear();
                }
                LeducStreet::Postflop => {
                    // showdown
                    state.terminal_payoffs = Some(showdown_payoffs(&state));
                }
            },
            RoundStatus::Continue => {}
        }

        state
    }

    fn chance_distribution(state: &LeducState) -> Vec<(LeducAction, f64)> {
        let phase = state
            .deal_phase()
            .expect("LeducGame::chance_distribution called on non-chance state");
        let used: Vec<u8> = match phase {
            DealPhase::Private(0) => Vec::new(),
            DealPhase::Private(1) => vec![state.cards[0]],
            DealPhase::Public => vec![state.cards[0], state.cards[1]],
            _ => Vec::new(),
        };
        let remaining: Vec<u8> = (0u8..6).filter(|c| !used.contains(c)).collect();
        let n = remaining.len() as f64;
        let prob = 1.0 / n;
        remaining
            .into_iter()
            .map(|c| (LeducAction::from_card(c), prob))
            .collect()
    }

    fn payoff(state: &LeducState, player: PlayerId) -> f64 {
        let payoffs = state
            .terminal_payoffs
            .expect("LeducGame::payoff called on non-terminal state");
        payoffs[player as usize]
    }
}

/// Postflop showdown：pair > rank > tie。
fn showdown_payoffs(state: &LeducState) -> [f64; 2] {
    let public = state.public_card.expect("showdown without public card");
    let p0_rank = rank_of(state.cards[0]);
    let p1_rank = rank_of(state.cards[1]);
    let public_rank = rank_of(public);

    let p0_pair = p0_rank == public_rank;
    let p1_pair = p1_rank == public_rank;

    let winner: Option<usize> = if p0_pair && !p1_pair {
        Some(0)
    } else if p1_pair && !p0_pair {
        Some(1)
    } else if p0_rank > p1_rank {
        Some(0)
    } else if p1_rank > p0_rank {
        Some(1)
    } else {
        None
    };

    match winner {
        Some(w) => {
            let l = 1 - w;
            let mut out = [0.0_f64; 2];
            out[w] = state.committed[l] as f64;
            out[l] = -(state.committed[l] as f64);
            out
        }
        None => [0.0, 0.0],
    }
}

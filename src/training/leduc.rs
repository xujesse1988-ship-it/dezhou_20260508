//! LeducGame（API-302 / D-311）。
//!
//! 标准 Leduc Poker：6 张牌 deck `{J♠, J♥, Q♠, Q♥, K♠, K♥}`（rank 11/12/13 ×
//! suit 0/1）/ 2 player / 每人发 1 张私有牌（preflop）+ 1 张公共牌（flop 后翻
//! 开）/ 各 ante `1` chip / 2 round betting / 每 round 最多 `2` voluntary raise /
//! preflop bet size = `2` chip、postflop bet size = `4` chip / showdown：先比
//! pair（私有 rank == 公共 rank → 自动赢）再比 rank。
//!
//! InfoSet 数估算：preflop 6 × |histories_preflop| + postflop 6 × 6 ×
//! |histories_postflop|，~`288` InfoSet（具体由 B2 \[实现\] 枚举确认）。
//!
//! A1 \[实现\] 阶段所有方法体 `unimplemented!()`；B2 \[实现\] 落地全部规则
//! transitions + exploitability `< 0.1` 收敛验收。

use crate::core::rng::RngSource;
use crate::training::game::{Game, NodeKind, PlayerId};

/// Zero-sized game token（API-302）。
#[derive(Clone, Copy, Debug, Default)]
pub struct LeducGame;

/// Leduc action 枚举（API-302 / D-311）。
///
/// 顺序与 [`Game::legal_actions`] 返回 `Vec<LeducAction>` 索引一一对应。
#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq)]
pub enum LeducAction {
    Check,
    Bet,
    Call,
    Fold,
    Raise,
}

/// Leduc 街阶段（API-302 / D-311）。
#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq)]
pub enum LeducStreet {
    Preflop,
    Postflop,
}

/// Leduc 公开历史（API-302）。
///
/// 编码每条街的 voluntary action 序列；每条街最多 2 raise（D-311）。A1 \[实现\]
/// 阶段使用 [`Vec<LeducAction>`] 作为后端（D-373 锁定 stage 3 新增 3 个 crate：
/// bincode + tempfile + thread-safety TBD；API-302 字面 `smallvec::SmallVec<[_; 8]>`
/// 的 SmallVec 优化路径留到 E2 hot path opt 与 D-373-revM 评估时引入，避免在
/// scaffold 阶段额外引入 `smallvec` crate 越过 D-373 锁定的 3 个新增依赖上限）。
///
/// 数据语义在 A1 阶段不变；公开类型别名 `LeducHistory` 不影响 [`Game::InfoSet`]
/// trait bound 满足（`Vec<T: Clone + Hash + Eq>` 与 `SmallVec<T>` 在 Send / Sync /
/// Clone / Hash / Eq / Debug 上等价）。
pub type LeducHistory = Vec<LeducAction>;

/// Leduc 玩家视角 InfoSet（API-302）。
///
/// `private_card ∈ {0..=5}` 编码 `{J♠, J♥, Q♠, Q♥, K♠, K♥}`；`public_card`
/// preflop `None`、postflop `Some(0..=5)`。
#[derive(Clone, Eq, Hash, Debug, PartialEq)]
pub struct LeducInfoSet {
    pub actor: PlayerId,
    pub private_card: u8,
    pub public_card: Option<u8>,
    pub street: LeducStreet,
    pub history: LeducHistory,
}

/// Leduc 完整状态（API-302）。
#[derive(Clone, Debug)]
pub struct LeducState {
    pub cards: [u8; 2],
    pub public_card: Option<u8>,
    pub street: LeducStreet,
    pub history: LeducHistory,
    pub committed: [u32; 2],
    pub terminal_payoffs: Option<[f64; 2]>,
}

impl Game for LeducGame {
    type State = LeducState;
    type Action = LeducAction;
    type InfoSet = LeducInfoSet;

    fn n_players(&self) -> usize {
        unimplemented!("stage 3 A1 scaffold: LeducGame::n_players (B2 实现)")
    }

    fn root(&self, _rng: &mut dyn RngSource) -> LeducState {
        unimplemented!("stage 3 A1 scaffold: LeducGame::root (B2 实现)")
    }

    fn current(_state: &LeducState) -> NodeKind {
        unimplemented!("stage 3 A1 scaffold: LeducGame::current (B2 实现)")
    }

    fn info_set(_state: &LeducState, _actor: PlayerId) -> LeducInfoSet {
        unimplemented!("stage 3 A1 scaffold: LeducGame::info_set (B2 实现)")
    }

    fn legal_actions(_state: &LeducState) -> Vec<LeducAction> {
        unimplemented!("stage 3 A1 scaffold: LeducGame::legal_actions (B2 实现)")
    }

    fn next(_state: LeducState, _action: LeducAction, _rng: &mut dyn RngSource) -> LeducState {
        unimplemented!("stage 3 A1 scaffold: LeducGame::next (B2 实现)")
    }

    fn chance_distribution(_state: &LeducState) -> Vec<(LeducAction, f64)> {
        unimplemented!("stage 3 A1 scaffold: LeducGame::chance_distribution (B2 实现)")
    }

    fn payoff(_state: &LeducState, _player: PlayerId) -> f64 {
        unimplemented!("stage 3 A1 scaffold: LeducGame::payoff (B2 实现)")
    }
}

//! `BestResponse` trait + `KuhnBestResponse` + `LeducBestResponse` + `exploitability`
//! 辅助函数（API-340..API-343 / D-340 / D-341 / D-344）。
//!
//! BestResponse 通过 full-tree backward induction 计算：给定对手策略 `σ_opp`，求
//! `target_player` 视角下最大化 EV 的 one-hot 策略（D-344 输出 `(strategy,
//! value)`）。
//!
//! Kuhn 12 InfoSet × 4 action ≤ 50 node，计算瞬时；Leduc ~288 InfoSet 多项式
//! 复杂度，计算 < 1s release（D-348 SLO）；简化 NLHE BestResponse 不纳入 stage 3
//! （D-346 LBR 同型留 stage 4）。
//!
//! `exploitability` = `(BR_0(σ_1) + BR_1(σ_0)) / 2`（D-340 / D-341），单位
//! chip/game 与 path.md §阶段 3 字面对齐（D-316）。
//!
//! A1 \[实现\] 阶段所有方法体 `unimplemented!()`；B2 \[实现\] 落地。

use std::collections::HashMap;

use crate::training::game::{Game, PlayerId};
use crate::training::kuhn::{KuhnGame, KuhnInfoSet};
use crate::training::leduc::{LeducGame, LeducInfoSet};

/// Best response 计算 trait（API-340 / D-344）。
///
/// 输出 `(one_hot_strategy, br_value)`：
/// - `one_hot_strategy: HashMap<G::InfoSet, Vec<f64>>` 每条 InfoSet 上 one-hot
///   分布（最大 EV action 概率 = 1.0、其他 = 0.0；多 action tie 由实现选择
///   determinism 路径）
/// - `br_value: f64` 对手按 `opponent_strategy` 行动时 target_player 视角下的
///   博弈树 EV（chip/game 单位，与 D-316 / path.md §阶段 3 字面对齐）
pub trait BestResponse<G: Game> {
    /// 计算 best response 输出。
    ///
    /// `opponent_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>` 让 caller 注入
    /// 任意来源的对手策略（典型来自 [`crate::training::Trainer::average_strategy`]）。
    /// `target_player ∈ {0, 1}` 标识 BR 求解的视角。
    fn compute(
        game: &G,
        opponent_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
        target_player: PlayerId,
    ) -> (HashMap<G::InfoSet, Vec<f64>>, f64);
}

/// Kuhn full-tree backward induction BR（API-341 / D-340）。
///
/// Zero-sized；不持有 state。Kuhn 12 InfoSet × 4 action 全枚举可瞬时完成
/// （D-348 SLO `< 100 ms` release）。
#[derive(Clone, Copy, Debug, Default)]
pub struct KuhnBestResponse;

impl BestResponse<KuhnGame> for KuhnBestResponse {
    fn compute(
        _game: &KuhnGame,
        _opponent_strategy: &dyn Fn(&KuhnInfoSet, usize) -> Vec<f64>,
        _target_player: PlayerId,
    ) -> (HashMap<KuhnInfoSet, Vec<f64>>, f64) {
        unimplemented!("stage 3 A1 scaffold: KuhnBestResponse::compute (B2 实现)")
    }
}

/// Leduc full-tree backward induction BR（API-342 / D-341）。
///
/// 与 [`KuhnBestResponse`] 同算法（backward induction，按 InfoSet 反向递归），
/// 多项式复杂度 in InfoSet count（~288 / D-311 估算）。`< 1 s` release SLO（D-348）。
#[derive(Clone, Copy, Debug, Default)]
pub struct LeducBestResponse;

impl BestResponse<LeducGame> for LeducBestResponse {
    fn compute(
        _game: &LeducGame,
        _opponent_strategy: &dyn Fn(&LeducInfoSet, usize) -> Vec<f64>,
        _target_player: PlayerId,
    ) -> (HashMap<LeducInfoSet, Vec<f64>>, f64) {
        unimplemented!("stage 3 A1 scaffold: LeducBestResponse::compute (B2 实现)")
    }
}

/// 计算 game 上的 exploitability（API-343 / D-340 / D-341）。
///
/// `exploitability(game, σ) = (BR_0(σ_1) + BR_1(σ_0)) / 2`，单位 chip/game
/// （D-316 与 path.md §阶段 3 字面对齐）。Kuhn 收敛阈值 `< 0.01`、Leduc `< 0.1`，
/// B2 \[实现\] 由 `tests/cfr_kuhn.rs::kuhn_vanilla_cfr_10k_iter_exploitability_less_than_0_01`
/// 与 `tests/cfr_leduc.rs::leduc_vanilla_cfr_10k_iter_exploitability_less_than_0_1` 钉死。
pub fn exploitability<G, BR>(_game: &G, _strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>) -> f64
where
    G: Game,
    BR: BestResponse<G>,
{
    unimplemented!("stage 3 A1 scaffold: exploitability (B2 实现)")
}

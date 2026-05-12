//! SimplifiedNlheGame（API-303 / D-313）+ stage 1 / stage 2 桥接（API-390..API-392）。
//!
//! 简化 NLHE 范围：2-player + 100 BB starting stack + 盲注 0.5/1.0 BB + 完整 4 街 +
//! stage 2 `DefaultActionAbstraction`（5-action）+ stage 2 `PreflopLossless169` +
//! `PostflopBucketAbstraction`（500/500/500 bucket）。复用 stage 1
//! [`crate::GameState`] + stage 2 [`crate::ActionAbstraction`] /
//! [`crate::InfoAbstraction`] / [`crate::BucketTable`]，仅在
//! `SimplifiedNlheGame` 适配层把 stage 1 `GameState` 包装成 [`Game`] trait state。
//!
//! A1 \[实现\] 阶段所有方法体 `unimplemented!()`；C2 \[实现\] 落地全部桥接 +
//! ES-MCCFR 单线程 ≥ 10K update/s SLO。
//!
//! `SimplifiedNlheGame::new` D-314 bucket table 依赖 deferred 到 C1 \[测试\] 起草
//! 前由 D-314-rev1（v2 528 MB）或 D-314-rev2（v1 95 KB fallback）lock；A1 阶段
//! 仅锁定签名 `Result<Self, TrainerError>`。

use std::sync::Arc;

use crate::abstraction::action::AbstractAction;
use crate::abstraction::bucket_table::BucketTable;
use crate::abstraction::info::InfoSetId;
use crate::core::rng::RngSource;
use crate::error::TrainerError;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};

/// 简化 NLHE action 桥接（API-303 / D-318）。
///
/// 直接走 stage 2 `AbstractAction`（5-action 顺序由 D-209 deterministic）；不再
/// 二次抽象。`Game::Action` trait bound `Copy + Eq + Debug` 由 stage 2 实现满足。
pub type SimplifiedNlheAction = AbstractAction;

/// 简化 NLHE InfoSet 桥接（API-303 / D-317）。
///
/// 直接走 stage 2 64-bit `InfoSetId`（D-215 layout）。preflop 走
/// `PreflopLossless169::map`、postflop 走 `PostflopBucketAbstraction::map`
/// （API-391 桥接逻辑由 C2 \[实现\] 落地）。
pub type SimplifiedNlheInfoSet = InfoSetId;

/// 简化 NLHE Game token（API-303）。
///
/// 构造时载入 stage 2 `BucketTable`（D-314 deferred lock）+ stage 1 `TableConfig`
/// （2-player + 100 BB starting stack）。字段 `pub(crate)` 让同 crate 测试 / bench
/// 访问内部状态而不暴露给外部消费者（D-376）。
#[allow(dead_code)] // C2 \[实现\] 落地 SimplifiedNlheGame::new + Game::root 后字段会被读取
pub struct SimplifiedNlheGame {
    pub(crate) bucket_table: Arc<BucketTable>,
    pub(crate) config: TableConfig,
}

impl SimplifiedNlheGame {
    /// 构造函数（API-303）。
    ///
    /// 校验项（C2 \[实现\] 落地）：
    /// - `BucketTable::schema_version()` ∈ `{1, 2}`（D-314-rev1 v2 / D-314-rev2 v1）
    /// - `BucketTable::config()` == `BucketConfig::default_500_500_500()`
    /// - `TableConfig::default_6max_100bb()` 之 2-player + 100 BB 子集（C2 \[实现\]
    ///   起步前由 D-313-revM 锁定具体 TableConfig 构造路径）
    ///
    /// 失败路径：[`TrainerError::UnsupportedBucketTable`]。
    pub fn new(_bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        unimplemented!("stage 3 A1 scaffold: SimplifiedNlheGame::new (C2 实现)")
    }
}

/// 简化 NLHE 完整状态（API-303）。
///
/// `game_state` wrap stage 1 [`GameState`]（API-390 桥接）；`action_history`
/// 累积 stage 2 [`AbstractAction`]（API-392 桥接）。
#[derive(Clone, Debug)]
pub struct SimplifiedNlheState {
    pub game_state: GameState,
    pub action_history: Vec<SimplifiedNlheAction>,
}

impl Game for SimplifiedNlheGame {
    type State = SimplifiedNlheState;
    type Action = SimplifiedNlheAction;
    type InfoSet = SimplifiedNlheInfoSet;

    fn n_players(&self) -> usize {
        unimplemented!("stage 3 A1 scaffold: SimplifiedNlheGame::n_players (C2 实现)")
    }

    fn root(&self, _rng: &mut dyn RngSource) -> SimplifiedNlheState {
        unimplemented!("stage 3 A1 scaffold: SimplifiedNlheGame::root (C2 实现)")
    }

    fn current(_state: &SimplifiedNlheState) -> NodeKind {
        unimplemented!("stage 3 A1 scaffold: SimplifiedNlheGame::current (C2 实现)")
    }

    fn info_set(_state: &SimplifiedNlheState, _actor: PlayerId) -> SimplifiedNlheInfoSet {
        // API-391 桥接：preflop 走 PreflopLossless169::map / postflop 走
        // PostflopBucketAbstraction::map（依赖 self.bucket_table，C2 \[实现\] 起步
        // 前可能由 D-312-revM / API-300-revM 评估是否调整 trait method receiver
        // 从 `state: &Self::State` 改为 `&self, state: &Self::State` 让简化 NLHE
        // 桥接路径可访问 `self.bucket_table`；A1 阶段仅锁定签名 `unimplemented!()`）。
        unimplemented!("stage 3 A1 scaffold: SimplifiedNlheGame::info_set (C2 实现)")
    }

    fn legal_actions(_state: &SimplifiedNlheState) -> Vec<SimplifiedNlheAction> {
        // API-392 桥接：DefaultActionAbstraction::abstract_actions（C2 \[实现\] 落地）。
        unimplemented!("stage 3 A1 scaffold: SimplifiedNlheGame::legal_actions (C2 实现)")
    }

    fn next(
        _state: SimplifiedNlheState,
        _action: SimplifiedNlheAction,
        _rng: &mut dyn RngSource,
    ) -> SimplifiedNlheState {
        // API-390 桥接：AbstractAction::to_concrete + GameState::apply（C2 \[实现\] 落地）。
        unimplemented!("stage 3 A1 scaffold: SimplifiedNlheGame::next (C2 实现)")
    }

    fn chance_distribution(_state: &SimplifiedNlheState) -> Vec<(SimplifiedNlheAction, f64)> {
        // 简化 NLHE chance node 主要来自 deal hole / deal board；C2 \[实现\] 落地。
        unimplemented!("stage 3 A1 scaffold: SimplifiedNlheGame::chance_distribution (C2 实现)")
    }

    fn payoff(_state: &SimplifiedNlheState, _player: PlayerId) -> f64 {
        // GameState::payouts → f64（D-316 chip 净收益直接当 utility）。
        unimplemented!("stage 3 A1 scaffold: SimplifiedNlheGame::payoff (C2 实现)")
    }
}

//! `Game` trait（API-300 / D-312）+ `NodeKind` / `PlayerId` 辅助类型。
//!
//! 通用游戏抽象，让 `Trainer<G: Game>` 在 Kuhn / Leduc / 简化 NLHE 上同型工作。
//! 关联类型 `State` / `Action` / `InfoSet` 由具体 game impl 锁定（Kuhn / Leduc 走
//! 独立 InfoSet 编码，简化 NLHE 继承 stage 2 `InfoSetId`；详见 D-317）。
//!
//! A1 \[实现\] 阶段所有具体 impl 方法体 `unimplemented!()`；B2 \[实现\] 落地
//! KuhnGame / LeducGame 全部方法，C2 \[实现\] 落地 SimplifiedNlheGame。

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::core::rng::RngSource;
use crate::error::GameVariant;

/// 0-indexed 玩家 id；2-player game `player ∈ {0, 1}`（API-300）。
pub type PlayerId = u8;

/// 当前节点角色（API-300）。Chance node 由 chance 分布采样转移；Player node 由
/// `Trainer` 决策；Terminal node 终止递归并计算 payoff。
#[derive(Clone, Copy, Eq, PartialEq, Debug, Hash)]
pub enum NodeKind {
    Chance,
    Player(PlayerId),
    Terminal,
}

/// 通用游戏 trait（API-300 / D-312）。
///
/// 不变量（API-300 invariants）：
/// - `n_players(&self) >= 2`（CFR 在 1-player 退化为单 player MDP，不属于 stage 3
///   范围）。
/// - `chance_distribution(state)` 的所有概率严格 `> 0.0`（零概率 outcome 应从分布
///   中剔除）；Σ probability = `1.0 ± 1e-12`。
/// - `info_set(state, actor)` 必须是 actor 视角下信息的**完整 hash**：相同 actor
///   在不同 hidden state（如对手手牌）下可能产生不同 InfoSet id，但对 actor 可
///   观察的所有信息必须确定性。
/// - `legal_actions(state)` 顺序必须**确定性**且与 [`crate::training::RegretTable`]
///   `Vec<f64>` 索引一一对应（D-324 action_count 训练全程恒定）。
/// - `next(state, action, rng)` 在 decision node 上**不消费** RNG（pure transition）；
///   仅 chance node 消费 RNG（D-308 sample-1 路径）。
pub trait Game {
    /// 完整游戏状态（含 chance + decision history）。`Clone + Send + Sync` 让
    /// 多线程 ES-MCCFR（D-321）按 owned clone 模式（D-319）传递 state。
    type State: Clone + Send + Sync;

    /// game-specific action：
    /// - Kuhn: `{ Check, Bet, Call, Fold }`
    /// - Leduc: `{ Check, Bet, Call, Fold, Raise }`
    /// - 简化 NLHE: stage 2 `AbstractAction`（D-318 不二次抽象）
    ///
    /// `Copy + Eq + Debug` 保证 RegretTable HashMap 索引 + 错误信息 Debug 格式化。
    type Action: Clone + Copy + Send + Sync + Eq + std::fmt::Debug;

    /// game-specific InfoSet id（D-317）：
    /// - Kuhn / Leduc: stage 3 独立编码（[`crate::training::KuhnInfoSet`] /
    ///   [`crate::training::LeducInfoSet`]）
    /// - 简化 NLHE: 继承 stage 2 [`crate::InfoSetId`]（64-bit layout）
    ///
    /// `Eq + Hash + Clone + Debug` 是 [`crate::training::RegretTable`] HashMap 键
    /// 的必要 bound；`Serialize + DeserializeOwned` 让 D-327 checkpoint bincode 序列化
    /// 走 derive 入口（D2 \[实现\]）。
    type InfoSet: Clone
        + Send
        + Sync
        + Eq
        + std::hash::Hash
        + std::fmt::Debug
        + Serialize
        + DeserializeOwned;

    /// 游戏变体 tag（D-356 多 game checkpoint 不兼容拒绝 / D-350 binary header
    /// offset 13）。`VanillaCfrTrainer<G>` / `EsMccfrTrainer<G>` 在
    /// [`crate::training::Trainer::save_checkpoint`] 里读取本常量回填到 header
    /// `game_variant` 字段；`load_checkpoint` 反向校验。
    const VARIANT: GameVariant;

    /// 当前 Game 配套 bucket_table 内容 BLAKE3 hash（D-350 binary header offset 60
    /// / D-356 BucketTableMismatch 校验）。Kuhn / Leduc 全零；SimplifiedNlhe 返回
    /// [`crate::BucketTable::content_hash`]。
    fn bucket_table_blake3(&self) -> [u8; 32] {
        [0u8; 32]
    }

    /// 玩家数（Kuhn / Leduc / 简化 NLHE 全部 = 2）。
    fn n_players(&self) -> usize;

    /// 初始状态（含 deal chance node 已完成；后续 chance node 在 `next` 内部触发）。
    fn root(&self, rng: &mut dyn RngSource) -> Self::State;

    /// 当前节点角色：[`NodeKind::Chance`] / [`NodeKind::Player`] / [`NodeKind::Terminal`]。
    fn current(state: &Self::State) -> NodeKind;

    /// 当前 InfoSet（actor 视角，含 actor 私有信息 + 公开历史）。
    ///
    /// 仅当 `current(state) == Player(_)` 时有意义；Chance / Terminal 调用 panic。
    fn info_set(state: &Self::State, actor: PlayerId) -> Self::InfoSet;

    /// 当前节点合法 action 列表（D-318）。
    /// - Kuhn / Leduc：直接返回 game-specific 枚举
    /// - 简化 NLHE：走 stage 2 `DefaultActionAbstraction::abstract_actions`
    fn legal_actions(state: &Self::State) -> Vec<Self::Action>;

    /// 执行 action 转移状态；chance node 走 `chance_distribution + rng` 采样
    /// （D-336 自实现 binary search 累积分布），decision node 直接 apply。
    fn next(state: Self::State, action: Self::Action, rng: &mut dyn RngSource) -> Self::State;

    /// chance node 上的离散分布（仅 chance node 调用，Player / Terminal panic）。
    ///
    /// 返回 `(action, probability)` 二元对；Σ probability = `1.0 ± 1e-12`。零概率
    /// outcome 应从分布中剔除而非保留为 0（API-300 invariant）。
    fn chance_distribution(state: &Self::State) -> Vec<(Self::Action, f64)>;

    /// terminal payoff（D-316 chip 净收益直接当 utility）。
    ///
    /// 仅 `current(state) == Terminal` 时有意义；Chance / Player 调用 panic。
    /// 严格零和约束（D-332）：`payoff(state, 0) + payoff(state, 1) = 0`，容差 `1e-6`。
    fn payoff(state: &Self::State, player: PlayerId) -> f64;
}

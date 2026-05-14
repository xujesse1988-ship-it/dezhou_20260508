//! `NlheGame6` 6-player NLHE Game trait impl（API-410..API-413 / D-410..D-419）。
//!
//! stage 3 [`Game`] trait 第 4 个 impl（继承 KuhnGame / LeducGame /
//! SimplifiedNlheGame）；走 stage 1 [`GameState`] n_seats=6 默认 multi-seat 分支，
//! 配 stage 2 `BucketTable` v3 production artifact (BLAKE3 = `67ee5554...`) 与
//! stage 4 [`PluribusActionAbstraction`] 14-action（D-420 字面）。
//!
//! **A1 \[实现\] 状态**：[`NlheGame6`] struct 签名锁；[`Game`] trait 8 method
//! 全 `unimplemented!()` 占位（含 [`Game::VARIANT`] / [`Game::n_players`] 等
//! const / 简单 getter，统一占位让 B1 \[测试\] 起步时全套 panic-fail 形态，
//! C2 \[实现\] 落地翻面）。
//!
//! **6-traverser routing**（D-412 / D-414）：[`NlheGame6::traverser_at_iter`]
//! 与 [`NlheGame6::traverser_for_thread`] 是无状态 helper（pure function of
//! iter index 与 tid），不依赖 self；C2 \[实现\] 起步前作为 6 套独立 RegretTable
//! 数组的 routing index 来源。
//!
//! **HU 退化路径**（D-416）：[`NlheGame6::new_hu`] 配 `n_seats=2`，对应 stage 3
//! [`crate::SimplifiedNlheGame`] 的 1M update × 3 BLAKE3 anchor 在 stage 4
//! commit 上字面 byte-equal 维持（C1 \[测试\] 钉死）。
//!
//! **D-424 lock**：bucket_table 必须是 stage 2 §G-batch1 §3.10 v3 production
//! artifact (schema_version=2 + 500/500/500 + BLAKE3 = `67ee5554...`)；A1 \[实现\]
//! 占位 [`NlheGame6::new`] 走 `unimplemented!()`，C2 \[实现\] 落地走与 stage 3
//! [`crate::SimplifiedNlheGame::new`] 同型校验路径。

use std::sync::Arc;

use crate::abstraction::action_pluribus::{PluribusAction, PluribusActionAbstraction};
use crate::abstraction::bucket_table::BucketTable;
use crate::abstraction::info::InfoSetId;
use crate::core::rng::RngSource;
use crate::core::SeatId;
use crate::error::TrainerError;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};

/// stage 4 [`NlheGame6`] action 类型（API-410）= [`PluribusAction`] 14-variant enum。
pub type NlheGame6Action = PluribusAction;

/// stage 4 [`NlheGame6`] InfoSet 类型（API-410 / D-423）= stage 2 64-bit
/// [`InfoSetId`] + D-423 14-action mask（[`InfoSetId::with_14action_mask`]）。
pub type NlheGame6InfoSet = InfoSetId;

/// 6-player NLHE Game token（API-410 / D-410）。
///
/// 持有 stage 2 [`BucketTable`] v3 production artifact (D-424) + stage 4
/// [`PluribusActionAbstraction`] 14-action（D-420）+ stage 1 [`TableConfig`]
/// （n_seats=6 默认 / 100 BB starting stack）。字段 `pub(crate)` 让同 crate
/// 测试 / bench 访问内部状态（继承 stage 3 [`crate::SimplifiedNlheGame`]
/// 同型政策 D-376）。
#[allow(dead_code)]
pub struct NlheGame6 {
    pub(crate) bucket_table: Arc<BucketTable>,
    pub(crate) action_abstraction: PluribusActionAbstraction,
    pub(crate) config: TableConfig,
}

impl NlheGame6 {
    /// stage 4 D-410 / D-424 默认构造（n_seats=6 + 100 BB + v3 production
    /// bucket table）。
    ///
    /// 校验项（D-424）：
    /// - `BucketTable::schema_version() == 2`
    /// - `BucketTable::config() == BucketConfig::new(500, 500, 500)`
    /// - `bucket_table_blake3 == expected v3 anchor` (`67ee5554...`)
    ///
    /// 失败路径：[`TrainerError::UnsupportedBucketTable`]（沿用 stage 3
    /// [`crate::SimplifiedNlheGame::new`] 同型 variant）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，C2 \[实现\] 落地走与
    /// stage 3 [`crate::SimplifiedNlheGame::new`] 同型校验路径 + n_seats=6 +
    /// PluribusActionAbstraction 默认。
    pub fn new(bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        let _ = bucket_table;
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::new 落地 C2 [实现] D-424")
    }

    /// stage 4 D-416 — HU 退化路径（n_seats=2）。
    ///
    /// stage 3 [`crate::SimplifiedNlheGame`] 路径上的 1M update × 3 BLAKE3
    /// anchor 在 stage 4 commit 上必须 byte-equal 维持（即 `NlheGame6::new_hu`
    /// 配 `EsMccfrTrainer::new` 跑 1M update × 3 → 同 BLAKE3 与 stage 3
    /// `SimplifiedNlheGame` 配 `EsMccfrTrainer::new` 跑 1M update × 3 byte-equal）。
    /// C1 \[测试\] 钉死该不变量。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，C2 \[实现\] 落地。
    pub fn new_hu(bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        let _ = bucket_table;
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::new_hu 落地 C2 [实现] D-416")
    }

    /// stage 4 D-410 — 通用 config 构造（test fixture / B1 \[测试\] 自定义
    /// n_seats 路径）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，C2 \[实现\] 落地。
    pub fn with_config(
        bucket_table: Arc<BucketTable>,
        config: TableConfig,
    ) -> Result<Self, TrainerError> {
        let _ = (bucket_table, config);
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::with_config 落地 C2 [实现] D-410")
    }

    /// stage 4 D-412 — 6-traverser alternating；返回 iter t 上的 traverser index。
    ///
    /// `t % 6` deterministic；与 [`Self::traverser_for_thread`] 在 `base_update_count
    /// = t` + `tid = 0` 时等价。无状态 pure function（不依赖 `self`，可在
    /// `step` / `step_parallel` 外部静态调用）。
    pub fn traverser_at_iter(t: u64) -> PlayerId {
        (t % 6) as PlayerId
    }

    /// stage 4 D-412 多线程并发 — `base_update_count + tid` routing。
    ///
    /// stage 3 D-321-rev2 rayon par_iter_mut 路径上对每 thread alternating
    /// traverser 的 D-307 字面扩展（n_players=6）。无状态 pure function。
    pub fn traverser_for_thread(base_update_count: u64, tid: usize) -> PlayerId {
        ((base_update_count + tid as u64) % 6) as PlayerId
    }

    /// stage 4 D-413 — actor_at_seat 桥接（trainer 内部 player_index 与 物理
    /// SeatId 解耦）。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，C2 \[实现\] 落地走
    /// stage 1 [`GameState`] 桥接（详 API-492）。
    pub fn actor_at_seat(state: &NlheGame6State, seat_id: SeatId) -> PlayerId {
        let _ = (state, seat_id);
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::actor_at_seat 落地 C2 [实现] D-413")
    }

    /// stage 4 D-423 — 14-action availability mask 计算（与
    /// [`InfoSetId::with_14action_mask`] 配对使用）。
    ///
    /// 走 [`PluribusActionAbstraction`] 输出的 14-action legal subset →
    /// `1 << PluribusAction tag` 累积。`(0..14)` 范围内的 14-bit mask。
    ///
    /// **A1 \[实现\] 状态**：方法体 `unimplemented!()`，C2 \[实现\] 落地。
    pub fn compute_14action_mask(state: &GameState) -> u16 {
        let _ = state;
        unimplemented!(
            "stage 4 A1 [实现] scaffold: NlheGame6::compute_14action_mask 落地 C2 [实现] D-423"
        )
    }
}

/// 6-player NLHE 完整状态（API-410）。
///
/// `game_state` wrap stage 1 [`GameState`] n_seats=6 默认 multi-seat 分支
/// （API-492 桥接）；`action_history` 累积 [`PluribusAction`]（API-410 桥接）；
/// `bucket_table` 字段是 [`NlheGame6`] 内部 bucket_table 的 Arc clone（继承
/// stage 3 `SimplifiedNlheState` 同型政策，让 [`Game::info_set`] 静态方法
/// 在 postflop 路径上能访问 lookup 表）。
#[derive(Clone)]
#[allow(dead_code)]
pub struct NlheGame6State {
    pub game_state: GameState,
    pub action_history: Vec<NlheGame6Action>,
    pub(crate) bucket_table: Arc<BucketTable>,
}

impl std::fmt::Debug for NlheGame6State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 继承 stage 3 [`crate::SimplifiedNlheState`] 同型 Debug 政策：
        // 跳过 `bucket_table` 的 Debug（stage 2 `BucketTable` 未实现 Debug；
        // 528 MiB body 也不适合 debug 打印）。
        f.debug_struct("NlheGame6State")
            .field("game_state", &self.game_state)
            .field("action_history", &self.action_history)
            .field("bucket_table", &"<Arc<BucketTable>>")
            .finish()
    }
}

impl Game for NlheGame6 {
    type State = NlheGame6State;
    type Action = NlheGame6Action;
    type InfoSet = NlheGame6InfoSet;

    /// stage 4 D-411 — `GameVariant::Nlhe6Max` 4th variant lock。
    const VARIANT: crate::error::GameVariant = crate::error::GameVariant::Nlhe6Max;

    fn bucket_table_blake3(&self) -> [u8; 32] {
        self.bucket_table.content_hash()
    }

    fn n_players(&self) -> usize {
        // stage 4 D-410 6-player NLHE 主路径；HU 退化路径走 [`Self::new_hu`]
        // 内部 `n_seats=2` config，但 `n_players()` 仍走 stage 1 `config.n_seats`
        // 实际值（C2 \[实现\] 落地走 self.config.n_seats）。A1 \[实现\] 占位
        // 走 `unimplemented!()`（即使签名能返回 6 也保持占位，让 B1 \[测试\]
        // panic-fail 形态全套统一）。
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::n_players 落地 C2 [实现] D-410")
    }

    fn root(&self, rng: &mut dyn RngSource) -> NlheGame6State {
        let _ = rng;
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::root 落地 C2 [实现] D-410")
    }

    fn current(state: &NlheGame6State) -> NodeKind {
        let _ = state;
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::current 落地 C2 [实现] D-410")
    }

    fn info_set(state: &NlheGame6State, actor: PlayerId) -> NlheGame6InfoSet {
        let _ = (state, actor);
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::info_set 落地 C2 [实现] D-423")
    }

    fn legal_actions(state: &NlheGame6State) -> Vec<NlheGame6Action> {
        let _ = state;
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::legal_actions 落地 C2 [实现] D-420")
    }

    fn next(
        state: NlheGame6State,
        action: NlheGame6Action,
        rng: &mut dyn RngSource,
    ) -> NlheGame6State {
        let _ = (state, action, rng);
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::next 落地 C2 [实现] D-422")
    }

    fn chance_distribution(state: &NlheGame6State) -> Vec<(NlheGame6Action, f64)> {
        let _ = state;
        // 6-player NLHE 继承 stage 3 [`crate::SimplifiedNlheGame`] 同型政策：
        // 没有独立 chance node（stage 1 [`GameState::with_rng`] 在 root 构造
        // 时一次性消费 rng 发底牌 + post blinds + 5 张 runout board）；
        // `current()` 永不返回 [`NodeKind::Chance`]，本方法不应被 ES-MCCFR /
        // Vanilla CFR 触发。C2 \[实现\] 落地走 `panic!()` 拒绝（继承 stage 3
        // [`crate::SimplifiedNlheGame::chance_distribution`] 形态）。
        unimplemented!(
            "stage 4 A1 [实现] scaffold: NlheGame6::chance_distribution 落地 C2 [实现] D-410"
        )
    }

    fn payoff(state: &NlheGame6State, player: PlayerId) -> f64 {
        let _ = (state, player);
        unimplemented!("stage 4 A1 [实现] scaffold: NlheGame6::payoff 落地 C2 [实现] D-410")
    }
}

//! SimplifiedNlheGame（API-303 / D-313）+ stage 1 / stage 2 桥接（API-390..API-392）。
//!
//! 简化 NLHE 范围：2-player + 200 BB starting stack + 盲注 0.5/1.0 BB +
//! 完整 4 街 + stage 2 `DefaultActionAbstraction`（6-action：{0.5p, 1p, 2p} +
//! Fold/Check/Call/AllIn）+ stage 2 `PreflopLossless169` +
//! `PostflopBucketAbstraction`（当前训练入口支持 500/500/500 与 1000/1000/1000
//! postflop bucket 表）。
//! 复用 stage 1 [`crate::GameState`] + stage 2 [`crate::ActionAbstraction`] /
//! [`crate::InfoAbstraction`] / [`crate::BucketTable`]，仅在 `SimplifiedNlheGame`
//! 适配层把 stage 1 `GameState` 包装成 [`Game`] trait state。
//!
//! **chance / decision 分流**（D-308 / D-315）：stage 1 [`GameState::with_rng`] 已
//! 把发底牌 + post blinds 在 root 构造时一次性消费 rng（D-028 deal protocol）；
//! board cards 由 stage 1 `GameState::deal_board_to` 在 betting round 切换内部
//! 自动从 `runout_board` 取出（已在 root 时发完 5 张存于 state）。因此简化 NLHE
//! 在 `Game` trait 视角下**没有独立 chance node**：[`Game::current`] 仅返回
//! [`NodeKind::Player`] 或 [`NodeKind::Terminal`]，[`Game::chance_distribution`]
//! 在调用时 panic（永远不应被 ES-MCCFR / Vanilla CFR 触发）。
//!
//! **D-022b-rev1 桥接**（2026-05-13 stage 1 decisions §修订历史）：n_seats=2
//! 走标准 HU NLHE 语义（button=SB / non-button=BB / postflop OOP=BB 先行）；
//! stage 1 `validate_config` 范围扩展为 `2..=9`；本模块构造 `TableConfig`
//! 时显式 `n_seats=2`。
//!
//! Bucket table 依赖 = production artifact（v4）
//! `artifacts/bucket_table_default_{500,1000}_{500,1000}_{500,1000}_seed_cafebabe_schemav4.bin`
//! （schema_version=4 / feature_set_id=2 / 16-dim hist+OCHS）。v4 = v3 layout +
//! shape-major canonical id 编号（旧 v3 artifact 需重排）。`SimplifiedNlheGame::new`
//! 校验 `schema_version() == 4` + supported postflop bucket config。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::abstraction::action::{AbstractAction, ActionAbstraction, StreetActionAbstraction};
use crate::abstraction::bucket_table::{BucketConfig, BucketTable, BUCKET_TABLE_SCHEMA_VERSION};
use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::abstraction::map::pack_info_set_id;
use crate::abstraction::postflop::canonical_observation_id;
use crate::abstraction::preflop::PreflopLossless169;
use crate::core::rng::RngSource;
use crate::core::SeatId;
use crate::error::TrainerError;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe_betting_tree::{
    AbstractActionTag, BettingAbstractionRules, Child, NodeId, PublicBettingTree,
};

/// 简化 NLHE action 桥接（API-303 / D-318）。
///
/// 直接走 stage 2 `AbstractAction`（6-action 顺序由 D-209 deterministic）；不再
/// 二次抽象。`Game::Action` trait bound `Copy + Eq + Debug` 由 stage 2 实现满足。
pub type SimplifiedNlheAction = AbstractAction;

/// 简化 NLHE InfoSet 桥接（API-303 / D-317）。
///
/// 直接走 stage 2 64-bit `InfoSetId`（D-215 layout）。preflop 走
/// `PreflopLossless169` hand_class 169 等价类；postflop 走 stage 2 `BucketTable`
/// lookup（D-314-rev1 v4 production bucket table）。
pub type SimplifiedNlheInfoSet = InfoSetId;

/// 简化 NLHE supported `BucketConfig`（D-313-rev）：preflop 169 lossless（不走
/// bucket table）+ postflop production bucket 表。500/500/500 是 HU 历史 baseline；
/// 1000/1000/1000 是 HU v4 重排后目标；**200/200/200 是 6-max 生产桶**（Pluribus 同档
/// 200 桶/街；S3 实测 HU 单对手桶可复用进 A3×A4 ≤3-way → 直接用 200 表，见
/// `six_max_nlhe_target.md` S3 决策记录）。
fn is_supported_bucket_config(cfg: BucketConfig) -> bool {
    let cfg_200 = BucketConfig::new(200, 200, 200)
        .expect("BucketConfig::new(200,200,200) within D-214 range");
    let cfg_1000 = BucketConfig::new(1000, 1000, 1000)
        .expect("BucketConfig::new(1000,1000,1000) within D-214 range");
    cfg == BucketConfig::default_500_500_500() || cfg == cfg_200 || cfg == cfg_1000
}

/// 简化 NLHE expected `BucketTable` schema_version。直接锚定
/// [`BUCKET_TABLE_SCHEMA_VERSION`]（当前 v4 = v3 layout + shape-major canonical id
/// 编号）；v1/v2/v3 artifact 不再可加载。
const EXPECTED_BUCKET_SCHEMA_VERSION: u32 = BUCKET_TABLE_SCHEMA_VERSION;

/// 简化 NLHE 生产 action abstraction 的**唯一来源**（D-318 桥接 + 按街扩张前置）。
///
/// `new()` 建 betting tree 与 [`SimplifiedNlheGame::legal_actions`] 运行期都从这里
/// 取——保证 tree 的 `legal_actions` tag 顺序与运行期 `abstract_actions` 输出严格
/// 一致（否则 regret 向量下标与 tree child 下标错位）。
///
/// 当前 = 全街 `{0.5,1,2}`（`StreetActionAbstraction::default_6_action`），与历史
/// `DefaultActionAbstraction::default_6_action()` byte-equal，树仍 240,096 节点。
/// bet-size 扩张（目标 flop `{0.33,0.66,1,2}`、其余 `{0.5,1,2}`）时只改这一处为
/// `StreetActionAbstraction::per_street([...])`，建树与运行期自动同步。
fn nlhe_action_abstraction() -> StreetActionAbstraction {
    StreetActionAbstraction::default_6_action()
}

/// 简化 NLHE `InfoSetId` v2 layout：把 26-bit node_id 写入 `InfoSetId` raw 高位
/// （bits 38..64）。低 38 bit 复用 stage-2 [`pack_info_set_id`] 字段位置以保留
/// `.bucket_id()` / `.street_tag()` 访问语义；其中 position_bucket / stack_bucket
/// / betting_state 在 NLHE codepath 上恒为 0（信息已被 node_id 内化，见
/// `docs/nlhe_infoset_history_investigation.md` 方案 A）。
///
/// stage-2 `InfoAbstraction::map`（preflop.rs / postflop.rs）走 IA-007 reserved bits = 0
/// 的旧路径不受影响——本 v2 packer 仅在 [`SimplifiedNlheGame::info_set`] 内使用。
///
/// `pub(crate)`：[`crate::training::nlhe_dense::NlheDenseIndexer`] 要按同一 shift
/// 从 `InfoSetId` 反解 node_id，pack / unpack 共用同一常量保证不会漂移。
pub(crate) const NLHE_V2_NODE_ID_SHIFT: u32 = 38;
pub(crate) const NLHE_V2_NODE_ID_BITS: u32 = 26;

/// `pub(crate)`：dense indexer 单元测试 + 未来 dense trainer 复用同一 packer 构造
/// `InfoSetId`，避免在测试里重抄一遍 bit 编码（抄错会让测试假绿）。
pub(crate) fn pack_info_set_v2(
    hand_bucket: u32,
    node_id: NodeId,
    street_tag: StreetTag,
) -> InfoSetId {
    debug_assert!(
        node_id < (1u32 << NLHE_V2_NODE_ID_BITS),
        "200BB 默认 6-action 实测节点数 240,096 << 2^26；node_id={node_id} 越界提示树规模超预期"
    );
    let base = pack_info_set_id(
        hand_bucket,
        0, // position_bucket 在 NLHE v2 由 node.player_acting 隐含，置 0
        0, // stack_bucket 在 NLHE v2 起手筹码固定，置 0
        crate::abstraction::info::BettingState::Open, // betting_state 由 node 隐含
        street_tag,
    );
    InfoSetId::from_raw_internal(base.raw() | (u64::from(node_id) << NLHE_V2_NODE_ID_SHIFT))
}

/// 简化 NLHE Game token（API-303）。
///
/// 构造时载入 stage 2 `BucketTable`（D-314-rev1 v3 artifact）+ stage 1
/// `TableConfig`（2-player + 200 BB 默认）。字段 `pub(crate)` 让同 crate 测试 / bench
/// 访问内部状态而不暴露给外部消费者（D-376）。
pub struct SimplifiedNlheGame {
    pub(crate) bucket_table: Arc<BucketTable>,
    pub(crate) config: TableConfig,
    /// 抽象 betting tree，构造时一次性建好（200BB 默认 + 6-action 实测 240,096 节点）。
    /// State 沿 `tree.node(current_node_id).children` 跳转；Phase 3 起 `info_set`
    /// 用 `current_node_id` 作为下注历史维度，根除跨街 collision。
    pub(crate) tree: Arc<PublicBettingTree>,
    /// 本 game 的 action abstraction（建树 + 运行期 `legal_actions` 同源）。P4 起从
    /// 硬编码 `nlhe_action_abstraction()` 改为可参数化——6-max A3×A4 走 first_small 菜单
    /// （`legal_actions` 必须用这一份、而非全局 `nlhe_action_abstraction()`，否则 6-max
    /// 算出错的动作集）。
    pub(crate) abs: Arc<StreetActionAbstraction>,
}

impl SimplifiedNlheGame {
    /// 构造函数（API-303）。
    ///
    /// 校验项（D-314-rev1）：
    /// - `BucketTable::schema_version()` == `4`（v1/v2/v3 已废弃）
    /// - `BucketTable::config()` is supported (`200/200/200`, `500/500/500` or `1000/1000/1000`)
    ///
    /// 失败路径：[`TrainerError::UnsupportedBucketTable`]。`expected` 字段
    /// 编码 `schema_version`；`got` 字段编码实际 schema_version（schema 不匹配）
    /// 或返回 `0`（config 不匹配，无另外 variant 表达）。
    pub fn new(bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        // HU 默认（200BB / 2 座 / {0.5,1,2} / 无 A3×A4 规则）——委托给参数化构造但
        // 入参全是历史默认值，故与历史 `new` 逐节点 byte-equal（守 240,096 节点 +
        // nlhe_infoset_semantics T1）。6-max A3×A4 走 [`new_with_abstraction`]。
        Self::new_with_abstraction(
            bucket_table,
            TableConfig::default_hu_200bb(),
            nlhe_action_abstraction(),
            BettingAbstractionRules::default(),
        )
    }

    /// 参数化构造（P4 去 HU 硬编码）：指定 `config`（座位数 / 码深）+ action
    /// `abstraction` + A3×A4 `rules`。6-max A3×A4 用 `TableConfig::default_6max_100bb()` +
    /// [`first_small_6max`](crate::training::nlhe_betting_tree::first_small_6max)（返回
    /// 配对的 abstraction + rules）。
    ///
    /// **桶表 caveat**：当前 `bucket_table` 仍是 HU 单对手 equity 桶（postflop 200/500/1000）；
    /// 6-max 多路桶是 S3、未做。故 6-max game 的 hand_bucket 语义是 **HU 占位**——可构造、
    /// CFR 机制能跑（plumbing 验证），但**有意义的训练须等 S3 多路桶**。
    ///
    /// 校验同 [`new`](Self::new)：bucket schema v4 + 支持的 postflop config（200/500/1000）。
    pub fn new_with_abstraction(
        bucket_table: Arc<BucketTable>,
        config: TableConfig,
        abstraction: StreetActionAbstraction,
        rules: BettingAbstractionRules,
    ) -> Result<Self, TrainerError> {
        let schema = bucket_table.schema_version();
        if schema != EXPECTED_BUCKET_SCHEMA_VERSION {
            return Err(TrainerError::UnsupportedBucketTable {
                expected: EXPECTED_BUCKET_SCHEMA_VERSION,
                got: schema,
            });
        }
        if !is_supported_bucket_config(bucket_table.config()) {
            // 复用 UnsupportedBucketTable variant 表达 config 不匹配（schema 路径也走该
            // variant；`got = 0` 让区分通过日志上下文判断）。
            return Err(TrainerError::UnsupportedBucketTable {
                expected: EXPECTED_BUCKET_SCHEMA_VERSION,
                got: 0,
            });
        }
        let tree = Arc::new(PublicBettingTree::build_with_rules(
            &config,
            &abstraction,
            rules,
        ));
        Ok(Self {
            bucket_table,
            config,
            abs: Arc::new(abstraction),
            tree,
        })
    }

    /// 公开 `PublicBettingTree` 引用，供诊断工具（如 `nlhe_preflop_strategy_dump`）
    /// 在不构造完整 `SimplifiedNlheState` 的情况下走树定位特定 spot 的 node_id。
    pub fn tree(&self) -> &PublicBettingTree {
        &self.tree
    }

    /// 直接为指定的 preflop `node_id` × `hole` 构造 `InfoSetId`（绕过 `Game::info_set`
    /// 对 `SimplifiedNlheState` 的依赖）。仅 preflop 路径——postflop hand bucket 依赖
    /// `state.game_state.board()`，那条路径走完整 `info_set`。
    pub fn preflop_info_set_for_hand(
        &self,
        node_id: NodeId,
        hole: [crate::core::Card; 2],
    ) -> InfoSetId {
        let node = self.tree.node(node_id);
        debug_assert_eq!(
            node.street,
            StreetTag::Preflop,
            "preflop_info_set_for_hand 只支持 Preflop 节点；node {node_id} street = {:?}",
            node.street
        );
        let hand_bucket = u32::from(PreflopLossless169::new().hand_class(hole));
        pack_info_set_v2(hand_bucket, node_id, StreetTag::Preflop)
    }

    /// 用注入的真实 hole+board 为指定 `node_id` 构造 `InfoSetId`，绕过 [`Game::info_set`]
    /// 对 `SimplifiedNlheState`（随机发牌）的依赖。Slumbot 实时对战 advisor 用：树位置
    /// 由抽象影子状态的 `current_node_id` 给出，手牌强度用对面发来的**真实牌**算。
    ///
    /// 与 [`Game::info_set`] **逐行同路径**：`street_tag = self.tree.node(node_id).street`；
    /// preflop 走 `PreflopLossless169::hand_class`，postflop 走 `canonical_observation_id`
    /// 接 `BucketTable::lookup`，最后同一个 [`pack_info_set_v2`] 打包。区别仅在没有
    /// per-trajectory hand_bucket cache（单次查询不需要）——cache 命中路径返回值与重算
    /// 路径 byte-equal，故对处于 `current_node_id == node_id` 且持有相同 `(hole, board)`
    /// 的 state，结果与该 state 上 `Game::info_set(state, actor)` 逐位相等
    /// （`tests/nlhe_infoset_semantics.rs` T1 钉死）。
    ///
    /// `board` 必须是该街实际公共牌（flop=3 / turn=4 / river=5；preflop 忽略），否则
    /// `canonical_observation_id` 内部 `board.len()` 断言 panic。
    pub fn info_set_for_cards(
        &self,
        node_id: NodeId,
        hole: [crate::core::Card; 2],
        board: &[crate::core::Card],
    ) -> InfoSetId {
        let street_tag = self.tree.node(node_id).street;
        let hand_bucket: u32 = match street_tag {
            StreetTag::Preflop => u32::from(PreflopLossless169::new().hand_class(hole)),
            StreetTag::Flop | StreetTag::Turn | StreetTag::River => {
                let observation = canonical_observation_id(street_tag, board, hole);
                self.bucket_table
                    .lookup(street_tag, observation)
                    .expect("BucketTable::lookup returned None on in-range observation_id")
            }
        };
        pack_info_set_v2(hand_bucket, node_id, street_tag)
    }
}

/// 简化 NLHE 完整状态（API-303）。
///
/// `game_state` wrap stage 1 [`GameState`]（API-390 桥接）；`action_history`
/// 累积 stage 2 [`AbstractAction`]（API-392 桥接）；`bucket_table` 是
/// `SimplifiedNlheGame::bucket_table` 的 Arc clone，让 [`Game::info_set`]
/// 静态方法在 postflop 路径上能访问 lookup 表（trait 方法签名 `state: &Self::State`
/// 不含 `&self`，因此 bucket_table 引用必须通过 state 携带；Arc clone 每次
/// `next` 增 1 引用计数，无堆复制）。
pub struct SimplifiedNlheState {
    pub game_state: GameState,
    pub action_history: Vec<SimplifiedNlheAction>,
    pub(crate) bucket_table: Arc<BucketTable>,
    /// 当前节点在 `tree` 中的 id（Phase 2 起）。`root` 时初始化为 `tree.root_id()`；
    /// `next` 沿 `tree.node(current_node_id).children[action_idx]` 跳转。
    /// Terminal 状态下保留进入 Terminal 前最后一个决策节点 id（CFR 不会再读取，
    /// 仅用于调试 / 测试 path-to-root 还原）。
    pub current_node_id: NodeId,
    pub(crate) tree: Arc<PublicBettingTree>,
    /// 本 game 的 action abstraction（Arc clone，运行期 `legal_actions` 用；见
    /// [`SimplifiedNlheGame::abs`]）。与 `tree` / `bucket_table` 同样每 `next` 增 1
    /// 引用计数、无堆复制。
    pub(crate) abs: Arc<StreetActionAbstraction>,
    /// info_set hand_bucket per-street cache（packed u64，layout 见
    /// [`pack_info_set_cache`]）。同一 trajectory 内 (street, actor) 不变时直接命中，
    /// 跳过 `canonical_observation_id` + `BucketTable::lookup`；street 切换时
    /// `info_set` 读到 packed `street_plus_one` mismatch 自动重算。Atomic 仅为满足
    /// `Game::State: Sync` bound（State 实际由单 worker 拥有，Relaxed 等价普通
    /// load/store）。
    pub(crate) info_set_cache: AtomicU64,
}

impl Clone for SimplifiedNlheState {
    fn clone(&self) -> Self {
        Self {
            game_state: self.game_state.clone(),
            action_history: self.action_history.clone(),
            bucket_table: Arc::clone(&self.bucket_table),
            current_node_id: self.current_node_id,
            tree: Arc::clone(&self.tree),
            abs: Arc::clone(&self.abs),
            info_set_cache: AtomicU64::new(self.info_set_cache.load(Ordering::Relaxed)),
        }
    }
}

/// info_set hand_bucket cache packed layout（小端，低位起）：
/// - bits 0..8:   `street_plus_one`（0 = invalid，1..=4 = Preflop..River + 1）
/// - bits 8..16:  `set_mask`（bit `actor` = actor 的 bucket 已缓存；HU 仅低 2 位有效）
/// - bits 16..32: actor 0 hand_bucket（u16；preflop ≤ 169 / postflop ≤ 500，远 < 65536）
/// - bits 32..48: actor 1 hand_bucket（u16）
/// - bits 48..64: reserved 0
#[inline]
fn pack_info_set_cache(street_plus_one: u8, mask: u8, bucket0: u16, bucket1: u16) -> u64 {
    (street_plus_one as u64)
        | ((mask as u64) << 8)
        | ((bucket0 as u64) << 16)
        | ((bucket1 as u64) << 32)
}

#[inline]
fn unpack_info_set_cache(packed: u64) -> (u8, u8, u16, u16) {
    (
        packed as u8,
        (packed >> 8) as u8,
        (packed >> 16) as u16,
        (packed >> 32) as u16,
    )
}

/// 计算 `(actor, street)` 的 hand_bucket（preflop 169 lossless / postflop `BucketTable`
/// lookup）。= [`SimplifiedNlheGame::info_set`] HU cache-miss 分支的**同源逻辑**，供 P4
/// 6-max uncached 分支复用。HU 分支保持逐字不动、刻意不抽取——避免动到生产热路径
/// （byte-equal by 不-touch，nlhe_infoset_semantics T1 钉死）；代价 = 这段 ~8 行重复。
fn compute_hand_bucket(state: &SimplifiedNlheState, actor: PlayerId, street_tag: StreetTag) -> u32 {
    let hole = state.game_state.players()[actor as usize]
        .hole_cards
        .expect("SimplifiedNlhe info_set: actor hole_cards must be present on decision node");
    match street_tag {
        StreetTag::Preflop => u32::from(PreflopLossless169::new().hand_class(hole)),
        StreetTag::Flop | StreetTag::Turn | StreetTag::River => {
            let observation = canonical_observation_id(street_tag, state.game_state.board(), hole);
            state
                .bucket_table
                .lookup(street_tag, observation)
                .expect("BucketTable::lookup returned None on in-range observation_id")
        }
    }
}

impl std::fmt::Debug for SimplifiedNlheState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 跳过 `bucket_table` 的 Debug（stage 2 `BucketTable` 未实现 Debug；
        // 528 MiB body 也不适合 debug 打印）。仅暴露 game_state +
        // action_history。
        f.debug_struct("SimplifiedNlheState")
            .field("game_state", &self.game_state)
            .field("action_history", &self.action_history)
            .field("current_node_id", &self.current_node_id)
            .field("bucket_table", &"<Arc<BucketTable>>")
            .field("tree", &"<Arc<PublicBettingTree>>")
            .field("abs", &"<Arc<StreetActionAbstraction>>")
            .finish()
    }
}

impl Game for SimplifiedNlheGame {
    type State = SimplifiedNlheState;
    type Action = SimplifiedNlheAction;
    type InfoSet = SimplifiedNlheInfoSet;

    const VARIANT: crate::error::GameVariant = crate::error::GameVariant::SimplifiedNlhe;

    fn bucket_table_blake3(&self) -> [u8; 32] {
        self.bucket_table.content_hash()
    }

    fn n_players(&self) -> usize {
        // P4：从硬编码 2 改为 config 驱动（HU config → 2，byte-equal；6-max → 6）。
        // trainer 已 N-generic（`trainer.rs` 用 `game.n_players()` 轮换 traverser、
        // 不假设 `1 - player`），故此处放开即支持 6 座自对弈。
        self.config.n_seats as usize
    }

    fn root(&self, rng: &mut dyn RngSource) -> SimplifiedNlheState {
        // stage 1 `GameState::with_rng_no_history` 在构造时按 D-028 deal protocol
        // 消费 RNG（51 次 `next_u64` Fisher-Yates）发底牌 + post blinds + 5 张
        // runout board。`seed` 参数仅作为 `HandHistory.seed` 标签写入，不参与
        // 发牌——实际 randomness 全部来自 `rng`（D-028 字面）。
        //
        // D-378 CFR fast path：走 `with_rng_no_history` 跳过 `history.actions`
        // 的 `with_capacity(32)` 预分配 + per-apply `push`；`payouts()` 不受影响
        // （走 `state.final_payouts` 字段）。NLHE 自身的 `action_history` 也只在
        // 调试 / trace 工具中被读取，CFR 路径上同步跳 push。
        let game_state = GameState::with_rng_no_history(&self.config, 0, rng);
        SimplifiedNlheState {
            game_state,
            action_history: Vec::new(),
            bucket_table: Arc::clone(&self.bucket_table),
            current_node_id: self.tree.root_id(),
            tree: Arc::clone(&self.tree),
            abs: Arc::clone(&self.abs),
            info_set_cache: AtomicU64::new(0),
        }
    }

    fn current(state: &SimplifiedNlheState) -> NodeKind {
        // 简化 NLHE 没有独立 chance node（D-308 / D-315 chance 在 root 一次性
        // 消费 + board 由 stage 1 `GameState::deal_board_to` 在 betting 切换内部
        // 自动 deal）；因此 `current` 仅返回 Player / Terminal。
        if state.game_state.is_terminal() {
            return NodeKind::Terminal;
        }
        match state.game_state.current_player() {
            Some(seat) => NodeKind::Player(seat.0 as PlayerId),
            None => NodeKind::Terminal,
        }
    }

    fn info_set(state: &SimplifiedNlheState, actor: PlayerId) -> SimplifiedNlheInfoSet {
        // Phase 3 v2 layout：(hand_bucket | node_id | street_tag)。
        // node_id 单射于抽象 betting tree 路径，subsume 旧 position/stack/betting_state
        // /action_signature 字段，并彻底解决跨街 collision（见
        // `docs/nlhe_infoset_history_investigation.md` 方案 A）。
        let node = state.tree.node(state.current_node_id);
        debug_assert_eq!(
            node.player_acting, actor,
            "info_set: actor {actor} mismatch with node.player_acting {} (CFR 走错节点)",
            node.player_acting
        );
        let street_tag = node.street;

        // P4 6-max（n_seats > 2）：uncached path。下方 HU 2-slot u64 cache（bits 16..48
        // 只放得下 2 座的 hand_bucket）容不下 6 座；multiway cache 落 post-S3 perf-tune。
        // 位置仍由 node_id 内化（每节点唯一 player_acting → 不同位置不同 node_id），故
        // pack_info_set_v2 无需位置位、6-max 无碰撞。bucket 计算与下方 HU cache-miss 同源
        // （[`compute_hand_bucket`]）。
        if state.game_state.config().n_seats > 2 {
            let hand_bucket = compute_hand_bucket(state, actor, street_tag);
            return pack_info_set_v2(hand_bucket, state.current_node_id, street_tag);
        }

        let street_plus_one: u8 = (street_tag as u8) + 1;
        debug_assert!(actor < 2, "HU NLHE actor must be 0 or 1, got {actor}");
        let actor_bit: u8 = 1u8 << actor;

        // info_set hand_bucket per-street cache（D-378 后续优化）：同一 trajectory
        // 内 (street, actor) 的 (board, hole) 不变 → hand_bucket 必相同；命中跳过
        // postflop `canonical_observation_id` 二进制搜索 + bucket_table.lookup。
        // street 切换时 packed `street_plus_one` mismatch 自动失效。
        let cached = state.info_set_cache.load(Ordering::Relaxed);
        let (cached_sp1, cached_mask, cached_b0, cached_b1) = unpack_info_set_cache(cached);
        if cached_sp1 == street_plus_one && (cached_mask & actor_bit) != 0 {
            let bucket = if actor == 0 { cached_b0 } else { cached_b1 } as u32;
            return pack_info_set_v2(bucket, state.current_node_id, street_tag);
        }

        let hole = state.game_state.players()[actor as usize]
            .hole_cards
            .expect("SimplifiedNlhe info_set: actor hole_cards must be present on decision node");

        let hand_bucket: u32 = match street_tag {
            StreetTag::Preflop => {
                // D-317 preflop：169 lossless hand_class 直接当 `bucket_id`。
                let preflop = PreflopLossless169::new();
                u32::from(preflop.hand_class(hole))
            }
            StreetTag::Flop | StreetTag::Turn | StreetTag::River => {
                // D-317 postflop：走 stage 2 `BucketTable::lookup` 命中
                // cluster id（D-218-rev2 真等价类 canonical_observation_id +
                // §G-batch1 §3.10 v4 production lookup 表）。
                let observation =
                    canonical_observation_id(street_tag, state.game_state.board(), hole);
                state
                    .bucket_table
                    .lookup(street_tag, observation)
                    .expect("BucketTable::lookup returned None on in-range observation_id")
            }
        };
        debug_assert!(
            hand_bucket < (1u32 << 16),
            "hand_bucket={hand_bucket} 超 u16 cache slot 上限；preflop ≤ 169 / postflop ≤ 1000"
        );
        let hand_bucket_u16 = hand_bucket as u16;

        let (new_mask, new_b0, new_b1) = if cached_sp1 == street_plus_one {
            // 同街已缓存对手 → 保留对手 bucket，写本 actor slot。
            let (b0, b1) = if actor == 0 {
                (hand_bucket_u16, cached_b1)
            } else {
                (cached_b0, hand_bucket_u16)
            };
            (cached_mask | actor_bit, b0, b1)
        } else {
            // 街切换 / 首次 → 仅本 actor slot 有效。
            let (b0, b1) = if actor == 0 {
                (hand_bucket_u16, 0)
            } else {
                (0, hand_bucket_u16)
            };
            (actor_bit, b0, b1)
        };
        state.info_set_cache.store(
            pack_info_set_cache(street_plus_one, new_mask, new_b0, new_b1),
            Ordering::Relaxed,
        );

        pack_info_set_v2(hand_bucket, state.current_node_id, street_tag)
    }

    fn legal_actions(state: &SimplifiedNlheState) -> Vec<SimplifiedNlheAction> {
        // D-318 桥接：stage 2 `ActionAbstraction::abstract_actions` 顺序由 D-209
        // deterministic（每次构造同型抽象，开销可忽略——仅 clone 配置）；Trainer 的
        // RegretTable `Vec<f64>` 索引一一对应（D-324 action_count 训练全程恒定）。
        //
        // 必须与 `new()` 建 tree 用同一 `nlhe_action_abstraction()`：按街分派下
        // `abstract_actions` 自动按 `state.street()` 选对应街 raise 集合，tag 顺序与
        // tree child 下标对齐。
        //
        // `into_actions()` 直接 move 出 set 的内部 Vec；之前走
        // `as_slice().to_vec()` 每节点会多 alloc + memcpy 一份。
        //
        // P4 derive-from-tree：动作全集用**本 game 的** `state.abs`（HU 默认 {0.5,1,2}；
        // 6-max A3×A4 走 first_small 菜单——不能用全局 `nlhe_action_abstraction()`，否则
        // 6-max 算出 {0.5,1,2} 的错集），再过滤到当前 node 的（可能被 A3×A4 剪过的）
        // legal_actions tag。树是唯一真相源 → 运行期与 tree child 下标恒一致（F17-free），
        // 运行期不需重算 A3×A4 entrants（建树已 baked-in）。fast path：node 未剪
        // （len 相等，HU 默认树即此）直接返回全集 = 零 per-action 开销、与历史 byte-equal；
        // 剪过则 filter（保 D-209 序，regret 槽对齐）。
        let full = state.abs.abstract_actions(&state.game_state).into_actions();
        let node = state.tree.node(state.current_node_id);
        if node.legal_actions.len() == full.len() {
            return full;
        }
        full.into_iter()
            .filter(|a| node.legal_actions.contains(&AbstractActionTag::of(a)))
            .collect()
    }

    fn next(
        state: SimplifiedNlheState,
        action: SimplifiedNlheAction,
        _rng: &mut dyn RngSource,
    ) -> SimplifiedNlheState {
        // API-390 桥接：`AbstractAction::to_concrete()` 无状态（D-318：Bet/Raise
        // 在构造时已区分），转 stage 1 `Action` 后 apply。
        let concrete = action.to_concrete();
        let mut next_state = state;

        // Phase 2: 沿 tree 跳转 current_node_id。先按 AbstractActionTag 在当前节点
        // legal_actions 里定位 edge index，再从 children[idx] 取下一个 NodeId。
        // 必须在 apply 之前查表——apply 之后 game_state.current_player 可能切人
        // 或进 Terminal，而 tree lookup 用的是动作本身的 tag，不依赖 chip 值。
        let tag = AbstractActionTag::of(&action);
        // 单次 tree.node 查表，先取 edge index 再取对应 child；
        // 两次 lookup Arc<PublicBettingTree>::node + Vec<TreeNode> 索引
        // LLVM 不一定能 CSE（Arc deref 阻塞 alias 分析）。
        let node = next_state.tree.node(next_state.current_node_id);
        let edge_idx = node.legal_actions.iter().position(|t| *t == tag).expect(
            "Phase 2 invariant: action tag must appear in current node legal_actions; \
                 mismatch indicates CFR走了 tree 外动作 or tree builder 漏 edge",
        );
        let child = node.children[edge_idx];

        next_state
            .game_state
            .apply(concrete)
            .expect("SimplifiedNlhe next: AbstractAction → Action apply must be legal");
        // D-378 CFR fast path：`game_state.track_history() == false` 时（NLHE root
        // 走 `with_rng_no_history`）跳过 `action_history.push` —— CFR 不读
        // `action_history`，避免每节点的 Vec push / clone 成本。trace / 调试
        // 路径走 `Game::root` 之外的入口仍正常累积。
        if next_state.game_state.track_history() {
            next_state.action_history.push(action);
        }

        // Tree 跳转后 invariant 自检：tree 标的 Terminal/Decision 必须跟 game_state
        // 实际 terminality 一致；不一致说明 builder 漏 case 或 game_state apply 行为
        // 跟 builder 期望不符。
        match child {
            Child::Decision(next_id) => {
                debug_assert!(
                    !next_state.game_state.is_terminal(),
                    "Phase 2 invariant: tree says Decision(id={next_id}) but game_state is Terminal"
                );
                next_state.current_node_id = next_id;
            }
            Child::Terminal => {
                debug_assert!(
                    next_state.game_state.is_terminal(),
                    "Phase 2 invariant: tree says Terminal but game_state is not"
                );
                // current_node_id 保留 Terminal 前最后一个 decision id（已不再被
                // info_set/recurse 读到；CFR 检测到 Terminal 后走 payoff 路径）。
            }
        }

        // 不消费 `_rng`（API-300 invariant：decision node `next` pure transition）。
        // stage 1 GameState 的 board 切换走 `deal_board_to` 从 `runout_board`
        // 取出已发的卡（在 root 时一次性消费 rng 发牌），不依赖 rng。
        next_state
    }

    fn chance_distribution(_state: &SimplifiedNlheState) -> Vec<(SimplifiedNlheAction, f64)> {
        // 简化 NLHE 没有独立 chance node（详见模块文档）；该方法不应被 ES-MCCFR
        // / Vanilla CFR 触发。任何调用是上层 algorithm 错误，panic 让调用方
        // 立即看到 stack trace（API-300 invariant：Chance 节点必须 returns
        // non-empty distribution; 此处 panic 是 `current()` 永不返回 Chance 的
        // 后果，不是规则违反）。
        panic!(
            "SimplifiedNlheGame::chance_distribution called: simplified NLHE has no in-game \
             chance nodes (all randomness consumed by Game::root via GameState::with_rng); \
             check `current(state)` returned NodeKind::Chance"
        );
    }

    fn payoff(state: &SimplifiedNlheState, player: PlayerId) -> f64 {
        // D-316 chip 净收益直接当 utility。stage 1 `GameState::payouts()` 在
        // terminal 时返回 `Some(Vec<(SeatId, i64)>)`（每条 entry = `awards[seat]
        // - committed[seat]`，即净 PnL）。`i64 → f64` lossless within chip 范围
        // ≤ 2^31（D-339 stage 3 chip 上限远低于 f64 mantissa 52 bit 上界）。
        let payouts = state
            .game_state
            .payouts()
            .expect("SimplifiedNlhe payoff: state must be terminal (Game::current == Terminal)");
        let target_seat = SeatId(player);
        let pnl = payouts
            .into_iter()
            .find(|(seat, _)| *seat == target_seat)
            .map(|(_, pnl)| pnl)
            .expect("SimplifiedNlhe payoff: payouts must include actor seat (stage 1 invariant)");
        pnl as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstraction::bucket_table::BucketConfig;
    use crate::core::rng::{ChaCha20Rng, RngSource};
    use crate::training::nlhe_betting_tree::first_small_6max;

    fn stub_table() -> Arc<BucketTable> {
        Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::default_500_500_500(),
        ))
    }

    /// P4 smoke：6-max A3×A4 game 构造 + 整套 `Game` 机制（root/current/legal_actions/
    /// info_set/next/payoff）端到端跑通不 panic。验去 HU 硬编码后 6 座 plumbing 正确：
    /// ① `n_players()==6`；② game 建的树 == probe 真值（78,852，N=2）；③ 多条确定性轨迹
    /// 走到 terminal，每决策节点 legal_actions 非空 + info_set（6-max uncached 分支）不 panic；
    /// ④ 6 座 payoff 守恒（Σ==0 筹码守恒）。桶是 HU 占位（S3 前不训练、只验机制）。
    /// N=2（树小、debug 快，且 debug 顺带触发建树 redirect 不变量 assert）。
    #[test]
    fn p4_6max_a3xa4_game_smoke() {
        let (abs, rules) = first_small_6max(2);
        let game = SimplifiedNlheGame::new_with_abstraction(
            stub_table(),
            TableConfig::default_6max_100bb(),
            abs,
            rules,
        )
        .expect("6-max A3×A4 game 应当构造成功（stub 500 桶表）");

        assert_eq!(game.n_players(), 6, "6-max n_players 应为 6");
        assert_eq!(
            game.tree().num_nodes(),
            78_852,
            "game 建的 6-max A3×A4 树应 == probe 真值 78,852（N=2）"
        );

        // 多条确定性轨迹（不同固定策略索引）各走到 terminal，验机制不 panic + 守恒。
        for policy in 0..3u64 {
            let mut rng = ChaCha20Rng::from_seed(0xA3A4_0000 + policy);
            let rng: &mut dyn RngSource = &mut rng;
            let mut state = game.root(rng);
            let mut guard = 0;
            loop {
                match SimplifiedNlheGame::current(&state) {
                    NodeKind::Terminal => break,
                    NodeKind::Player(actor) => {
                        let actions = SimplifiedNlheGame::legal_actions(&state);
                        assert!(!actions.is_empty(), "决策节点 legal_actions 不应为空");
                        // info_set 不 panic（6-max uncached 分支）+ actor 与节点一致。
                        let _ = SimplifiedNlheGame::info_set(&state, actor);
                        let idx = (policy as usize) % actions.len();
                        state = SimplifiedNlheGame::next(state, actions[idx], rng);
                    }
                    NodeKind::Chance => unreachable!("简化 NLHE 无 chance 节点"),
                }
                guard += 1;
                assert!(guard < 100, "轨迹深度爆炸（A3×A4 N=2 max depth 17）");
            }
            // 6 座净 PnL 守恒（筹码守恒，与桶无关）。
            let sum: f64 = (0..6)
                .map(|p| SimplifiedNlheGame::payoff(&state, p as PlayerId))
                .sum();
            assert!(sum.abs() < 1e-6, "6 座 payoff 应守恒 Σ==0，实得 {sum}");
        }
    }

    /// P4 参数化没破 HU 默认路径：`new` 仍构造 240,096 节点默认树、2 座。
    #[test]
    fn p4_hu_new_default_tree_unchanged() {
        let game = SimplifiedNlheGame::new(stub_table()).expect("HU game 构造");
        assert_eq!(game.n_players(), 2, "HU n_players 应为 2");
        assert_eq!(
            game.tree().num_nodes(),
            240_096,
            "HU 默认树应仍 240,096（P4 参数化未破默认）"
        );
    }
}

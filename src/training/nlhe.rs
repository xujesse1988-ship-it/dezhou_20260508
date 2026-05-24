//! SimplifiedNlheGame（API-303 / D-313）+ stage 1 / stage 2 桥接（API-390..API-392）。
//!
//! 简化 NLHE 范围：2-player + 200 BB starting stack + 盲注 0.5/1.0 BB +
//! 完整 4 街 + stage 2 `DefaultActionAbstraction`（6-action：{0.5p, 1p, 2p} +
//! Fold/Check/Call/AllIn）+ stage 2 `PreflopLossless169` +
//! `PostflopBucketAbstraction`（500/500/500 bucket）。
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
//! Bucket table 依赖 = stage 2 v3 production artifact
//! `artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav3.bin`
//! （schema_version=3 / feature_set_id=2 / 16-dim hist+OCHS / body BLAKE3
//! `1c22c1ee...`）。`SimplifiedNlheGame::new` 校验 `schema_version() == 3` +
//! `config() == BucketConfig::new(500, 500, 500)`。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::abstraction::action::{AbstractAction, ActionAbstraction, DefaultActionAbstraction};
use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::abstraction::map::pack_info_set_id;
use crate::abstraction::postflop::canonical_observation_id;
use crate::abstraction::preflop::PreflopLossless169;
use crate::core::rng::RngSource;
use crate::core::SeatId;
use crate::error::TrainerError;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{ActionVec, Game, NodeKind, PlayerId};
use crate::training::nlhe_betting_tree::{AbstractActionTag, Child, NodeId, PublicBettingTree};

/// 简化 NLHE action 桥接（API-303 / D-318）。
///
/// 直接走 stage 2 `AbstractAction`（6-action 顺序由 D-209 deterministic）；不再
/// 二次抽象。`Game::Action` trait bound `Copy + Eq + Debug` 由 stage 2 实现满足。
pub type SimplifiedNlheAction = AbstractAction;

/// 简化 NLHE InfoSet 桥接（API-303 / D-317）。
///
/// 直接走 stage 2 64-bit `InfoSetId`（D-215 layout）。preflop 走
/// `PreflopLossless169` hand_class 169 等价类；postflop 走 stage 2 `BucketTable`
/// lookup（D-314-rev1 v3 artifact 500/500/500 bucket）。
pub type SimplifiedNlheInfoSet = InfoSetId;

/// 简化 NLHE expected `BucketConfig`（D-313）：preflop 169 lossless（不走 bucket
/// table）+ flop/turn/river 各 500 bucket（§G-batch1 §3.10 v3 production lock）。
fn expected_bucket_config() -> BucketConfig {
    // BucketConfig::new 失败仅当任意 street bucket count 越界 [10, 10_000]
    // （D-214）；500/500/500 严格在范围内，构造永远成功。
    BucketConfig::new(500, 500, 500).expect("BucketConfig::new(500,500,500) within D-214 range")
}

/// 简化 NLHE expected `BucketTable` schema_version。stage 2 已升 v3
/// (16-dim hist+OCHS feature)；v1/v2 artifact 不再可加载。
const EXPECTED_BUCKET_SCHEMA_VERSION: u32 = 3;

/// 简化 NLHE `InfoSetId` v2 layout：把 26-bit node_id 写入 `InfoSetId` raw 高位
/// （bits 38..64）。低 38 bit 复用 stage-2 [`pack_info_set_id`] 字段位置以保留
/// `.bucket_id()` / `.street_tag()` 访问语义；其中 position_bucket / stack_bucket
/// / betting_state 在 NLHE codepath 上恒为 0（信息已被 node_id 内化，见
/// `docs/nlhe_infoset_history_investigation.md` 方案 A）。
///
/// stage-2 `InfoAbstraction::map`（preflop.rs / postflop.rs）走 IA-007 reserved bits = 0
/// 的旧路径不受影响——本 v2 packer 仅在 [`SimplifiedNlheGame::info_set`] 内使用。
const NLHE_V2_NODE_ID_SHIFT: u32 = 38;
const NLHE_V2_NODE_ID_BITS: u32 = 26;

fn pack_info_set_v2(hand_bucket: u32, node_id: NodeId, street_tag: StreetTag) -> InfoSetId {
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
}

impl SimplifiedNlheGame {
    /// 构造函数（API-303）。
    ///
    /// 校验项（D-314-rev1）：
    /// - `BucketTable::schema_version()` == `3`（v1/v2 已废弃）
    /// - `BucketTable::config()` == `BucketConfig::new(500, 500, 500)`
    ///
    /// 失败路径：[`TrainerError::UnsupportedBucketTable`]。`expected` 字段
    /// 编码 `schema_version`；`got` 字段编码实际 schema_version（schema 不匹配）
    /// 或返回 `0`（config 不匹配，无另外 variant 表达）。
    pub fn new(bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        let schema = bucket_table.schema_version();
        if schema != EXPECTED_BUCKET_SCHEMA_VERSION {
            return Err(TrainerError::UnsupportedBucketTable {
                expected: EXPECTED_BUCKET_SCHEMA_VERSION,
                got: schema,
            });
        }
        let cfg = bucket_table.config();
        let expected = expected_bucket_config();
        if cfg.flop != expected.flop || cfg.turn != expected.turn || cfg.river != expected.river {
            // 复用 UnsupportedBucketTable variant 表达 config 不匹配（schema 路径
            // 也走该 variant；`got = 0` 让区分通过日志上下文判断）。stage 3
            // F1 / F2 [测试 / 实现] 评估是否引入新 variant
            // `TrainerError::UnsupportedBucketConfig { expected, got }`。
            return Err(TrainerError::UnsupportedBucketTable {
                expected: EXPECTED_BUCKET_SCHEMA_VERSION,
                got: 0,
            });
        }
        let config = TableConfig::default_hu_200bb();
        let tree = Arc::new(PublicBettingTree::build(&config));
        Ok(Self {
            bucket_table,
            config,
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
        // D-313 简化 NLHE 范围严格 2-player（不向 6-max 通用化；stage 4 走单独
        // 6-max blueprint）。
        2
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
                // §G-batch1 §3.10 v3 production lookup 表）。
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
            "hand_bucket={hand_bucket} 超 u16 cache slot 上限；preflop ≤ 169 / postflop ≤ 500"
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

    fn legal_actions(state: &SimplifiedNlheState) -> ActionVec<SimplifiedNlheAction> {
        // D-318 桥接：stage 2 `DefaultActionAbstraction::abstract_actions`
        // 顺序由 D-209 deterministic（每次构造同型 6-action 抽象，开销可忽略
        // —— `DefaultActionAbstraction::new` 仅 clone 配置）；Trainer 的 RegretTable
        // `Vec<f64>` 索引一一对应（D-324 action_count 训练全程恒定）。
        //
        // 第四轮 perf：`into_actions()` 直接 move 出 set 的内部
        // `AbstractActionVec` = `SmallVec<[AbstractAction; 8]>`，与本 trait 返回
        // 类型 `ActionVec<A>` 同型，避免每节点一次 Vec 堆分配 + memcpy。
        let abs = DefaultActionAbstraction::default_6_action();
        abs.abstract_actions(&state.game_state).into_actions()
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

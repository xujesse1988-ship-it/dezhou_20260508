//! SimplifiedNlheGame（API-303 / D-313）+ stage 1 / stage 2 桥接（API-390..API-392）。
//!
//! 简化 NLHE 范围（D-313）：2-player + 100 BB starting stack + 盲注 0.5/1.0 BB +
//! 完整 4 街 + stage 2 `DefaultActionAbstraction`（5-action）+ stage 2
//! `PreflopLossless169` + `PostflopBucketAbstraction`（500/500/500 bucket）。
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
//! **D-314-rev1 lock**（2026-05-13 stage 3 decisions §10.1）：bucket table 依赖
//! = §G-batch1 §3.10 production v3 artifact `artifacts/bucket_table_default_500_
//! 500_500_seed_cafebabe_v3.bin`（schema_version=2 / body BLAKE3 `67ee5554...`）。
//! `SimplifiedNlheGame::new` 校验 `schema_version() == 2`（v1 95 KB fallback
//! D-314-rev2 已废弃，构造时拒绝）+ `config() == BucketConfig::new(500, 500, 500)`。

use std::sync::Arc;

use crate::abstraction::action::{
    AbstractAction, ActionAbstraction, BetRatio, DefaultActionAbstraction,
};
use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::abstraction::map::pack_info_set_id;
use crate::abstraction::postflop::canonical_observation_id;
use crate::abstraction::preflop::{
    compute_betting_state, compute_position_bucket, compute_stack_bucket, compute_street_tag,
    PreflopLossless169,
};
use crate::core::rng::RngSource;
use crate::core::{ChipAmount, SeatId};
use crate::error::TrainerError;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};
use crate::training::nlhe_betting_tree::{AbstractActionTag, Child, NodeId, PublicBettingTree};

/// 简化 NLHE action 桥接（API-303 / D-318）。
///
/// 直接走 stage 2 `AbstractAction`（5-action 顺序由 D-209 deterministic）；不再
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

/// 简化 NLHE expected `BucketTable` schema_version（D-314-rev1）。v1 95 KB
/// fallback (D-314-rev2) 已废弃；C2 [实现] 拒绝 schema_version=1 输入。
const EXPECTED_BUCKET_SCHEMA_VERSION: u32 = 2;

/// NLHE action semantic signature 写入 `InfoSetId` raw bits 38..64。
///
/// `docs/h3_500m_checkpoint_investigation.md` 指出旧 D-317-rev1 只把 6-bit
/// availability mask 写入 `bucket_id`，无法区分 "Call 200 / Raise 400" 与
/// "Call 300 / Raise 600" 这类同角色、不同金额语义的决策点。CFR 表按
/// `(InfoSetId, action_index)` 累积 regret，因此 action index 的抽象语义必须是
/// key 的一部分。
///
/// 本 stage-3 NLHE overlay 保留 stage-2 低 38 bit layout（bucket/position/stack/
/// betting/street getter 继续可用），只在 `SimplifiedNlheGame::info_set` 返回值的
/// 高 26 bit 写入私有 action signature。stage-2 `InfoAbstraction::map` 路径仍
/// 保持 IA-007 reserved bits = 0。
const NLHE_ACTION_SIGNATURE_SHIFT: u32 = 38;

/// bits 38..64 共 26 bit。
const NLHE_ACTION_SIGNATURE_BITS: u32 = 26;

/// D-209 slots: 6-bit role mask + 4 个 5-bit 金额桶 = 26 bit。
const ACTION_ROLE_MASK_BITS: u32 = 6;
const ACTION_AMOUNT_BUCKET_BITS: u32 = 5;
const CALL_BUCKET_SHIFT: u32 = ACTION_ROLE_MASK_BITS;
const HALF_POT_BUCKET_SHIFT: u32 = CALL_BUCKET_SHIFT + ACTION_AMOUNT_BUCKET_BITS;
const FULL_POT_BUCKET_SHIFT: u32 = HALF_POT_BUCKET_SHIFT + ACTION_AMOUNT_BUCKET_BITS;
const ALL_IN_BUCKET_SHIFT: u32 = FULL_POT_BUCKET_SHIFT + ACTION_AMOUNT_BUCKET_BITS;

/// 金额桶的上界，以 half-BB 为单位。bucket 0 是 "slot 不存在"，bucket 1..31
/// 表示实际金额；1..20 精确覆盖 0.5BB..10BB，后段逐步变粗到 80BB+。
const AMOUNT_BUCKET_CEIL_HALF_BB: [u64; 31] = [
    1,
    2,
    3,
    4,
    5,
    6,
    7,
    8,
    9,
    10,
    11,
    12,
    13,
    14,
    15,
    16,
    17,
    18,
    19,
    20,
    24,
    28,
    32,
    36,
    40,
    50,
    60,
    80,
    120,
    160,
    u64::MAX,
];

/// 把 [`DefaultActionAbstraction::abstract_actions`] 输出折成 26-bit action semantic
/// signature（H3 500M investigation 修复）。编码：
///
/// - bits 0..6: D-209 role availability mask
/// - bits 6..11: Call `to` 金额桶
/// - bits 11..16: Bet|Raise @ `BetRatio::HALF_POT` 的 `to` 金额桶
/// - bits 16..21: Bet|Raise @ `BetRatio::FULL_POT` 的 `to` 金额桶
/// - bits 21..26: AllIn `to` 金额桶
///
/// 这不是 exact-chip public history；它是 NLHE-specific betting geometry
/// abstraction，目标是让同一 InfoSetId 下 action index 的角色和金额桶语义一致。
fn action_semantic_signature(state: &GameState, actions: &[AbstractAction]) -> u32 {
    let big_blind = state.config().big_blind;
    let mut role_mask = 0u32;
    let mut call_bucket = 0u32;
    let mut half_pot_bucket = 0u32;
    let mut full_pot_bucket = 0u32;
    let mut all_in_bucket = 0u32;

    for action in actions {
        let bit = match action {
            AbstractAction::Fold => 0,
            AbstractAction::Check => 1,
            AbstractAction::Call { to } => {
                call_bucket = u32::from(action_amount_bucket(*to, big_blind));
                2
            }
            AbstractAction::Bet { ratio_label, .. } | AbstractAction::Raise { ratio_label, .. } => {
                if ratio_label.as_milli() == BetRatio::HALF_POT.as_milli() {
                    3
                } else {
                    // DefaultActionAbstraction::default_5_action() only emits
                    // HALF_POT / FULL_POT ratios; any other ratio comes from
                    // custom config (stage 3 simplified NLHE固定 default_5_action,
                    // 不应触发)。fall-through 走 bit 4 让 mask 不退化为 0（保留
                    // 区分性；ratio_label 编码 by-stage-3 一致性留 stage 4 评估）。
                    4
                }
            }
            AbstractAction::AllIn { to } => {
                all_in_bucket = u32::from(action_amount_bucket(*to, big_blind));
                5
            }
        };
        role_mask |= 1u32 << bit;

        if let AbstractAction::Bet { to, ratio_label } | AbstractAction::Raise { to, ratio_label } =
            action
        {
            let bucket = u32::from(action_amount_bucket(*to, big_blind));
            if ratio_label.as_milli() == BetRatio::HALF_POT.as_milli() {
                half_pot_bucket = bucket;
            } else {
                full_pot_bucket = bucket;
            }
        }
    }

    let signature = role_mask
        | (call_bucket << CALL_BUCKET_SHIFT)
        | (half_pot_bucket << HALF_POT_BUCKET_SHIFT)
        | (full_pot_bucket << FULL_POT_BUCKET_SHIFT)
        | (all_in_bucket << ALL_IN_BUCKET_SHIFT);
    debug_assert!(signature < (1u32 << NLHE_ACTION_SIGNATURE_BITS));
    signature
}

fn action_amount_bucket(to: ChipAmount, big_blind: ChipAmount) -> u8 {
    let bb = big_blind.as_u64().max(1);
    let half_bb_units = ((u128::from(to.as_u64()) * 2).div_ceil(u128::from(bb))) as u64;
    for (idx, &ceil_units) in AMOUNT_BUCKET_CEIL_HALF_BB.iter().enumerate() {
        if half_bb_units <= ceil_units {
            return (idx + 1) as u8;
        }
    }
    AMOUNT_BUCKET_CEIL_HALF_BB.len() as u8
}

/// 简化 NLHE Game token（API-303）。
///
/// 构造时载入 stage 2 `BucketTable`（D-314-rev1 v3 artifact）+ stage 1
/// `TableConfig`（2-player + 100 BB）。字段 `pub(crate)` 让同 crate 测试 / bench
/// 访问内部状态而不暴露给外部消费者（D-376）。
pub struct SimplifiedNlheGame {
    pub(crate) bucket_table: Arc<BucketTable>,
    pub(crate) config: TableConfig,
    /// 抽象 betting tree，构造时一次性建好（Phase 1 节点数实测 48,224）。
    /// State 沿 `tree.node(current_node_id).children` 跳转；Phase 3 起 `info_set`
    /// 用 `current_node_id` 作为下注历史维度，根除跨街 collision。
    pub(crate) tree: Arc<PublicBettingTree>,
}

impl SimplifiedNlheGame {
    /// 构造函数（API-303）。
    ///
    /// 校验项（D-314-rev1）：
    /// - `BucketTable::schema_version()` == `2`（v1 fallback 已废弃）
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
        let config = TableConfig::default_hu_100bb();
        let tree = Arc::new(PublicBettingTree::build(&config));
        Ok(Self {
            bucket_table,
            config,
            tree,
        })
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
#[derive(Clone)]
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
        // stage 1 `GameState::with_rng` 在构造时按 D-028 deal protocol 消费
        // RNG（51 次 `next_u64` Fisher-Yates）发底牌 + post blinds + 5 张
        // runout board。`seed` 参数仅作为 `HandHistory.seed` 标签写入，不参与
        // 发牌——实际 randomness 全部来自 `rng`（D-028 字面）。
        let game_state = GameState::with_rng(&self.config, 0, rng);
        SimplifiedNlheState {
            game_state,
            action_history: Vec::new(),
            bucket_table: Arc::clone(&self.bucket_table),
            current_node_id: self.tree.root_id(),
            tree: Arc::clone(&self.tree),
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
        let actor_seat = SeatId(actor);
        let hole = state.game_state.players()[actor as usize]
            .hole_cards
            .expect("SimplifiedNlhe info_set: actor hole_cards must be present on decision node");
        let position_bucket = compute_position_bucket(&state.game_state, actor_seat);
        let stack_bucket = compute_stack_bucket(&state.game_state, actor_seat);
        let betting_state = compute_betting_state(&state.game_state);
        let street_tag = compute_street_tag(state.game_state.street());

        let base_bucket: u32 = match street_tag {
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

        let abs = DefaultActionAbstraction::default_5_action();
        let action_set = abs.abstract_actions(&state.game_state);
        let base_info = pack_info_set_id(
            base_bucket,
            position_bucket,
            stack_bucket,
            betting_state,
            street_tag,
        );
        let action_signature = action_semantic_signature(&state.game_state, action_set.as_slice());
        debug_assert!(action_signature < (1u32 << NLHE_ACTION_SIGNATURE_BITS));
        InfoSetId::from_raw_internal(
            base_info.raw() | (u64::from(action_signature) << NLHE_ACTION_SIGNATURE_SHIFT),
        )
    }

    fn legal_actions(state: &SimplifiedNlheState) -> Vec<SimplifiedNlheAction> {
        // D-318 桥接：stage 2 `DefaultActionAbstraction::abstract_actions`
        // 顺序由 D-209 deterministic（每次构造同型 5-action 抽象，开销可忽略
        // —— `DefaultActionAbstraction::new` 仅 clone 配置）；Trainer 的 RegretTable
        // `Vec<f64>` 索引一一对应（D-324 action_count 训练全程恒定）。
        let abs = DefaultActionAbstraction::default_5_action();
        let set = abs.abstract_actions(&state.game_state);
        set.as_slice().to_vec()
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
        let edge_idx = {
            let node = next_state.tree.node(next_state.current_node_id);
            node.legal_actions.iter().position(|t| *t == tag).expect(
                "Phase 2 invariant: action tag must appear in current node legal_actions; \
                     mismatch indicates CFR走了 tree 外动作 or tree builder 漏 edge",
            )
        };
        let child = next_state.tree.node(next_state.current_node_id).children[edge_idx];

        next_state
            .game_state
            .apply(concrete)
            .expect("SimplifiedNlhe next: AbstractAction → Action apply must be legal");
        next_state.action_history.push(action);

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

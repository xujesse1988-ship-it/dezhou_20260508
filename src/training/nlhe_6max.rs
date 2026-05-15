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
use crate::rules::action::Action;
use crate::rules::config::TableConfig;
use crate::rules::state::GameState;
use crate::training::game::{Game, NodeKind, PlayerId};

/// 6-max NLHE expected `BucketConfig`（D-424）：flop/turn/river 各 500 bucket，
/// stage 2 §G-batch1 §3.10 v3 production lock 字面继承（与 stage 3
/// [`crate::SimplifiedNlheGame`] 同 v3 artifact，[D-424 lock]）。
fn expected_bucket_config() -> BucketConfig {
    BucketConfig::new(500, 500, 500).expect("BucketConfig::new(500,500,500) within D-214 range")
}

/// 6-max NLHE expected `BucketTable` schema_version（D-424；继承 stage 3
/// [`crate::SimplifiedNlheGame`] 同 schema=2 字面）。
const EXPECTED_BUCKET_SCHEMA_VERSION: u32 = 2;

/// 6-max NLHE 默认 `TableConfig`（D-410 + stage 1 D-022）：6 座、起始 100BB、
/// SB=50 / BB=100 / ante=0 / button=seat 0。等价 [`TableConfig::default_6max_100bb`]
/// 但本模块独立构造一遍让 [`NlheGame6::new`] 在 stage 1 接口变更时影响面收窄。
fn default_6max_table_config() -> TableConfig {
    TableConfig {
        n_seats: 6,
        starting_stacks: vec![ChipAmount::new(10_000); 6],
        small_blind: ChipAmount::new(50),
        big_blind: ChipAmount::new(100),
        ante: ChipAmount::ZERO,
        button_seat: SeatId(0),
    }
}

/// HU 退化 [`TableConfig`]（D-416；与 stage 3 [`crate::SimplifiedNlheGame`] 同型
/// HU 100BB 起手栈，stage 1 D-022b-rev1 HU NLHE 语义）。
fn hu_table_config() -> TableConfig {
    TableConfig {
        n_seats: 2,
        starting_stacks: vec![ChipAmount::new(10_000); 2],
        small_blind: ChipAmount::new(50),
        big_blind: ChipAmount::new(100),
        ante: ChipAmount::ZERO,
        button_seat: SeatId(0),
    }
}

/// stage 4 D-424 v3 artifact 校验 — schema_version=2 + cluster (500,500,500)。
/// BLAKE3 v3 anchor (`67ee5554...`) 由 caller 通过 [`BucketTable::content_hash`]
/// 额外校验，本函数仅锁两个 metadata 字段。失败路径走
/// [`TrainerError::UnsupportedBucketTable`]，与 stage 3
/// [`crate::SimplifiedNlheGame::new`] 同型 variant 字面继承。
fn validate_bucket_table(bucket_table: &Arc<BucketTable>) -> Result<(), TrainerError> {
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
        // 也走该 variant；`got = 0` 让区分通过日志上下文判断，继承 stage 3
        // [`crate::SimplifiedNlheGame::new`] 同型政策）。
        return Err(TrainerError::UnsupportedBucketTable {
            expected: EXPECTED_BUCKET_SCHEMA_VERSION,
            got: 0,
        });
    }
    Ok(())
}

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
    /// **C2 \[实现\] 状态**（2026-05-15）：落地走与 stage 3
    /// [`crate::SimplifiedNlheGame::new`] 同型 schema_version + cluster
    /// (500,500,500) 校验 + n_seats=6 默认 6-max 100BB config + 默认
    /// [`PluribusActionAbstraction`]。
    pub fn new(bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        validate_bucket_table(&bucket_table)?;
        Ok(Self {
            bucket_table,
            action_abstraction: PluribusActionAbstraction,
            config: default_6max_table_config(),
        })
    }

    /// stage 4 D-416 — HU 退化路径（n_seats=2）。
    ///
    /// stage 3 [`crate::SimplifiedNlheGame`] 路径上的 1M update × 3 BLAKE3
    /// anchor 在 stage 4 commit 上必须 byte-equal 维持（即 `NlheGame6::new_hu`
    /// 配 `EsMccfrTrainer::new` 跑 1M update × 3 → 同 BLAKE3 与 stage 3
    /// `SimplifiedNlheGame` 配 `EsMccfrTrainer::new` 跑 1M update × 3 byte-equal）。
    /// C1 \[测试\] 钉死该不变量。
    ///
    /// **C2 \[实现\] 状态**（2026-05-15）：落地走 `hu_table_config()` 内部 helper
    /// (`n_seats=2` / 100 BB / SB=50 / BB=100 / button=seat 0) — 与 stage 3
    /// [`crate::SimplifiedNlheGame`] 默认 config 字面一致；HU 退化路径上同 master
    /// seed 配 [`crate::training::EsMccfrTrainer::new`] 跑 1M update × 3 BLAKE3
    /// 由 D1 \[测试\] 钉死 byte-equal 不变量。
    pub fn new_hu(bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        validate_bucket_table(&bucket_table)?;
        Ok(Self {
            bucket_table,
            action_abstraction: PluribusActionAbstraction,
            config: hu_table_config(),
        })
    }

    /// stage 4 D-410 — 通用 config 构造（test fixture / B1 \[测试\] 自定义
    /// n_seats 路径）。
    ///
    /// **C2 \[实现\] 状态**（2026-05-15）：走 `validate_bucket_table` 内部 helper
    /// 校验后接 caller 提供的 [`TableConfig`]；不校验 `config.n_seats` 范围
    /// （stage 1 `validate_config` 在 [`GameState::with_rng`] 调用时统一负责
    /// 校验 `n_seats ∈ [2, 9]` 范围）。
    pub fn with_config(
        bucket_table: Arc<BucketTable>,
        config: TableConfig,
    ) -> Result<Self, TrainerError> {
        validate_bucket_table(&bucket_table)?;
        Ok(Self {
            bucket_table,
            action_abstraction: PluribusActionAbstraction,
            config,
        })
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
    /// **C2 \[实现\] 状态**（2026-05-15）：stage 1 `GameState` 字面上
    /// `PlayerId == SeatId.0`（继承 stage 3 [`crate::SimplifiedNlheGame::info_set`]
    /// 同型 `let actor_seat = SeatId(actor); ...` 桥接政策），实际返回
    /// `seat_id.0` 即可；`state` 参数保留作为 stage 5+ multi-seat 重排后扩展
    /// 占位（C2 不消费，签名与 A1 字面一致让 api_signatures trip-wire 不漂移）。
    pub fn actor_at_seat(state: &NlheGame6State, seat_id: SeatId) -> PlayerId {
        let _ = state;
        seat_id.0
    }

    /// stage 4 D-423 — 14-action availability mask 计算（与
    /// [`InfoSetId::with_14action_mask`] 配对使用）。
    ///
    /// 走 [`PluribusActionAbstraction`] 输出的 14-action legal subset →
    /// `1 << PluribusAction tag` 累积。`(0..14)` 范围内的 14-bit mask。
    ///
    /// **C2 \[实现\] 状态**（2026-05-15）：调用静态
    /// [`PluribusActionAbstraction::actions`] 输出，按 `action as u8` 索引置位。
    pub fn compute_14action_mask(state: &GameState) -> u16 {
        let abstraction = PluribusActionAbstraction;
        let legal = abstraction.actions(state);
        let mut mask: u16 = 0;
        for action in legal {
            mask |= 1u16 << (action as u8);
        }
        debug_assert!(
            u32::from(mask) < (1u32 << 14),
            "compute_14action_mask: mask {mask} 越界 14 bit"
        );
        mask
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
        // C2 \[实现\]：6-player 主路径 = 6；HU 退化路径 [`NlheGame6::new_hu`] 走
        // `n_seats=2`，返 `config.n_seats as usize`（D-410 / D-416 字面）。
        self.config.n_seats as usize
    }

    fn root(&self, rng: &mut dyn RngSource) -> NlheGame6State {
        // stage 1 `GameState::with_rng` 按 D-028 发牌协议消费 RNG（Fisher-Yates
        // 洗牌 + 发 hole + 5 张 runout board），与 stage 3
        // [`crate::SimplifiedNlheGame::root`] 同型政策；`seed` 仅作为
        // `HandHistory.seed` 标签写入。`bucket_table` 走 Arc clone 让
        // `Game::info_set` 静态方法在 postflop 路径上访问 lookup 表。
        let game_state = GameState::with_rng(&self.config, 0, rng);
        NlheGame6State {
            game_state,
            action_history: Vec::new(),
            bucket_table: Arc::clone(&self.bucket_table),
        }
    }

    fn current(state: &NlheGame6State) -> NodeKind {
        // 6-player NLHE 没有独立 chance node（继承 stage 3
        // [`crate::SimplifiedNlheGame`] 同型政策）；`current` 仅返
        // Player / Terminal。
        if state.game_state.is_terminal() {
            return NodeKind::Terminal;
        }
        match state.game_state.current_player() {
            Some(seat) => NodeKind::Player(seat.0),
            None => NodeKind::Terminal,
        }
    }

    fn info_set(state: &NlheGame6State, actor: PlayerId) -> NlheGame6InfoSet {
        // D-423 字面：6-max NLHE 路径 InfoSetId 编码 =
        //   ① stage 2 `pack_info_set_id`(bucket_id, position, stack, betting,
        //      street) 基础 64-bit layout（preflop bucket_id = 169 lossless
        //      hand_class；postflop bucket_id = stage 2 BucketTable lookup）
        //   ② `.with_14action_mask(compute_14action_mask(state))` 写入 bits
        //      33..47（D-423 字面继承 [`InfoSetId::with_14action_mask`] 文档约束）
        let actor_seat = SeatId(actor);
        let hole = state.game_state.players()[actor as usize]
            .hole_cards
            .expect("NlheGame6 info_set: actor hole_cards must be present on decision node");
        let position_bucket = compute_position_bucket(&state.game_state, actor_seat);
        let stack_bucket = compute_stack_bucket(&state.game_state, actor_seat);
        let betting_state = compute_betting_state(&state.game_state);
        let street_tag = compute_street_tag(state.game_state.street());

        let bucket_id: u32 = match street_tag {
            StreetTag::Preflop => {
                let preflop = PreflopLossless169::new();
                u32::from(preflop.hand_class(hole))
            }
            StreetTag::Flop | StreetTag::Turn | StreetTag::River => {
                let observation =
                    canonical_observation_id(street_tag, state.game_state.board(), hole);
                state
                    .bucket_table
                    .lookup(street_tag, observation)
                    .expect("BucketTable::lookup returned None on in-range observation_id")
            }
        };

        let base = pack_info_set_id(
            bucket_id,
            position_bucket,
            stack_bucket,
            betting_state,
            street_tag,
        );
        let mask = NlheGame6::compute_14action_mask(&state.game_state);
        base.with_14action_mask(mask)
    }

    fn legal_actions(state: &NlheGame6State) -> Vec<NlheGame6Action> {
        // D-420 字面：走 [`PluribusActionAbstraction::actions`] 输出
        // 14-action legal subset。`Vec<PluribusAction>` 与 `Vec<NlheGame6Action>`
        // 字面等价（[`NlheGame6Action`] = type alias to [`PluribusAction`]）。
        let abstraction = PluribusActionAbstraction;
        abstraction.actions(&state.game_state)
    }

    fn next(
        state: NlheGame6State,
        action: NlheGame6Action,
        _rng: &mut dyn RngSource,
    ) -> NlheGame6State {
        // D-422 字面：[`PluribusAction`] → stage 1 [`Action`] 桥接 + stage 1
        // [`GameState::apply`]（继承 stage 3
        // [`crate::SimplifiedNlheGame::next`] 同型政策）。Raise X Pot 走
        // [`PluribusActionAbstraction::compute_raise_to`]（D-422 字面 floor
        // rounding 与 B1 [测试] `tests/nlhe_6max_raise_sizes.rs` ±1 chip 容差
        // 一致）；bet vs raise 分流由 stage 1 `LegalActionSet.bet_range` 决定。
        let mut next_state = state;
        let concrete: Action = match action {
            PluribusAction::Fold => Action::Fold,
            PluribusAction::Check => Action::Check,
            PluribusAction::Call => Action::Call,
            PluribusAction::AllIn => Action::AllIn,
            raise => {
                let mult = raise
                    .raise_multiplier()
                    .expect("non-raise variants matched above");
                let abstraction = PluribusActionAbstraction;
                let to = abstraction.compute_raise_to(&next_state.game_state, mult);
                let la = next_state.game_state.legal_actions();
                if la.bet_range.is_some() {
                    Action::Bet { to }
                } else {
                    Action::Raise { to }
                }
            }
        };
        next_state
            .game_state
            .apply(concrete)
            .expect("NlheGame6 next: PluribusAction → Action apply must be legal");
        next_state.action_history.push(action);
        next_state
    }

    fn chance_distribution(_state: &NlheGame6State) -> Vec<(NlheGame6Action, f64)> {
        // 6-player NLHE 没有独立 chance node（继承 stage 3
        // [`crate::SimplifiedNlheGame::chance_distribution`] 同型政策）；本方法
        // 不应被 ES-MCCFR / Vanilla CFR 触发，panic 让调用方立即看到 stack trace。
        panic!(
            "NlheGame6::chance_distribution called: 6-player NLHE has no in-game chance \
             nodes (all randomness consumed by Game::root via GameState::with_rng); \
             check `current(state)` returned NodeKind::Chance"
        );
    }

    fn payoff(state: &NlheGame6State, player: PlayerId) -> f64 {
        // D-316 chip 净收益直接当 utility（继承 stage 3
        // [`crate::SimplifiedNlheGame::payoff`] 同型政策）。stage 1
        // `GameState::payouts()` 在 terminal 时返 `Some(Vec<(SeatId, i64)>)`，
        // 每条 entry = `awards[seat] - committed[seat]`。
        let payouts = state
            .game_state
            .payouts()
            .expect("NlheGame6 payoff: state must be terminal (Game::current == Terminal)");
        let target_seat = SeatId(player);
        let pnl = payouts
            .into_iter()
            .find(|(seat, _)| *seat == target_seat)
            .map(|(_, pnl)| pnl)
            .expect("NlheGame6 payoff: payouts must include actor seat (stage 1 invariant)");
        pnl as f64
    }
}

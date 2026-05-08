//! 游戏状态机（API §4）。

use crate::core::rng::RngSource;
use crate::core::{Card, ChipAmount, Player, SeatId, Street};
use crate::error::RuleError;
use crate::history::HandHistory;
use crate::rules::action::{Action, LegalActionSet};
use crate::rules::config::TableConfig;

/// NLHE 6-max 状态机。
///
/// 内部字段不公开（API §4）。**关键不变量**（实现 agent 必须保证、测试 agent
/// 应在 invariant suite 中验证；详见 `docs/pluribus_stage1_api.md` §4）：
///
/// - **I-001** 任意时刻 `sum(player.stack) + pot() = sum(starting_stacks)`。
///   `TableConfig.starting_stacks` 是发盲注 / ante **之前** 的座位栈（D-024）；
///   盲注 / ante 是 stack→pot 的转移，总量守恒。
/// - **I-002** 任意 `Player.stack >= 0`（用 `u64` 表达自然成立，但减法路径
///   必须有下溢检查，见 [`ChipAmount`] D-026b 的 panic 语义）。
/// - **I-003** 任意一手内不出现重复 `Card`。
/// - **I-004** 每个 betting round 结束时，所有 `Active` 状态玩家的
///   `committed_this_round` 相等。
/// - **I-005** `apply` 失败时 `GameState` 不变（错误返回为纯函数式语义）。
/// - **I-006** 全员 all-in（除 ≤ 1 名 `Active` 外）后 `current_player == None`；
///   状态机会在同一 `apply` 调用内连续发完剩余公共牌、切 `Showdown`、设
///   `is_terminal == true`（多街快进，D-036）。
/// - **I-007** 终局必有获胜者（pot 必有归属）。
pub struct GameState {
    /// B2 阶段填入。当前为占位以保持类型不可外部构造。
    _placeholder: (),
}

impl GameState {
    /// 初始化一手新牌（生产路径）。
    ///
    /// 内部以 `ChaCha20Rng::from_seed(seed)` 构造 rng，按 D-028 发牌协议抽牌、
    /// 布盲、按钮位由 `config` 指定。`HandHistory.seed` 自动记为该 `seed`，
    /// `replay()` 即可复现。
    pub fn new(config: &TableConfig, seed: u64) -> GameState {
        let _ = (config, seed);
        unimplemented!()
    }

    /// 初始化一手新牌（测试 / fuzz 路径）。
    ///
    /// 注入自定义 [`RngSource`]，典型用于 stacked deck（构造指定牌序，参见 D-028）。
    /// `seed` 仅作为 `HandHistory.seed` 的标签写入，**不参与发牌**；调用方需自负
    /// rng 与 seed 的语义一致性 —— 若期望 `replay()` 能复现，则注入的 rng 必须等价于
    /// `ChaCha20Rng::from_seed(seed)`，否则 `replay()` 在底牌 / 公共牌校验阶段
    /// 会返回 [`HistoryError::ReplayDiverged`].
    ///
    /// **B1 推荐用法**：fuzz / 单元测试中使用 stacked rng + 固定 sentinel seed
    /// （如 `0`），并不要求 `replay()` 复现 —— stacked rng 用于构造指定牌序的
    /// fixed scenario，与 `replay()` 复现是两个独立用途。
    ///
    /// [`HistoryError::ReplayDiverged`]: crate::error::HistoryError::ReplayDiverged
    pub fn with_rng(config: &TableConfig, seed: u64, rng: &mut dyn RngSource) -> GameState {
        let _ = (config, seed, rng);
        unimplemented!()
    }

    /// 当前要行动的玩家。手牌结束 / 全员 all-in 跳轮时返回 `None`。
    pub fn current_player(&self) -> Option<SeatId> {
        unimplemented!()
    }

    /// 当前合法动作集合。无玩家行动时返回"空集合"（所有字段 false / None，LA-008）。
    pub fn legal_actions(&self) -> LegalActionSet {
        unimplemented!()
    }

    /// 应用一个动作。失败时返回错误，状态不改变（I-005）。
    pub fn apply(&mut self, action: Action) -> Result<(), RuleError> {
        let _ = action;
        unimplemented!()
    }

    pub fn street(&self) -> Street {
        unimplemented!()
    }

    /// 当前桌面公共牌（Flop=3, Turn=4, River=5）。
    pub fn board(&self) -> &[Card] {
        unimplemented!()
    }

    /// 当前总 pot（含主池 + 所有 side pot）。
    pub fn pot(&self) -> ChipAmount {
        unimplemented!()
    }

    /// 当前所有玩家状态快照（按 `SeatId` 排序）。
    pub fn players(&self) -> &[Player] {
        unimplemented!()
    }

    /// 牌局是否结束（已 showdown 或全员弃牌）。
    pub fn is_terminal(&self) -> bool {
        unimplemented!()
    }

    /// 终局每个玩家的净收益（正 = 赢、负 = 输）。仅 `is_terminal` 后有效。
    pub fn payouts(&self) -> Option<Vec<(SeatId, i64)>> {
        unimplemented!()
    }

    /// 当前 hand history 的引用，可随时序列化或回放。
    pub fn hand_history(&self) -> &HandHistory {
        unimplemented!()
    }
}

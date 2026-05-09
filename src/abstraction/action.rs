//! Action abstraction（API §1）。
//!
//! `AbstractAction` / `AbstractActionSet` / `ActionAbstractionConfig` / `BetRatio` /
//! `ActionAbstraction` trait + `DefaultActionAbstraction`。
//!
//! 不变量 AA-001..AA-008（含 AA-003-rev1 / AA-004-rev1）见
//! `docs/pluribus_stage2_api.md` §1；A1 阶段所有方法体走 `unimplemented!()`。

use thiserror::Error;

use crate::core::ChipAmount;
use crate::rules::action::Action;
use crate::rules::state::GameState;

use crate::abstraction::info::StreetTag;

/// 抽象动作。pot ratio 编码进 `Bet` / `Raise` 变体；apply 时取 `to`。
///
/// `Bet` 与 `Raise` 在构造时由 stage 1 `LegalActionSet`（LA-002 互斥）选定：
/// 本下注轮无前序 bet ⇒ `Bet`，已有前序 bet ⇒ `Raise`。该拆分让 `to_concrete()`
/// 无状态可调用（见 §7），同时 D-212 `betting_state` 字段在 `Bet` 与 `Raise`
/// 之间的转移无歧义（`Bet` 把 `Open` 推进到 `FacingBetNoRaise`；`Raise` 把任何
/// 状态推进到 `FacingRaise{1,2,3+}`）。
///
/// `ratio_label` 仅作为 InfoSet 编码区分性（D-207 / D-209），不参与 apply 计算。
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AbstractAction {
    Fold,
    Check,
    Call {
        to: ChipAmount,
    },
    /// 本下注轮无前序 bet（`legal_actions().bet_range.is_some()`）。
    Bet {
        to: ChipAmount,
        ratio_label: BetRatio,
    },
    /// 本下注轮已有前序 bet（`legal_actions().raise_range.is_some()`）。
    Raise {
        to: ChipAmount,
        ratio_label: BetRatio,
    },
    AllIn {
        to: ChipAmount,
    },
}

/// pot ratio 标签的整数编码，避免 `f64` 进入 `Eq` / `Hash`。
///
/// 内部存 `ratio × 1000` 的 `u32`（D-200 默认值：`Half = 500`、`Full = 1000`）。
/// `ActionAbstractionConfig` 接受 `f64` 输入但内部规整为该整数表示。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct BetRatio(u32);

impl BetRatio {
    pub const HALF_POT: BetRatio = BetRatio(500);
    pub const FULL_POT: BetRatio = BetRatio(1000);

    /// 量化协议（D-202-rev1 / BetRatio::from_f64-rev1）：
    ///
    /// 1. **rounding mode**：bankers-rounding (half-to-even)，
    ///    `(ratio * 1000.0).round_ties_even() as i64`，再校验范围。
    /// 2. **合法范围**：`ratio ∈ [0.001, 4_294_967.295]`（含端点），量化后
    ///    `u32 ∈ [1, u32::MAX]`；越界（< 0.001 / > 4_294_967.295 / NaN / Inf /
    ///    负数 / 0.0）返回 `None`。
    /// 3. **重复处理**：本函数本身不去重；多输入量化到同一 milli 值由
    ///    `ActionAbstractionConfig::new` 检测，返回 `ConfigError::DuplicateRatio`。
    pub fn from_f64(_ratio: f64) -> Option<BetRatio> {
        unimplemented!("A1 stub; B2 implements per D-202-rev1 / BetRatio::from_f64-rev1")
    }

    /// 返回内部整数表示（D-200，`milli = ratio × 1000`）。
    pub fn as_milli(self) -> u32 {
        unimplemented!("A1 stub; B2 implements")
    }
}

/// 抽象动作集合输出。顺序固定为 D-209：
/// `[Fold?, Check?, Call?, Bet(0.5×pot)? | Raise(0.5×pot)?, Bet(1.0×pot)? | Raise(1.0×pot)?, AllIn?]`
/// `?` 表示不存在则跳过；同一 ratio 槽位 `Bet` 与 `Raise` 互斥（由 stage 1
/// LA-002 保证）。
#[derive(Clone, Debug)]
pub struct AbstractActionSet {
    #[allow(dead_code)] // A1 stub; B2 fills.
    actions: Vec<AbstractAction>,
}

impl AbstractActionSet {
    pub fn iter(&self) -> std::slice::Iter<'_, AbstractAction> {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn len(&self) -> usize {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn is_empty(&self) -> bool {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn contains(&self, _action: AbstractAction) -> bool {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn as_slice(&self) -> &[AbstractAction] {
        unimplemented!("A1 stub; B2 implements")
    }
}

/// `ActionAbstractionConfig`：raise size 集合（D-202）。
/// `raise_pot_ratios` 长度 ∈ [1, 14]，每个元素 ∈ (0.0, +∞)。
#[derive(Clone, Debug)]
pub struct ActionAbstractionConfig {
    pub raise_pot_ratios: Vec<BetRatio>,
}

impl ActionAbstractionConfig {
    /// 默认 5-action 配置：`[BetRatio::HALF_POT, BetRatio::FULL_POT]`（D-200）。
    pub fn default_5_action() -> ActionAbstractionConfig {
        unimplemented!("A1 stub; B2 implements per D-200")
    }

    /// 自定义构造。长度 / 范围越界 / 量化后 milli 重复均返回 `ConfigError`
    /// （见 §9 BetRatio::from_f64-rev1 量化协议；D-202-rev1）。
    pub fn new(_raise_pot_ratios: Vec<f64>) -> Result<ActionAbstractionConfig, ConfigError> {
        unimplemented!("A1 stub; B2 implements per D-202-rev1")
    }

    pub fn raise_count(&self) -> usize {
        unimplemented!("A1 stub; B2 implements")
    }
}

/// 配置错误（D-202-rev1 含 `DuplicateRatio` 变体）。
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("raise_pot_ratios length out of range: expected [1, 14], got {0}")]
    RaiseCountOutOfRange(usize),

    #[error("raise pot ratio not positive finite: {0}")]
    RaiseRatioInvalid(f64),

    /// `BucketConfig::new` 越界：每条街 bucket 数应 ∈ [10, 10_000]（D-214）。
    #[error("bucket count out of range for {street:?}: expected [10, 10_000], got {got}")]
    BucketCountOutOfRange { street: StreetTag, got: u32 },

    /// 多个 `raise_pot_ratios` 元素经 `BetRatio::from_f64` 量化后落到同一 milli 值
    /// （D-202-rev1 / BetRatio::from_f64-rev1）。caller 责任去重，避免 D-209
    /// 输出顺序与 `raise_count()` 不一致。
    #[error("duplicate raise pot ratio after quantization: milli = {milli}")]
    DuplicateRatio { milli: u32 },
}

/// Action abstraction trait（API §1）。
pub trait ActionAbstraction: Send + Sync {
    /// 给定当前 `GameState`，返回抽象动作集合（D-200..D-209 全部 fallback 已应用）。
    fn abstract_actions(&self, state: &GameState) -> AbstractActionSet;

    /// off-tree action 映射（D-201 PHM stub；stage 2 仅占位实现，stage 6c 完整数值验证）。
    ///
    /// `real_to` 是对手实际下注的 `to` 字段（绝对金额，与 stage 1
    /// `Action::Bet/Raise { to }` 同语义）。
    fn map_off_tree(&self, state: &GameState, real_to: ChipAmount) -> AbstractAction;

    /// 配置只读访问。
    fn config(&self) -> &ActionAbstractionConfig;
}

/// 默认 5-action 抽象（D-200）。
pub struct DefaultActionAbstraction {
    #[allow(dead_code)] // A1 stub; B2 fills.
    config: ActionAbstractionConfig,
}

impl DefaultActionAbstraction {
    pub fn new(_config: ActionAbstractionConfig) -> DefaultActionAbstraction {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn default_5_action() -> DefaultActionAbstraction {
        unimplemented!("A1 stub; B2 implements per D-200")
    }
}

impl ActionAbstraction for DefaultActionAbstraction {
    fn abstract_actions(&self, _state: &GameState) -> AbstractActionSet {
        unimplemented!("A1 stub; B2 implements per D-200..D-209 + AA-003-rev1 / AA-004-rev1")
    }

    fn map_off_tree(&self, _state: &GameState, _real_to: ChipAmount) -> AbstractAction {
        unimplemented!("A1 stub; D-201 PHM stub; stage 6c 完整验证")
    }

    fn config(&self) -> &ActionAbstractionConfig {
        unimplemented!("A1 stub; B2 implements")
    }
}

// ===========================================================================
// §7 桥接：AbstractAction → stage 1 Action
// ===========================================================================

impl AbstractAction {
    /// `AbstractAction` → 实际可 apply 的 `Action`（stage 1 类型）。**无状态**——
    /// `AbstractAction::Bet` / `Raise` 在构造时已由 stage 1 `LegalActionSet` 区分，
    /// 转换无歧义。映射规则：
    ///
    /// - `Fold` → `Action::Fold`
    /// - `Check` → `Action::Check`
    /// - `Call { .. }` → `Action::Call`（stage 1 `Action::Call` 不带 `to`，跟注
    ///   金额由 state machine 推导）
    /// - `Bet { to, .. }` → `Action::Bet { to }`
    /// - `Raise { to, .. }` → `Action::Raise { to }`
    /// - `AllIn { .. }` → `Action::AllIn`（state machine 自动归一化，`to` 字段
    ///   作为 InfoSet 编码标签即可丢弃）
    pub fn to_concrete(self) -> Action {
        unimplemented!("A1 stub; B2 implements per API §7 字段提取规则")
    }
}

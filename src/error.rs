//! 公开错误类型（API §8）。

use thiserror::Error;

use crate::core::ChipAmount;
use crate::rules::action::Action;

/// 规则引擎与状态机错误。`apply` / `replay` 失败时返回。
#[derive(Debug, Error)]
pub enum RuleError {
    #[error("not the current player's turn")]
    NotPlayerTurn,

    #[error("hand already terminated")]
    HandTerminated,

    /// 动作种类与当前下注轮状态不匹配。典型情形：本轮已有 bet 时收到 `Bet`（应为 `Raise`）；
    /// 本轮无 bet 时收到 `Raise` 或 `Call`（应为 `Bet` 或 `Check`）；本轮已有 bet 时收到 `Check`。
    /// `reason` 为 `&'static str`，限定使用预定义的几种字面量，避免字符串拼接进入热路径。
    #[error("wrong action for state: {action:?} ({reason})")]
    WrongActionForState {
        action: Action,
        reason: &'static str,
    },

    /// 前序为 incomplete raise / short all-in，按 D-033 不重开 raise option，
    /// 但当前玩家尝试 `Raise`（无论是 `Action::Raise` 显式 raise，还是 `AllIn` 归一化后构成 raise）。
    #[error("raise option not reopened (previous raise was incomplete / short all-in)")]
    RaiseOptionNotReopened,

    /// raise 加注差额小于本轮最大有效加注差额（D-035），或首次 bet/raise 小于 BB（D-034）。
    #[error("min raise violation: required to >= {required:?}, got to = {got:?}")]
    MinRaiseViolation {
        required: ChipAmount,
        got: ChipAmount,
    },

    /// `to` 字段本身越界：`to <= committed_this_round_before`（动作扣款 ≤ 0），
    /// 或 `to > committed_this_round_before + stack`（超出剩余筹码）。
    #[error("invalid amount: {0:?}")]
    InvalidAmount(ChipAmount),

    /// 玩家剩余 stack 不足以执行该动作的扣款（典型见 `AllIn` 在 `stack == 0` 时被调用）。
    #[error("insufficient stack")]
    InsufficientStack,

    /// 兜底变体：上述具名变体未覆盖、但实现 / 测试代码确实需要拒绝的情况。
    /// **新增违规类型时优先升级为具名变体**，不要长期依赖该兜底（测试 agent 在 invariant
    /// suite 中不应基于 `reason` 字符串内容做断言）。
    #[error("illegal action: {reason}")]
    IllegalAction { reason: String },
}

/// HandHistory 序列化 / 反序列化 / 回放错误。
#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("schema version mismatch: found {found}, supported {supported}")]
    SchemaVersionMismatch { found: u32, supported: u32 },

    #[error("corrupted history: {0}")]
    Corrupted(String),

    #[error("invalid protobuf: {0}")]
    InvalidProto(String),

    #[error("replay diverged at action index {index}: {reason}")]
    ReplayDiverged { index: usize, reason: String },

    /// 记录的动作序列在 `actions[index]` 处被规则引擎拒绝（典型见 corrupted history、
    /// 跨版本不兼容残余、或上游写入端 bug）。`source` 携带底层 `RuleError`，可通过
    /// `std::error::Error::source()` 链式访问；外层 `HistoryError` 表明该错误发生在
    /// history replay 上下文（API-001-rev1）。
    #[error("replay action {index} rejected by rule engine")]
    Rule {
        index: usize,
        #[source]
        source: RuleError,
    },
}

//! 公开错误类型（API §8）。
//!
//! Stage 3 D-374 追加 [`CheckpointError`] + [`TrainerError`]（继承 stage 1 + stage 2
//! 错误追加不删模式）。具体签名锁在 `docs/pluribus_stage3_api.md` API-313 / API-351。

use std::path::PathBuf;

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

/// Checkpoint 读写错误（D-351 / API-351）。
///
/// 5 类错误对应阶段 3 决策 D-350 (schema_version 校验) / D-351 (5 variant) / D-352
/// (trailer BLAKE3 eager 校验) / D-356 (多 game 不兼容) / 通用 corruption。
///
/// 继承 stage 2 [`crate::BucketTableError`] 5 类形态。
#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("checkpoint file not found: {path:?}")]
    FileNotFound { path: PathBuf },

    #[error("checkpoint schema mismatch: expected {expected}, got {got}")]
    SchemaMismatch { expected: u32, got: u32 },

    #[error("checkpoint trainer mismatch: expected {expected:?}, got {got:?}")]
    TrainerMismatch {
        expected: (TrainerVariant, GameVariant),
        got: (TrainerVariant, GameVariant),
    },

    #[error("checkpoint bucket_table BLAKE3 mismatch: expected {expected:02x?}, got {got:02x?}")]
    BucketTableMismatch { expected: [u8; 32], got: [u8; 32] },

    #[error("checkpoint corrupted at offset {offset}: {reason}")]
    Corrupted { offset: u64, reason: String },
}

/// 训练运行时错误（D-324 / D-325 / D-330 / API-313）。
///
/// 5 类错误覆盖：① action_count 训练全程恒定约束（D-324）；② RSS 上界（D-325）；
/// ③ bucket table schema 版本不支持（D-323 / D-314）；④ regret matching 概率 sum
/// 容差越界（D-330 path.md §阶段 3 字面 `1e-9` 约束）；⑤ [`CheckpointError`] 传播
/// （`#[from]` propagate；让 `Trainer::save_checkpoint` 失败路径无须包装）。
///
/// 继承 stage 1 [`RuleError`] / [`HistoryError`] + stage 2
/// [`crate::BucketTableError`] / [`crate::EquityError`] 错误追加不删模式（D-374）。
#[derive(Debug, Error)]
pub enum TrainerError {
    #[error("info_set {info_set:?} action_count mismatch: expected {expected}, got {got}")]
    ActionCountMismatch {
        info_set: String,
        expected: usize,
        got: usize,
    },

    #[error("training process RSS {rss_bytes} exceeded limit {limit}")]
    OutOfMemory { rss_bytes: u64, limit: u64 },

    #[error("bucket table schema {got} not supported (expected {expected})")]
    UnsupportedBucketTable { expected: u32, got: u32 },

    #[error("regret matching probability sum {got} out of tolerance {tolerance}")]
    ProbabilitySumOutOfTolerance { got: f64, tolerance: f64 },

    #[error("checkpoint error: {0}")]
    Checkpoint(#[from] CheckpointError),
}

/// Trainer 变体 tag（API-350 binary schema offset 12 / D-356 跨 trainer 不兼容拒绝）。
///
/// 定义在 `src/error.rs` 而非 `src/training/checkpoint.rs` 以避免 `error.rs` ↔
/// `training/checkpoint.rs` 循环依赖（[`CheckpointError::TrainerMismatch`] 需引用该类型）。
/// `src/training/checkpoint.rs` 通过 `pub use crate::error::TrainerVariant;` 再导出，
/// 与 API-350 锁定的 `module: training::checkpoint` 公开路径保持一致。
#[repr(u8)]
#[derive(Clone, Copy, Eq, PartialEq, Debug, Hash)]
pub enum TrainerVariant {
    VanillaCfr = 0,
    EsMccfr = 1,
}

/// Game 变体 tag（API-350 binary schema offset 13 / D-356 跨 game 不兼容拒绝）。
///
/// 定义位置同 [`TrainerVariant`]，避免循环依赖。
#[repr(u8)]
#[derive(Clone, Copy, Eq, PartialEq, Debug, Hash)]
pub enum GameVariant {
    Kuhn = 0,
    Leduc = 1,
    SimplifiedNlhe = 2,
}

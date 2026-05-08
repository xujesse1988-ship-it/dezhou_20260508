//! 6-max NLHE poker AI — 阶段 1 crate。
//!
//! 此 crate 的公开类型与方法签名严格对应 `docs/pluribus_stage1_api.md`。
//! 当前阶段 (B2 pass 1)：已落地朴素规则状态机、side pot 结算、手牌评估器、
//! hand history protobuf roundtrip 与确定性 RNG。后续 C2 / D2 / E2 / F2
//! 继续补完整覆盖、规模 fuzz、性能 SLO 和兼容性错误路径。
//!
//! 模块组织（D-011）：
//! - [`core`]：基础类型 + 显式注入随机源
//! - [`rules`]：动作 / 桌面配置 / 状态机
//! - [`eval`]：手牌评估器接口
//! - [`history`]：手牌历史与回放
//! - [`error`]：公开错误类型

pub mod core;
pub mod error;
pub mod eval;
pub mod history;
pub mod rules;

// 顶层 re-export（与 `docs/pluribus_stage1_api.md` §9 保持一致）。
pub use crate::core::rng::{ChaCha20Rng, RngCoreAdapter, RngSource};
pub use crate::core::{
    Card, ChipAmount, Player, PlayerStatus, Position, Rank, SeatId, Street, Suit,
};
pub use crate::error::{HistoryError, RuleError};
pub use crate::eval::{HandCategory, HandEvaluator, HandRank};
pub use crate::history::{HandHistory, RecordedAction};
pub use crate::rules::action::{Action, LegalActionSet};
pub use crate::rules::config::TableConfig;
pub use crate::rules::state::GameState;

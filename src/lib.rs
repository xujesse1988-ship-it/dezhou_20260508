//! 6-max NLHE poker AI crate — stage 1 锁定 + stage 2 抽象层骨架。
//!
//! 公开 API 严格对应 `docs/pluribus_stage1_api.md`（§API-001..API-099 锁定）+
//! `docs/pluribus_stage2_api.md`（§API-200..API-302，A1 \[实现\] 阶段骨架，函数体
//! `unimplemented!()`，B2/C2/D2/E2/F2 逐步填充）。
//!
//! 阶段 1 模块（D-011，闭合于 `stage1-v1.0`）：
//! - [`core`]：基础类型 + 显式注入随机源
//! - [`rules`]：动作 / 桌面配置 / 状态机
//! - [`eval`]：手牌评估器接口
//! - [`history`]：手牌历史与回放
//! - [`error`]：公开错误类型
//!
//! 阶段 2 模块（A1 \[实现\] 起步，A0 决策已锁定）：
//! - [`abstraction`]：抽象层（action / info / preflop / postflop / equity /
//!   feature / cluster / bucket_table / map）

pub mod abstraction;
pub mod core;
pub mod error;
pub mod eval;
pub mod history;
pub mod rules;

// 阶段 1 顶层 re-export（与 `docs/pluribus_stage1_api.md` §9 保持一致）。
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

// 阶段 2 顶层 re-export（D-253-rev1，与 `docs/pluribus_stage2_api.md` §6 保持一致）。
pub use crate::abstraction::action::{
    AbstractAction, AbstractActionSet, ActionAbstraction, ActionAbstractionConfig, BetRatio,
    ConfigError, DefaultActionAbstraction,
};
pub use crate::abstraction::bucket_table::{
    BucketConfig, BucketTable, BucketTableError, TrainingMode,
};
pub use crate::abstraction::equity::{EquityCalculator, EquityError, MonteCarloEquity};
pub use crate::abstraction::info::{BettingState, InfoAbstraction, InfoSetId, StreetTag};
pub use crate::abstraction::postflop::{canonical_observation_id, PostflopBucketAbstraction};
pub use crate::abstraction::preflop::{canonical_hole_id, PreflopLossless169};

// D-228 公开 contract：sub-stream 派生函数 + op_id 命名常量。
// `cluster` / `feature` / `map` 子模块本身不顶层 re-export（D-254 内部子模块隔离）；
// `cluster::rng_substream` 例外，由 `lib.rs` 直接 `pub use` 暴露便于 [测试] 独立
// 构造 sub-stream 验证 byte-equal。
pub use crate::abstraction::cluster::rng_substream;

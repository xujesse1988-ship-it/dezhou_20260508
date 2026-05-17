//! Stage 3 训练层模块树（API-300..API-392，A1 \[实现\] 阶段骨架）。
//!
//! 公开类型 / trait / 方法签名严格对应 `docs/pluribus_stage3_api.md`。A1 阶段所有
//! 方法体走 `unimplemented!()` 占位（B2 / C2 / D2 / E2 / F2 逐步填充）。
//!
//! 模块组织（D-370 / D-374 / D-375 / D-376）：
//!
//! - [`game`]：`Game` trait + `NodeKind` / `PlayerId` 辅助类型
//! - [`kuhn`]：`KuhnGame` + `KuhnAction` + `KuhnInfoSet` + `KuhnHistory` + `KuhnState`
//! - [`leduc`]：`LeducGame` + `LeducAction` + `LeducInfoSet` + `LeducStreet` +
//!   `LeducHistory` + `LeducState`
//! - [`nlhe`]：`SimplifiedNlheGame` + `SimplifiedNlheState` + type alias 桥接 stage 2
//! - [`nlhe_eval`]：H3 blueprint-only baseline 评测 + local BR proxy
//! - [`regret`]：`RegretTable` + `StrategyAccumulator`（允许 `f64` 浮点；D-379）
//! - [`trainer`]：`Trainer` trait + `VanillaCfrTrainer` + `EsMccfrTrainer`
//! - [`sampling`]：`derive_substream_seed` + `sample_discrete` + 6 个 op_id 常量
//! - [`best_response`]：`BestResponse` trait + `KuhnBestResponse` + `LeducBestResponse` +
//!   `exploitability` 辅助函数
//! - [`checkpoint`]：`Checkpoint` binary schema + `save` / `open`
//!
//! 错误类型 [`crate::error::CheckpointError`] + [`crate::error::TrainerError`] 与
//! [`crate::error::TrainerVariant`] / [`crate::error::GameVariant`] 由 stage 3
//! D-374 锁在 `src/error.rs`（追加不删模式）；本子模块通过 `pub use` 再导出，让外部
//! 走 `poker::training::{Checkpoint, CheckpointError, TrainerError, ...}` 短路径访问，
//! 与 API-380 / API-350 公开路径一致。

pub mod best_response;
pub mod checkpoint;
pub mod game;
pub mod kuhn;
pub mod leduc;
pub mod nlhe;
pub mod nlhe_eval;
pub mod regret;
pub mod sampling;
pub mod trainer;

// API-300 / API-380 顶层公开 surface（与 `docs/pluribus_stage3_api.md` §8 对齐）。
pub use best_response::{exploitability, BestResponse, KuhnBestResponse, LeducBestResponse};
pub use checkpoint::Checkpoint;
pub use game::{Game, NodeKind, PlayerId};
pub use kuhn::{KuhnAction, KuhnGame, KuhnHistory, KuhnInfoSet, KuhnState};
pub use leduc::{LeducAction, LeducGame, LeducHistory, LeducInfoSet, LeducState, LeducStreet};
pub use nlhe::{
    SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet, SimplifiedNlheState,
};
pub use nlhe_eval::{
    estimate_simplified_nlhe_lbr, evaluate_blueprint_vs_baseline, NlheBaselinePolicy,
    NlheEvaluationConfig, NlheEvaluationReport, NlheLbrConfig, NlheLbrReport,
};
pub use regret::{RegretTable, StrategyAccumulator};
pub use trainer::{EsMccfrTrainer, Trainer, VanillaCfrTrainer};

// CheckpointError + TrainerError + TrainerVariant + GameVariant 物理位置在
// `src/error.rs`（D-374），逻辑路径 `poker::training::{Checkpoint, CheckpointError, ...}`
// 由本 `pub use` 暴露，与 API-313 / API-351 / API-350 公开路径一致。
pub use crate::error::{
    CheckpointError, GameVariant, NlheEvaluationError, TrainerError, TrainerVariant,
};

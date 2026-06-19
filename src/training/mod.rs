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
//! - [`monitor`]：6-max 训练收敛监控（average-regret / entropy / 动作概率震荡，S4）
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

pub mod aivat;
pub mod aivat_multiway;
pub mod aivat_nlhe;
pub mod aivat_value;
pub mod best_response;
pub mod blueprint_advisor;
pub mod checkpoint;
pub mod game;
pub mod kuhn;
pub mod lbr;
pub mod leduc;
pub mod monitor;
pub mod nlhe;
pub mod nlhe_betting_tree;
pub mod nlhe_dense;
pub mod nlhe_dense_checkpoint;
pub mod nlhe_dense_trainer;
pub mod nlhe_eval;
pub mod nlhe_replay;
pub mod openpoker_hh;
pub mod opponent_profile;
pub mod regret;
pub mod sampling;
pub mod subgame;
pub mod subgame_leaf_value;
pub mod trainer;

// API-300 / API-380 顶层公开 surface（与 `docs/pluribus_stage3_api.md` §8 对齐）。
pub use aivat::{enumerate_aivat_moments, exact_state_value, AivatMoments};
pub use best_response::{exploitability, BestResponse, KuhnBestResponse, LeducBestResponse};
pub use blueprint_advisor::{
    advance_shadow_by_applied, evaluate_cross_abstraction_h2h, outgoing_action,
    play_cross_abstraction_hand, Contestant, CrossAbstractionH2hReport, CrossH2hConfig, HandError,
    SearchObserver,
};
pub use checkpoint::Checkpoint;
pub use game::{Game, NodeKind, PlayerId};
pub use kuhn::{KuhnAction, KuhnGame, KuhnHistory, KuhnInfoSet, KuhnState};
pub use lbr::{estimate_lbr, estimate_lbr_filtered, LbrConfig, LbrReport};
pub use leduc::{LeducAction, LeducGame, LeducHistory, LeducInfoSet, LeducState, LeducStreet};
pub use monitor::{ConvergenceMonitor, MonitorReport, StrategySnapshot};
pub use nlhe::{
    SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet, SimplifiedNlheState,
};
pub use nlhe_eval::{
    estimate_simplified_nlhe_lbr, estimate_simplified_nlhe_lbr_filtered,
    evaluate_blueprint_vs_baseline, evaluate_blueprint_vs_baseline_multiway,
    evaluate_blueprint_vs_blueprint_multiway, NlheBaselinePolicy, NlheBlueprintH2hReport,
    NlheEvaluationConfig, NlheEvaluationReport, NlheLbrConfig, NlheLbrReport,
    NlheMultiwayEvalReport,
};
pub use regret::{RegretTable, StrategyAccumulator};
pub use subgame::{
    should_search, subgame_search, ResolveRoot, SearchTrigger, SubgameNlheGame, SubgameSearchConfig,
};
pub use subgame_leaf_value::{
    build_leaf_value_tables, default_continuations, BiasKind, ContinuationSpec, LeafValueTables,
};
pub use trainer::{EsMccfrTrainer, Trainer, VanillaCfrTrainer};

// CheckpointError + TrainerError + TrainerVariant + GameVariant 物理位置在
// `src/error.rs`（D-374），逻辑路径 `poker::training::{Checkpoint, CheckpointError, ...}`
// 由本 `pub use` 暴露，与 API-313 / API-351 / API-350 公开路径一致。
pub use crate::error::{
    CheckpointError, GameVariant, NlheEvaluationError, TrainerError, TrainerVariant,
};

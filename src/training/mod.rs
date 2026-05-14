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
pub mod regret;
pub mod sampling;
pub mod trainer;

// stage 4 D-410 / API-410 — `NlheGame6` 6-player NLHE Game trait impl + 14-action
// abstraction + 6-traverser routing + HU 退化路径（A1 \[实现\] scaffold；C2
// \[实现\] 起步前 lock 全 trait method 翻面）。
pub mod nlhe_6max;
// stage 4 D-450 / API-450 — `LbrEvaluator` Rust 自实现 + 6-traverser average
// LBR + OpenSpiel sanity export（A1 \[实现\] scaffold；E2 \[实现\] 落地）。
pub mod lbr;
// stage 4 D-460 / API-460 — Slumbot HTTP bridge + Head-to-Head 100K 手评测
// + OpenSpiel HU fallback（A1 \[实现\] scaffold；F2 \[实现\] 落地）。
pub mod slumbot_eval;
// stage 4 D-480 / API-480 — `Opponent6Max` trait + 3 baseline impl + 1M 手
// sanity 评测（A1 \[实现\] scaffold；F2 \[实现\] 落地）。
pub mod baseline_eval;
// stage 4 D-470 / API-470 — `TrainingMetrics` 9 字段 + `TrainingAlarm` 5
// variant + JSONL log（A1 \[实现\] scaffold；F2 \[实现\] 落地）。
pub mod metrics;

// API-300 / API-380 顶层公开 surface（与 `docs/pluribus_stage3_api.md` §8 对齐）。
pub use best_response::{exploitability, BestResponse, KuhnBestResponse, LeducBestResponse};
pub use checkpoint::Checkpoint;
pub use game::{Game, NodeKind, PlayerId};
pub use kuhn::{KuhnAction, KuhnGame, KuhnHistory, KuhnInfoSet, KuhnState};
pub use leduc::{LeducAction, LeducGame, LeducHistory, LeducInfoSet, LeducState, LeducStreet};
pub use nlhe::{
    SimplifiedNlheAction, SimplifiedNlheGame, SimplifiedNlheInfoSet, SimplifiedNlheState,
};
pub use regret::{RegretTable, StrategyAccumulator};
pub use trainer::{DecayStrategy, EsMccfrTrainer, Trainer, TrainerConfig, VanillaCfrTrainer};

// stage 4 公开 surface re-export（API-498 lib.rs surface lock）。
pub use baseline_eval::{
    evaluate_vs_baseline, BaselineEvalResult, CallStationOpponent, Opponent6Max, RandomOpponent,
    TagOpponent,
};
pub use lbr::{LbrEvaluator, LbrResult, SixTraverserLbrResult};
pub use metrics::{write_metrics_jsonl, MetricsCollector, TrainingAlarm, TrainingMetrics};
pub use nlhe_6max::{NlheGame6, NlheGame6Action, NlheGame6InfoSet, NlheGame6State};
pub use slumbot_eval::{
    Head2HeadResult, HuHandResult, OpenSpielHuBaseline, SlumbotBridge, SlumbotHandResult,
};

// CheckpointError + TrainerError + TrainerVariant + GameVariant 物理位置在
// `src/error.rs`（D-374），逻辑路径 `poker::training::{Checkpoint, CheckpointError, ...}`
// 由本 `pub use` 暴露，与 API-313 / API-351 / API-350 公开路径一致。
pub use crate::error::{CheckpointError, GameVariant, TrainerError, TrainerVariant};

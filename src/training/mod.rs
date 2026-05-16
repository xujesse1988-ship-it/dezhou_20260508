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

// stage 5 D-510 / API-500 — 紧凑 RegretTable + StrategyAccumulator（Open-
// addressed Robin Hood + FxHash + load factor 0.75 + SoA 三 Vec + capacity
// 2^20 起步；q15 quantization + per-row scale；section_bytes 给 D-540 内存
// SLO 测量路径）。A1 \[实现\] scaffold 全 `unimplemented!()` 占位；B2 \[实现\]
// 落地真实 probe + quant/dequant + SIMD path。**不替换** `regret.rs` 既有
// HashMap-backed RegretTable（stage 3 D-321-rev2 维持作为 fallback + ablation
// baseline + stage 4 schema=2 checkpoint 加载路径必需）。
pub mod regret_compact;
// stage 5 D-511 / API-501 — q15 quantization helper（per-row scale + RM+
// in-place clamp + Linear discounting lazy decay 路径）。A1 \[实现\] scaffold
// 全 `unimplemented!()` 占位；B2 \[实现\] 落地。
pub mod quantize;
// stage 5 D-512 / API-502 — 256 shard mmap + LRU 128 pin + Arc<RwLock> 并发
// 安全 + madvise(MADV_DONTNEED) eviction。A1 \[实现\] scaffold 全
// `unimplemented!()` 占位；C2 \[实现\] 落地真实 mmap-backed dispatch。
pub mod shard;
// stage 5 D-520 + D-521 / API-503 — 极负 regret pruning（阈值 -300M）+ 周期
// ε resurface（周期 1e7 iter / 比例 0.05 / reset -150M）。A1 \[实现\] scaffold
// 落地 `PruningConfig` 字段集 + `Default` impl 真值；`should_prune` /
// `resurface_pass` 占位；E2 \[实现\] 落地 step 路径接入。
pub mod pruning;

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

// stage 5 公开 surface re-export（API-500..API-579 字面）。
pub use pruning::{resurface_pass, should_prune, PruningConfig, ResurfaceMetrics};
pub use quantize::{
    compute_row_scale, dequantize_action, dequantize_row, f32_to_q15, q15_to_f32, quantize_row,
};
pub use regret_compact::{
    CollisionMetrics, RegretTableCompact, RegretTableCompactIter, StrategyAccumulatorCompact,
    StrategyAccumulatorCompactIter,
};
pub use shard::{
    shard_file_path, shard_id_from_info_set, RegretShard, ShardError, ShardLoader, ShardMetrics,
};
pub use trainer::EsMccfrLinearRmPlusCompactTrainer;

// CheckpointError + TrainerError + TrainerVariant + GameVariant 物理位置在
// `src/error.rs`（D-374），逻辑路径 `poker::training::{Checkpoint, CheckpointError, ...}`
// 由本 `pub use` 暴露，与 API-313 / API-351 / API-350 公开路径一致。
pub use crate::error::{CheckpointError, GameVariant, TrainerError, TrainerVariant};

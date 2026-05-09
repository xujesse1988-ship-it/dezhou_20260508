//! Stage 2 抽象层模块树（API-200..API-302，A1 \[实现\] 阶段骨架）。
//!
//! 公开类型 / trait / 方法签名严格对应 `docs/pluribus_stage2_api.md`，A1 阶段所有
//! 函数体走 `unimplemented!()` / `todo!()` 占位（B2 / C2 / D2 / E2 / F2 逐步填充）。
//!
//! 模块组织（D-211 / D-212 / D-215 / D-220 / D-244 / D-252 / D-253-rev1 / D-254）：
//!
//! - [`action`]：抽象动作集合 + `DefaultActionAbstraction`（5-action 默认）
//! - [`info`]：`InfoSetId` 64-bit 编码 + `InfoAbstraction` trait + `BettingState` /
//!   `StreetTag` enum
//! - [`preflop`]：169 lossless preflop 抽象 + `canonical_hole_id` helper
//! - [`postflop`]：mmap 后端 `PostflopBucketAbstraction` + `canonical_observation_id`
//!   helper
//! - [`equity`]：`EquityCalculator` trait + `MonteCarloEquity`（允许浮点）
//! - [`feature`]：特征提取（EHS² / OCHS / histogram）（允许浮点；模块私有）
//! - [`cluster`]：k-means / EMD 聚类（允许浮点；模块私有，但子模块
//!   [`cluster::rng_substream`] 公开 D-228 contract）
//! - [`bucket_table`]：mmap 文件格式 + `schema_version` + 错误路径
//! - [`map`]：运行时映射热路径子模块（**禁止浮点**，
//!   `#![deny(clippy::float_arithmetic)]`，D-252）
//!
//! D-254 内部子模块隔离：`cluster` / `feature` / `map` 不在 `lib.rs` 顶层 re-export，
//! 仅通过路径 `poker::abstraction::*` 访问；`cluster::rng_substream` 例外，由
//! `lib.rs` 直接 `pub use` 暴露（D-228 公开 contract）。

pub mod action;
pub mod bucket_table;
pub mod cluster;
pub mod equity;
pub mod feature;
pub mod info;
pub mod map;
pub mod postflop;
pub mod preflop;

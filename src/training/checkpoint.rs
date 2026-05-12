//! Checkpoint binary schema + save / open（API-350 / D-350..D-359）。
//!
//! 二进制 schema（API-350 binary layout）：
//!
//! | 字段 | 起始偏移 | 长度 | 编码 |
//! |---|---|---|---|
//! | `magic` | 0 | 8 | `b"PLCKPT\0\0"` |
//! | `schema_version` | 8 | 4 | u32 LE |
//! | `trainer_variant` | 12 | 1 | u8 |
//! | `game_variant` | 13 | 1 | u8 |
//! | `pad` | 14 | 6 | 0 |
//! | `update_count` | 20 | 8 | u64 LE |
//! | `rng_state` | 28 | 32 | bytes |
//! | `bucket_table_blake3` | 60 | 32 | bytes（Kuhn / Leduc 全零）|
//! | `regret_table_offset` | 92 | 8 | u64 LE（≥ 108）|
//! | `strategy_sum_offset` | 100 | 8 | u64 LE |
//! | `regret_table_body` | `regret_table_offset` | varies | bincode 1.x serialized HashMap |
//! | `strategy_sum_body` | `strategy_sum_offset` | varies | bincode 1.x serialized HashMap |
//! | `trailer_blake3` | `len - 32` | 32 | bytes |
//!
//! Header 实际 108 byte，8 byte aligned；bincode body 起点 = `regret_table_offset`
//! 字段值，由写入器精确控制。
//!
//! D-327 bincode serialize HashMap 走 InfoSet Debug-sort 顺序写入（确保 BLAKE3
//! byte-equal across hosts）。D-352 trailer BLAKE3 eager 校验；D-353 write-to-temp +
//! atomic rename；D-356 多 game 不兼容 → [`crate::error::CheckpointError::TrainerMismatch`]。
//!
//! A1 \[实现\] 阶段所有方法体 `unimplemented!()`；D2 \[实现\] 落地 save / open 全
//! 套（让 `tests/checkpoint_round_trip.rs::*` 通过）。

use std::path::Path;

use crate::error::CheckpointError;

// API-350 公开路径 `module: training::checkpoint`；TrainerVariant + GameVariant
// 物理位置在 `src/error.rs`（D-374 避免循环依赖），通过 `pub use` 再导出与
// API doc 对齐。
pub use crate::error::{GameVariant, TrainerVariant};

/// Checkpoint magic header（API-350 binary layout offset 0）。
///
/// 8 byte `b"PLCKPT\0\0"`；`PL` = Pluribus / `CKPT` = checkpoint / 后 2 byte
/// `\0\0` pad 让 magic 8 byte aligned 与 header 后续 8 byte aligned 字段对齐。
pub const MAGIC: [u8; 8] = *b"PLCKPT\0\0";

/// 当前 schema version（API-350 / D-350）。
///
/// 起步值 `1`；任何 schema 字段语义 / 顺序 / 长度变更必须 bump 该版本并在 D-350
/// 修订历史落地（与 stage 2 `BucketTable::SCHEMA_VERSION` 同型 bump 政策）。
pub const SCHEMA_VERSION: u32 = 1;

/// Checkpoint 二进制结构（API-350）。
///
/// 在内存中按 deserialized 形式持有；序列化 / 反序列化由 [`Checkpoint::save`] /
/// [`Checkpoint::open`] 串行执行。`regret_table_bytes` / `strategy_sum_bytes`
/// 是 bincode-serialized 子段，让 [`crate::training::RegretTable`] /
/// [`crate::training::StrategyAccumulator`] 在 Trainer 内部按需 `bincode::deserialize`
/// 重建（避免 [`Checkpoint`] 本身依赖泛型 `<I>`）。
#[derive(Clone, Debug)]
pub struct Checkpoint {
    pub schema_version: u32,
    pub trainer_variant: TrainerVariant,
    pub game_variant: GameVariant,
    pub update_count: u64,
    pub rng_state: [u8; 32],
    pub bucket_table_blake3: [u8; 32],
    pub regret_table_bytes: Vec<u8>,
    pub strategy_sum_bytes: Vec<u8>,
}

impl Checkpoint {
    /// 写出 checkpoint 到 `path`（D-353 write-to-temp + atomic rename + D-352
    /// trailer BLAKE3 + D-358 full snapshot 不做 incremental）。
    ///
    /// 失败路径：[`CheckpointError::Corrupted`]（I/O 失败 / 序列化失败 / atomic
    /// rename 失败均归类到 Corrupted 兜底，D2 \[实现\] 收紧到具体 sub-variant
    /// 时由 [`CheckpointError`] 修订历史追加）。
    pub fn save(&self, _path: &Path) -> Result<(), CheckpointError> {
        unimplemented!("stage 3 A1 scaffold: Checkpoint::save (D2 实现)")
    }

    /// 从 `path` 加载（D-352 eager BLAKE3 校验 + D-350 schema 校验 + D-356 多
    /// game 不兼容拒绝）。
    ///
    /// 失败路径覆盖 5 类 [`CheckpointError`]（D-351）：FileNotFound /
    /// SchemaMismatch / TrainerMismatch / BucketTableMismatch / Corrupted。
    pub fn open(_path: &Path) -> Result<Self, CheckpointError> {
        unimplemented!("stage 3 A1 scaffold: Checkpoint::open (D2 实现)")
    }
}

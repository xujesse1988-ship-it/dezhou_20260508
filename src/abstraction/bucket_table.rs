//! Bucket lookup table（API §4，mmap-backed）。
//!
//! `BucketConfig` + `BucketTable` + `BucketTableError`
//! （D-240..D-249，含 D-244-rev1 80-byte header 偏移表 / D-244-rev1 联合
//! observation 索引 / BT-005-rev1 / BT-008-rev1）。

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::abstraction::action::ConfigError;
use crate::abstraction::info::StreetTag;

/// 每条街 bucket 数（D-213 / D-214）。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct BucketConfig {
    pub flop: u32,
    pub turn: u32,
    pub river: u32,
}

impl BucketConfig {
    /// stage 2 默认验收配置（D-213）：`flop = turn = river = 500`。
    pub const fn default_500_500_500() -> BucketConfig {
        BucketConfig {
            flop: 500,
            turn: 500,
            river: 500,
        }
    }

    /// 校验每条街 ∈ [10, 10_000]（D-214）。任一越界返回
    /// `ConfigError::BucketCountOutOfRange`。
    pub fn new(_flop: u32, _turn: u32, _river: u32) -> Result<BucketConfig, ConfigError> {
        unimplemented!("A1 stub; B2 implements per D-214 [10, 10_000] 校验")
    }
}

/// mmap-backed bucket lookup table（D-240..D-249）。
///
/// 文件 layout（D-244 / D-244-rev1；80-byte 定长 header + 变长 body + 32-byte
/// trailer，全部 little-endian；reader 通过 header §⑨ 偏移表定位变长段，**不**
/// 依赖前段累积 size）：
///
/// ```text
/// // ===== header (80 bytes, 8-byte aligned) =====
/// offset 0x00: magic: [u8; 8] = b"PLBKT\0\0\0"                        // D-240
/// offset 0x08: schema_version: u32 LE = 1                             // D-240
/// offset 0x0C: feature_set_id: u32 LE = 1 (EHS² + OCHS(N=8))          // D-240
/// offset 0x10: bucket_count_flop:  u32 LE                             // D-214
/// offset 0x14: bucket_count_turn:  u32 LE
/// offset 0x18: bucket_count_river: u32 LE
/// offset 0x1C: n_canonical_observation_flop:   u32 LE                 // D-218-rev1 / D-244-rev1 / F19
/// offset 0x20: n_canonical_observation_turn:   u32 LE
/// offset 0x24: n_canonical_observation_river:  u32 LE
/// offset 0x28: n_dims:             u8                                 // D-221 (=9)
/// offset 0x29: pad:                [u8; 7] = 0                        // 8-byte align
/// offset 0x30: training_seed:      u64 LE                             // D-237
/// offset 0x38: centroid_metadata_offset: u64 LE                       // F13 (绝对偏移)
/// offset 0x40: centroid_data_offset:     u64 LE
/// offset 0x48: lookup_table_offset:      u64 LE
/// // ===== body (变长，按 header 偏移定位) =====
/// // centroid_metadata (3 streets × n_dims × (min: f32, max: f32))
/// // centroid_data     (3 streets × bucket_count(street) × n_dims × u8)  // D-241 / D-236b 重编号顺序
/// // lookup_table:
/// //   preflop:  [u32 LE; 1326]                                       // D-239 / D-245
/// //   flop:     [u32 LE; n_canonical_observation_flop]               // D-244-rev1
/// //   turn:     [u32 LE; n_canonical_observation_turn]
/// //   river:    [u32 LE; n_canonical_observation_river]
/// // ===== trailer (32 bytes) =====
/// // blake3: [u8; 32] = BLAKE3(file_body[..len-32])                   // D-243
/// ```
///
/// reader 必须按 §⑨ 偏移表定位变长段（不允许 const-bake 段 size 推算），
/// 任何 offset 越界 / 不递增 / 不 8-byte 对齐均视为
/// `BucketTableError::Corrupted`。
pub struct BucketTable {
    /// mmap 内部状态、缓存元数据（B2/C2 填充）。
    #[allow(dead_code)]
    _opaque: (),
}

impl BucketTable {
    /// **eager 校验**：mmap → 读 header → 校验 schema_version / feature_set_id /
    /// 文件总大小 → 计算 BLAKE3 trailer → 比对 → 任一失败立即返回错误。
    /// 全 5 类错误路径见 [`BucketTableError`]。
    pub fn open(_path: &Path) -> Result<BucketTable, BucketTableError> {
        unimplemented!("A1 stub; B2/C2 implements mmap + eager BLAKE3 校验")
    }

    /// `(street, observation_canonical_id) → bucket_id`（BT-005-rev1 /
    /// D-216-rev1 / D-218-rev1 / §9）。
    ///
    /// `observation_canonical_id` 来源：
    ///
    /// - **preflop**（`StreetTag::Preflop`）：= `canonical_hole_id(hole)` ∈ 0..1326；
    ///   不需要 board（preflop board 为空）。
    /// - **postflop**（`Flop` / `Turn` / `River`）：=
    ///   `canonical_observation_id(street, board, hole)` ∈
    ///   0..n_canonical_observation(street)；联合 (board, hole) 花色对称等价类。
    ///
    /// 越界返回 `None`（`observation_canonical_id >= n_canonical_observation(street)`
    /// 或 preflop `>= 1326`）。
    ///
    /// **接口接 [`StreetTag`]（不接 stage 1 `Street`）**——`StreetTag` 仅含 4 个
    /// betting 街变体，不含 `Showdown`。caller 必须在调用前把 `Street::Showdown`
    /// 局面分流（Showdown 不存在 InfoSet 决策点，调用 `lookup` 是语义错误）。
    pub fn lookup(&self, _street: StreetTag, _observation_canonical_id: u32) -> Option<u32> {
        unimplemented!("A1 stub; B2/C2 implements")
    }

    pub fn schema_version(&self) -> u32 {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn feature_set_id(&self) -> u32 {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn config(&self) -> BucketConfig {
        unimplemented!("A1 stub; B2 implements")
    }

    pub fn training_seed(&self) -> u64 {
        unimplemented!("A1 stub; B2 implements")
    }

    /// 每条街 bucket 数；`StreetTag::Preflop` 固定返回 169。
    pub fn bucket_count(&self, _street: StreetTag) -> u32 {
        unimplemented!("A1 stub; B2 implements")
    }

    /// 每条街联合 (board, hole) canonical observation id 总数（D-244-rev1）：
    /// preflop 固定返回 1326；postflop 返回 header `n_canonical_observation_<street>`。
    pub fn n_canonical_observation(&self, _street: StreetTag) -> u32 {
        unimplemented!("A1 stub; B2 implements per D-244-rev1")
    }

    /// 文件 BLAKE3 自校验值（D-243）。同 mmap 加载后 byte-equal。
    pub fn content_hash(&self) -> [u8; 32] {
        unimplemented!("A1 stub; B2 implements")
    }
}

/// bucket table 加载错误（D-247；5 类）。
#[derive(Debug, Error)]
pub enum BucketTableError {
    #[error("bucket table file not found: {path:?}")]
    FileNotFound { path: PathBuf },

    #[error("bucket table schema mismatch: expected {expected}, got {got}")]
    SchemaMismatch { expected: u32, got: u32 },

    #[error("bucket table feature_set_id mismatch: expected {expected}, got {got}")]
    FeatureSetMismatch { expected: u32, got: u32 },

    /// mmap 边界越界 / 文件被截断 / header 字段声明的 size 与实际文件不符。
    #[error("bucket table size mismatch: expected {expected} bytes, got {got}")]
    SizeMismatch { expected: u64, got: u64 },

    /// magic bytes 错误 / BLAKE3 trailer 不匹配 / 字段越界 / 内部不一致 /
    /// 偏移表违反 BT-008-rev1 不变量。
    #[error("bucket table corrupted at offset {offset}: {reason}")]
    Corrupted { offset: u64, reason: String },
}

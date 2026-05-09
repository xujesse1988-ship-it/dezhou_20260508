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
    pub fn new(flop: u32, turn: u32, river: u32) -> Result<BucketConfig, ConfigError> {
        for (street, got) in [
            (StreetTag::Flop, flop),
            (StreetTag::Turn, turn),
            (StreetTag::River, river),
        ] {
            if !(10..=10_000).contains(&got) {
                return Err(ConfigError::BucketCountOutOfRange { street, got });
            }
        }
        Ok(BucketConfig { flop, turn, river })
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
    /// 内部状态。B2 阶段仅 in-memory stub 路径填充（C2 接入真实 mmap 后切换到
    /// `Mmap` + 偏移寻址）。`stub_for_postflop` 构造的实例 lookup 永远返回
    /// `Some(0)`（与 §B2 §输出 line 274 "PostflopBucketAbstraction 占位实现：
    /// 每条街固定返回 bucket_id = 0" 协议匹配）。
    config: BucketConfig,
    schema_version: u32,
    feature_set_id: u32,
    training_seed: u64,
    n_canonical_flop: u32,
    n_canonical_turn: u32,
    n_canonical_river: u32,
    /// `true` 时 lookup 返回 `Some(0)`（B-rev0 carve-out option (1) stub 路径）。
    is_stub: bool,
}

impl BucketTable {
    /// **eager 校验**：mmap → 读 header → 校验 schema_version / feature_set_id /
    /// 文件总大小 → 计算 BLAKE3 trailer → 比对 → 任一失败立即返回错误。
    /// 全 5 类错误路径见 [`BucketTableError`]。
    pub fn open(_path: &Path) -> Result<BucketTable, BucketTableError> {
        // B2 stub: real mmap implementation lives in C2. B1 / B2 测试侧通过
        // `BucketTable::stub_for_postflop` 构造 in-memory 实例，不走 mmap。
        unimplemented!("B2 stub; C2 落地 mmap + eager BLAKE3 校验路径")
    }

    /// **B-rev0 carve-out option (1)**：test-only / B2 stub 构造路径。让
    /// `tests/info_id_encoding.rs::info_abs_postflop_bucket_id_in_range` 在 B2
    /// 闭合时取消 `#[ignore]` 后可调用 `PostflopBucketAbstraction::bucket_id`。
    /// `lookup` 永远返回 `Some(0)`（C2 接入真实 mmap 后由 §B2 line 274 协议
    /// 自动接管）。
    pub fn stub_for_postflop(config: BucketConfig) -> BucketTable {
        BucketTable {
            config,
            schema_version: 1,
            feature_set_id: 1,
            training_seed: 0,
            n_canonical_flop: 2_000_000,
            n_canonical_turn: 20_000_000,
            n_canonical_river: 200_000_000,
            is_stub: true,
        }
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
    pub fn lookup(&self, street: StreetTag, observation_canonical_id: u32) -> Option<u32> {
        let upper = self.n_canonical_observation(street);
        if observation_canonical_id >= upper {
            return None;
        }
        if self.is_stub {
            // B2 stub: §B2 line 274 协议——每条街固定返回 bucket_id = 0。
            return Some(0);
        }
        // C2 真实 mmap 路径占位（§A1 / §C2 接入）。
        unimplemented!("C2 real mmap lookup not yet landed")
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn feature_set_id(&self) -> u32 {
        self.feature_set_id
    }

    pub fn config(&self) -> BucketConfig {
        self.config
    }

    pub fn training_seed(&self) -> u64 {
        self.training_seed
    }

    /// 每条街 bucket 数；`StreetTag::Preflop` 固定返回 169。
    pub fn bucket_count(&self, street: StreetTag) -> u32 {
        match street {
            StreetTag::Preflop => 169,
            StreetTag::Flop => self.config.flop,
            StreetTag::Turn => self.config.turn,
            StreetTag::River => self.config.river,
        }
    }

    /// 每条街联合 (board, hole) canonical observation id 总数（D-244-rev1）：
    /// preflop 固定返回 1326；postflop 返回 header `n_canonical_observation_<street>`。
    pub fn n_canonical_observation(&self, street: StreetTag) -> u32 {
        match street {
            StreetTag::Preflop => 1326,
            StreetTag::Flop => self.n_canonical_flop,
            StreetTag::Turn => self.n_canonical_turn,
            StreetTag::River => self.n_canonical_river,
        }
    }

    /// 文件 BLAKE3 自校验值（D-243）。同 mmap 加载后 byte-equal。
    pub fn content_hash(&self) -> [u8; 32] {
        if self.is_stub {
            // Stub: deterministic constant value (no real mmap content to hash).
            [0u8; 32]
        } else {
            unimplemented!("C2 real mmap content_hash not yet landed")
        }
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

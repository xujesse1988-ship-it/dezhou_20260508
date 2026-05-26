//! Bucket lookup table（schema v4 / feature_set_id = 2 / 16-dim hist + OCHS）。
//!
//! `BucketConfig` + `BucketTable` + `BucketTableError` + `StreetFeaturesV3`。
//!
//! v4 与 v3 二进制 layout 完全相同；唯一区别是 lookup 段按 `canonical_enum` 的
//! shape-major direct combinatorial rank 编号 canonical observation id（2026-05
//! 重写）。feature 语义沿用 v3，严格对应 `docs/bucket_feature_design.md` §6.3 + §7：
//!
//! - feature 语义：flop / turn = `equity_hist_8 || OCHS_8` (16 维)；river = `OCHS_16` (16 维)
//! - OCHS warmup 走 postflop-histogram 路径（`docs/bucket_feature_design.md` §2.4）
//! - K-means 走 cluster.rs::kmeans_fit_production；当前阶段距离全 L2（hist 维度的
//!   1D-EMD ablation 在 §6.3 列为后续实验，不阻塞 Stage 1）
//! - centroid u8 per-dim min/max 量化（`cluster.rs::quantize_centroids_u8`）
//! - 文件 trailer = BLAKE3(file[..len-32])
//! - header 0x58 / 0x78 / 0x98 嵌入 features_<street>.bin 的 BLAKE3 形成可验证 hash chain
//!
//! schema v2 (9-dim EHS² + OCHS_8) / v3（旧 canonical id 编号）均已退出；reader
//! 拒绝 (schema_version, feature_set_id) ≠ (4, 2) 的 artifact（参
//! `docs/bucket_feature_design.md` §7）。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use blake3;
use rayon::prelude::*;
use thiserror::Error;

use crate::abstraction::action::ConfigError;
use crate::abstraction::cluster::{
    kmeans_fit_production, quantize_centroids_u8, reorder_by_ehs_median, rng_substream,
    KMeansConfig,
};
use crate::abstraction::info::StreetTag;
use crate::abstraction::postflop::{
    N_CANONICAL_OBSERVATION_FLOP, N_CANONICAL_OBSERVATION_RIVER, N_CANONICAL_OBSERVATION_TURN,
};
use crate::abstraction::preflop::canonical_hole_id;
use crate::core::Card;

// ============================================================================
// BucketConfig
// ============================================================================

/// 每条街 bucket 数。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct BucketConfig {
    pub flop: u32,
    pub turn: u32,
    pub river: u32,
}

impl BucketConfig {
    /// stage 2 验收配置：`flop = turn = river = 500`。
    pub const fn default_500_500_500() -> BucketConfig {
        BucketConfig {
            flop: 500,
            turn: 500,
            river: 500,
        }
    }

    /// 校验每条街 ∈ [10, 10_000]。
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

// ============================================================================
// schema 常量（v3 / feature_set_id = 2）
// ============================================================================

/// `magic: [u8; 8] = b"PLBKT\0\0\0"`。
pub const BUCKET_TABLE_MAGIC: [u8; 8] = *b"PLBKT\0\0\0";

/// schema 版本。
///
/// - v3 = 16 维 hist + OCHS feature 集，header 含 features_<street>.bin BLAKE3
///   hash chain（参 `docs/bucket_feature_design.md` §7）。
/// - v4（2026-05）= 二进制 layout 与 v3 完全相同，但 lookup 段的 canonical
///   observation id 编号改为 `canonical_enum` 的 shape-major direct combinatorial
///   rank。v3 表 lookup 行按旧「整表 sort」编号建，行↔等价类对应关系已变，语义不
///   兼容——reader 拒绝 v3 表（强制重算），避免静默读错 bucket。
pub const BUCKET_TABLE_SCHEMA_VERSION: u32 = 4;

/// 默认 feature_set_id。v3 schema 仅支持 `feature_set_id = 2`（16 维 hist + OCHS）。
/// 历史 `feature_set_id = 1`（9 维 EHS² + OCHS_8）已与 v2 schema 一同退出。
pub const BUCKET_TABLE_DEFAULT_FEATURE_SET_ID: u32 = 2;

/// `feature_set_id = 2` 对应的 centroid 维度（`docs/bucket_feature_design.md` §2 表）。
pub const BUCKET_TABLE_FEATURE_SET_2_DIMS: u8 = 16;

/// header 长度。v3 = 0xB8 = 184 字节（原 80 字节 v2 header + 8 字节 pad + 3 个 32-byte
/// feature_<street>_blake3）。
pub const BUCKET_TABLE_HEADER_LEN: u64 = 0xB8;

/// trailer BLAKE3 长度。
pub const BUCKET_TABLE_TRAILER_LEN: u64 = 32;

/// preflop lookup 段固定长度。
pub const PREFLOP_LOOKUP_LEN: u32 = 1326;

// header 内部字段偏移（v3）
const HDR_OFF_MAGIC: usize = 0x00;
const HDR_OFF_SCHEMA_VERSION: usize = 0x08;
const HDR_OFF_FEATURE_SET_ID: usize = 0x0C;
const HDR_OFF_BUCKET_COUNT_FLOP: usize = 0x10;
const HDR_OFF_BUCKET_COUNT_TURN: usize = 0x14;
const HDR_OFF_BUCKET_COUNT_RIVER: usize = 0x18;
const HDR_OFF_N_CANONICAL_FLOP: usize = 0x1C;
const HDR_OFF_N_CANONICAL_TURN: usize = 0x20;
const HDR_OFF_N_CANONICAL_RIVER: usize = 0x24;
const HDR_OFF_N_DIMS: usize = 0x28;
// 0x29..0x30 pad (7 bytes) = 0
const HDR_OFF_TRAINING_SEED: usize = 0x30;
const HDR_OFF_CENTROID_METADATA_OFFSET: usize = 0x38;
const HDR_OFF_CENTROID_DATA_OFFSET: usize = 0x40;
const HDR_OFF_LOOKUP_TABLE_OFFSET: usize = 0x48;
// 0x50..0x58 pad (8 bytes) = 0
const HDR_OFF_FEATURE_FLOP_BLAKE3: usize = 0x58;
const HDR_OFF_FEATURE_TURN_BLAKE3: usize = 0x78;
const HDR_OFF_FEATURE_RIVER_BLAKE3: usize = 0x98;
// 0x98 + 32 = 0xB8 = 184 = header end

// ============================================================================
// 输入结构：v3 训练所需的 per-street feature 包
// ============================================================================

/// 单街 v3 训练输入。caller 责任：
///
/// - `features_f32.len() == n_canonical_observation(street)`，行 i 对应
///   `canonical_enum::nth_canonical_form(street, i)`。
/// - `reorder_key_ehs.len() == features_f32.len()`；每个 sample 一个 `[0.0, 1.0]`
///   范围内的 EHS 标量，D-236b 重编号按 cluster 内 median 升序（cluster 0 = weakest）。
///   - flop / turn 推荐：hist 一阶矩 = Σ_k p_k · (k + 0.5) / 8（从 features 维度
///     `0..8` 直接算）
///   - river：`equity::equity_river_exact` 输出
/// - `feature_blake3` 必须等于 `features_<street>.bin` 实际文件 BLAKE3（v3 artifact
///   header 0x58 / 0x78 / 0x98 直接嵌入用，形成可验证 hash chain；training-time 与
///   runtime 校验责任划分参 `docs/bucket_feature_design.md` §7 末表）。
pub struct StreetFeaturesV3 {
    pub features_f32: Vec<[f32; 16]>,
    pub reorder_key_ehs: Vec<f64>,
    pub feature_blake3: [u8; 32],
}

// ============================================================================
// BucketTable
// ============================================================================

/// bucket lookup table（schema v4）。
///
/// 文件 layout（184-byte 定长 header + 变长 body + 32-byte trailer，全部 little-
/// endian）：
///
/// ```text
/// // ===== header (184 bytes = 0xB8, 8-byte aligned) =====
/// offset 0x00: magic: [u8; 8] = b"PLBKT\0\0\0"
/// offset 0x08: schema_version: u32 LE = 4
/// offset 0x0C: feature_set_id: u32 LE = 2 (16 dim hist + OCHS)
/// offset 0x10: bucket_count_flop:  u32 LE
/// offset 0x14: bucket_count_turn:  u32 LE
/// offset 0x18: bucket_count_river: u32 LE
/// offset 0x1C: n_canonical_observation_flop:   u32 LE
/// offset 0x20: n_canonical_observation_turn:   u32 LE
/// offset 0x24: n_canonical_observation_river:  u32 LE
/// offset 0x28: n_dims:             u8 = 16
/// offset 0x29: pad:                [u8; 7] = 0
/// offset 0x30: training_seed:      u64 LE
/// offset 0x38: centroid_metadata_offset: u64 LE
/// offset 0x40: centroid_data_offset:     u64 LE
/// offset 0x48: lookup_table_offset:      u64 LE
/// offset 0x50: pad:                [u8; 8] = 0
/// offset 0x58: feature_flop_blake3:  [u8; 32]
/// offset 0x78: feature_turn_blake3:  [u8; 32]
/// offset 0x98: feature_river_blake3: [u8; 32]
/// // ===== body (变长，按 header 偏移定位) =====
/// // centroid_metadata (3 streets × 16 × (min: f32, max: f32))
/// // centroid_data     (3 streets × bucket_count(street) × 16 × u8)
/// // lookup_table:
/// //   preflop:  [u32 LE; 1326]   (= hand_class_169 / lossless)
/// //   flop:     [u32 LE; n_canonical_observation_flop]
/// //   turn:     [u32 LE; n_canonical_observation_turn]
/// //   river:    [u32 LE; n_canonical_observation_river]
/// // ===== trailer (32 bytes) =====
/// // blake3: [u8; 32] = BLAKE3(file_body[..len-32])
/// ```
///
/// reader 按 header 偏移表定位变长段，任何 offset 越界 / 不递增 / 不 8-byte 对齐 /
/// 全文件 BLAKE3 不匹配 / `(schema_version, feature_set_id) ≠ (4, 2)` 均视为
/// `BucketTableError::Corrupted` / `SchemaMismatch` / `FeatureSetMismatch`。
pub struct BucketTable {
    config: BucketConfig,
    schema_version: u32,
    feature_set_id: u32,
    training_seed: u64,
    n_canonical_flop: u32,
    n_canonical_turn: u32,
    n_canonical_river: u32,
    is_stub: bool,
    raw: Option<BucketTableRaw>,
}

struct BucketTableRaw {
    bytes: Vec<u8>,
    #[allow(dead_code)]
    centroid_metadata_offset: u64,
    #[allow(dead_code)]
    centroid_data_offset: u64,
    #[allow(dead_code)]
    lookup_table_offset: u64,
    preflop_offset_in_lookup: u64,
    flop_offset_in_lookup: u64,
    turn_offset_in_lookup: u64,
    river_offset_in_lookup: u64,
    content_hash: [u8; 32],
}

impl BucketTableRaw {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn lookup_offsets(&self) -> (u64, u64, u64, u64) {
        (
            self.preflop_offset_in_lookup,
            self.flop_offset_in_lookup,
            self.turn_offset_in_lookup,
            self.river_offset_in_lookup,
        )
    }
    fn content_hash(&self) -> [u8; 32] {
        self.content_hash
    }
}

impl BucketTable {
    /// 整段 `std::fs::read` 加载 + eager 校验。runtime 责任见
    /// `docs/bucket_feature_design.md` §7 末表（仅校验 schema + trailer + 文件自完整性；
    /// feature hash chain 不校验，由离线工具承担）。
    pub fn open(path: &Path) -> Result<BucketTable, BucketTableError> {
        let bytes = std::fs::read(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BucketTableError::FileNotFound {
                    path: path.to_path_buf(),
                }
            } else {
                BucketTableError::Corrupted {
                    offset: 0,
                    reason: format!("io error opening file: {e}"),
                }
            }
        })?;
        Self::from_bytes(bytes)
    }

    /// 把 BucketTable 字节内容写到 `path`（先写 `<path>.tmp` 再 rename，原子）。
    pub fn write_to_path(&self, path: &Path) -> Result<(), std::io::Error> {
        let bytes = match &self.raw {
            Some(raw) => raw.bytes(),
            None => panic!("BucketTable::write_to_path called on stub instance"),
        };
        let tmp_path = {
            let mut p = path.to_path_buf();
            let mut name = p
                .file_name()
                .map(|n| n.to_owned())
                .unwrap_or_else(|| std::ffi::OsString::from("bucket_table.bin"));
            name.push(".tmp");
            p.set_file_name(name);
            p
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        {
            let mut f = File::create(&tmp_path)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// `(street, observation_canonical_id) → bucket_id`。越界返回 `None`。
    pub fn lookup(&self, street: StreetTag, observation_canonical_id: u32) -> Option<u32> {
        let upper = self.n_canonical_observation(street);
        if observation_canonical_id >= upper {
            return None;
        }
        if self.is_stub {
            return Some(0);
        }
        let raw = self.raw.as_ref().expect("non-stub must hold raw");
        let bytes = raw.bytes();
        let (preflop_off, flop_off, turn_off, river_off) = raw.lookup_offsets();
        let entry_off = match street {
            StreetTag::Preflop => preflop_off + (observation_canonical_id as u64) * 4,
            StreetTag::Flop => flop_off + (observation_canonical_id as u64) * 4,
            StreetTag::Turn => turn_off + (observation_canonical_id as u64) * 4,
            StreetTag::River => river_off + (observation_canonical_id as u64) * 4,
        };
        let id = read_u32_le(bytes, entry_off as usize);
        Some(id)
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

    /// 每条街 canonical observation 数。preflop 固定 1326。
    pub fn n_canonical_observation(&self, street: StreetTag) -> u32 {
        match street {
            StreetTag::Preflop => PREFLOP_LOOKUP_LEN,
            StreetTag::Flop => self.n_canonical_flop,
            StreetTag::Turn => self.n_canonical_turn,
            StreetTag::River => self.n_canonical_river,
        }
    }

    /// 文件 BLAKE3 自校验值（trailer）。stub 返回 `[0; 32]`。
    pub fn content_hash(&self) -> [u8; 32] {
        if self.is_stub {
            [0u8; 32]
        } else {
            self.raw
                .as_ref()
                .expect("non-stub must hold raw")
                .content_hash()
        }
    }

    /// header 内 features_<street>.bin BLAKE3 hash（v3 hash chain 字段）。
    /// preflop / stub 返回 `[0; 32]`（preflop 走 lossless 169，无 feature file 依赖）。
    pub fn feature_blake3(&self, street: StreetTag) -> [u8; 32] {
        if self.is_stub || street == StreetTag::Preflop {
            return [0u8; 32];
        }
        let raw = self.raw.as_ref().expect("non-stub must hold raw");
        let bytes = raw.bytes();
        let off = match street {
            StreetTag::Flop => HDR_OFF_FEATURE_FLOP_BLAKE3,
            StreetTag::Turn => HDR_OFF_FEATURE_TURN_BLAKE3,
            StreetTag::River => HDR_OFF_FEATURE_RIVER_BLAKE3,
            StreetTag::Preflop => unreachable!(),
        };
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes[off..off + 32]);
        out
    }

    /// test fixture：返回 `lookup` 永远 `Some(0)` 的 stub instance。
    pub fn stub_for_postflop(config: BucketConfig) -> BucketTable {
        BucketTable {
            config,
            schema_version: BUCKET_TABLE_SCHEMA_VERSION,
            feature_set_id: BUCKET_TABLE_DEFAULT_FEATURE_SET_ID,
            training_seed: 0,
            n_canonical_flop: N_CANONICAL_OBSERVATION_FLOP,
            n_canonical_turn: N_CANONICAL_OBSERVATION_TURN,
            n_canonical_river: N_CANONICAL_OBSERVATION_RIVER,
            is_stub: true,
            raw: None,
        }
    }

    /// v3 in-memory 训练。
    ///
    /// 三街独立 k-means + D-236b reorder + u8 centroid 量化，组装成完整 v3 artifact。
    /// 同 (config, training_seed, flop/turn/river feature 内容 + feature_blake3) 输入下
    /// artifact byte-equal。
    ///
    /// 输入校验（panic if violated）：
    /// - 每街 `features_f32.len() == n_canonical_observation(street)`
    /// - 每街 `reorder_key_ehs.len() == features_f32.len()`
    /// - `config` 每街 ∈ [10, 10_000]
    pub fn train_v3_in_memory(
        config: BucketConfig,
        training_seed: u64,
        flop: StreetFeaturesV3,
        turn: StreetFeaturesV3,
        river: StreetFeaturesV3,
    ) -> BucketTable {
        assert_eq!(
            flop.features_f32.len(),
            N_CANONICAL_OBSERVATION_FLOP as usize,
            "flop features count != n_canonical_observation_flop"
        );
        assert_eq!(
            turn.features_f32.len(),
            N_CANONICAL_OBSERVATION_TURN as usize,
            "turn features count != n_canonical_observation_turn"
        );
        assert_eq!(
            river.features_f32.len(),
            N_CANONICAL_OBSERVATION_RIVER as usize,
            "river features count != n_canonical_observation_river"
        );

        let flop_blake3 = flop.feature_blake3;
        let turn_blake3 = turn.feature_blake3;
        let river_blake3 = river.feature_blake3;

        let train_flop = train_one_street_v3(StreetTag::Flop, config.flop, training_seed, flop);
        let train_turn = train_one_street_v3(StreetTag::Turn, config.turn, training_seed, turn);
        let train_river = train_one_street_v3(StreetTag::River, config.river, training_seed, river);

        let bytes = build_bucket_table_v3_bytes(
            config,
            training_seed,
            flop_blake3,
            turn_blake3,
            river_blake3,
            train_flop,
            train_turn,
            train_river,
        );
        Self::from_bytes(bytes).expect("build_bucket_table_v3_bytes 自洽产物 byte-validate 应成功")
    }

    /// 测试专用：deterministic v3 synthetic fixture（不调 k-means，无需 features）。
    ///
    /// 用法场景：`tests/bucket_table_corruption.rs` / `tests/bucket_table_schema_compat.rs`
    /// 等需要 v3 artifact 但不需要真实 cluster 语义的测试。
    ///
    /// 算法：
    /// - centroids[c][d] = `((seed ⊕ ((c<<5)|d)).rotate_left(d)).rem(256) / 255.0`（确定性）
    /// - lookup[id] = Knuth hash `(id * 2654435761) % K`
    /// - feature_blake3[street] = BLAKE3(seed.to_le_bytes() ++ street_code.to_le_bytes())
    /// - centroid 量化 u8 经 `quantize_centroids_u8` 正常路径
    ///
    /// byte-equal: 同 (config, seed) → 同 BucketTable。
    pub fn synthetic_v3_for_tests(config: BucketConfig, seed: u64) -> BucketTable {
        let mut streets: Vec<StreetTrainingV3> = Vec::with_capacity(3);
        let mut blake3_chain: Vec<[u8; 32]> = Vec::with_capacity(3);
        for (street, n_canonical, k) in [
            (StreetTag::Flop, N_CANONICAL_OBSERVATION_FLOP, config.flop),
            (StreetTag::Turn, N_CANONICAL_OBSERVATION_TURN, config.turn),
            (
                StreetTag::River,
                N_CANONICAL_OBSERVATION_RIVER,
                config.river,
            ),
        ] {
            let mut centroids: Vec<Vec<f64>> = vec![vec![0.0_f64; 16]; k as usize];
            for (c, row) in centroids.iter_mut().enumerate() {
                for (d, slot) in row.iter_mut().enumerate() {
                    let key =
                        seed ^ (((c as u64) << 8) | (d as u64)) ^ (street_code(street) as u64);
                    let mix = key.rotate_left((d as u32) % 64);
                    *slot = ((mix % 256) as f64) / 255.0;
                }
            }
            let (q, min_per_dim, max_per_dim) = quantize_centroids_u8(&centroids);
            let lookup_table: Vec<u32> = (0..n_canonical)
                .map(|id| ((id as u64).wrapping_mul(2654435761) % (k as u64)) as u32)
                .collect();
            streets.push(StreetTrainingV3 {
                centroids_quantized: q,
                centroid_min_per_dim: min_per_dim,
                centroid_max_per_dim: max_per_dim,
                lookup_table,
            });
            let mut hasher = blake3::Hasher::new();
            hasher.update(&seed.to_le_bytes());
            hasher.update(&street_code(street).to_le_bytes());
            blake3_chain.push(*hasher.finalize().as_bytes());
        }
        let mut iter = streets.into_iter();
        let flop_train = iter.next().expect("3 streets pushed");
        let turn_train = iter.next().expect("3 streets pushed");
        let river_train = iter.next().expect("3 streets pushed");
        let bytes = build_bucket_table_v3_bytes(
            config,
            seed,
            blake3_chain[0],
            blake3_chain[1],
            blake3_chain[2],
            flop_train,
            turn_train,
            river_train,
        );
        Self::from_bytes(bytes).expect("synthetic v3 fixture byte-validate 应成功")
    }

    fn from_bytes(bytes: Vec<u8>) -> Result<BucketTable, BucketTableError> {
        let bytes_slice: &[u8] = &bytes;
        let len = bytes_slice.len() as u64;
        if len < BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN {
            return Err(BucketTableError::SizeMismatch {
                expected: BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN,
                got: len,
            });
        }

        if bytes_slice[HDR_OFF_MAGIC..HDR_OFF_MAGIC + 8] != BUCKET_TABLE_MAGIC {
            return Err(BucketTableError::Corrupted {
                offset: HDR_OFF_MAGIC as u64,
                reason: "magic bytes mismatch".into(),
            });
        }
        let schema_version = read_u32_le(bytes_slice, HDR_OFF_SCHEMA_VERSION);
        if schema_version != BUCKET_TABLE_SCHEMA_VERSION {
            return Err(BucketTableError::SchemaMismatch {
                expected: BUCKET_TABLE_SCHEMA_VERSION,
                got: schema_version,
            });
        }
        let feature_set_id = read_u32_le(bytes_slice, HDR_OFF_FEATURE_SET_ID);
        if feature_set_id != BUCKET_TABLE_DEFAULT_FEATURE_SET_ID {
            return Err(BucketTableError::FeatureSetMismatch {
                expected: BUCKET_TABLE_DEFAULT_FEATURE_SET_ID,
                got: feature_set_id,
            });
        }
        let bucket_count_flop = read_u32_le(bytes_slice, HDR_OFF_BUCKET_COUNT_FLOP);
        let bucket_count_turn = read_u32_le(bytes_slice, HDR_OFF_BUCKET_COUNT_TURN);
        let bucket_count_river = read_u32_le(bytes_slice, HDR_OFF_BUCKET_COUNT_RIVER);
        for (field, val) in [
            ("bucket_count_flop", bucket_count_flop),
            ("bucket_count_turn", bucket_count_turn),
            ("bucket_count_river", bucket_count_river),
        ] {
            if !(10..=10_000).contains(&val) {
                return Err(BucketTableError::Corrupted {
                    offset: HDR_OFF_BUCKET_COUNT_FLOP as u64,
                    reason: format!("{field} out of range: expected [10, 10_000], got {val}"),
                });
            }
        }
        let n_canonical_flop = read_u32_le(bytes_slice, HDR_OFF_N_CANONICAL_FLOP);
        let n_canonical_turn = read_u32_le(bytes_slice, HDR_OFF_N_CANONICAL_TURN);
        let n_canonical_river = read_u32_le(bytes_slice, HDR_OFF_N_CANONICAL_RIVER);
        if n_canonical_flop != N_CANONICAL_OBSERVATION_FLOP
            || n_canonical_turn != N_CANONICAL_OBSERVATION_TURN
            || n_canonical_river != N_CANONICAL_OBSERVATION_RIVER
        {
            return Err(BucketTableError::Corrupted {
                offset: HDR_OFF_N_CANONICAL_FLOP as u64,
                reason: format!(
                    "n_canonical_observation not matching enumeration: \
                     flop={n_canonical_flop} turn={n_canonical_turn} river={n_canonical_river}"
                ),
            });
        }
        let n_dims = bytes_slice[HDR_OFF_N_DIMS];
        if n_dims != BUCKET_TABLE_FEATURE_SET_2_DIMS {
            return Err(BucketTableError::Corrupted {
                offset: HDR_OFF_N_DIMS as u64,
                reason: format!(
                    "n_dims mismatch: expected {} for feature_set_id=2, got {n_dims}",
                    BUCKET_TABLE_FEATURE_SET_2_DIMS
                ),
            });
        }
        // pad 0x29..0x30 (7 bytes) 必须为 0
        for (off, b) in bytes_slice
            .iter()
            .enumerate()
            .take(HDR_OFF_TRAINING_SEED)
            .skip(HDR_OFF_N_DIMS + 1)
        {
            if *b != 0 {
                return Err(BucketTableError::Corrupted {
                    offset: off as u64,
                    reason: "header pad bytes (0x29..0x30) must be zero".into(),
                });
            }
        }
        let training_seed = read_u64_le(bytes_slice, HDR_OFF_TRAINING_SEED);
        let centroid_metadata_offset = read_u64_le(bytes_slice, HDR_OFF_CENTROID_METADATA_OFFSET);
        let centroid_data_offset = read_u64_le(bytes_slice, HDR_OFF_CENTROID_DATA_OFFSET);
        let lookup_table_offset = read_u64_le(bytes_slice, HDR_OFF_LOOKUP_TABLE_OFFSET);
        // pad 0x50..0x58 (8 bytes) 必须为 0
        for (off, b) in bytes_slice
            .iter()
            .enumerate()
            .take(HDR_OFF_FEATURE_FLOP_BLAKE3)
            .skip(HDR_OFF_LOOKUP_TABLE_OFFSET + 8)
        {
            if *b != 0 {
                return Err(BucketTableError::Corrupted {
                    offset: off as u64,
                    reason: "header pad bytes (0x50..0x58) must be zero".into(),
                });
            }
        }

        // 偏移完整性
        let body_start = BUCKET_TABLE_HEADER_LEN;
        let body_end = len - BUCKET_TABLE_TRAILER_LEN;
        if !(centroid_metadata_offset >= body_start
            && centroid_metadata_offset < centroid_data_offset
            && centroid_data_offset < lookup_table_offset
            && lookup_table_offset <= body_end)
        {
            return Err(BucketTableError::Corrupted {
                offset: HDR_OFF_CENTROID_METADATA_OFFSET as u64,
                reason: format!(
                    "section offset invariant violated: meta={centroid_metadata_offset} \
                     data={centroid_data_offset} lookup={lookup_table_offset} body=[{body_start}, {body_end}]"
                ),
            });
        }
        for (field_name, off, off_field_addr) in [
            (
                "centroid_metadata",
                centroid_metadata_offset,
                HDR_OFF_CENTROID_METADATA_OFFSET as u64,
            ),
            (
                "centroid_data",
                centroid_data_offset,
                HDR_OFF_CENTROID_DATA_OFFSET as u64,
            ),
            (
                "lookup_table",
                lookup_table_offset,
                HDR_OFF_LOOKUP_TABLE_OFFSET as u64,
            ),
        ] {
            if off % 8 != 0 {
                return Err(BucketTableError::Corrupted {
                    offset: off_field_addr,
                    reason: format!("{field_name} offset {off} not 8-byte aligned"),
                });
            }
        }
        let centroid_metadata_size: u64 = 3 * (n_dims as u64) * 8;
        let centroid_data_size: u64 =
            (bucket_count_flop as u64 + bucket_count_turn as u64 + bucket_count_river as u64)
                * (n_dims as u64);
        let lookup_table_size_bytes: u64 = (PREFLOP_LOOKUP_LEN as u64
            + n_canonical_flop as u64
            + n_canonical_turn as u64
            + n_canonical_river as u64)
            * 4;
        if centroid_data_offset.saturating_sub(centroid_metadata_offset) < centroid_metadata_size {
            return Err(BucketTableError::Corrupted {
                offset: centroid_metadata_offset,
                reason: "centroid_metadata segment size mismatch".into(),
            });
        }
        if lookup_table_offset.saturating_sub(centroid_data_offset) < centroid_data_size {
            return Err(BucketTableError::Corrupted {
                offset: centroid_data_offset,
                reason: "centroid_data segment size mismatch".into(),
            });
        }
        if body_end.saturating_sub(lookup_table_offset) != lookup_table_size_bytes {
            return Err(BucketTableError::SizeMismatch {
                expected: lookup_table_offset + lookup_table_size_bytes + BUCKET_TABLE_TRAILER_LEN,
                got: len,
            });
        }

        // BLAKE3 trailer eager 校验
        let body_hash = blake3::hash(&bytes_slice[..(len - BUCKET_TABLE_TRAILER_LEN) as usize]);
        let body_hash_bytes: [u8; 32] = *body_hash.as_bytes();
        let mut stored_hash = [0u8; 32];
        stored_hash.copy_from_slice(
            &bytes_slice[(len - BUCKET_TABLE_TRAILER_LEN) as usize..(len) as usize],
        );
        if body_hash_bytes != stored_hash {
            return Err(BucketTableError::Corrupted {
                offset: len - BUCKET_TABLE_TRAILER_LEN,
                reason: "blake3 trailer mismatch".into(),
            });
        }

        let preflop_offset_in_lookup = lookup_table_offset;
        let flop_offset_in_lookup = preflop_offset_in_lookup + (PREFLOP_LOOKUP_LEN as u64) * 4;
        let turn_offset_in_lookup = flop_offset_in_lookup + (n_canonical_flop as u64) * 4;
        let river_offset_in_lookup = turn_offset_in_lookup + (n_canonical_turn as u64) * 4;

        let raw = BucketTableRaw {
            bytes,
            centroid_metadata_offset,
            centroid_data_offset,
            lookup_table_offset,
            preflop_offset_in_lookup,
            flop_offset_in_lookup,
            turn_offset_in_lookup,
            river_offset_in_lookup,
            content_hash: stored_hash,
        };

        Ok(BucketTable {
            config: BucketConfig {
                flop: bucket_count_flop,
                turn: bucket_count_turn,
                river: bucket_count_river,
            },
            schema_version,
            feature_set_id,
            training_seed,
            n_canonical_flop,
            n_canonical_turn,
            n_canonical_river,
            is_stub: false,
            raw: Some(raw),
        })
    }
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Error)]
pub enum BucketTableError {
    #[error("bucket table file not found: {path:?}")]
    FileNotFound { path: PathBuf },
    #[error("bucket table schema mismatch: expected {expected}, got {got}")]
    SchemaMismatch { expected: u32, got: u32 },
    #[error("bucket table feature_set_id mismatch: expected {expected}, got {got}")]
    FeatureSetMismatch { expected: u32, got: u32 },
    #[error("bucket table size mismatch: expected {expected} bytes, got {got}")]
    SizeMismatch { expected: u64, got: u64 },
    #[error("bucket table corrupted at offset {offset}: {reason}")]
    Corrupted { offset: u64, reason: String },
}

// ============================================================================
// Internal: per-street v3 训练流水线 + 字节组装
// ============================================================================

struct StreetTrainingV3 {
    centroids_quantized: Vec<Vec<u8>>,
    centroid_min_per_dim: Vec<f32>,
    centroid_max_per_dim: Vec<f32>,
    lookup_table: Vec<u32>,
}

fn train_one_street_v3(
    street: StreetTag,
    bucket_count: u32,
    training_seed: u64,
    input: StreetFeaturesV3,
) -> StreetTrainingV3 {
    let kmeans_pp_op = match street {
        StreetTag::Flop => rng_substream::KMEANS_PP_INIT_FLOP,
        StreetTag::Turn => rng_substream::KMEANS_PP_INIT_TURN,
        StreetTag::River => rng_substream::KMEANS_PP_INIT_RIVER,
        StreetTag::Preflop => unreachable!("preflop excluded from v3 postflop pipeline"),
    };
    let split_op = match street {
        StreetTag::Flop => rng_substream::EMPTY_CLUSTER_SPLIT_FLOP,
        StreetTag::Turn => rng_substream::EMPTY_CLUSTER_SPLIT_TURN,
        StreetTag::River => rng_substream::EMPTY_CLUSTER_SPLIT_RIVER,
        StreetTag::Preflop => unreachable!(),
    };

    let StreetFeaturesV3 {
        features_f32,
        reorder_key_ehs,
        feature_blake3: _,
    } = input;
    assert_eq!(features_f32.len(), reorder_key_ehs.len());

    let t_convert = std::time::Instant::now();
    let n = features_f32.len();
    // f32 → Vec<Vec<f64>>（kmeans_fit_production 接口要求）。
    // par_iter 让 N=123M river 转换走 rayon；features_f32 移动后 drop。
    let features_f64: Vec<Vec<f64>> = features_f32
        .into_par_iter()
        .map(|row| {
            let mut v = Vec::with_capacity(16);
            for &x in row.iter() {
                v.push(x as f64);
            }
            v
        })
        .collect();
    eprintln!(
        "[train_one_street_v3] street={street:?} f32→f64 convert {} samples wall={:?}",
        n,
        t_convert.elapsed()
    );

    // k-means
    let t_kmeans = std::time::Instant::now();
    let kmeans_cfg = KMeansConfig::default_d232(bucket_count);
    let kmeans_res = kmeans_fit_production(
        &features_f64,
        kmeans_cfg,
        training_seed,
        kmeans_pp_op,
        split_op,
    );
    eprintln!(
        "[train_one_street_v3] street={street:?} kmeans K={bucket_count} wall={:?}",
        t_kmeans.elapsed()
    );

    // f64 buffer 已不再需要，drop 释放内存
    drop(features_f64);

    // D-236b reorder（按 per-sample reorder_key_ehs 的 cluster 内 median 升序）
    let (centroids, assignments) = reorder_by_ehs_median(
        kmeans_res.centroids,
        kmeans_res.assignments,
        &reorder_key_ehs,
    );

    // centroid u8 量化
    let (centroids_quantized, centroid_min_per_dim, centroid_max_per_dim) =
        quantize_centroids_u8(&centroids);

    // lookup_table：v3 production = `lookup[id] = assignments[id]` 直接（100% canonical
    // 覆盖，Stage 1 feature file 按 canonical_id 顺序枚举全 N）。
    debug_assert_eq!(assignments.len(), n);
    let lookup_table: Vec<u32> = assignments;

    StreetTrainingV3 {
        centroids_quantized,
        centroid_min_per_dim,
        centroid_max_per_dim,
        lookup_table,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_bucket_table_v3_bytes(
    config: BucketConfig,
    training_seed: u64,
    feature_blake3_flop: [u8; 32],
    feature_blake3_turn: [u8; 32],
    feature_blake3_river: [u8; 32],
    train_flop: StreetTrainingV3,
    train_turn: StreetTrainingV3,
    train_river: StreetTrainingV3,
) -> Vec<u8> {
    let n_dims = BUCKET_TABLE_FEATURE_SET_2_DIMS;
    let n_canonical_flop = N_CANONICAL_OBSERVATION_FLOP;
    let n_canonical_turn = N_CANONICAL_OBSERVATION_TURN;
    let n_canonical_river = N_CANONICAL_OBSERVATION_RIVER;

    // 各段 size + 偏移（8-byte aligned）
    let centroid_metadata_size: u64 = 3 * (n_dims as u64) * 8;
    let centroid_data_size: u64 =
        (config.flop + config.turn + config.river) as u64 * (n_dims as u64);
    let lookup_table_size: u64 = (PREFLOP_LOOKUP_LEN as u64
        + n_canonical_flop as u64
        + n_canonical_turn as u64
        + n_canonical_river as u64)
        * 4;

    let centroid_metadata_offset = align8(BUCKET_TABLE_HEADER_LEN);
    let centroid_data_offset = align8(centroid_metadata_offset + centroid_metadata_size);
    let lookup_table_offset = align8(centroid_data_offset + centroid_data_size);
    let body_end = lookup_table_offset + lookup_table_size;
    let total_len = body_end + BUCKET_TABLE_TRAILER_LEN;

    let mut bytes: Vec<u8> = vec![0u8; total_len as usize];

    // header
    bytes[HDR_OFF_MAGIC..HDR_OFF_MAGIC + 8].copy_from_slice(&BUCKET_TABLE_MAGIC);
    write_u32_le(
        &mut bytes,
        HDR_OFF_SCHEMA_VERSION,
        BUCKET_TABLE_SCHEMA_VERSION,
    );
    write_u32_le(
        &mut bytes,
        HDR_OFF_FEATURE_SET_ID,
        BUCKET_TABLE_DEFAULT_FEATURE_SET_ID,
    );
    write_u32_le(&mut bytes, HDR_OFF_BUCKET_COUNT_FLOP, config.flop);
    write_u32_le(&mut bytes, HDR_OFF_BUCKET_COUNT_TURN, config.turn);
    write_u32_le(&mut bytes, HDR_OFF_BUCKET_COUNT_RIVER, config.river);
    write_u32_le(&mut bytes, HDR_OFF_N_CANONICAL_FLOP, n_canonical_flop);
    write_u32_le(&mut bytes, HDR_OFF_N_CANONICAL_TURN, n_canonical_turn);
    write_u32_le(&mut bytes, HDR_OFF_N_CANONICAL_RIVER, n_canonical_river);
    bytes[HDR_OFF_N_DIMS] = n_dims;
    // pad 0x29..0x30 已是 0
    write_u64_le(&mut bytes, HDR_OFF_TRAINING_SEED, training_seed);
    write_u64_le(
        &mut bytes,
        HDR_OFF_CENTROID_METADATA_OFFSET,
        centroid_metadata_offset,
    );
    write_u64_le(
        &mut bytes,
        HDR_OFF_CENTROID_DATA_OFFSET,
        centroid_data_offset,
    );
    write_u64_le(&mut bytes, HDR_OFF_LOOKUP_TABLE_OFFSET, lookup_table_offset);
    // pad 0x50..0x58 已是 0
    bytes[HDR_OFF_FEATURE_FLOP_BLAKE3..HDR_OFF_FEATURE_FLOP_BLAKE3 + 32]
        .copy_from_slice(&feature_blake3_flop);
    bytes[HDR_OFF_FEATURE_TURN_BLAKE3..HDR_OFF_FEATURE_TURN_BLAKE3 + 32]
        .copy_from_slice(&feature_blake3_turn);
    bytes[HDR_OFF_FEATURE_RIVER_BLAKE3..HDR_OFF_FEATURE_RIVER_BLAKE3 + 32]
        .copy_from_slice(&feature_blake3_river);

    // centroid_metadata: 3 街 × n_dims × (min: f32, max: f32)
    let mut off = centroid_metadata_offset as usize;
    for train in [&train_flop, &train_turn, &train_river] {
        for d in 0..(n_dims as usize) {
            write_f32_le(&mut bytes, off, train.centroid_min_per_dim[d]);
            off += 4;
            write_f32_le(&mut bytes, off, train.centroid_max_per_dim[d]);
            off += 4;
        }
    }

    // centroid_data: 3 街 × bucket_count(street) × n_dims × u8
    let mut off = centroid_data_offset as usize;
    for train in [&train_flop, &train_turn, &train_river] {
        for centroid in train.centroids_quantized.iter() {
            for &b in centroid.iter() {
                bytes[off] = b;
                off += 1;
            }
        }
    }

    // lookup_table: preflop(1326) + flop + turn + river
    let mut off = lookup_table_offset as usize;
    for hole_id in 0..PREFLOP_LOOKUP_LEN {
        let hand_class = hand_class_169_from_hole_id(hole_id);
        write_u32_le(&mut bytes, off, hand_class);
        off += 4;
    }
    for lookup in [
        &train_flop.lookup_table,
        &train_turn.lookup_table,
        &train_river.lookup_table,
    ] {
        for &id in lookup.iter() {
            write_u32_le(&mut bytes, off, id);
            off += 4;
        }
    }

    debug_assert_eq!(off as u64, body_end);

    // BLAKE3 trailer
    let body_hash = blake3::hash(&bytes[..body_end as usize]);
    bytes[body_end as usize..total_len as usize].copy_from_slice(body_hash.as_bytes());

    bytes
}

fn align8(x: u64) -> u64 {
    (x + 7) & !7
}

/// 1326 hole canonical id → 169 hand class（preflop lossless lookup）。
fn hand_class_169_from_hole_id(hole_id: u32) -> u32 {
    let mut idx: u32 = 0;
    for lo in 0u8..52 {
        for hi in (lo + 1)..52 {
            if idx == hole_id {
                let card_lo = Card::from_u8(lo).expect("0..52");
                let card_hi = Card::from_u8(hi).expect("0..52");
                let suited = card_lo.suit() == card_hi.suit();
                let a = card_lo.rank() as u8;
                let b = card_hi.rank() as u8;
                let (high, low) = if a >= b { (a, b) } else { (b, a) };
                let class = if high == low {
                    high
                } else if suited {
                    13 + high * (high - 1) / 2 + low
                } else {
                    91 + high * (high - 1) / 2 + low
                };
                debug_assert_eq!(canonical_hole_id([card_lo, card_hi]), hole_id);
                return u32::from(class);
            }
            idx += 1;
        }
    }
    panic!("hand_class_169_from_hole_id: hole_id {hole_id} >= 1326");
}

fn street_code(street: StreetTag) -> u32 {
    match street {
        StreetTag::Preflop => unreachable!("street_code excludes preflop"),
        StreetTag::Flop => 0,
        StreetTag::Turn => 1,
        StreetTag::River => 2,
    }
}

// ============================================================================
// 字节读写 helper
// ============================================================================

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 4 bytes"))
}

fn read_u64_le(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64 8 bytes"))
}

fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn write_f32_le(bytes: &mut [u8], offset: usize, value: f32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

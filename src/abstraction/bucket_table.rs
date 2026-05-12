//! Bucket lookup table（API §4，mmap-backed）。
//!
//! `BucketConfig` + `BucketTable` + `BucketTableError`
//! （D-240..D-249，含 D-244-rev1 80-byte header 偏移表 / D-244-rev1 联合
//! observation 索引 / BT-005-rev1 / BT-008-rev1）。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use blake3;
use rayon::prelude::*;
use thiserror::Error;

use crate::abstraction::action::ConfigError;
use crate::abstraction::canonical_enum;
use crate::abstraction::cluster::{
    self, kmeans_fit, quantize_centroids_u8, reorder_by_ehs_median, KMeansConfig,
};
use crate::abstraction::equity::{EquityCalculator, MonteCarloEquity};
use crate::abstraction::info::StreetTag;
use crate::abstraction::postflop::{
    canonical_observation_id, N_CANONICAL_OBSERVATION_FLOP, N_CANONICAL_OBSERVATION_RIVER,
    N_CANONICAL_OBSERVATION_TURN,
};
use crate::abstraction::preflop::canonical_hole_id;
use crate::core::rng::{ChaCha20Rng, RngSource};
use crate::core::Card;
use crate::eval::HandEvaluator;

/// 每条街 bucket 数（D-213 / D-214）。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct BucketConfig {
    pub flop: u32,
    pub turn: u32,
    pub river: u32,
}

/// 训练模式（§G-batch1 §3.3 \[实现\] 引入 enum；§G-batch1 §3.4 \[实现\] dual-phase
/// 实现）。
///
/// 选择 `train_one_street` 的执行 pipeline：
///
/// - [`TrainingMode::Fixture`]（默认；§G-batch1 §3.2 落地形态 byte-equal）：单
///   phase 路径——`n_train = max(K × 10, min(4 × N_canonical, K × 100))`，K×100
///   cap 让 fixture 训练时间与 N 无关，仅与 K 相关（K=10 → 1000；K=100 → 10000；
///   K=500 → 50000）。stage 2 test fixture / bench / capture 路径走该模式。
///   覆盖率 K×100/N 极低（K=500 / N=123M → 0.04%）；剩余 obs_ids 走 Knuth hash
///   fallback；D-236 0 空 bucket 不变量在统计意义上成立。
/// - [`TrainingMode::Production`]（CLI 默认；§G-batch1 §3.4 dual-phase 实现）：
///   两 phase 路径——
///   - Phase 1：`n_train = min(N_canonical, [`PRODUCTION_PHASE1_MAX_SAMPLES`] =
///     2_000_000)` 随机采样 → 计算 features → k-means → K centroids。
///   - Phase 2：枚举每个 canonical_id ∈ \[0, N) → [`canonical_enum::nth_canonical_form`]
///     逆函数解码 → 计算 feature → 分配到最近 centroid → lookup_table\[id\] =
///     nearest_centroid_id。100% canonical 覆盖，无 Knuth hash fallback；
///     bucket_quality 4 类门槛（path.md 字面 EHS std dev / EMD / monotonicity /
///     0 空 bucket）由 proper k-means assignment 保障。
///
///   Phase 1 features memory 上限 ~240 MB（2M samples × ~120 bytes）；phase 2 单
///   sample feature 临时占用；total ~3 GB peak（含 canonical_enum lazy table）
///   在 vultr 7.7 GB host 内。详见 [`PRODUCTION_PHASE1_MAX_SAMPLES`] 文档。
///
///   §G-batch1 §3.3 commit `7e2bd2e` 历史 `n_train = 4 × N`（unfeasibly OOM on
///   vultr）被本 §3.4 \[实现\] 修订为 dual-phase + cap；D-244-rev2 §5 footnote
///   "option (c) canonical-enumeration inverse + 100% 覆盖" 实测路径。
///
/// 同 (config, training_seed, evaluator, cluster_iter, mode) 输入下 artifact
/// byte-equal（D-237）。改 mode 触发不同 RNG draw 序列 → BLAKE3 漂移。
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum TrainingMode {
    /// 单 phase K×100 cap 公式（fixture / 测试路径）。
    #[default]
    Fixture,
    /// Dual-phase canonical-inverse + 100% canonical 覆盖（CLI production 路径；
    /// §G-batch1 §3.4 实现）。
    Production,
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

// ============================================================================
// On-disk file format constants（D-240 / D-244 / D-244-rev1）
// ============================================================================

/// `magic: [u8; 8] = b"PLBKT\0\0\0"`（D-240）。
pub const BUCKET_TABLE_MAGIC: [u8; 8] = *b"PLBKT\0\0\0";
/// 当前 schema 版本（D-240 / D-244-rev2）。
///
/// §G-batch1 §3.2 \[实现\]：bump 1 → 2（D-244-rev2 §1 字面 mandate）。v1 artifact
/// 由 §G-batch1 §3.2 之前的 D-218-rev1 FNV-1a hash mod 设计生成（lookup_table 大小
/// 3K/6K/10K，canonical_observation_id 走 hash 路径）；v2 artifact 由 §G-batch1
/// §3.2 之后的 D-218-rev2 真等价类枚举设计生成（lookup_table 大小 1.28M/13.96M/
/// 123.16M，canonical_observation_id 走 colex ranking 路径）。两者 schema 不兼容，
/// reader 必须靠 schema_version 字段区分。
pub const BUCKET_TABLE_SCHEMA_VERSION: u32 = 2;
/// 默认 feature_set_id（D-221 EHS² + OCHS(N=8) = 9 维）。
pub const BUCKET_TABLE_DEFAULT_FEATURE_SET_ID: u32 = 1;
/// feature_set_id=1 对应的 centroid 维度（D-221）。
pub const BUCKET_TABLE_FEATURE_SET_1_DIMS: u8 = 9;
/// header 长度（D-244 §⑨）。
pub const BUCKET_TABLE_HEADER_LEN: u64 = 80;
/// trailer BLAKE3 长度（D-243）。
pub const BUCKET_TABLE_TRAILER_LEN: u64 = 32;
/// preflop lookup 段固定长度（D-239 / D-245）。
pub const PREFLOP_LOOKUP_LEN: u32 = 1326;

// ============================================================================
// BucketTable struct
// ============================================================================

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
    config: BucketConfig,
    schema_version: u32,
    feature_set_id: u32,
    training_seed: u64,
    n_canonical_flop: u32,
    n_canonical_turn: u32,
    n_canonical_river: u32,
    /// `true` 时 lookup 返回 `Some(0)`（B-rev0 carve-out option (1) stub 路径，
    /// 仅用于 B1 test fixture，C2 闭合后 stage 2 主路径走 `train_in_memory` /
    /// `open`）。
    is_stub: bool,
    /// 文件内容（mmap 或 in-memory）。stub 路径下空。
    raw: Option<BucketTableRaw>,
}

/// 内部 raw 表示。整段 Vec<u8> 持有文件内容（C2 落地：`memmap2::Mmap` 内部使用
/// unsafe，与 stage 1 D-275 `unsafe_code = "forbid"` 冲突——按 D-275 carve-out
/// 走整段 `std::fs::read` → `Vec<u8>` 路径替代 mmap。1.4MB 文件加载无显著差异，
/// 后续 mmap 真路径若需启用，由 stage 3+ 通过 D-275-revM 评估）。
struct BucketTableRaw {
    bytes: Vec<u8>,
    /// 这些字段为 from_bytes 解析后缓存，避免每次 lookup 重复读 header。
    /// `centroid_metadata_offset` / `centroid_data_offset` / `lookup_table_offset`
    /// 在 reader 路径仅作为偏移完整性 sanity 校验来源（解析时检查），lookup 热路径
    /// 直接走 `*_offset_in_lookup`；保留字段供未来 stage 4+ centroid 读取（D-241
    /// 反量化）使用。
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
    /// **eager 校验**：read → 解析 header → 校验 schema_version / feature_set_id /
    /// 文件总大小 → 计算 BLAKE3 trailer → 比对 → 任一失败立即返回错误。
    /// 全 5 类错误路径见 [`BucketTableError`]。
    ///
    /// **注**：A0 D-255 / D-244 锁定 mmap 加载路径，但 `memmap2::Mmap::map` 内部
    /// 使用 `unsafe`，与 stage 1 D-275 `unsafe_code = "forbid"` 冲突。C2 \[实现\]
    /// 路径走 `std::fs::read` 整段加载到 `Vec<u8>`，与 mmap 在语义上等价（同样
    /// 给出 `&[u8]` 全文件视图），文件 ≤ 2 MB 加载耗时 < 5 ms 无 SLO 风险。若
    /// stage 3+ 需要真 mmap（巨大 bucket table 跨进程共享），由后续走 D-275-revM
    /// 评估。
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

    /// in-memory 训练（`tools/train_bucket_table.rs` CLI 与 \[测试\] 共享路径）。
    /// 同 (config, training_seed, evaluator, cluster_iter) 输入下 byte-equal（D-237）。
    ///
    /// §G-batch1 §3.3：`train_in_memory` 走 [`TrainingMode::Fixture`] (K×100 cap
    /// 公式，与 §G-batch1 §3.2 落地行为 byte-equal)；stage 2 test fixture / bench
    /// / capture 路径继承既有 byte-equal artifact。CLI production 路径
    /// (`tools/train_bucket_table.rs --mode production`) 走
    /// [`BucketTable::train_in_memory_with_mode`] + [`TrainingMode::Production`]。
    ///
    /// `cluster_iter` = MonteCarloEquity 训练时使用的 iter 数（默认 D-220 = 10_000；
    /// 测试加速可降到 200~1_000，特征向量数值噪声相应增大）。
    pub fn train_in_memory(
        config: BucketConfig,
        training_seed: u64,
        evaluator: Arc<dyn HandEvaluator>,
        cluster_iter: u32,
    ) -> BucketTable {
        Self::train_in_memory_with_mode(
            config,
            training_seed,
            evaluator,
            cluster_iter,
            TrainingMode::Fixture,
        )
    }

    /// in-memory 训练（显式 [`TrainingMode`]）。详见 [`TrainingMode`] 文档对每个
    /// 模式 `n_train` 公式与覆盖率取舍的说明。
    ///
    /// 同 (config, training_seed, evaluator, cluster_iter, mode) 输入下 byte-equal
    /// （D-237）；改 mode 触发不同的 RNG draw 序列 → BLAKE3 content_hash 漂移。
    pub fn train_in_memory_with_mode(
        config: BucketConfig,
        training_seed: u64,
        evaluator: Arc<dyn HandEvaluator>,
        cluster_iter: u32,
        mode: TrainingMode,
    ) -> BucketTable {
        let bytes = build_bucket_table_bytes(config, training_seed, evaluator, cluster_iter, mode);
        Self::from_bytes(bytes).expect("build_bucket_table_bytes 自洽产物 byte-validate 应成功")
    }

    /// 把当前 BucketTable 的字节内容写出到 `path`。文件以原子 rename 风格
    /// 创建（先写到 `<path>.tmp`，再 rename），与 stage 1 `HandHistory` 文件 I/O
    /// 风格一致。
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

    /// **B-rev0 carve-out option (1)**：test-only / B2 stub 构造路径。让
    /// `tests/info_id_encoding.rs::info_abs_postflop_bucket_id_in_range` 在 B2
    /// 闭合时取消 `#[ignore]` 后可调用 `PostflopBucketAbstraction::bucket_id`。
    /// `lookup` 永远返回 `Some(0)`。C2 闭合后大部分 \[测试\] 改走
    /// `BucketTable::train_in_memory(...)` 真实路径。
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

    fn from_bytes(bytes: Vec<u8>) -> Result<BucketTable, BucketTableError> {
        let bytes_slice: &[u8] = &bytes;
        let len = bytes_slice.len() as u64;
        if len < BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN {
            return Err(BucketTableError::SizeMismatch {
                expected: BUCKET_TABLE_HEADER_LEN + BUCKET_TABLE_TRAILER_LEN,
                got: len,
            });
        }

        // BT-001 magic
        if bytes_slice[0..8] != BUCKET_TABLE_MAGIC {
            return Err(BucketTableError::Corrupted {
                offset: 0,
                reason: "magic bytes mismatch".into(),
            });
        }
        // BT-002 schema_version
        let schema_version = read_u32_le(bytes_slice, 0x08);
        if schema_version != BUCKET_TABLE_SCHEMA_VERSION {
            return Err(BucketTableError::SchemaMismatch {
                expected: BUCKET_TABLE_SCHEMA_VERSION,
                got: schema_version,
            });
        }
        // BT-003 feature_set_id
        let feature_set_id = read_u32_le(bytes_slice, 0x0C);
        if feature_set_id != BUCKET_TABLE_DEFAULT_FEATURE_SET_ID {
            return Err(BucketTableError::FeatureSetMismatch {
                expected: BUCKET_TABLE_DEFAULT_FEATURE_SET_ID,
                got: feature_set_id,
            });
        }
        let bucket_count_flop = read_u32_le(bytes_slice, 0x10);
        let bucket_count_turn = read_u32_le(bytes_slice, 0x14);
        let bucket_count_river = read_u32_le(bytes_slice, 0x18);
        let n_canonical_flop = read_u32_le(bytes_slice, 0x1C);
        let n_canonical_turn = read_u32_le(bytes_slice, 0x20);
        let n_canonical_river = read_u32_le(bytes_slice, 0x24);
        let n_dims = bytes_slice[0x28];
        // BT-008-rev1 范围 sanity
        for &(field, val) in [
            ("bucket_count_flop", bucket_count_flop),
            ("bucket_count_turn", bucket_count_turn),
            ("bucket_count_river", bucket_count_river),
        ]
        .iter()
        {
            if !(10..=10_000).contains(&val) {
                return Err(BucketTableError::Corrupted {
                    offset: 0x10,
                    reason: format!("{field} out of range: expected [10, 10_000], got {val}"),
                });
            }
        }
        // §G-batch1 §3.2 / D-244-rev2 §3 BT-008-rev2 bound 收紧：从 D-244-rev1
        // 保守上界 (2M / 20M / 200M) 到 D-218-rev2 §2 实测精确值
        // (1,286,792 / 13,960,050 / 123,156,254)。≠ 精确值视为 Corrupted。
        if n_canonical_flop != N_CANONICAL_OBSERVATION_FLOP
            || n_canonical_turn != N_CANONICAL_OBSERVATION_TURN
            || n_canonical_river != N_CANONICAL_OBSERVATION_RIVER
        {
            return Err(BucketTableError::Corrupted {
                offset: 0x1C,
                reason: format!(
                    "n_canonical_observation not matching D-218-rev2 enumeration: \
                     flop={n_canonical_flop} turn={n_canonical_turn} river={n_canonical_river}"
                ),
            });
        }
        if n_dims != BUCKET_TABLE_FEATURE_SET_1_DIMS {
            return Err(BucketTableError::Corrupted {
                offset: 0x28,
                reason: format!(
                    "n_dims mismatch: expected {} for feature_set_id=1, got {n_dims}",
                    BUCKET_TABLE_FEATURE_SET_1_DIMS
                ),
            });
        }
        // pad 必须为 0
        for (off, b) in bytes_slice.iter().enumerate().take(0x30).skip(0x29) {
            if *b != 0 {
                return Err(BucketTableError::Corrupted {
                    offset: off as u64,
                    reason: "header pad bytes must be zero".into(),
                });
            }
        }
        let training_seed = read_u64_le(bytes_slice, 0x30);
        let centroid_metadata_offset = read_u64_le(bytes_slice, 0x38);
        let centroid_data_offset = read_u64_le(bytes_slice, 0x40);
        let lookup_table_offset = read_u64_le(bytes_slice, 0x48);

        // BT-008-rev1：偏移表完整性
        let body_start = BUCKET_TABLE_HEADER_LEN;
        let body_end = len - BUCKET_TABLE_TRAILER_LEN;
        if !(centroid_metadata_offset >= body_start
            && centroid_metadata_offset < centroid_data_offset
            && centroid_data_offset < lookup_table_offset
            && lookup_table_offset <= body_end)
        {
            return Err(BucketTableError::Corrupted {
                offset: 0x38,
                reason: format!(
                    "section offset invariant violated: meta={centroid_metadata_offset} \
                     data={centroid_data_offset} lookup={lookup_table_offset} body=[{body_start}, {body_end}]"
                ),
            });
        }
        for (field_name, off, off_field_addr) in [
            ("centroid_metadata", centroid_metadata_offset, 0x38u64),
            ("centroid_data", centroid_data_offset, 0x40),
            ("lookup_table", lookup_table_offset, 0x48),
        ] {
            if off % 8 != 0 {
                return Err(BucketTableError::Corrupted {
                    offset: off_field_addr,
                    reason: format!("{field_name} offset {off} not 8-byte aligned"),
                });
            }
        }
        // 各段长度 sanity
        let centroid_metadata_size: u64 = 3 * (n_dims as u64) * 8; // 3 streets × n_dims × (min:f32, max:f32)
        let centroid_data_size: u64 =
            (bucket_count_flop as u64 + bucket_count_turn as u64 + bucket_count_river as u64)
                * (n_dims as u64); // 3 streets × bucket_count × n_dims × u8
        let lookup_table_size_u32: u64 = PREFLOP_LOOKUP_LEN as u64
            + n_canonical_flop as u64
            + n_canonical_turn as u64
            + n_canonical_river as u64;
        let lookup_table_size_bytes: u64 = lookup_table_size_u32 * 4;
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

        // BT-004 BLAKE3 trailer eager 校验
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

    /// `(street, observation_canonical_id) → bucket_id`（BT-005-rev1 /
    /// D-216-rev1 / D-218-rev1 / §9）。
    ///
    /// 越界返回 `None`（`observation_canonical_id >= n_canonical_observation(street)`
    /// 或 preflop `>= 1326`）。
    pub fn lookup(&self, street: StreetTag, observation_canonical_id: u32) -> Option<u32> {
        let upper = self.n_canonical_observation(street);
        if observation_canonical_id >= upper {
            return None;
        }
        if self.is_stub {
            // B2 stub: §B2 line 274 协议——每条街固定返回 bucket_id = 0。
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

    /// 每条街联合 (board, hole) canonical observation id 总数（D-244-rev1）：
    /// preflop 固定返回 1326；postflop 返回 header `n_canonical_observation_<street>`。
    pub fn n_canonical_observation(&self, street: StreetTag) -> u32 {
        match street {
            StreetTag::Preflop => PREFLOP_LOOKUP_LEN,
            StreetTag::Flop => self.n_canonical_flop,
            StreetTag::Turn => self.n_canonical_turn,
            StreetTag::River => self.n_canonical_river,
        }
    }

    /// 文件 BLAKE3 自校验值（D-243）。同 mmap 加载后 byte-equal。stub 返回 [0; 32]。
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
}

// ============================================================================
// On-disk file builder（C2 落地：训练 + 序列化 + BLAKE3 trailer）
// ============================================================================

/// 训练并序列化 bucket table 到 in-memory Vec<u8>。同 (config, training_seed,
/// cluster_iter, mode) 输入产出 byte-equal（D-237）。
fn build_bucket_table_bytes(
    config: BucketConfig,
    training_seed: u64,
    evaluator: Arc<dyn HandEvaluator>,
    cluster_iter: u32,
    mode: TrainingMode,
) -> Vec<u8> {
    // 1. 三街独立训练（D-238 多街顺序）。
    let train_flop = train_one_street(
        StreetTag::Flop,
        config.flop,
        training_seed,
        Arc::clone(&evaluator),
        cluster_iter,
        mode,
    );
    let train_turn = train_one_street(
        StreetTag::Turn,
        config.turn,
        training_seed,
        Arc::clone(&evaluator),
        cluster_iter,
        mode,
    );
    let train_river = train_one_street(
        StreetTag::River,
        config.river,
        training_seed,
        Arc::clone(&evaluator),
        cluster_iter,
        mode,
    );

    let n_dims = BUCKET_TABLE_FEATURE_SET_1_DIMS;
    let n_canonical_flop = N_CANONICAL_OBSERVATION_FLOP;
    let n_canonical_turn = N_CANONICAL_OBSERVATION_TURN;
    let n_canonical_river = N_CANONICAL_OBSERVATION_RIVER;

    // 2. 计算各段 size + 偏移（8-byte aligned）。
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
    bytes[0..8].copy_from_slice(&BUCKET_TABLE_MAGIC);
    write_u32_le(&mut bytes, 0x08, BUCKET_TABLE_SCHEMA_VERSION);
    write_u32_le(&mut bytes, 0x0C, BUCKET_TABLE_DEFAULT_FEATURE_SET_ID);
    write_u32_le(&mut bytes, 0x10, config.flop);
    write_u32_le(&mut bytes, 0x14, config.turn);
    write_u32_le(&mut bytes, 0x18, config.river);
    write_u32_le(&mut bytes, 0x1C, n_canonical_flop);
    write_u32_le(&mut bytes, 0x20, n_canonical_turn);
    write_u32_le(&mut bytes, 0x24, n_canonical_river);
    bytes[0x28] = n_dims;
    // pad 0x29..0x30 已经是 0
    write_u64_le(&mut bytes, 0x30, training_seed);
    write_u64_le(&mut bytes, 0x38, centroid_metadata_offset);
    write_u64_le(&mut bytes, 0x40, centroid_data_offset);
    write_u64_le(&mut bytes, 0x48, lookup_table_offset);

    // centroid_metadata：3 街 × n_dims × (min: f32, max: f32)
    let mut off = centroid_metadata_offset as usize;
    for train in [&train_flop, &train_turn, &train_river] {
        for d in 0..(n_dims as usize) {
            write_f32_le(&mut bytes, off, train.centroid_min_per_dim[d]);
            off += 4;
            write_f32_le(&mut bytes, off, train.centroid_max_per_dim[d]);
            off += 4;
        }
    }

    // centroid_data：3 街 × bucket_count(street) × n_dims × u8
    let mut off = centroid_data_offset as usize;
    for train in [&train_flop, &train_turn, &train_river] {
        for centroid in train.centroids_quantized.iter() {
            for &b in centroid.iter() {
                bytes[off] = b;
                off += 1;
            }
        }
    }

    // lookup_table：preflop（1326）+ flop（n_canonical_flop）+ turn + river
    // preflop：D-217 closed-form hand_class_169 → bucket id（preflop 169 lossless
    // 直接写 hand_class_169，与 PreflopLossless169::map 一致）。
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

struct StreetTraining {
    /// `centroids_quantized[c]` = bucket c 的 u8 量化向量（n_dims 长）。
    centroids_quantized: Vec<Vec<u8>>,
    /// `centroid_min_per_dim[d]` / `centroid_max_per_dim[d]`：每维量化区间。
    centroid_min_per_dim: Vec<f32>,
    centroid_max_per_dim: Vec<f32>,
    /// `lookup_table[obs_id]` = bucket id ∈ 0..bucket_count。
    lookup_table: Vec<u32>,
}

/// 单街训练：sample 候选 (board, hole) → 计算特征 → k-means → 量化 → 写 lookup。
fn train_one_street(
    street: StreetTag,
    bucket_count: u32,
    training_seed: u64,
    evaluator: Arc<dyn HandEvaluator>,
    cluster_iter: u32,
    mode: TrainingMode,
) -> StreetTraining {
    let cluster_op = match street {
        StreetTag::Flop => cluster::rng_substream::CLUSTER_MAIN_FLOP,
        StreetTag::Turn => cluster::rng_substream::CLUSTER_MAIN_TURN,
        StreetTag::River => cluster::rng_substream::CLUSTER_MAIN_RIVER,
        StreetTag::Preflop => unreachable!("preflop 不走 train_one_street 路径"),
    };
    let kmeans_pp_op = match street {
        StreetTag::Flop => cluster::rng_substream::KMEANS_PP_INIT_FLOP,
        StreetTag::Turn => cluster::rng_substream::KMEANS_PP_INIT_TURN,
        StreetTag::River => cluster::rng_substream::KMEANS_PP_INIT_RIVER,
        StreetTag::Preflop => unreachable!(),
    };
    let split_op = match street {
        StreetTag::Flop => cluster::rng_substream::EMPTY_CLUSTER_SPLIT_FLOP,
        StreetTag::Turn => cluster::rng_substream::EMPTY_CLUSTER_SPLIT_TURN,
        StreetTag::River => cluster::rng_substream::EMPTY_CLUSTER_SPLIT_RIVER,
        StreetTag::Preflop => unreachable!(),
    };
    let n_canonical = match street {
        StreetTag::Flop => N_CANONICAL_OBSERVATION_FLOP,
        StreetTag::Turn => N_CANONICAL_OBSERVATION_TURN,
        StreetTag::River => N_CANONICAL_OBSERVATION_RIVER,
        StreetTag::Preflop => unreachable!(),
    };
    let board_len = match street {
        StreetTag::Flop => 3,
        StreetTag::Turn => 4,
        StreetTag::River => 5,
        StreetTag::Preflop => unreachable!(),
    };

    // 1. 候选 sample：`n_train` 由 [`TrainingMode`] 选公式（§G-batch1 §3.3 / §3.4）。
    //
    // - [`TrainingMode::Fixture`]（§G-batch1 §3.2 落地形态 byte-equal）：
    //   `n_train = max(K × 10, min(4 × N_canonical, K × 100))`。K×100 cap 让
    //   fixture 训练时间与 N 无关，仅与 K 相关：
    //
    //     K=10  → n_train = max(100, min(4N, 1000))   = 1000（fixture / 10/10/10）
    //     K=100 → n_train = max(1000, min(4N, 10000)) = 10000（小 fixture）
    //     K=500 → n_train = max(5000, min(4N, 50000)) = 50000（fixture 边界）
    //
    //   覆盖率 K × 100 / N 极低（K=500 / N=123M → 0.04%）；剩余 obs_ids 走 Knuth
    //   hash fallback；D-236 0 空 bucket 不变量在统计意义上成立。
    //
    // - [`TrainingMode::Production`]（§G-batch1 §3.4 dual-phase memory feasibility
    //   option (c)：canonical-inverse + dual-phase）：
    //   `n_train = min(N_canonical, PRODUCTION_PHASE1_MAX_SAMPLES = 2_000_000)`。
    //   Phase 1 在 ≤ 2M 候选子集上跑 k-means → 得到 K 个 centroids；Phase 2 枚举
    //   全 N canonical_ids（[`nth_canonical_form`] 逆函数解码）→ 计算 feature →
    //   分配到最近 centroid → 100% canonical 覆盖（无 Knuth hash fallback）。
    //
    //     flop  → n_train = 1,286,792（全覆盖，等价于 phase 1 + phase 2 同集合）
    //     turn  → n_train = 2,000,000（phase 1 子集；phase 2 覆盖剩余 11.96M）
    //     river → n_train = 2,000,000（phase 1 子集；phase 2 覆盖剩余 121.16M）
    //
    //   Phase 1 features memory: 2M × ~120 bytes ≈ 240 MB peak；phase 2 单 sample
    //   feature 临时占用，bounded RAM。Total ~3 GB peak（含 canonical_enum lazy
    //   table）在 vultr 7.7 GB host 内。
    //
    //   §G-batch1 §3.3 commit `7e2bd2e` 历史形态 `n_train = 4 × N`（unfeasibly OOM
    //   on vultr）被本 §3.4 [实现] 修订为 `min(N, 2M)`。
    let n_train: usize = match mode {
        TrainingMode::Fixture => ((bucket_count as usize) * 10)
            .max(((n_canonical as usize) * 4).min((bucket_count as usize) * 100)),
        TrainingMode::Production => (n_canonical as usize).min(PRODUCTION_PHASE1_MAX_SAMPLES),
    };
    // 进度日志（§G-batch1 §3.4-batch1）：long-running Production retrain 走 stderr
    // 实时观测 + Fixture 路径同型保持一致。在 release ~120 min budget 内不引入
    // 显著 I/O overhead（每街仅几次 eprintln 起止）。
    let t_street_start = std::time::Instant::now();
    eprintln!(
        "[train_one_street] street={street:?} mode={mode:?} K={bucket_count} \
         n_canonical={n_canonical} n_train_phase1={n_train} cluster_iter={cluster_iter}"
    );
    let mut sample_rng = ChaCha20Rng::from_seed(cluster::rng_substream::derive_substream_seed(
        training_seed,
        cluster_op,
        0,
    ));
    let t_sample = std::time::Instant::now();
    let candidates = sample_n_postflop_candidates(&mut sample_rng, board_len, n_train);
    eprintln!(
        "[train_one_street] street={street:?} phase=1 sampled {} candidates in {:?}",
        candidates.len(),
        t_sample.elapsed()
    );

    // 2. 计算特征向量（D-221 EHS² + OCHS_8 = 9 维）。
    //
    // **C2 carve-out (cluster_iter ≤ ~500 路径)**：D-221 EHS² 的精确计算需要 outer
    // 公共牌枚举 + inner equity MC（flop 1081 outer × 200 iter = 216K evals/sample
    // 主导 cluster 训练耗时）；为让 stage 2 fixture 训练在 < 30s 内完成，cluster_iter
    // ≤ 500 时改用 **EHS² ≈ equity²** 近似（单 MC，无 outer 枚举），与 D-227
    // river 状态退化路径 (`outer = 0 → ehs² = equity²`) 同公式但应用在所有街。
    // 牺牲 EHS² 二阶矩信息（potential-aware），换取训练速度——OCHS(N=8) 仍精确，
    // 足以驱动 8 维 cluster 距离的主要分量。`cluster_iter > 500` 时切回 D-221 精确
    // EHS² 路径（C2 production / E2 SLO 使用）。
    //
    // 该取舍由 stage-2 §C-rev0 carve-out 追认：D-221 字面 "EHS²" 在 fixture 路径
    // 改 EHS² ≈ equity²，feature_set_id 仍为 1（schema 不 bump）；production
    // CLI（`--cluster-iter 10000`）走精确路径，无影响。
    let use_proxy = cluster_iter <= 500;
    let calc = MonteCarloEquity::new(Arc::clone(&evaluator)).with_iter(cluster_iter);
    let ehs_op = match street {
        StreetTag::Flop => cluster::rng_substream::EHS2_INNER_EQUITY_FLOP,
        StreetTag::Turn => cluster::rng_substream::EHS2_INNER_EQUITY_TURN,
        StreetTag::River => cluster::rng_substream::EHS2_INNER_EQUITY_RIVER,
        StreetTag::Preflop => unreachable!(),
    };
    let ochs_op = cluster::rng_substream::OCHS_FEATURE_INNER;

    let t_features = std::time::Instant::now();
    // §G-batch1 §3.4-batch1.5 [实现]：rayon par_iter 数据并行。每 sample 的 RNG
    // 通过 derive_substream_seed(seed, op, i) 派生（pure function of i），与执行
    // 顺序无关；rayon `.collect()` 保留迭代序 → 输出 features / ehs_per_sample
    // 顺序与 sequential `.iter().collect()` byte-equal。
    //
    // chunk-based 进度日志（n_train ≥ 200K 时每 5% / 50K 取大；fixture n_train ≤
    // 50K 走单 chunk 无中间日志保持 fixture 路径安静）。chunked 让 par_iter 在
    // chunk 内并行 + chunk 间串行打日志，比 lock-based 全局 progress counter 简单。
    let chunk_size: usize = if n_train >= 200_000 {
        (n_train / 20).max(50_000)
    } else {
        n_train.max(1)
    };
    let n_chunks: usize = n_train.div_ceil(chunk_size);
    let mut features: Vec<Vec<f64>> = Vec::with_capacity(n_train);
    let mut ehs_per_sample: Vec<f64> = Vec::with_capacity(n_train);
    for chunk_idx in 0..n_chunks {
        let start = chunk_idx * chunk_size;
        let end = (start + chunk_size).min(n_train);
        if chunk_idx > 0 && n_chunks > 1 {
            eprintln!(
                "[train_one_street] street={street:?} phase=1 features chunk {chunk_idx}/{n_chunks} \
                 [{start}..{end}) elapsed={:?}",
                t_features.elapsed()
            );
        }
        let chunk_results: Vec<(Vec<f64>, f64)> = candidates[start..end]
            .par_iter()
            .enumerate()
            .map(|(j, (board, hole))| {
                let i: u32 = (start + j) as u32;
                let mut rng_ehs = ChaCha20Rng::from_seed(
                    cluster::rng_substream::derive_substream_seed(training_seed, ehs_op, i),
                );
                let mut rng_ochs = ChaCha20Rng::from_seed(
                    cluster::rng_substream::derive_substream_seed(training_seed, ochs_op, i),
                );
                let ehs = calc
                    .equity(*hole, board, &mut rng_ehs)
                    .unwrap_or(0.5)
                    .clamp(0.0, 1.0);
                let ehs2 = if use_proxy {
                    ehs * ehs
                } else {
                    // production 精确路径（cluster_iter > 500）。
                    calc.ehs_squared(*hole, board, &mut rng_ehs)
                        .unwrap_or(ehs * ehs)
                        .clamp(0.0, 1.0)
                };
                let ochs = calc
                    .ochs(*hole, board, &mut rng_ochs)
                    .unwrap_or_else(|_| vec![0.5; 8]);
                let mut feat: Vec<f64> = Vec::with_capacity(9);
                feat.push(ehs2);
                for v in ochs.iter().take(8) {
                    feat.push((*v).clamp(0.0, 1.0));
                }
                // 若 OCHS 长度 < 8（理论不会，n_opp_clusters 默认 8），用 0.5 padding。
                while feat.len() < (BUCKET_TABLE_FEATURE_SET_1_DIMS as usize) {
                    feat.push(0.5);
                }
                // D-236b 重编号 key：直接用 ehs（EHS = `equity()` 输出，非 EHS²）。
                (feat, ehs)
            })
            .collect();
        for (feat, ehs) in chunk_results {
            features.push(feat);
            ehs_per_sample.push(ehs);
        }
    }

    eprintln!(
        "[train_one_street] street={street:?} phase=1 features done {} samples / {:?}",
        features.len(),
        t_features.elapsed()
    );

    // 3. k-means + L2（D-230 / D-231 / D-232）。
    let t_kmeans = std::time::Instant::now();
    let kmeans_cfg = KMeansConfig::default_d232(bucket_count);
    let kmeans_res = kmeans_fit(&features, kmeans_cfg, training_seed, kmeans_pp_op, split_op);
    eprintln!(
        "[train_one_street] street={street:?} phase=1 kmeans done K={bucket_count} / {:?}",
        t_kmeans.elapsed()
    );

    // 4. D-236b 重编号（按 EHS 中位数升序）。
    let (centroids, assignments) = reorder_by_ehs_median(
        kmeans_res.centroids,
        kmeans_res.assignments,
        &ehs_per_sample,
    );

    // 5. centroid u8 量化（D-241）。
    let (centroids_quantized, centroid_min_per_dim, centroid_max_per_dim) =
        quantize_centroids_u8(&centroids);

    // 6. 构建 lookup_table：obs_id → bucket id。模式分流（§G-batch1 §3.4）：
    //
    // - [`TrainingMode::Fixture`]（§G-batch1 §3.2 形态 byte-equal）：每个 sample 的
    //   (board, hole) → canonical_observation_id → 第一个命中的 sample 的 bucket
    //   id 写入 lookup_table[obs_id]；剩余未命中的 obs_id 用 Knuth hash fallback
    //   （`(obs_id × 2654435761) mod bucket_count`），让 "无空 bucket" 不变量在
    //   统计意义上成立。
    // - [`TrainingMode::Production`]（§G-batch1 §3.4 dual-phase）：枚举所有 N
    //   canonical_ids → [`nth_canonical_form`] 解码 (board, hole) → 同 phase 1
    //   pipeline 计算 feature → 找最近 centroid（L2 距离，post-D-236b 重编号顺序）
    //   → lookup_table[id] = nearest_centroid_id。100% canonical 覆盖，无 Knuth
    //   hash fallback；bucket_quality 4 类门槛（path.md 字面）由 proper k-means
    //   assignment 保障。
    let mut lookup_table: Vec<u32> = vec![u32::MAX; n_canonical as usize];
    match mode {
        TrainingMode::Fixture => {
            for (i, (board, hole)) in candidates.iter().enumerate() {
                let obs_id = canonical_observation_id(street, board, *hole);
                if lookup_table[obs_id as usize] == u32::MAX {
                    lookup_table[obs_id as usize] = assignments[i];
                }
            }
            // Knuth hash fallback for unsampled obs_ids。
            for obs_id in 0..n_canonical {
                if lookup_table[obs_id as usize] == u32::MAX {
                    let h = (obs_id as u64).wrapping_mul(2654435761);
                    lookup_table[obs_id as usize] = (h % (bucket_count as u64)) as u32;
                }
            }
        }
        TrainingMode::Production => {
            // Phase 2 (§G-batch1 §3.4)：枚举每个 canonical_id → decode → feature →
            // 最近 centroid。RNG op_ids 与 phase 1 相同（ehs_op / ochs_op），但 ramp
            // 使用 `id` 而非 phase 1 sample index `i`——同 training_seed 下二者
            // 重叠区域 (id < n_train_phase1) 会取到同样的 substream seed，但
            // (board, hole) 输入不同 → feature 自然不同；determinism 不变。
            //
            // §G-batch1 §3.4-batch1.5 [实现]：rayon par_iter chunked 数据并行。
            // 每 id 的 RNG 通过 derive_substream_seed(seed, op, id) 派生（pure
            // function of id），与执行顺序无关；rayon `.collect()` 保留迭代序 →
            // lookup_table 写入字节顺序 byte-equal sequential 路径。chunk 间
            // 串行打日志（含 ETA），chunk 内 par_iter 并行。
            let t_phase2 = std::time::Instant::now();
            let chunk_size_p2: usize = ((n_canonical as usize) / 20).max(200_000);
            let n_chunks_p2: usize = (n_canonical as usize).div_ceil(chunk_size_p2);
            eprintln!(
                "[train_one_street] street={street:?} phase=2 enumerate-assign N={n_canonical} \
                 starting (chunks={n_chunks_p2} chunk_size={chunk_size_p2})"
            );
            let n_dims = BUCKET_TABLE_FEATURE_SET_1_DIMS as usize;
            for chunk_idx in 0..n_chunks_p2 {
                let start = chunk_idx * chunk_size_p2;
                let end = ((start + chunk_size_p2) as u32).min(n_canonical) as usize;
                if chunk_idx > 0 {
                    let elapsed = t_phase2.elapsed();
                    let pct = 100.0 * (start as f64) / (n_canonical as f64);
                    let eta_sec = elapsed.as_secs_f64() * ((n_canonical as usize - start) as f64)
                        / (start as f64);
                    eprintln!(
                        "[train_one_street] street={street:?} phase=2 chunk {chunk_idx}/{n_chunks_p2} \
                         [{start}..{end}) ({:.1}%) elapsed={:?} eta={:.0}s",
                        pct, elapsed, eta_sec
                    );
                }
                let chunk_results: Vec<u32> = (start..end)
                    .into_par_iter()
                    .map(|id_usize| {
                        let id: u32 = id_usize as u32;
                        let (board, hole) = canonical_enum::nth_canonical_form(street, id);
                        let mut rng_ehs =
                            ChaCha20Rng::from_seed(cluster::rng_substream::derive_substream_seed(
                                training_seed,
                                ehs_op,
                                id,
                            ));
                        let mut rng_ochs =
                            ChaCha20Rng::from_seed(cluster::rng_substream::derive_substream_seed(
                                training_seed,
                                ochs_op,
                                id,
                            ));
                        let ehs = calc
                            .equity(hole, &board, &mut rng_ehs)
                            .unwrap_or(0.5)
                            .clamp(0.0, 1.0);
                        let ehs2 = if use_proxy {
                            ehs * ehs
                        } else {
                            calc.ehs_squared(hole, &board, &mut rng_ehs)
                                .unwrap_or(ehs * ehs)
                                .clamp(0.0, 1.0)
                        };
                        let ochs = calc
                            .ochs(hole, &board, &mut rng_ochs)
                            .unwrap_or_else(|_| vec![0.5; 8]);
                        let mut feat: [f64; 9] = [0.5; 9];
                        feat[0] = ehs2;
                        for (d, v) in ochs.iter().take(8).enumerate() {
                            feat[d + 1] = (*v).clamp(0.0, 1.0);
                        }
                        // 找最近 centroid（L2 距离，post-D-236b 重编号顺序，centroids
                        // 是 f64 reference 值；u8 量化仅用于 artifact 存储）。
                        let mut best_id: u32 = 0;
                        let mut best_dist: f64 = f64::INFINITY;
                        for (c_id, c) in centroids.iter().enumerate() {
                            let mut dist: f64 = 0.0;
                            for d in 0..n_dims {
                                let diff = feat[d] - c[d];
                                dist += diff * diff;
                            }
                            if dist < best_dist {
                                best_dist = dist;
                                best_id = c_id as u32;
                            }
                        }
                        best_id
                    })
                    .collect();
                lookup_table[start..end].copy_from_slice(&chunk_results);
            }
            eprintln!(
                "[train_one_street] street={street:?} phase=2 done {} ids / {:?}",
                n_canonical,
                t_phase2.elapsed()
            );
        }
    }

    eprintln!(
        "[train_one_street] street={street:?} mode={mode:?} total wall {:?}",
        t_street_start.elapsed()
    );
    StreetTraining {
        centroids_quantized,
        centroid_min_per_dim,
        centroid_max_per_dim,
        lookup_table,
    }
}

/// Production-mode phase 1 candidate cap（§G-batch1 §3.4 \[实现\] memory feasibility
/// option (c)）。Phase 1 在 ≤ 2M candidates 上训 k-means → 得到 K centroids；
/// phase 2 通过 [`crate::abstraction::canonical_enum::nth_canonical_form`] 逆函数
/// 100% 枚举 N canonical_ids → 分配到最近 centroid。该 cap 让 phase 1 features
/// memory ≤ 2M × ~120 bytes ≈ 240 MB peak，在 vultr 7.7 GB host 内可承受。
///
/// 数值选择 2M 是 trade-off：(a) 足够大让 K=500 centroids 收敛稳定（每 centroid
/// ~4K samples 期望）；(b) 足够小让 feature 计算时间（2M × 9 × cluster_iter 评估）
/// + memory 在 ~30 min release 内完成。§G-batch1 §3.4-batch2 实跑前可调整。
pub const PRODUCTION_PHASE1_MAX_SAMPLES: usize = 2_000_000;

/// 训练用 (board, hole) 候选采样：每次随机抽 `board_len + 2` 张不重复牌。
fn sample_n_postflop_candidates(
    rng: &mut ChaCha20Rng,
    board_len: usize,
    n: usize,
) -> Vec<(Vec<Card>, [Card; 2])> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let mut deck: [u8; 52] = [0; 52];
        for (i, slot) in deck.iter_mut().enumerate() {
            *slot = i as u8;
        }
        // Fisher-Yates 部分洗牌：抽 board_len + 2 张。
        let total = board_len + 2;
        for i in 0..total {
            let j = i + (rng.next_u64() % ((52 - i) as u64)) as usize;
            deck.swap(i, j);
        }
        let mut board: Vec<Card> = Vec::with_capacity(board_len);
        for d in deck.iter().take(board_len) {
            board.push(Card::from_u8(*d).expect("0..52 valid"));
        }
        let hole = [
            Card::from_u8(deck[board_len]).expect("0..52"),
            Card::from_u8(deck[board_len + 1]).expect("0..52"),
        ];
        out.push((board, hole));
    }
    out
}

/// 1326 个 hole canonical id → 169 hand class（preflop lookup table 写入路径）。
fn hand_class_169_from_hole_id(hole_id: u32) -> u32 {
    // canonical_hole_id 是单射 0..1326；逆映射通过遍历找。
    // 1326 = C(52, 2)。简单的逆映射：按 lo, hi 顺序遍历。
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
                // 校验与 canonical_hole_id 互逆
                debug_assert_eq!(canonical_hole_id([card_lo, card_hi]), hole_id);
                return u32::from(class);
            }
            idx += 1;
        }
    }
    panic!("hand_class_169_from_hole_id: hole_id {hole_id} >= 1326");
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

// `RngSource` trait 在本文件经 `ChaCha20Rng::next_u64()` 间接调用，需要在
// scope 内才能解析方法。
#[allow(dead_code)]
fn _ensure_rng_trait_in_scope(_rng: &dyn RngSource) {}

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

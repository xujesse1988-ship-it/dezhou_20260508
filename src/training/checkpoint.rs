//! Checkpoint binary schema + save / open（API-350 / D-350..D-359 + stage 4
//! D-449 / API-440 / API-441）。
//!
//! stage 4 D2 \[实现\] 落地：schema_version 1 ↔ 2 双路径 dispatch。
//!
//! **stage 3 schema_version=1 layout**（HEADER_LEN_V1 = 108 byte）：
//!
//! | 字段 | 起始偏移 | 长度 | 编码 |
//! |---|---|---|---|
//! | `magic` | 0 | 8 | `b"PLCKPT\0\0"` |
//! | `schema_version` | 8 | 4 | u32 LE = 1 |
//! | `trainer_variant` | 12 | 1 | u8 |
//! | `game_variant` | 13 | 1 | u8 |
//! | `pad` | 14 | 6 | 0 |
//! | `update_count` | 20 | 8 | u64 LE |
//! | `rng_state` | 28 | 32 | bytes |
//! | `bucket_table_blake3` | 60 | 32 | bytes（Kuhn / Leduc 全零）|
//! | `regret_table_offset` | 92 | 8 | u64 LE（≥ 108）|
//! | `strategy_sum_offset` | 100 | 8 | u64 LE |
//! | `regret_table_body` | `regret_table_offset` | varies | bincode body |
//! | `strategy_sum_body` | `strategy_sum_offset` | varies | bincode body |
//! | `trailer_blake3` | `len - 32` | 32 | bytes |
//!
//! **stage 4 schema_version=2 layout**（HEADER_LEN = 128 byte）：
//!
//! | 字段 | 起始偏移 | 长度 | 编码 |
//! |---|---|---|---|
//! | `magic` | 0 | 8 | `b"PLCKPT\0\0"` |
//! | `schema_version` | 8 | 4 | u32 LE = 2 |
//! | `trainer_variant` | 12 | 1 | u8 |
//! | `game_variant` | 13 | 1 | u8 |
//! | `traverser_count` | 14 | 1 | u8（stage 3 path = 1 / NlheGame6 = 6）|
//! | `linear_weighting_enabled` | 15 | 1 | u8 ∈ {0, 1} |
//! | `rm_plus_enabled` | 16 | 1 | u8 ∈ {0, 1} |
//! | `warmup_complete` | 17 | 1 | u8 ∈ {0, 1} |
//! | `pad_a` | 18 | 6 | 0 |
//! | `update_count` | 24 | 8 | u64 LE |
//! | `rng_state` | 32 | 32 | bytes |
//! | `bucket_table_blake3` | 64 | 32 | bytes |
//! | `regret_offset` | 96 | 8 | u64 LE（≥ 128）|
//! | `strategy_offset` | 104 | 8 | u64 LE |
//! | `pad_b` | 112 | 16 | 0 |
//! | `regret_table_body` | `regret_offset` | varies | bincode body |
//! | `strategy_sum_body` | `strategy_offset` | varies | bincode body |
//! | `trailer_blake3` | `len - 32` | 32 | bytes |
//!
//! D-327 bincode 序列化时按 InfoSet Debug-sort 顺序写入（确保 BLAKE3 byte-equal
//! across hosts）。D-352 trailer BLAKE3 eager 校验；D-353 write-to-temp + atomic
//! rename；D-356 多 game 不兼容 → [`crate::error::CheckpointError::TrainerMismatch`]
//! 由 [`crate::training::Trainer::load_checkpoint`] 在 [`Checkpoint::open`] 之前
//! eager 校验。
//!
//! **§D2-revM dispatch carve-out**（2026-05-15，用户授权 Option A）：
//! [`Checkpoint::open`] / `Checkpoint::parse_bytes` 按文件 `schema_version`
//! 字段分流 v1 / v2 解析（接受两个版本），让 stage 3 既有 corruption /
//! round-trip / warmup 测试套件全部 byte-equal 维持。`SCHEMA_VERSION` 常量
//! bump 到 2 是 "latest 支持版本" 标记；stage 3 trainer 仍写 schema=1，stage 4
//! `EsMccfrTrainer<NlheGame6>` 在 `linear_weighting_enabled && rm_plus_enabled`
//! 时写 schema=2（其它 trainer 仍写 schema=1）。

use std::io::Write;
use std::path::Path;

use blake3::Hasher;

use crate::error::CheckpointError;

// API-350 公开路径 `module: training::checkpoint`；TrainerVariant + GameVariant
// 物理位置在 `src/error.rs`（D-374 避免循环依赖），通过 `pub use` 再导出与
// API doc 对齐。
pub use crate::error::{GameVariant, TrainerVariant};

/// Checkpoint magic header（API-350 binary layout offset 0；stage 3 / 4 共享）。
///
/// 8 byte `b"PLCKPT\0\0"`；`PL` = Pluribus / `CKPT` = checkpoint / 后 2 byte
/// `\0\0` pad 让 magic 8 byte aligned 与 header 后续 8 byte aligned 字段对齐。
pub const MAGIC: [u8; 8] = *b"PLCKPT\0\0";

/// 当前支持的最新 schema version（stage 4 D-449 字面）。
///
/// 起步值 `1`（stage 3）；stage 4 D2 \[实现\] bump 到 `2`。
/// [`Checkpoint::save`] / [`Checkpoint::open`] 按 schema_version 字段分流：
/// - `schema_version == 1` → v1 layout（[`HEADER_LEN_V1`] = 108 byte）
/// - `schema_version == 2` → v2 layout（[`HEADER_LEN`] = 128 byte）
/// - 其它 → [`crate::error::CheckpointError::SchemaMismatch`]
pub const SCHEMA_VERSION: u32 = 2;

/// stage 3 legacy schema version（D2 后仍由 stage 3 trainer 写入；
/// [`Checkpoint::open`] dispatch 时接受）。
pub const SCHEMA_VERSION_V1: u32 = 1;

/// Header 长度（stage 4 v2 layout；D-449 字面 128 byte）。
pub const HEADER_LEN: usize = 128;

/// stage 3 v1 layout header 长度（保留让 trainer / 测试桥接到 legacy 路径）。
pub const HEADER_LEN_V1: usize = 108;

/// Trailer BLAKE3 长度（API-350 / D-352；stage 3 / 4 共享）。
pub const TRAILER_LEN: usize = 32;

// ---------------------------------------------------------------------------
// stage 4 v2 layout offsets（128-byte header；API-440 字面）。
// ---------------------------------------------------------------------------

const OFFSET_MAGIC: usize = 0;
const OFFSET_SCHEMA_VERSION: usize = 8;
const OFFSET_TRAINER_VARIANT: usize = 12;
const OFFSET_GAME_VARIANT: usize = 13;

/// v2 字面 — traverser_count: u8（API-440 / D-449）。
pub const OFFSET_TRAVERSER_COUNT: usize = 14;
/// v2 字面 — linear_weighting_enabled: u8（API-440 / D-449）。
pub const OFFSET_LINEAR_WEIGHTING: usize = 15;
/// v2 字面 — rm_plus_enabled: u8（API-440 / D-449）。
pub const OFFSET_RM_PLUS: usize = 16;
/// v2 字面 — warmup_complete: u8（API-440 / D-449）。
pub const OFFSET_WARMUP_COMPLETE: usize = 17;

const OFFSET_PAD_A: usize = 18;
const OFFSET_UPDATE_COUNT: usize = 24;
const OFFSET_RNG_STATE: usize = 32;
pub(crate) const OFFSET_BUCKET_TABLE_BLAKE3: usize = 64;

/// v2 字面 — regret table body offset: u64 LE（API-440）。
pub const OFFSET_REGRET_OFFSET: usize = 96;
/// v2 字面 — strategy_sum body offset: u64 LE（API-440）。
pub const OFFSET_STRATEGY_OFFSET: usize = 104;
/// v2 字面 — pad_b reserved 16 byte（API-440）。
pub const OFFSET_PAD_B: usize = 112;

// ---------------------------------------------------------------------------
// stage 3 v1 layout offsets（108-byte header；保留给 v1 dispatch 路径）。
// `OFFSET_MAGIC` / `OFFSET_SCHEMA_VERSION` / `OFFSET_TRAINER_VARIANT` /
// `OFFSET_GAME_VARIANT` 与 v2 共享前 14 byte。
// ---------------------------------------------------------------------------

const OFFSET_V1_PAD: usize = 14;
const OFFSET_V1_UPDATE_COUNT: usize = 20;
const OFFSET_V1_RNG_STATE: usize = 28;
pub(crate) const OFFSET_V1_BUCKET_TABLE_BLAKE3: usize = 60;
const OFFSET_V1_REGRET_TABLE_OFFSET: usize = 92;
const OFFSET_V1_STRATEGY_SUM_OFFSET: usize = 100;

/// Checkpoint 二进制结构（API-350 + 阶段 4 D-449 扩展 4 字段）。
///
/// 在内存中按 deserialized 形式持有；序列化 / 反序列化由 [`Checkpoint::save`] /
/// [`Checkpoint::open`] 串行执行。`regret_table_bytes` / `strategy_sum_bytes`
/// 是 bincode-serialized 子段，让 [`crate::training::RegretTable`] /
/// [`crate::training::StrategyAccumulator`] 在 Trainer 内部按需 `bincode::deserialize`
/// 重建（避免 [`Checkpoint`] 本身依赖泛型 `<I>`）。
///
/// **schema_version 字段语义**：
/// - `1` → stage 3 layout，4 个 stage 4 字段（`traverser_count` /
///   `linear_weighting_enabled` / `rm_plus_enabled` / `warmup_complete`）走默认值
///   （1 / false / false / false），即 v1 反序列化时填充。
/// - `2` → stage 4 layout，4 个新字段从 header 反序列化。
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

    /// stage 4 D-449 / D-412 — 6-traverser dimension（stage 3 path = 1；
    /// `NlheGame6` Linear+RM+ path = 6）。
    pub traverser_count: u8,
    /// stage 4 D-449 / D-401 — Linear discounting on/off（v1 path = false）。
    pub linear_weighting_enabled: bool,
    /// stage 4 D-449 / D-402 — RM+ in-place clamp on/off（v1 path = false）。
    pub rm_plus_enabled: bool,
    /// stage 4 D-449 / D-409 — warm-up phase 已完成（v1 path = false）。
    pub warmup_complete: bool,
}

impl Checkpoint {
    /// 写出 checkpoint 到 `path`（D-353 write-to-temp + atomic rename + D-352
    /// trailer BLAKE3 + D-358 full snapshot 不做 incremental）。
    ///
    /// **schema_version dispatch**：
    /// - `1` → 写 v1 layout（108-byte header；4 个 stage 4 字段不持久化）。
    /// - `2` → 写 v2 layout（128-byte header；4 个 stage 4 字段从本 struct 持久化）。
    /// - 其它 → [`CheckpointError::SchemaMismatch`]。
    ///
    /// 失败路径：[`CheckpointError::Corrupted`]（I/O 失败 / 序列化失败 / atomic
    /// rename 失败均归类到 Corrupted 兜底）。
    pub fn save(&self, path: &Path) -> Result<(), CheckpointError> {
        let buf = match self.schema_version {
            SCHEMA_VERSION_V1 => self.encode_v1(),
            SCHEMA_VERSION => self.encode_v2(),
            other => {
                return Err(CheckpointError::SchemaMismatch {
                    expected: SCHEMA_VERSION,
                    got: other,
                });
            }
        };
        write_atomic(path, &buf)
    }

    /// 编码 stage 3 v1 layout（108-byte header；保留兼容路径）。
    fn encode_v1(&self) -> Vec<u8> {
        let regret_len = self.regret_table_bytes.len();
        let strategy_len = self.strategy_sum_bytes.len();
        let regret_table_offset = HEADER_LEN_V1 as u64;
        let strategy_sum_offset = regret_table_offset + regret_len as u64;
        let body_end = strategy_sum_offset + strategy_len as u64;
        let total_len = body_end as usize + TRAILER_LEN;

        let mut buf = vec![0u8; total_len];
        buf[OFFSET_MAGIC..OFFSET_SCHEMA_VERSION].copy_from_slice(&MAGIC);
        buf[OFFSET_SCHEMA_VERSION..OFFSET_TRAINER_VARIANT]
            .copy_from_slice(&self.schema_version.to_le_bytes());
        buf[OFFSET_TRAINER_VARIANT] = self.trainer_variant as u8;
        buf[OFFSET_GAME_VARIANT] = self.game_variant as u8;
        // pad (offset 14..20) 已由 vec![0; ..] 初始化为 0
        buf[OFFSET_V1_UPDATE_COUNT..OFFSET_V1_RNG_STATE]
            .copy_from_slice(&self.update_count.to_le_bytes());
        buf[OFFSET_V1_RNG_STATE..OFFSET_V1_BUCKET_TABLE_BLAKE3].copy_from_slice(&self.rng_state);
        buf[OFFSET_V1_BUCKET_TABLE_BLAKE3..OFFSET_V1_REGRET_TABLE_OFFSET]
            .copy_from_slice(&self.bucket_table_blake3);
        buf[OFFSET_V1_REGRET_TABLE_OFFSET..OFFSET_V1_STRATEGY_SUM_OFFSET]
            .copy_from_slice(&regret_table_offset.to_le_bytes());
        buf[OFFSET_V1_STRATEGY_SUM_OFFSET..HEADER_LEN_V1]
            .copy_from_slice(&strategy_sum_offset.to_le_bytes());
        buf[regret_table_offset as usize..strategy_sum_offset as usize]
            .copy_from_slice(&self.regret_table_bytes);
        buf[strategy_sum_offset as usize..body_end as usize]
            .copy_from_slice(&self.strategy_sum_bytes);

        let mut hasher = Hasher::new();
        hasher.update(&buf[..body_end as usize]);
        let trailer: [u8; 32] = hasher.finalize().into();
        buf[body_end as usize..total_len].copy_from_slice(&trailer);
        buf
    }

    /// 编码 stage 4 v2 layout（128-byte header；D-449 字面）。
    fn encode_v2(&self) -> Vec<u8> {
        let regret_len = self.regret_table_bytes.len();
        let strategy_len = self.strategy_sum_bytes.len();
        let regret_offset = HEADER_LEN as u64;
        let strategy_offset = regret_offset + regret_len as u64;
        let body_end = strategy_offset + strategy_len as u64;
        let total_len = body_end as usize + TRAILER_LEN;

        let mut buf = vec![0u8; total_len];
        buf[OFFSET_MAGIC..OFFSET_SCHEMA_VERSION].copy_from_slice(&MAGIC);
        buf[OFFSET_SCHEMA_VERSION..OFFSET_TRAINER_VARIANT]
            .copy_from_slice(&self.schema_version.to_le_bytes());
        buf[OFFSET_TRAINER_VARIANT] = self.trainer_variant as u8;
        buf[OFFSET_GAME_VARIANT] = self.game_variant as u8;
        buf[OFFSET_TRAVERSER_COUNT] = self.traverser_count;
        buf[OFFSET_LINEAR_WEIGHTING] = u8::from(self.linear_weighting_enabled);
        buf[OFFSET_RM_PLUS] = u8::from(self.rm_plus_enabled);
        buf[OFFSET_WARMUP_COMPLETE] = u8::from(self.warmup_complete);
        // pad_a (18..24) 已由 vec![0; ..] 初始化为 0
        buf[OFFSET_UPDATE_COUNT..OFFSET_RNG_STATE]
            .copy_from_slice(&self.update_count.to_le_bytes());
        buf[OFFSET_RNG_STATE..OFFSET_BUCKET_TABLE_BLAKE3].copy_from_slice(&self.rng_state);
        buf[OFFSET_BUCKET_TABLE_BLAKE3..OFFSET_REGRET_OFFSET]
            .copy_from_slice(&self.bucket_table_blake3);
        buf[OFFSET_REGRET_OFFSET..OFFSET_STRATEGY_OFFSET]
            .copy_from_slice(&regret_offset.to_le_bytes());
        buf[OFFSET_STRATEGY_OFFSET..OFFSET_PAD_B].copy_from_slice(&strategy_offset.to_le_bytes());
        // pad_b (112..128) 已由 vec![0; ..] 初始化为 0
        buf[regret_offset as usize..strategy_offset as usize]
            .copy_from_slice(&self.regret_table_bytes);
        buf[strategy_offset as usize..body_end as usize].copy_from_slice(&self.strategy_sum_bytes);

        let mut hasher = Hasher::new();
        hasher.update(&buf[..body_end as usize]);
        let trailer: [u8; 32] = hasher.finalize().into();
        buf[body_end as usize..total_len].copy_from_slice(&trailer);
        buf
    }

    /// 从 `path` 加载（D-352 eager BLAKE3 校验 + D-350 schema 校验）。
    ///
    /// **§D2-revM dispatch**：按 file `schema_version` 字段分流 v1 / v2 解析。
    /// stage 3 既有调用路径（Kuhn / Leduc / SimplifiedNlhe schema=1 文件）
    /// 走 v1 解析，4 个 stage 4 字段以默认值填充；stage 4 NlheGame6 + Linear+RM+
    /// 写出的 schema=2 文件走 v2 解析。
    ///
    /// 失败路径覆盖 3 类 [`CheckpointError`]：FileNotFound / SchemaMismatch /
    /// Corrupted（magic / pad / trailer BLAKE3 / offset 表越界 / 未知 variant tag）。
    ///
    /// 注意：`TrainerMismatch` / `BucketTableMismatch` 不由本方法返回——
    /// [`crate::training::Trainer::load_checkpoint`] 在调用本方法之前 eager
    /// 检查 trainer_variant + game_variant + bucket_table_blake3 字段，
    /// 命中 mismatch 立即返回相应 variant；那些路径上 trailer BLAKE3 通常
    /// 也已破坏，因此必须在 trailer 校验前拦截。
    pub fn open(path: &Path) -> Result<Self, CheckpointError> {
        let bytes = read_file_bytes(path)?;
        Self::parse_bytes(&bytes)
    }

    /// 从已读取的 bytes 中解析（[`Self::open`] 内部入口）。
    ///
    /// **§D2-revM dispatch**：peek `schema_version` 后分流 v1 / v2 解析；其它
    /// schema 值（含 0 / u32::MAX / > 2）直接返 [`CheckpointError::SchemaMismatch`]
    /// `{ expected: SCHEMA_VERSION = 2, got: <file value> }`。
    pub(crate) fn parse_bytes(bytes: &[u8]) -> Result<Self, CheckpointError> {
        let len = bytes.len();
        // 文件至少要容纳 v1 header + trailer（更小不可能是任何合法 schema）。
        if len < HEADER_LEN_V1 + TRAILER_LEN {
            return Err(CheckpointError::Corrupted {
                offset: 0,
                reason: format!(
                    "file too short: {len} bytes (min header_v1+trailer {})",
                    HEADER_LEN_V1 + TRAILER_LEN
                ),
            });
        }

        // 1. magic（两个 schema 共享 offset 0..8）
        if bytes[OFFSET_MAGIC..OFFSET_SCHEMA_VERSION] != MAGIC {
            return Err(CheckpointError::Corrupted {
                offset: 0,
                reason: "magic mismatch (expected b\"PLCKPT\\0\\0\")".to_string(),
            });
        }

        // 2. schema dispatch（D2 落地：v1 ↔ v2 双路径分流；其它走 SchemaMismatch）
        let schema = u32::from_le_bytes(
            bytes[OFFSET_SCHEMA_VERSION..OFFSET_TRAINER_VARIANT]
                .try_into()
                .unwrap(),
        );
        match schema {
            SCHEMA_VERSION_V1 => Self::parse_bytes_v1(bytes),
            SCHEMA_VERSION => Self::parse_bytes_v2(bytes),
            other => Err(CheckpointError::SchemaMismatch {
                expected: SCHEMA_VERSION,
                got: other,
            }),
        }
    }

    /// stage 3 schema_version=1 layout 解析（HEADER_LEN_V1 = 108 byte）。
    ///
    /// 4 个 stage 4 新字段以默认值填充（traverser_count=1 / linear=false /
    /// rm_plus=false / warmup=false），让 v1 数据在 v2 binary 内表达为
    /// "stage 3 single-traverser standard CFR + RM" 等价形态。
    fn parse_bytes_v1(bytes: &[u8]) -> Result<Self, CheckpointError> {
        let len = bytes.len();
        if len < HEADER_LEN_V1 + TRAILER_LEN {
            return Err(CheckpointError::Corrupted {
                offset: 0,
                reason: format!(
                    "v1 file too short: {len} bytes (min {})",
                    HEADER_LEN_V1 + TRAILER_LEN
                ),
            });
        }

        // trainer_variant / game_variant tag 解析
        let trainer_variant =
            TrainerVariant::from_u8(bytes[OFFSET_TRAINER_VARIANT]).ok_or_else(|| {
                CheckpointError::Corrupted {
                    offset: OFFSET_TRAINER_VARIANT as u64,
                    reason: format!(
                        "unknown trainer_variant tag {} at offset {OFFSET_TRAINER_VARIANT}",
                        bytes[OFFSET_TRAINER_VARIANT]
                    ),
                }
            })?;
        let game_variant = GameVariant::from_u8(bytes[OFFSET_GAME_VARIANT]).ok_or_else(|| {
            CheckpointError::Corrupted {
                offset: OFFSET_GAME_VARIANT as u64,
                reason: format!(
                    "unknown game_variant tag {} at offset {OFFSET_GAME_VARIANT}",
                    bytes[OFFSET_GAME_VARIANT]
                ),
            }
        })?;

        // pad 区必须全 0（offset 14..20）
        for (i, &b) in bytes[OFFSET_V1_PAD..OFFSET_V1_UPDATE_COUNT]
            .iter()
            .enumerate()
        {
            if b != 0 {
                let off = OFFSET_V1_PAD + i;
                return Err(CheckpointError::Corrupted {
                    offset: off as u64,
                    reason: format!("pad byte non-zero at offset {off}: 0x{b:02x}"),
                });
            }
        }

        // trailer BLAKE3 eager 校验
        let trailer_start = len - TRAILER_LEN;
        let mut hasher = Hasher::new();
        hasher.update(&bytes[..trailer_start]);
        let actual: [u8; 32] = hasher.finalize().into();
        let stored: [u8; 32] = bytes[trailer_start..len].try_into().unwrap();
        if actual != stored {
            return Err(CheckpointError::Corrupted {
                offset: trailer_start as u64,
                reason: "trailer BLAKE3 mismatch (body/header tampered)".to_string(),
            });
        }

        let update_count = u64::from_le_bytes(
            bytes[OFFSET_V1_UPDATE_COUNT..OFFSET_V1_RNG_STATE]
                .try_into()
                .unwrap(),
        );
        let rng_state: [u8; 32] = bytes[OFFSET_V1_RNG_STATE..OFFSET_V1_BUCKET_TABLE_BLAKE3]
            .try_into()
            .unwrap();
        let bucket_table_blake3: [u8; 32] = bytes
            [OFFSET_V1_BUCKET_TABLE_BLAKE3..OFFSET_V1_REGRET_TABLE_OFFSET]
            .try_into()
            .unwrap();
        let regret_table_offset = u64::from_le_bytes(
            bytes[OFFSET_V1_REGRET_TABLE_OFFSET..OFFSET_V1_STRATEGY_SUM_OFFSET]
                .try_into()
                .unwrap(),
        );
        let strategy_sum_offset = u64::from_le_bytes(
            bytes[OFFSET_V1_STRATEGY_SUM_OFFSET..HEADER_LEN_V1]
                .try_into()
                .unwrap(),
        );

        let trailer_start_u64 = trailer_start as u64;
        if regret_table_offset < HEADER_LEN_V1 as u64
            || regret_table_offset > strategy_sum_offset
            || strategy_sum_offset > trailer_start_u64
        {
            return Err(CheckpointError::Corrupted {
                offset: OFFSET_V1_REGRET_TABLE_OFFSET as u64,
                reason: format!(
                    "v1 offset table out of range: regret={regret_table_offset} \
                     strategy={strategy_sum_offset} trailer_start={trailer_start}"
                ),
            });
        }

        let regret_table_bytes =
            bytes[regret_table_offset as usize..strategy_sum_offset as usize].to_vec();
        let strategy_sum_bytes = bytes[strategy_sum_offset as usize..trailer_start].to_vec();

        Ok(Checkpoint {
            schema_version: SCHEMA_VERSION_V1,
            trainer_variant,
            game_variant,
            update_count,
            rng_state,
            bucket_table_blake3,
            regret_table_bytes,
            strategy_sum_bytes,
            traverser_count: 1,
            linear_weighting_enabled: false,
            rm_plus_enabled: false,
            warmup_complete: false,
        })
    }

    /// stage 4 schema_version=2 layout 解析（HEADER_LEN = 128 byte；D-449 字面）。
    fn parse_bytes_v2(bytes: &[u8]) -> Result<Self, CheckpointError> {
        let len = bytes.len();
        if len < HEADER_LEN + TRAILER_LEN {
            return Err(CheckpointError::Corrupted {
                offset: 0,
                reason: format!(
                    "v2 file too short: {len} bytes (min {})",
                    HEADER_LEN + TRAILER_LEN
                ),
            });
        }

        let trainer_variant =
            TrainerVariant::from_u8(bytes[OFFSET_TRAINER_VARIANT]).ok_or_else(|| {
                CheckpointError::Corrupted {
                    offset: OFFSET_TRAINER_VARIANT as u64,
                    reason: format!(
                        "unknown trainer_variant tag {} at offset {OFFSET_TRAINER_VARIANT}",
                        bytes[OFFSET_TRAINER_VARIANT]
                    ),
                }
            })?;
        let game_variant = GameVariant::from_u8(bytes[OFFSET_GAME_VARIANT]).ok_or_else(|| {
            CheckpointError::Corrupted {
                offset: OFFSET_GAME_VARIANT as u64,
                reason: format!(
                    "unknown game_variant tag {} at offset {OFFSET_GAME_VARIANT}",
                    bytes[OFFSET_GAME_VARIANT]
                ),
            }
        })?;
        let traverser_count = bytes[OFFSET_TRAVERSER_COUNT];
        let linear_weighting_byte = bytes[OFFSET_LINEAR_WEIGHTING];
        let rm_plus_byte = bytes[OFFSET_RM_PLUS];
        let warmup_byte = bytes[OFFSET_WARMUP_COMPLETE];
        let linear_weighting_enabled =
            bool_from_u8(linear_weighting_byte, OFFSET_LINEAR_WEIGHTING)?;
        let rm_plus_enabled = bool_from_u8(rm_plus_byte, OFFSET_RM_PLUS)?;
        let warmup_complete = bool_from_u8(warmup_byte, OFFSET_WARMUP_COMPLETE)?;

        // pad_a (18..24) 必须全 0
        for (i, &b) in bytes[OFFSET_PAD_A..OFFSET_UPDATE_COUNT].iter().enumerate() {
            if b != 0 {
                let off = OFFSET_PAD_A + i;
                return Err(CheckpointError::Corrupted {
                    offset: off as u64,
                    reason: format!("pad_a byte non-zero at offset {off}: 0x{b:02x}"),
                });
            }
        }

        // pad_b (112..128) 必须全 0
        for (i, &b) in bytes[OFFSET_PAD_B..HEADER_LEN].iter().enumerate() {
            if b != 0 {
                let off = OFFSET_PAD_B + i;
                return Err(CheckpointError::Corrupted {
                    offset: off as u64,
                    reason: format!("pad_b byte non-zero at offset {off}: 0x{b:02x}"),
                });
            }
        }

        // trailer BLAKE3 eager 校验
        let trailer_start = len - TRAILER_LEN;
        let mut hasher = Hasher::new();
        hasher.update(&bytes[..trailer_start]);
        let actual: [u8; 32] = hasher.finalize().into();
        let stored: [u8; 32] = bytes[trailer_start..len].try_into().unwrap();
        if actual != stored {
            return Err(CheckpointError::Corrupted {
                offset: trailer_start as u64,
                reason: "v2 trailer BLAKE3 mismatch (body/header tampered)".to_string(),
            });
        }

        let update_count = u64::from_le_bytes(
            bytes[OFFSET_UPDATE_COUNT..OFFSET_RNG_STATE]
                .try_into()
                .unwrap(),
        );
        let rng_state: [u8; 32] = bytes[OFFSET_RNG_STATE..OFFSET_BUCKET_TABLE_BLAKE3]
            .try_into()
            .unwrap();
        let bucket_table_blake3: [u8; 32] = bytes[OFFSET_BUCKET_TABLE_BLAKE3..OFFSET_REGRET_OFFSET]
            .try_into()
            .unwrap();
        let regret_offset = u64::from_le_bytes(
            bytes[OFFSET_REGRET_OFFSET..OFFSET_STRATEGY_OFFSET]
                .try_into()
                .unwrap(),
        );
        let strategy_offset = u64::from_le_bytes(
            bytes[OFFSET_STRATEGY_OFFSET..OFFSET_PAD_B]
                .try_into()
                .unwrap(),
        );

        let trailer_start_u64 = trailer_start as u64;
        if regret_offset < HEADER_LEN as u64
            || regret_offset > strategy_offset
            || strategy_offset > trailer_start_u64
        {
            return Err(CheckpointError::Corrupted {
                offset: OFFSET_REGRET_OFFSET as u64,
                reason: format!(
                    "v2 offset table out of range: regret={regret_offset} \
                     strategy={strategy_offset} trailer_start={trailer_start}"
                ),
            });
        }

        let regret_table_bytes = bytes[regret_offset as usize..strategy_offset as usize].to_vec();
        let strategy_sum_bytes = bytes[strategy_offset as usize..trailer_start].to_vec();

        Ok(Checkpoint {
            schema_version: SCHEMA_VERSION,
            trainer_variant,
            game_variant,
            update_count,
            rng_state,
            bucket_table_blake3,
            regret_table_bytes,
            strategy_sum_bytes,
            traverser_count,
            linear_weighting_enabled,
            rm_plus_enabled,
            warmup_complete,
        })
    }
}

/// `u8 ∈ {0, 1}` → `bool`；越界 → [`CheckpointError::Corrupted`]
/// （v2 layout 4 个新字段共享 helper）。
fn bool_from_u8(b: u8, offset: usize) -> Result<bool, CheckpointError> {
    match b {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(CheckpointError::Corrupted {
            offset: offset as u64,
            reason: format!("bool field at offset {offset} out of range: 0x{other:02x}"),
        }),
    }
}

/// D-353 atomic write：tempfile in 同 parent dir → persist (rename) 到目标
/// 路径；持有期间任意 SIGKILL / OOM / 断电中断都不会污染既有 `<path>`。
fn write_atomic(path: &Path, buf: &[u8]) -> Result<(), CheckpointError> {
    let parent_dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut tmp =
        tempfile::NamedTempFile::new_in(parent_dir).map_err(|e| CheckpointError::Corrupted {
            offset: 0,
            reason: format!("create temp file in {parent_dir:?} failed: {e}"),
        })?;
    tmp.write_all(buf).map_err(|e| CheckpointError::Corrupted {
        offset: 0,
        reason: format!("write to temp file failed: {e}"),
    })?;
    tmp.as_file_mut()
        .sync_all()
        .map_err(|e| CheckpointError::Corrupted {
            offset: 0,
            reason: format!("fsync temp file failed: {e}"),
        })?;
    tmp.persist(path).map_err(|e| CheckpointError::Corrupted {
        offset: 0,
        reason: format!("atomic rename failed: {e}"),
    })?;
    Ok(())
}

impl TrainerVariant {
    /// `u8` tag → 枚举值（D-350 binary header offset 12）。未知 tag 返回 `None`，
    /// 由 [`Checkpoint::open`] 包装成
    /// [`crate::error::CheckpointError::Corrupted`]。
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            0 => Some(TrainerVariant::VanillaCfr),
            1 => Some(TrainerVariant::EsMccfr),
            // stage 4 API-441 — EsMccfrLinearRmPlus 走 schema_version=2 路径。
            2 => Some(TrainerVariant::EsMccfrLinearRmPlus),
            _ => None,
        }
    }
}

impl GameVariant {
    /// `u8` tag → 枚举值（D-350 binary header offset 13）。
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            0 => Some(GameVariant::Kuhn),
            1 => Some(GameVariant::Leduc),
            2 => Some(GameVariant::SimplifiedNlhe),
            // stage 4 API-411 — Nlhe6Max 走 schema_version=2 路径（同
            // TrainerVariant::EsMccfrLinearRmPlus 注释）。
            3 => Some(GameVariant::Nlhe6Max),
            _ => None,
        }
    }
}

/// 共享 IO helper：读取 `path` → `Vec<u8>`，处理 FileNotFound 与一般 IO 错误。
///
/// [`Checkpoint::open`] + [`crate::training::Trainer::load_checkpoint`] 共用，
/// 避免重复 dispatch FileNotFound。
pub(crate) fn read_file_bytes(path: &Path) -> Result<Vec<u8>, CheckpointError> {
    match std::fs::read(path) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(CheckpointError::FileNotFound {
            path: path.to_owned(),
        }),
        Err(e) => Err(CheckpointError::Corrupted {
            offset: 0,
            reason: format!("io error reading {path:?}: {e}"),
        }),
    }
}

/// trainer 侧 preflight：在 [`Checkpoint::parse_bytes`]（含 trailer BLAKE3
/// 校验）之前 eager 校验 trainer_variant + game_variant + bucket_table_blake3。
///
/// 命中 mismatch 立即返回 [`CheckpointError::TrainerMismatch`] /
/// [`CheckpointError::BucketTableMismatch`]（必要 — 这两类失败路径
/// 通常也会破坏 trailer BLAKE3，若由 `parse_bytes` 先校验 trailer 就只能
/// 返回 `Corrupted`，掩盖了具体 mismatch 原因）。
///
/// 文件长度不足 / magic / schema / variant tag 不合法时，preflight 返回 `Ok(())`
/// 让后续 [`Checkpoint::parse_bytes`] 走标准 dispatch 返回更精确的错误
/// （FileNotFound 在 `read_file_bytes` 之前已处理）。
///
/// **§D2-revM dispatch**：bucket_table_blake3 在 v1 / v2 layout 字段偏移不同
///（v1 = 60 / v2 = 64），通过 schema_version 字段分流读取。
pub(crate) fn preflight_trainer(
    bytes: &[u8],
    expected_trainer: TrainerVariant,
    expected_game: GameVariant,
    expected_bucket_blake3: [u8; 32],
) -> Result<(), CheckpointError> {
    if bytes.len() < HEADER_LEN_V1 {
        return Ok(());
    }
    if bytes[OFFSET_MAGIC..OFFSET_SCHEMA_VERSION] != MAGIC {
        return Ok(());
    }
    let schema = u32::from_le_bytes(
        bytes[OFFSET_SCHEMA_VERSION..OFFSET_TRAINER_VARIANT]
            .try_into()
            .unwrap(),
    );
    let bucket_blake3_offset = match schema {
        SCHEMA_VERSION_V1 => OFFSET_V1_BUCKET_TABLE_BLAKE3,
        SCHEMA_VERSION => {
            if bytes.len() < HEADER_LEN {
                return Ok(());
            }
            OFFSET_BUCKET_TABLE_BLAKE3
        }
        // 不在受支持的 schema 集合里，把决定权移交 parse_bytes 走 SchemaMismatch。
        _ => return Ok(()),
    };
    let Some(tv) = TrainerVariant::from_u8(bytes[OFFSET_TRAINER_VARIANT]) else {
        return Ok(());
    };
    let Some(gv) = GameVariant::from_u8(bytes[OFFSET_GAME_VARIANT]) else {
        return Ok(());
    };
    if (tv, gv) != (expected_trainer, expected_game) {
        return Err(CheckpointError::TrainerMismatch {
            expected: (expected_trainer, expected_game),
            got: (tv, gv),
        });
    }
    let header_blake3: [u8; 32] = bytes[bucket_blake3_offset..bucket_blake3_offset + 32]
        .try_into()
        .unwrap();
    if header_blake3 != expected_bucket_blake3 {
        return Err(CheckpointError::BucketTableMismatch {
            expected: expected_bucket_blake3,
            got: header_blake3,
        });
    }
    Ok(())
}

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
//! | `regret_table_body` | `regret_table_offset` | varies | bincode 1.x serialized `Vec<(InfoSet, Vec<f64>)>` |
//! | `strategy_sum_body` | `strategy_sum_offset` | varies | bincode 1.x serialized `Vec<(InfoSet, Vec<f64>)>` |
//! | `trailer_blake3` | `len - 32` | 32 | bytes |
//!
//! Header 实际 108 byte，8 byte aligned；bincode body 起点 = `regret_table_offset`
//! 字段值，由写入器精确控制。
//!
//! D-327 bincode 序列化时按 InfoSet Debug-sort 顺序写入（确保 BLAKE3 byte-equal
//! across hosts）。D-352 trailer BLAKE3 eager 校验；D-353 write-to-temp + atomic
//! rename；D-356 多 game 不兼容 → [`crate::error::CheckpointError::TrainerMismatch`]
//! 由 [`crate::training::Trainer::load_checkpoint`] 在 [`Checkpoint::open`] 之前
//! eager 校验。

use std::io::Write;
use std::path::Path;

use blake3::Hasher;

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

/// Header 长度（API-350 binary layout）。
pub const HEADER_LEN: usize = 108;
/// Trailer BLAKE3 长度（API-350 / D-352）。
pub const TRAILER_LEN: usize = 32;

const OFFSET_MAGIC: usize = 0;
const OFFSET_SCHEMA_VERSION: usize = 8;
const OFFSET_TRAINER_VARIANT: usize = 12;
const OFFSET_GAME_VARIANT: usize = 13;
const OFFSET_PAD: usize = 14;
const OFFSET_UPDATE_COUNT: usize = 20;
const OFFSET_RNG_STATE: usize = 28;
pub(crate) const OFFSET_BUCKET_TABLE_BLAKE3: usize = 60;
const OFFSET_REGRET_TABLE_OFFSET: usize = 92;
const OFFSET_STRATEGY_SUM_OFFSET: usize = 100;

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
    /// rename 失败均归类到 Corrupted 兜底）。
    pub fn save(&self, path: &Path) -> Result<(), CheckpointError> {
        let regret_len = self.regret_table_bytes.len();
        let strategy_len = self.strategy_sum_bytes.len();
        let regret_table_offset = HEADER_LEN as u64;
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
        buf[OFFSET_UPDATE_COUNT..OFFSET_RNG_STATE]
            .copy_from_slice(&self.update_count.to_le_bytes());
        buf[OFFSET_RNG_STATE..OFFSET_BUCKET_TABLE_BLAKE3].copy_from_slice(&self.rng_state);
        buf[OFFSET_BUCKET_TABLE_BLAKE3..OFFSET_REGRET_TABLE_OFFSET]
            .copy_from_slice(&self.bucket_table_blake3);
        buf[OFFSET_REGRET_TABLE_OFFSET..OFFSET_STRATEGY_SUM_OFFSET]
            .copy_from_slice(&regret_table_offset.to_le_bytes());
        buf[OFFSET_STRATEGY_SUM_OFFSET..HEADER_LEN]
            .copy_from_slice(&strategy_sum_offset.to_le_bytes());
        buf[regret_table_offset as usize..strategy_sum_offset as usize]
            .copy_from_slice(&self.regret_table_bytes);
        buf[strategy_sum_offset as usize..body_end as usize]
            .copy_from_slice(&self.strategy_sum_bytes);

        let mut hasher = Hasher::new();
        hasher.update(&buf[..body_end as usize]);
        let trailer: [u8; 32] = hasher.finalize().into();
        buf[body_end as usize..total_len].copy_from_slice(&trailer);

        // D-353 atomic write：tempfile in 同 parent dir → persist (rename) 到目标
        // 路径；持有期间任意 SIGKILL / OOM / 断电中断都不会污染既有 `<path>`。
        let parent_dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let mut tmp = tempfile::NamedTempFile::new_in(parent_dir).map_err(|e| {
            CheckpointError::Corrupted {
                offset: 0,
                reason: format!("create temp file in {parent_dir:?} failed: {e}"),
            }
        })?;
        tmp.write_all(&buf)
            .map_err(|e| CheckpointError::Corrupted {
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

    /// 从 `path` 加载（D-352 eager BLAKE3 校验 + D-350 schema 校验）。
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
    /// 与 [`Self::open`] 同源 dispatch（FileNotFound 在 `open` 端已转换；本方法
    /// 不再触发 FileNotFound）。[`crate::training::Trainer::load_checkpoint`]
    /// 走 [`read_file_bytes`] + [`preflight_trainer`] + 本方法 3 步避免重复 IO。
    pub(crate) fn parse_bytes(bytes: &[u8]) -> Result<Self, CheckpointError> {
        let len = bytes.len();
        if len < HEADER_LEN + TRAILER_LEN {
            return Err(CheckpointError::Corrupted {
                offset: 0,
                reason: format!(
                    "file too short: {len} bytes (min header+trailer {})",
                    HEADER_LEN + TRAILER_LEN
                ),
            });
        }

        // 1. magic
        if bytes[OFFSET_MAGIC..OFFSET_SCHEMA_VERSION] != MAGIC {
            return Err(CheckpointError::Corrupted {
                offset: 0,
                reason: "magic mismatch (expected b\"PLCKPT\\0\\0\")".to_string(),
            });
        }

        // 2. schema (SchemaMismatch 比 trailer BLAKE3 优先；测试 schema_mismatch_via_byte_flip_at_offset_8 字面约束)
        let schema = u32::from_le_bytes(
            bytes[OFFSET_SCHEMA_VERSION..OFFSET_TRAINER_VARIANT]
                .try_into()
                .unwrap(),
        );
        if schema != SCHEMA_VERSION {
            return Err(CheckpointError::SchemaMismatch {
                expected: SCHEMA_VERSION,
                got: schema,
            });
        }

        // 3. trainer_variant / game_variant tag 解码（未知 tag → Corrupted；
        //    实际 variant 兼容性由 Trainer::load_checkpoint 在 open() 调用前已
        //    eager 拦截，本层只负责 tag → enum 解析）
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

        // 4. pad 区必须全 0（pad 优先于 trailer BLAKE3；测试
        //    corrupted_pad_nonzero_returns_corrupted 字面约束 reason 含 "pad"）
        for (i, &b) in bytes[OFFSET_PAD..OFFSET_UPDATE_COUNT].iter().enumerate() {
            if b != 0 {
                let off = OFFSET_PAD + i;
                return Err(CheckpointError::Corrupted {
                    offset: off as u64,
                    reason: format!("pad byte non-zero at offset {off}: 0x{b:02x}"),
                });
            }
        }

        // 5. trailer BLAKE3 eager 校验（D-352）
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

        // 6. 读取剩余 header 字段
        let update_count = u64::from_le_bytes(
            bytes[OFFSET_UPDATE_COUNT..OFFSET_RNG_STATE]
                .try_into()
                .unwrap(),
        );
        let rng_state: [u8; 32] = bytes[OFFSET_RNG_STATE..OFFSET_BUCKET_TABLE_BLAKE3]
            .try_into()
            .unwrap();
        let bucket_table_blake3: [u8; 32] = bytes
            [OFFSET_BUCKET_TABLE_BLAKE3..OFFSET_REGRET_TABLE_OFFSET]
            .try_into()
            .unwrap();
        let regret_table_offset = u64::from_le_bytes(
            bytes[OFFSET_REGRET_TABLE_OFFSET..OFFSET_STRATEGY_SUM_OFFSET]
                .try_into()
                .unwrap(),
        );
        let strategy_sum_offset = u64::from_le_bytes(
            bytes[OFFSET_STRATEGY_SUM_OFFSET..HEADER_LEN]
                .try_into()
                .unwrap(),
        );

        // 7. offset 表越界校验
        let trailer_start_u64 = trailer_start as u64;
        if regret_table_offset < HEADER_LEN as u64
            || regret_table_offset > strategy_sum_offset
            || strategy_sum_offset > trailer_start_u64
        {
            return Err(CheckpointError::Corrupted {
                offset: OFFSET_REGRET_TABLE_OFFSET as u64,
                reason: format!(
                    "offset table out of range: regret={regret_table_offset} \
                     strategy={strategy_sum_offset} trailer_start={trailer_start}"
                ),
            });
        }

        let regret_table_bytes =
            bytes[regret_table_offset as usize..strategy_sum_offset as usize].to_vec();
        let strategy_sum_bytes = bytes[strategy_sum_offset as usize..trailer_start].to_vec();

        Ok(Checkpoint {
            schema_version: schema,
            trainer_variant,
            game_variant,
            update_count,
            rng_state,
            bucket_table_blake3,
            regret_table_bytes,
            strategy_sum_bytes,
        })
    }
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
            // stage 3 schema_version=1 文件读到 tag=2 时由 SCHEMA_VERSION
            // mismatch 拒绝（schema_version 在 from_u8 之前 eager 校验）；本
            // helper 仅完成 tag → enum 映射，schema dispatch 由 stage 4 D2
            // \[实现\] 落地。
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
pub(crate) fn preflight_trainer(
    bytes: &[u8],
    expected_trainer: TrainerVariant,
    expected_game: GameVariant,
    expected_bucket_blake3: [u8; 32],
) -> Result<(), CheckpointError> {
    if bytes.len() < HEADER_LEN {
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
    if schema != SCHEMA_VERSION {
        return Ok(());
    }
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
    let header_blake3: [u8; 32] = bytes[OFFSET_BUCKET_TABLE_BLAKE3..OFFSET_REGRET_TABLE_OFFSET]
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

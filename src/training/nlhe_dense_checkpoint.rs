//! NLHE dense checkpoint v3（`docs/temp/nlhe_dense_infoset_table_plan_2026_05_26.md`
//! Phase 4）。把 [`DenseNlheTable`] 的 full dense 两表 + 训练元数据序列化成独立二进制
//! 格式，区别于 HashMap 路径的 [`crate::training::checkpoint`]（`PLCKPT\0\0` / schema
//! v2）。
//!
//! **为什么独立格式而非 bump 全局 schema**：HashMap path 的 schema v2 layout 没有
//! storage_kind / layout fingerprint 字段，直接 bump `SCHEMA_VERSION` 会废掉所有既有
//! v2 checkpoint 测试与产物。plan §非目标 明确「第一版不追求 old / dense checkpoint
//! 双向无损兼容，dense 需要 schema bump」——所以 dense 用自己的 magic
//! ([`DENSE_MAGIC`]) + schema_version 3 + storage_kind，与 v2 物理隔离。HashMap →
//! dense 的单向加载走 [`crate::training::nlhe_dense_trainer`] 的
//! `from_hashmap_checkpoint`（读 v2 ckpt 逐 entry 填 dense 表），不经本模块。
//!
//! **二进制 layout（全部 little-endian）**：
//!
//! | 字段 | 偏移 | 长度 | 说明 |
//! |---|---|---|---|
//! | `magic` | 0 | 8 | `b"PLDNCKPT"` |
//! | `schema_version` | 8 | 4 | u32 = 3 |
//! | `storage_kind` | 12 | 1 | u8 = 1（DenseNlheV1）|
//! | `trainer_variant` | 13 | 1 | u8 = EsMccfr |
//! | `game_variant` | 14 | 1 | u8 = SimplifiedNlhe |
//! | `lcfr_rescale_regret` | 15 | 1 | u8 bool |
//! | `update_count` | 16 | 8 | u64 |
//! | `lcfr_period_size` | 24 | 8 | u64（0 = None / vanilla）|
//! | `lcfr_periods_completed` | 32 | 8 | u64 |
//! | `num_nodes` | 40 | 8 | u64（fingerprint）|
//! | `total_rows` | 48 | 8 | u64（fingerprint）|
//! | `total_slots` | 56 | 8 | u64（fingerprint）|
//! | `rng_state` | 64 | 32 | bytes |
//! | `bucket_table_blake3` | 96 | 32 | bytes（fingerprint）|
//! | `action_count_blake3` | 128 | 32 | per-node (street,bucket_count,action_count) hash |
//! | `regret_touched` | 160 | `8·⌈rows/64⌉` | bitset words u64 LE |
//! | `strategy_touched` | … | `8·⌈rows/64⌉` | bitset words u64 LE |
//! | `regret_values` | … | `8·total_slots` | logical f64 LE（lazy scale 已 materialize 到流中） |
//! | `strategy_values` | … | `8·total_slots` | logical f64 LE（lazy scale 已 materialize 到流中） |
//! | `trailer_blake3` | `len-32` | 32 | BLAKE3 over 上面全部 byte |
//!
//! save / load 都 **streaming**（chunked，BufWriter / BufReader + 增量 BLAKE3），峰值
//! 内存 ≈ 两表本身 + 小缓冲，不额外整文件 buffer——目标 profile 两表 13.48 GiB 时这是
//! 必须的（plan §验证门槛 checkpoint 峰值约束）。save 走 write-to-temp + fsync +
//! atomic rename（同 [`crate::training::checkpoint`] D-353）。
//!
//! **fingerprint 校验**：load 时用调用方 game 重建的 indexer 算出 expected
//! fingerprint，与文件头逐字段比；bucket_table_blake3 不符 →
//! [`CheckpointError::BucketTableMismatch`]，node 数 / row / slot / per-node
//! action_count hash 不符 → [`CheckpointError::Corrupted`]（reason 含 layout
//! fingerprint）。这拦住「用 A 树 / abstraction 的数组误读成 B profile」的静默错误
//! （plan §风险 action abstraction 改变导致 index 错读）。

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use blake3::Hasher;

use crate::error::{CheckpointError, GameVariant, TrainerVariant};
use crate::training::nlhe_betting_tree::NodeId;
use crate::training::nlhe_dense::{DenseNlheTable, NlheDenseIndexer};

/// dense checkpoint magic（区别于 HashMap path 的 `PLCKPT\0\0`）。
pub const DENSE_MAGIC: [u8; 8] = *b"PLDNCKPT";
/// dense checkpoint schema version（plan §Checkpoint = 3）。
pub const DENSE_SCHEMA_VERSION: u32 = 3;
/// storage_kind tag = DenseNlheV1。
pub const STORAGE_KIND_DENSE_NLHE_V1: u8 = 1;
/// Header 长度（二进制 layout 表，全部定长字段）。
pub const HEADER_LEN: usize = 160;
/// Trailer BLAKE3 长度。
pub const TRAILER_LEN: usize = 32;

// streaming chunk：每次转换 / 读写 4096 个 u64/f64 = 32 KiB 栈缓冲。
const CHUNK_ELEMS: usize = 4096;
const CHUNK_BYTES: usize = CHUNK_ELEMS * 8;

// header 字段偏移
const OFF_MAGIC: usize = 0;
const OFF_SCHEMA: usize = 8;
const OFF_STORAGE_KIND: usize = 12;
const OFF_TRAINER_VARIANT: usize = 13;
const OFF_GAME_VARIANT: usize = 14;
const OFF_LCFR_RESCALE_REGRET: usize = 15;
const OFF_UPDATE_COUNT: usize = 16;
const OFF_LCFR_PERIOD_SIZE: usize = 24;
const OFF_LCFR_PERIODS_COMPLETED: usize = 32;
const OFF_NUM_NODES: usize = 40;
const OFF_TOTAL_ROWS: usize = 48;
const OFF_TOTAL_SLOTS: usize = 56;
const OFF_RNG_STATE: usize = 64;
const OFF_BUCKET_BLAKE3: usize = 96;
const OFF_ACTION_BLAKE3: usize = 128;

/// dense 表布局指纹（plan §Checkpoint indexer fingerprint）。
///
/// `action_count_blake3` 把每个节点的 `(street, bucket_count, action_count)` 序列
/// hash 进去——只要 betting tree 结构 / 按街 abstraction / bucket 数任一变化，hash 即
/// 变，加载旧数组立刻被拒。`num_nodes` / `total_rows` / `total_slots` 是冗余但便于
/// 早判（payload 尺寸即由它们决定）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DenseLayoutFingerprint {
    pub bucket_table_blake3: [u8; 32],
    pub num_nodes: u64,
    pub total_rows: u64,
    pub total_slots: u64,
    pub action_count_blake3: [u8; 32],
}

impl DenseLayoutFingerprint {
    /// 从 indexer + game bucket table content hash 算指纹。`action_count_blake3`
    /// 逐节点 feed `street as u8` + `bucket_count` LE + `action_count`。
    pub fn from_indexer(indexer: &NlheDenseIndexer, bucket_table_blake3: [u8; 32]) -> Self {
        let mut hasher = Hasher::new();
        for id in 0..indexer.num_nodes() as NodeId {
            let m = indexer.node_meta(id);
            hasher.update(&[m.street as u8]);
            hasher.update(&m.bucket_count.to_le_bytes());
            hasher.update(&[m.action_count]);
        }
        Self {
            bucket_table_blake3,
            num_nodes: indexer.num_nodes() as u64,
            total_rows: indexer.total_rows(),
            total_slots: indexer.total_slots(),
            action_count_blake3: hasher.finalize().into(),
        }
    }
}

/// dense checkpoint 训练元数据（plan §Checkpoint payload update_count / rng_state /
/// lcfr metadata）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DenseCheckpointMeta {
    pub update_count: u64,
    pub rng_state: [u8; 32],
    /// LCFR period 大小；`None` = vanilla ES-MCCFR。
    pub lcfr_period_size: Option<u64>,
    pub lcfr_periods_completed: u64,
    pub lcfr_rescale_regret: bool,
}

fn corrupted(offset: u64, reason: impl Into<String>) -> CheckpointError {
    CheckpointError::Corrupted {
        offset,
        reason: reason.into(),
    }
}

/// 写出 dense checkpoint 到 `path`（streaming + atomic rename）。
///
/// `regret` / `strategy_sum` 必须共享与 `fingerprint` 一致的 indexer（同棵 betting
/// tree slot 布局）——debug_assert 校验 `total_slots` / `total_rows`。
pub fn save_dense_checkpoint(
    path: &Path,
    fingerprint: &DenseLayoutFingerprint,
    meta: &DenseCheckpointMeta,
    regret: &DenseNlheTable,
    strategy_sum: &DenseNlheTable,
) -> Result<(), CheckpointError> {
    debug_assert_eq!(regret.raw_values().len() as u64, fingerprint.total_slots);
    debug_assert_eq!(
        strategy_sum.raw_values().len() as u64,
        fingerprint.total_slots
    );
    debug_assert_eq!(
        regret.touched_words().len(),
        strategy_sum.touched_words().len()
    );

    let mut header = [0u8; HEADER_LEN];
    header[OFF_MAGIC..OFF_SCHEMA].copy_from_slice(&DENSE_MAGIC);
    header[OFF_SCHEMA..OFF_STORAGE_KIND].copy_from_slice(&DENSE_SCHEMA_VERSION.to_le_bytes());
    header[OFF_STORAGE_KIND] = STORAGE_KIND_DENSE_NLHE_V1;
    header[OFF_TRAINER_VARIANT] = TrainerVariant::EsMccfr as u8;
    header[OFF_GAME_VARIANT] = GameVariant::SimplifiedNlhe as u8;
    header[OFF_LCFR_RESCALE_REGRET] = u8::from(meta.lcfr_rescale_regret);
    header[OFF_UPDATE_COUNT..OFF_LCFR_PERIOD_SIZE]
        .copy_from_slice(&meta.update_count.to_le_bytes());
    header[OFF_LCFR_PERIOD_SIZE..OFF_LCFR_PERIODS_COMPLETED]
        .copy_from_slice(&meta.lcfr_period_size.unwrap_or(0).to_le_bytes());
    header[OFF_LCFR_PERIODS_COMPLETED..OFF_NUM_NODES]
        .copy_from_slice(&meta.lcfr_periods_completed.to_le_bytes());
    header[OFF_NUM_NODES..OFF_TOTAL_ROWS].copy_from_slice(&fingerprint.num_nodes.to_le_bytes());
    header[OFF_TOTAL_ROWS..OFF_TOTAL_SLOTS].copy_from_slice(&fingerprint.total_rows.to_le_bytes());
    header[OFF_TOTAL_SLOTS..OFF_RNG_STATE].copy_from_slice(&fingerprint.total_slots.to_le_bytes());
    header[OFF_RNG_STATE..OFF_BUCKET_BLAKE3].copy_from_slice(&meta.rng_state);
    header[OFF_BUCKET_BLAKE3..OFF_ACTION_BLAKE3].copy_from_slice(&fingerprint.bucket_table_blake3);
    header[OFF_ACTION_BLAKE3..HEADER_LEN].copy_from_slice(&fingerprint.action_count_blake3);

    // atomic write：temp file in 同 parent dir → fsync → persist（rename）。
    let parent_dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let tmp = tempfile::NamedTempFile::new_in(parent_dir)
        .map_err(|e| corrupted(0, format!("create temp file in {parent_dir:?} failed: {e}")))?;
    let mut writer = BufWriter::new(tmp);
    let mut hasher = Hasher::new();

    write_hashed(&mut writer, &mut hasher, &header)?;
    write_words(&mut writer, &mut hasher, regret.touched_words())?;
    write_words(&mut writer, &mut hasher, strategy_sum.touched_words())?;
    write_f64s_scaled(
        &mut writer,
        &mut hasher,
        regret.raw_values(),
        regret.global_scale(),
    )?;
    write_f64s_scaled(
        &mut writer,
        &mut hasher,
        strategy_sum.raw_values(),
        strategy_sum.global_scale(),
    )?;
    let trailer: [u8; 32] = hasher.finalize().into();
    writer
        .write_all(&trailer)
        .map_err(|e| corrupted(0, format!("write trailer failed: {e}")))?;
    writer
        .flush()
        .map_err(|e| corrupted(0, format!("flush failed: {e}")))?;

    let tmp = writer
        .into_inner()
        .map_err(|e| corrupted(0, format!("BufWriter into_inner failed: {e}")))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| corrupted(0, format!("fsync temp file failed: {e}")))?;
    tmp.persist(path)
        .map_err(|e| corrupted(0, format!("atomic rename failed: {e}")))?;
    Ok(())
}

/// 从 `path` 加载 dense checkpoint，重建两张 [`DenseNlheTable`]（共享 `indexer`）。
///
/// `expected` 必须由同一 `indexer` + game bucket hash 经
/// [`DenseLayoutFingerprint::from_indexer`] 算出；header 指纹与之不符即拒绝
/// （见模块文档 fingerprint 校验）。streaming 读：先头后 bitset 后 raw f64，最后
/// 比对 trailer BLAKE3。
pub fn load_dense_checkpoint(
    path: &Path,
    expected: &DenseLayoutFingerprint,
    indexer: Arc<NlheDenseIndexer>,
) -> Result<(DenseCheckpointMeta, DenseNlheTable, DenseNlheTable), CheckpointError> {
    let file = File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CheckpointError::FileNotFound {
                path: path.to_owned(),
            }
        } else {
            corrupted(0, format!("io error opening {path:?}: {e}"))
        }
    })?;
    let mut reader = BufReader::new(file);
    let mut hasher = Hasher::new();

    let mut header = [0u8; HEADER_LEN];
    read_hashed(&mut reader, &mut hasher, &mut header)
        .map_err(|e| corrupted(0, format!("read header failed: {e}")))?;

    if header[OFF_MAGIC..OFF_SCHEMA] != DENSE_MAGIC {
        return Err(corrupted(0, "not a dense NLHE checkpoint (magic mismatch)"));
    }
    let schema = u32::from_le_bytes(header[OFF_SCHEMA..OFF_STORAGE_KIND].try_into().unwrap());
    if schema != DENSE_SCHEMA_VERSION {
        return Err(CheckpointError::SchemaMismatch {
            expected: DENSE_SCHEMA_VERSION,
            got: schema,
        });
    }
    if header[OFF_STORAGE_KIND] != STORAGE_KIND_DENSE_NLHE_V1 {
        return Err(corrupted(
            OFF_STORAGE_KIND as u64,
            format!("unknown storage_kind {}", header[OFF_STORAGE_KIND]),
        ));
    }
    let tv = TrainerVariant::from_u8(header[OFF_TRAINER_VARIANT]).ok_or_else(|| {
        corrupted(
            OFF_TRAINER_VARIANT as u64,
            format!(
                "unknown trainer_variant tag {}",
                header[OFF_TRAINER_VARIANT]
            ),
        )
    })?;
    let gv = GameVariant::from_u8(header[OFF_GAME_VARIANT]).ok_or_else(|| {
        corrupted(
            OFF_GAME_VARIANT as u64,
            format!("unknown game_variant tag {}", header[OFF_GAME_VARIANT]),
        )
    })?;
    if (tv, gv) != (TrainerVariant::EsMccfr, GameVariant::SimplifiedNlhe) {
        return Err(CheckpointError::TrainerMismatch {
            expected: (TrainerVariant::EsMccfr, GameVariant::SimplifiedNlhe),
            got: (tv, gv),
        });
    }

    let lcfr_rescale_regret = header[OFF_LCFR_RESCALE_REGRET] != 0;
    let update_count = u64::from_le_bytes(
        header[OFF_UPDATE_COUNT..OFF_LCFR_PERIOD_SIZE]
            .try_into()
            .unwrap(),
    );
    let lcfr_period_raw = u64::from_le_bytes(
        header[OFF_LCFR_PERIOD_SIZE..OFF_LCFR_PERIODS_COMPLETED]
            .try_into()
            .unwrap(),
    );
    let lcfr_periods_completed = u64::from_le_bytes(
        header[OFF_LCFR_PERIODS_COMPLETED..OFF_NUM_NODES]
            .try_into()
            .unwrap(),
    );
    let num_nodes = u64::from_le_bytes(header[OFF_NUM_NODES..OFF_TOTAL_ROWS].try_into().unwrap());
    let total_rows =
        u64::from_le_bytes(header[OFF_TOTAL_ROWS..OFF_TOTAL_SLOTS].try_into().unwrap());
    let total_slots =
        u64::from_le_bytes(header[OFF_TOTAL_SLOTS..OFF_RNG_STATE].try_into().unwrap());
    let rng_state: [u8; 32] = header[OFF_RNG_STATE..OFF_BUCKET_BLAKE3].try_into().unwrap();
    let bucket_blake3: [u8; 32] = header[OFF_BUCKET_BLAKE3..OFF_ACTION_BLAKE3]
        .try_into()
        .unwrap();
    let action_blake3: [u8; 32] = header[OFF_ACTION_BLAKE3..HEADER_LEN].try_into().unwrap();

    // fingerprint 校验：bucket hash 先（语义最明确），再整体 layout。
    if bucket_blake3 != expected.bucket_table_blake3 {
        return Err(CheckpointError::BucketTableMismatch {
            expected: expected.bucket_table_blake3,
            got: bucket_blake3,
        });
    }
    let got = DenseLayoutFingerprint {
        bucket_table_blake3: bucket_blake3,
        num_nodes,
        total_rows,
        total_slots,
        action_count_blake3: action_blake3,
    };
    if got != *expected {
        return Err(corrupted(
            OFF_NUM_NODES as u64,
            format!(
                "layout fingerprint mismatch: file(nodes={}, rows={}, slots={}) \
                 action_hash={} vs expected(nodes={}, rows={}, slots={})",
                num_nodes,
                total_rows,
                total_slots,
                if action_blake3 == expected.action_count_blake3 {
                    "match"
                } else {
                    "differ"
                },
                expected.num_nodes,
                expected.total_rows,
                expected.total_slots,
            ),
        ));
    }
    // fp == expected ⇒ indexer 的 total_rows/slots 与 header 一致，payload 尺寸自洽。
    debug_assert_eq!(indexer.total_slots(), total_slots);
    debug_assert_eq!(indexer.total_rows(), total_rows);

    let regret = DenseNlheTable::new(Arc::clone(&indexer));
    let strategy_sum = DenseNlheTable::new(indexer);

    read_words(&mut reader, &mut hasher, regret.touched_words())
        .map_err(|e| corrupted(HEADER_LEN as u64, format!("read regret touched: {e}")))?;
    read_words(&mut reader, &mut hasher, strategy_sum.touched_words())
        .map_err(|e| corrupted(0, format!("read strategy touched: {e}")))?;
    read_f64s(&mut reader, &mut hasher, regret.raw_values())
        .map_err(|e| corrupted(0, format!("read regret values: {e}")))?;
    read_f64s(&mut reader, &mut hasher, strategy_sum.raw_values())
        .map_err(|e| corrupted(0, format!("read strategy values: {e}")))?;

    // trailer：直接读 32 byte（不喂 hasher），与已累加的 body hash 比。
    let mut trailer = [0u8; TRAILER_LEN];
    reader
        .read_exact(&mut trailer)
        .map_err(|e| corrupted(0, format!("read trailer failed: {e}")))?;
    let actual: [u8; 32] = hasher.finalize().into();
    if actual != trailer {
        return Err(corrupted(
            0,
            "trailer BLAKE3 mismatch (body/header tampered)",
        ));
    }

    let meta = DenseCheckpointMeta {
        update_count,
        rng_state,
        lcfr_period_size: if lcfr_period_raw == 0 {
            None
        } else {
            Some(lcfr_period_raw)
        },
        lcfr_periods_completed,
        lcfr_rescale_regret,
    };
    Ok((meta, regret, strategy_sum))
}

// ---------------------------------------------------------------------------
// streaming helpers（chunked + 增量 BLAKE3）
// ---------------------------------------------------------------------------

fn write_hashed<W: Write>(w: &mut W, h: &mut Hasher, bytes: &[u8]) -> Result<(), CheckpointError> {
    h.update(bytes);
    w.write_all(bytes)
        .map_err(|e| corrupted(0, format!("write failed: {e}")))
}

fn write_words<W: Write>(
    w: &mut W,
    h: &mut Hasher,
    words: &[AtomicU64],
) -> Result<(), CheckpointError> {
    let mut buf = [0u8; CHUNK_BYTES];
    for chunk in words.chunks(CHUNK_ELEMS) {
        let mut off = 0;
        for cell in chunk {
            let word = cell.load(Ordering::Relaxed);
            buf[off..off + 8].copy_from_slice(&word.to_le_bytes());
            off += 8;
        }
        write_hashed(w, h, &buf[..off])?;
    }
    Ok(())
}

fn write_f64s_scaled<W: Write>(
    w: &mut W,
    h: &mut Hasher,
    vals: &[AtomicU64],
    scale: f64,
) -> Result<(), CheckpointError> {
    let mut buf = [0u8; CHUNK_BYTES];
    for chunk in vals.chunks(CHUNK_ELEMS) {
        let mut off = 0;
        for cell in chunk {
            let v = f64::from_bits(cell.load(Ordering::Relaxed));
            buf[off..off + 8].copy_from_slice(&(v * scale).to_le_bytes());
            off += 8;
        }
        write_hashed(w, h, &buf[..off])?;
    }
    Ok(())
}

fn read_hashed<R: Read>(r: &mut R, h: &mut Hasher, buf: &mut [u8]) -> std::io::Result<()> {
    r.read_exact(buf)?;
    h.update(buf);
    Ok(())
}

fn read_words<R: Read>(r: &mut R, h: &mut Hasher, words: &[AtomicU64]) -> std::io::Result<()> {
    let mut buf = [0u8; CHUNK_BYTES];
    for chunk in words.chunks(CHUNK_ELEMS) {
        let nbytes = chunk.len() * 8;
        read_hashed(r, h, &mut buf[..nbytes])?;
        for (cell, b) in chunk.iter().zip(buf[..nbytes].chunks_exact(8)) {
            cell.store(u64::from_le_bytes(b.try_into().unwrap()), Ordering::Relaxed);
        }
    }
    Ok(())
}

fn read_f64s<R: Read>(r: &mut R, h: &mut Hasher, vals: &[AtomicU64]) -> std::io::Result<()> {
    let mut buf = [0u8; CHUNK_BYTES];
    for chunk in vals.chunks(CHUNK_ELEMS) {
        let nbytes = chunk.len() * 8;
        read_hashed(r, h, &mut buf[..nbytes])?;
        for (cell, b) in chunk.iter().zip(buf[..nbytes].chunks_exact(8)) {
            let v = f64::from_le_bytes(b.try_into().unwrap());
            cell.store(v.to_bits(), Ordering::Relaxed);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstraction::info::StreetTag;
    use crate::training::nlhe::pack_info_set_v2;
    use crate::training::nlhe_dense::NlheNodeSpec;

    fn spec(street: StreetTag, bucket_count: u32, action_count: u8) -> NlheNodeSpec {
        NlheNodeSpec {
            street,
            bucket_count,
            action_count,
        }
    }

    // xorshift64 → 确定性伪随机 f64 ∈ [-1, 1)（含负 regret）。
    fn next_f64(state: &mut u64) -> f64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        ((x >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }

    fn fake_bucket_blake3(tag: u8) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0] = tag;
        b
    }

    /// 建两张已填合成 delta 的 dense 表（部分 bucket 未访问 + 一次 rescale）。
    fn build_filled_tables(
        idx: &Arc<NlheDenseIndexer>,
        specs: &[NlheNodeSpec],
    ) -> (DenseNlheTable, DenseNlheTable) {
        let mut regret = DenseNlheTable::new(Arc::clone(idx));
        let mut strategy = DenseNlheTable::new(Arc::clone(idx));
        let mut rng = 0x1234_5678_9ABC_DEF0_u64;
        for (node_id, s) in specs.iter().enumerate() {
            // 偶数 bucket 访问，奇数留作未访问行（验证 touched bitset roundtrip）。
            for bucket in (0..s.bucket_count).step_by(2) {
                let info = pack_info_set_v2(bucket, node_id as NodeId, s.street);
                let n = usize::from(s.action_count);
                let rd: Vec<f64> = (0..n).map(|_| next_f64(&mut rng) * 3.0).collect();
                let sd: Vec<f64> = (0..n).map(|_| (next_f64(&mut rng) + 1.0) * 0.5).collect();
                regret.accumulate_by_info(info, &rd);
                strategy.accumulate_by_info(info, &sd);
            }
        }
        // 触发一次 rescale，确保非平凡浮点值穿越序列化。
        regret.rescale_all(2.0 / 3.0);
        strategy.rescale_all(2.0 / 3.0);
        (regret, strategy)
    }

    fn meta_sample() -> DenseCheckpointMeta {
        DenseCheckpointMeta {
            update_count: 4242,
            rng_state: [7u8; 32],
            lcfr_period_size: Some(1000),
            lcfr_periods_completed: 4,
            lcfr_rescale_regret: true,
        }
    }

    fn logical_bits(table: &DenseNlheTable) -> Vec<u64> {
        let scale = table.global_scale();
        table
            .raw_values()
            .iter()
            .map(|c| (f64::from_bits(c.load(Ordering::Relaxed)) * scale).to_bits())
            .collect()
    }

    /// touched bitset words 加载成 `Vec<u64>`（test-only helper：AtomicU64 不实现
    /// `PartialEq`，直接 `assert_eq!` 两 slice 不行）。
    fn touched_word_bits(table: &DenseNlheTable) -> Vec<u64> {
        table
            .touched_words()
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect()
    }

    /// save → load 后 meta / 两表 raw values（byte-equal）/ touched bitset 完全一致。
    #[test]
    fn dense_checkpoint_roundtrip_byte_equal() {
        let specs = [
            spec(StreetTag::Preflop, 5, 3),
            spec(StreetTag::Flop, 4, 7),
            spec(StreetTag::Flop, 6, 6),
            spec(StreetTag::Turn, 3, 2),
            spec(StreetTag::River, 7, 4),
        ];
        let idx = Arc::new(NlheDenseIndexer::from_node_specs(specs.iter().copied()));
        let (regret, strategy) = build_filled_tables(&idx, &specs);
        let fp = DenseLayoutFingerprint::from_indexer(&idx, fake_bucket_blake3(0xAB));
        let meta = meta_sample();

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dense.ckpt");
        save_dense_checkpoint(&path, &fp, &meta, &regret, &strategy).expect("save");

        let (loaded_meta, loaded_regret, loaded_strategy) =
            load_dense_checkpoint(&path, &fp, Arc::clone(&idx)).expect("load");

        assert_eq!(loaded_meta, meta, "meta roundtrip");
        assert_eq!(
            logical_bits(&loaded_regret),
            logical_bits(&regret),
            "regret logical values byte-equal"
        );
        assert_eq!(
            logical_bits(&loaded_strategy),
            logical_bits(&strategy),
            "strategy logical values byte-equal"
        );
        assert_eq!(loaded_regret.global_scale().to_bits(), 1.0_f64.to_bits());
        assert_eq!(loaded_strategy.global_scale().to_bits(), 1.0_f64.to_bits());
        assert_eq!(
            touched_word_bits(&loaded_regret),
            touched_word_bits(&regret),
            "regret touched bitset roundtrip"
        );
        assert_eq!(
            touched_word_bits(&loaded_strategy),
            touched_word_bits(&strategy),
            "strategy touched bitset roundtrip"
        );
        // 未访问行加载后仍未 touched（区分 0 值 vs 未访问）。
        let unvisited = pack_info_set_v2(1, 0, StreetTag::Preflop);
        assert!(!loaded_regret.touched_row(idx.locate(unvisited).row_index));
    }

    /// fingerprint：bucket_table_blake3 不符 → BucketTableMismatch。
    #[test]
    fn dense_checkpoint_rejects_bucket_table_mismatch() {
        let specs = [spec(StreetTag::Preflop, 4, 3), spec(StreetTag::Flop, 5, 6)];
        let idx = Arc::new(NlheDenseIndexer::from_node_specs(specs.iter().copied()));
        let (regret, strategy) = build_filled_tables(&idx, &specs);
        let fp_save = DenseLayoutFingerprint::from_indexer(&idx, fake_bucket_blake3(0x11));
        let fp_load = DenseLayoutFingerprint::from_indexer(&idx, fake_bucket_blake3(0x22));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dense.ckpt");
        save_dense_checkpoint(&path, &fp_save, &meta_sample(), &regret, &strategy).expect("save");

        let err = load_dense_checkpoint(&path, &fp_load, Arc::clone(&idx)).unwrap_err();
        assert!(
            matches!(err, CheckpointError::BucketTableMismatch { .. }),
            "expected BucketTableMismatch, got {err:?}"
        );
    }

    /// fingerprint：node 数 / total_slots 不同（不同树）→ Corrupted（layout）。
    #[test]
    fn dense_checkpoint_rejects_different_tree() {
        let specs_a = [spec(StreetTag::Preflop, 4, 3), spec(StreetTag::Flop, 5, 6)];
        let idx_a = Arc::new(NlheDenseIndexer::from_node_specs(specs_a.iter().copied()));
        let (regret, strategy) = build_filled_tables(&idx_a, &specs_a);
        let bucket = fake_bucket_blake3(0x33);
        let fp_a = DenseLayoutFingerprint::from_indexer(&idx_a, bucket);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dense.ckpt");
        save_dense_checkpoint(&path, &fp_a, &meta_sample(), &regret, &strategy).expect("save");

        // 不同树：多一个节点 → num_nodes / total_slots 不同。
        let specs_b = [
            spec(StreetTag::Preflop, 4, 3),
            spec(StreetTag::Flop, 5, 6),
            spec(StreetTag::Turn, 2, 2),
        ];
        let idx_b = Arc::new(NlheDenseIndexer::from_node_specs(specs_b.iter().copied()));
        let fp_b = DenseLayoutFingerprint::from_indexer(&idx_b, bucket);

        let err = load_dense_checkpoint(&path, &fp_b, idx_b).unwrap_err();
        assert!(
            matches!(err, CheckpointError::Corrupted { .. }),
            "expected Corrupted(layout fingerprint), got {err:?}"
        );
    }

    /// fingerprint 强校验：**同 total_rows/total_slots/num_nodes 但 action_count
    /// 序列不同**（abstraction 改了 raise 集合）也必须被 action_count_blake3 抓到。
    /// A=[(P,6,2),(F,6,3)] 与 B=[(P,6,3),(F,6,2)] 的 rows/slots 完全相同（30 slot /
    /// 12 row），只有 per-node action 分布不同。
    #[test]
    fn dense_checkpoint_rejects_same_totals_different_action_layout() {
        let specs_a = [spec(StreetTag::Preflop, 6, 2), spec(StreetTag::Flop, 6, 3)];
        let specs_b = [spec(StreetTag::Preflop, 6, 3), spec(StreetTag::Flop, 6, 2)];
        let idx_a = Arc::new(NlheDenseIndexer::from_node_specs(specs_a.iter().copied()));
        let idx_b = Arc::new(NlheDenseIndexer::from_node_specs(specs_b.iter().copied()));
        // 前置：两者 totals 真的相等，证明只有 action 序列区分。
        assert_eq!(idx_a.total_rows(), idx_b.total_rows());
        assert_eq!(idx_a.total_slots(), idx_b.total_slots());
        assert_eq!(idx_a.num_nodes(), idx_b.num_nodes());

        let (regret, strategy) = build_filled_tables(&idx_a, &specs_a);
        let bucket = fake_bucket_blake3(0x44);
        let fp_a = DenseLayoutFingerprint::from_indexer(&idx_a, bucket);
        let fp_b = DenseLayoutFingerprint::from_indexer(&idx_b, bucket);
        assert_ne!(
            fp_a.action_count_blake3, fp_b.action_count_blake3,
            "action_count hash 必须区分两种布局"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dense.ckpt");
        save_dense_checkpoint(&path, &fp_a, &meta_sample(), &regret, &strategy).expect("save");

        let err = load_dense_checkpoint(&path, &fp_b, idx_b).unwrap_err();
        assert!(
            matches!(err, CheckpointError::Corrupted { .. }),
            "expected Corrupted(action layout), got {err:?}"
        );
    }

    /// trailer：翻一个 body byte → BLAKE3 mismatch → Corrupted。
    #[test]
    fn dense_checkpoint_rejects_corrupted_body() {
        let specs = [spec(StreetTag::Preflop, 4, 3), spec(StreetTag::Flop, 5, 6)];
        let idx = Arc::new(NlheDenseIndexer::from_node_specs(specs.iter().copied()));
        let (regret, strategy) = build_filled_tables(&idx, &specs);
        let fp = DenseLayoutFingerprint::from_indexer(&idx, fake_bucket_blake3(0x55));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dense.ckpt");
        save_dense_checkpoint(&path, &fp, &meta_sample(), &regret, &strategy).expect("save");

        // 翻一个 values 区的 byte（HEADER 之后偏移足够深，避免命中 fingerprint 字段）。
        let mut bytes = std::fs::read(&path).expect("read");
        let flip = bytes.len() - TRAILER_LEN - 8; // body 末尾附近
        bytes[flip] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("rewrite");

        let err = load_dense_checkpoint(&path, &fp, Arc::clone(&idx)).unwrap_err();
        assert!(
            matches!(err, CheckpointError::Corrupted { .. }),
            "expected Corrupted(trailer BLAKE3), got {err:?}"
        );
    }

    /// 不存在的路径 → FileNotFound。
    #[test]
    fn dense_checkpoint_file_not_found() {
        let specs = [spec(StreetTag::Preflop, 4, 3)];
        let idx = Arc::new(NlheDenseIndexer::from_node_specs(specs.iter().copied()));
        let fp = DenseLayoutFingerprint::from_indexer(&idx, fake_bucket_blake3(0x66));
        let err =
            load_dense_checkpoint(Path::new("/nonexistent/dense.ckpt"), &fp, idx).unwrap_err();
        assert!(
            matches!(err, CheckpointError::FileNotFound { .. }),
            "expected FileNotFound, got {err:?}"
        );
    }

    /// schema version 不符（手动改文件 offset 8）→ SchemaMismatch。
    #[test]
    fn dense_checkpoint_rejects_schema_mismatch() {
        let specs = [spec(StreetTag::Preflop, 4, 3), spec(StreetTag::Flop, 5, 6)];
        let idx = Arc::new(NlheDenseIndexer::from_node_specs(specs.iter().copied()));
        let (regret, strategy) = build_filled_tables(&idx, &specs);
        let fp = DenseLayoutFingerprint::from_indexer(&idx, fake_bucket_blake3(0x77));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dense.ckpt");
        save_dense_checkpoint(&path, &fp, &meta_sample(), &regret, &strategy).expect("save");

        let mut bytes = std::fs::read(&path).expect("read");
        bytes[OFF_SCHEMA] = 99; // schema 低字节改 99
        std::fs::write(&path, &bytes).expect("rewrite");

        let err = load_dense_checkpoint(&path, &fp, idx).unwrap_err();
        assert!(
            matches!(err, CheckpointError::SchemaMismatch { got: 99, .. }),
            "expected SchemaMismatch got=99, got {err:?}"
        );
    }
}

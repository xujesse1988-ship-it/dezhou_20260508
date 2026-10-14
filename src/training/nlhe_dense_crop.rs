//! dense blueprint 按街裁切（只保留前若干街，如翻前 + 翻牌）。
//!
//! 输入 = dense checkpoint v3（[`crate::training::nlhe_dense_checkpoint`]，两张 f64 表全树）；
//! 输出 = 独立的 crop 文件，只含 `street <= keep_through` 节点的行，**数值逐位不变**（只删
//! 行，不量化、不重算）。对局 / 评测只读 average strategy，转牌 / 河牌由实时搜索接管时，
//! 裁掉后两街可把 blueprint 从全树大小压到前两街的大小（9-max cap4 500 桶：51.8 → 7.8 GiB，
//! `keep_regret = false` 再减半）。
//!
//! **布局不存逐节点映射**：betting tree 是 DFS 编号，翻前 / 翻牌节点与转牌 / 河牌节点交错，
//! 保留节点不是连续前缀；但「哪些节点保留、各占多少行 / slot」完全由（全树 indexer +
//! `keep_through`）决定。故 crop 文件只记全树 layout 指纹，读方用同一 game 重建全树
//! indexer → 校验指纹 → 现算紧凑前缀和。指纹不符（不同树 / 抽象 / 桶表）即拒绝。
//!
//! **二进制 layout（全部 little-endian）**：
//!
//! | 字段 | 偏移 | 长度 | 说明 |
//! |---|---|---|---|
//! | `magic` | 0 | 8 | `b"PLDNCROP"` |
//! | `schema_version` | 8 | 4 | u32 = 1 |
//! | `keep_through` | 12 | 1 | [`StreetTag`] as u8（保留 `street <= keep_through`）|
//! | `has_regret` | 13 | 1 | u8 bool（是否保留 regret 值表）|
//! | `lcfr_rescale_regret` | 14 | 1 | u8 bool（源 ckpt 元数据，仅记录）|
//! | `reserved` | 15 | 1 | 0 |
//! | `update_count` | 16 | 8 | 源 ckpt 的 update 数 |
//! | `lcfr_period_size` | 24 | 8 | 源 ckpt（0 = vanilla）|
//! | `lcfr_periods_completed` | 32 | 8 | 源 ckpt |
//! | `src_num_nodes` / `src_total_rows` / `src_total_slots` | 40 / 48 / 56 | 8 ×3 | 全树指纹 |
//! | `kept_nodes` / `kept_rows` / `kept_slots` | 64 / 72 / 80 | 8 ×3 | 裁切后尺寸 |
//! | `bucket_table_blake3` | 88 | 32 | 全树指纹 |
//! | `action_count_blake3` | 120 | 32 | 全树指纹 |
//! | `regret_touched` | 152 | `8·⌈kept_rows/64⌉` | 保留行的 bitset |
//! | `strategy_touched` | … | `8·⌈kept_rows/64⌉` | 同上 |
//! | `regret_values` | … | `8·kept_slots`（`has_regret` 时）| f64 逻辑值 |
//! | `strategy_values` | … | `8·kept_slots` | f64 逻辑值 |
//! | `trailer_blake3` | `len-32` | 32 | BLAKE3 over 上面全部 byte |
//!
//! 两张 touched bitset 无论 `has_regret` 都保留：[`CroppedBlueprint::average_strategy`] 的
//! 「已访问」判定 = 任一表 touched（与 `DenseNlheEsMccfrTrainer::average_strategy` 同），
//! 缺 regret bitset 会把「只有 regret 被写过」的行误判为未访问。

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;

use blake3::Hasher;

use crate::abstraction::info::{InfoSetId, StreetTag};
use crate::error::CheckpointError;
use crate::training::game::Game;
use crate::training::nlhe::{SimplifiedNlheGame, NLHE_V2_NODE_ID_SHIFT};
use crate::training::nlhe_betting_tree::NodeId;
use crate::training::nlhe_dense::NlheDenseIndexer;
use crate::training::nlhe_dense_checkpoint::{
    corrupted, parse_header, read_hashed, DenseCheckpointMeta, DenseLayoutFingerprint, HEADER_LEN,
    TRAILER_LEN,
};
use crate::training::nlhe_dense_trainer::bucket_count_by_street;

/// crop 文件 magic（区别于 dense ckpt 的 `PLDNCKPT`）。
pub const CROP_MAGIC: [u8; 8] = *b"PLDNCROP";
/// crop 文件 schema version。
pub const CROP_SCHEMA_VERSION: u32 = 1;
/// crop header 长度。
pub const CROP_HEADER_LEN: usize = 152;

const OFF_MAGIC: usize = 0;
const OFF_SCHEMA: usize = 8;
const OFF_KEEP_THROUGH: usize = 12;
const OFF_HAS_REGRET: usize = 13;
const OFF_LCFR_RESCALE_REGRET: usize = 14;
const OFF_UPDATE_COUNT: usize = 16;
const OFF_LCFR_PERIOD_SIZE: usize = 24;
const OFF_LCFR_PERIODS_COMPLETED: usize = 32;
const OFF_SRC_NUM_NODES: usize = 40;
const OFF_SRC_TOTAL_ROWS: usize = 48;
const OFF_SRC_TOTAL_SLOTS: usize = 56;
const OFF_KEPT_NODES: usize = 64;
const OFF_KEPT_ROWS: usize = 72;
const OFF_KEPT_SLOTS: usize = 80;
const OFF_BUCKET_BLAKE3: usize = 88;
const OFF_ACTION_BLAKE3: usize = 120;

/// 流式拷贝缓冲（1 MiB）。
const COPY_BUF_BYTES: usize = 1 << 20;
/// 读写 BufReader / BufWriter 容量（8 MiB，大文件顺序 IO）。
const IO_BUF_BYTES: usize = 8 << 20;

/// 裁切选项。
#[derive(Clone, Copy, Debug)]
pub struct CropOptions {
    /// 保留 `street <= keep_through` 的节点（`Flop` = 翻前 + 翻牌）。
    pub keep_through: StreetTag,
    /// 是否保留 regret 值表。对局 / 评测只读 average strategy（strategy 表），`false`
    /// 可再减半；touched bitset 两张都保留（见模块文档）。
    pub keep_regret: bool,
}

/// 裁切结果摘要。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CropSummary {
    pub source_update_count: u64,
    pub source_nodes: u64,
    pub source_rows: u64,
    pub source_slots: u64,
    pub kept_nodes: u64,
    pub kept_rows: u64,
    pub kept_slots: u64,
    /// 输出文件字节数。
    pub output_bytes: u64,
}

/// 保留节点的紧凑布局：`base[node] = (row_base, slot_base)`，不保留 = `None`。
struct KeptLayout {
    base: Vec<Option<(u64, u64)>>,
    kept_nodes: u64,
    kept_rows: u64,
    kept_slots: u64,
}

impl KeptLayout {
    fn new(indexer: &NlheDenseIndexer, keep_through: StreetTag) -> Self {
        let mut base = Vec::with_capacity(indexer.num_nodes());
        let (mut kept_nodes, mut kept_rows, mut kept_slots) = (0u64, 0u64, 0u64);
        for id in 0..indexer.num_nodes() as NodeId {
            let m = indexer.node_meta(id);
            if m.street as u8 <= keep_through as u8 {
                base.push(Some((kept_rows, kept_slots)));
                kept_nodes += 1;
                kept_rows += u64::from(m.bucket_count);
                kept_slots += u64::from(m.bucket_count) * u64::from(m.action_count);
            } else {
                base.push(None);
            }
        }
        Self {
            base,
            kept_nodes,
            kept_rows,
            kept_slots,
        }
    }
}

fn words_for_rows(rows: u64) -> usize {
    rows.div_ceil(64) as usize
}

fn bit_get(words: &[u64], i: u64) -> bool {
    (words[(i / 64) as usize] >> (i % 64)) & 1 == 1
}

fn bit_set(words: &mut [u64], i: u64) {
    words[(i / 64) as usize] |= 1u64 << (i % 64);
}

fn street_from_u8(v: u8) -> Option<StreetTag> {
    Some(match v {
        0 => StreetTag::Preflop,
        1 => StreetTag::Flop,
        2 => StreetTag::Turn,
        3 => StreetTag::River,
        _ => return None,
    })
}

/// 用 `game` 重建全树 indexer（与 `DenseNlheEsMccfrTrainer::new` 同一构造）+ 期望指纹。
fn full_indexer_and_fingerprint(
    game: &SimplifiedNlheGame,
) -> (NlheDenseIndexer, DenseLayoutFingerprint) {
    let indexer = NlheDenseIndexer::from_tree(game.tree(), bucket_count_by_street(game));
    let fp = DenseLayoutFingerprint::from_indexer(&indexer, game.bucket_table_blake3());
    (indexer, fp)
}

fn read_words_hashed<R: Read>(
    r: &mut R,
    h: &mut Hasher,
    n_words: usize,
) -> std::io::Result<Vec<u64>> {
    let mut words = vec![0u64; n_words];
    let mut buf = vec![0u8; COPY_BUF_BYTES];
    for chunk in words.chunks_mut(COPY_BUF_BYTES / 8) {
        let nbytes = chunk.len() * 8;
        read_hashed(r, h, &mut buf[..nbytes])?;
        for (w, b) in chunk.iter_mut().zip(buf[..nbytes].chunks_exact(8)) {
            *w = u64::from_le_bytes(b.try_into().unwrap());
        }
    }
    Ok(words)
}

fn write_words_hashed<W: Write>(w: &mut W, h: &mut Hasher, words: &[u64]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(COPY_BUF_BYTES);
    for chunk in words.chunks(COPY_BUF_BYTES / 8) {
        buf.clear();
        for word in chunk {
            buf.extend_from_slice(&word.to_le_bytes());
        }
        h.update(&buf);
        w.write_all(&buf)?;
    }
    Ok(())
}

/// 从 `r` 读 `nbytes`（喂 `rh`）；`out = Some` 时原样写出（喂 `wh`），`None` 时丢弃。
fn copy_or_skip<R: Read, W: Write>(
    r: &mut R,
    rh: &mut Hasher,
    out: Option<(&mut W, &mut Hasher)>,
    mut nbytes: u64,
    buf: &mut [u8],
) -> std::io::Result<()> {
    let mut out = out;
    while nbytes > 0 {
        let n = nbytes.min(buf.len() as u64) as usize;
        read_hashed(r, rh, &mut buf[..n])?;
        if let Some((w, wh)) = out.as_mut() {
            wh.update(&buf[..n]);
            w.write_all(&buf[..n])?;
        }
        nbytes -= n as u64;
    }
    Ok(())
}

/// 读 32 byte trailer 并与 `h` 比对，再确认已到 EOF（多余尾字节 = 文件异常）。
fn verify_trailer_and_eof<R: Read>(
    r: &mut R,
    h: Hasher,
    what: &str,
) -> Result<(), CheckpointError> {
    let mut trailer = [0u8; TRAILER_LEN];
    r.read_exact(&mut trailer)
        .map_err(|e| corrupted(0, format!("{what}: read trailer failed: {e}")))?;
    let actual: [u8; 32] = h.finalize().into();
    if actual != trailer {
        return Err(corrupted(
            0,
            format!("{what}: trailer BLAKE3 mismatch (body/header tampered)"),
        ));
    }
    let mut extra = [0u8; 1];
    match r.read(&mut extra) {
        Ok(0) => Ok(()),
        Ok(_) => Err(corrupted(
            0,
            format!("{what}: trailing bytes after trailer"),
        )),
        Err(e) => Err(corrupted(
            0,
            format!("{what}: read after trailer failed: {e}"),
        )),
    }
}

fn open_reader(path: &Path) -> Result<BufReader<File>, CheckpointError> {
    let file = File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CheckpointError::FileNotFound {
                path: path.to_owned(),
            }
        } else {
            corrupted(0, format!("io error opening {path:?}: {e}"))
        }
    })?;
    Ok(BufReader::with_capacity(IO_BUF_BYTES, file))
}

/// 把 dense checkpoint `src` 裁成只含 `street <= opts.keep_through` 的 crop 文件 `dst`。
///
/// `game` 必须与训练 `src` 时同一（树 / 抽象 / 桶表）——先用它重建全树 layout 指纹与 `src`
/// header 比对，不符即拒绝。全程流式：内存 ≈ 全树 indexer + 两张全树 touched bitset
/// （9-max cap4 500 桶 ≈ 0.5 GiB），不载入两张值表。读完 `src` 校验其 trailer BLAKE3，
/// 失败则不产出 `dst`（写 temp file，成功才 fsync + atomic rename）。
pub fn crop_dense_checkpoint(
    src: &Path,
    dst: &Path,
    game: &SimplifiedNlheGame,
    opts: CropOptions,
) -> Result<CropSummary, CheckpointError> {
    let (indexer, expected) = full_indexer_and_fingerprint(game);
    let layout = KeptLayout::new(&indexer, opts.keep_through);

    let mut reader = open_reader(src)?;
    let mut rh = Hasher::new();
    let mut header = [0u8; HEADER_LEN];
    read_hashed(&mut reader, &mut rh, &mut header)
        .map_err(|e| corrupted(0, format!("read source header failed: {e}")))?;
    let meta: DenseCheckpointMeta = parse_header(&header, &expected)?;

    // 1. 两张全树 touched bitset → 只取保留行，压成紧凑 bitset。
    let src_words = words_for_rows(expected.total_rows);
    let regret_touched = read_words_hashed(&mut reader, &mut rh, src_words)
        .map_err(|e| corrupted(HEADER_LEN as u64, format!("read regret touched: {e}")))?;
    let strategy_touched = read_words_hashed(&mut reader, &mut rh, src_words)
        .map_err(|e| corrupted(0, format!("read strategy touched: {e}")))?;
    let kept_words = words_for_rows(layout.kept_rows);
    let mut kept_regret_touched = vec![0u64; kept_words];
    let mut kept_strategy_touched = vec![0u64; kept_words];
    for id in 0..indexer.num_nodes() as NodeId {
        let Some((kept_row_base, _)) = layout.base[id as usize] else {
            continue;
        };
        let m = indexer.node_meta(id);
        for b in 0..u64::from(m.bucket_count) {
            if bit_get(&regret_touched, m.row_base + b) {
                bit_set(&mut kept_regret_touched, kept_row_base + b);
            }
            if bit_get(&strategy_touched, m.row_base + b) {
                bit_set(&mut kept_strategy_touched, kept_row_base + b);
            }
        }
    }
    drop((regret_touched, strategy_touched));

    // 2. crop header。
    let mut out_header = [0u8; CROP_HEADER_LEN];
    out_header[OFF_MAGIC..OFF_SCHEMA].copy_from_slice(&CROP_MAGIC);
    out_header[OFF_SCHEMA..OFF_KEEP_THROUGH].copy_from_slice(&CROP_SCHEMA_VERSION.to_le_bytes());
    out_header[OFF_KEEP_THROUGH] = opts.keep_through as u8;
    out_header[OFF_HAS_REGRET] = u8::from(opts.keep_regret);
    out_header[OFF_LCFR_RESCALE_REGRET] = u8::from(meta.lcfr_rescale_regret);
    let u64_fields = [
        (OFF_UPDATE_COUNT, meta.update_count),
        (OFF_LCFR_PERIOD_SIZE, meta.lcfr_period_size.unwrap_or(0)),
        (OFF_LCFR_PERIODS_COMPLETED, meta.lcfr_periods_completed),
        (OFF_SRC_NUM_NODES, expected.num_nodes),
        (OFF_SRC_TOTAL_ROWS, expected.total_rows),
        (OFF_SRC_TOTAL_SLOTS, expected.total_slots),
        (OFF_KEPT_NODES, layout.kept_nodes),
        (OFF_KEPT_ROWS, layout.kept_rows),
        (OFF_KEPT_SLOTS, layout.kept_slots),
    ];
    for (off, v) in u64_fields {
        out_header[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }
    out_header[OFF_BUCKET_BLAKE3..OFF_ACTION_BLAKE3].copy_from_slice(&expected.bucket_table_blake3);
    out_header[OFF_ACTION_BLAKE3..CROP_HEADER_LEN].copy_from_slice(&expected.action_count_blake3);

    let parent_dir = match dst.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let tmp = tempfile::NamedTempFile::new_in(parent_dir)
        .map_err(|e| corrupted(0, format!("create temp file in {parent_dir:?} failed: {e}")))?;
    let mut writer = BufWriter::with_capacity(IO_BUF_BYTES, tmp);
    let mut wh = Hasher::new();
    let werr = |e: std::io::Error| corrupted(0, format!("write crop output failed: {e}"));

    wh.update(&out_header);
    writer.write_all(&out_header).map_err(werr)?;
    write_words_hashed(&mut writer, &mut wh, &kept_regret_touched).map_err(werr)?;
    write_words_hashed(&mut writer, &mut wh, &kept_strategy_touched).map_err(werr)?;

    // 3. 两张值表按节点顺序流式过：保留节点的 slot 段拷出，其余读过即弃（仍喂源 hash）。
    let mut buf = vec![0u8; COPY_BUF_BYTES];
    for (table, keep_table) in [("regret", opts.keep_regret), ("strategy", true)] {
        for id in 0..indexer.num_nodes() as NodeId {
            let m = indexer.node_meta(id);
            let nbytes = u64::from(m.bucket_count) * u64::from(m.action_count) * 8;
            let out = (keep_table && layout.base[id as usize].is_some())
                .then_some((&mut writer, &mut wh));
            copy_or_skip(&mut reader, &mut rh, out, nbytes, &mut buf)
                .map_err(|e| corrupted(0, format!("copy {table} values (node {id}): {e}")))?;
        }
    }

    // 4. 源 trailer 校验通过才落盘输出。
    verify_trailer_and_eof(&mut reader, rh, "source checkpoint")?;
    let trailer: [u8; 32] = wh.finalize().into();
    writer.write_all(&trailer).map_err(werr)?;
    writer.flush().map_err(werr)?;
    let tmp = writer
        .into_inner()
        .map_err(|e| corrupted(0, format!("BufWriter into_inner failed: {e}")))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| corrupted(0, format!("fsync temp file failed: {e}")))?;
    let file = tmp
        .persist(dst)
        .map_err(|e| corrupted(0, format!("atomic rename failed: {e}")))?;
    let output_bytes = file
        .metadata()
        .map_err(|e| corrupted(0, format!("stat output failed: {e}")))?
        .len();

    Ok(CropSummary {
        source_update_count: meta.update_count,
        source_nodes: expected.num_nodes,
        source_rows: expected.total_rows,
        source_slots: expected.total_slots,
        kept_nodes: layout.kept_nodes,
        kept_rows: layout.kept_rows,
        kept_slots: layout.kept_slots,
        output_bytes,
    })
}

/// 加载后的裁切 blueprint：保留街上的 average strategy 查询与原
/// `DenseNlheEsMccfrTrainer::average_strategy` **逐位一致**；被裁掉的街返回 `None`。
pub struct CroppedBlueprint {
    indexer: Arc<NlheDenseIndexer>,
    layout: KeptLayout,
    keep_through: StreetTag,
    update_count: u64,
    regret_touched: Vec<u64>,
    strategy_touched: Vec<u64>,
    regret: Option<Vec<f64>>,
    strategy: Vec<f64>,
}

impl CroppedBlueprint {
    /// 从 crop 文件加载。`game` 须与训练源 ckpt 时同一（全树指纹不符即拒绝）。
    pub fn load(path: &Path, game: &SimplifiedNlheGame) -> Result<Self, CheckpointError> {
        let (indexer, expected) = full_indexer_and_fingerprint(game);
        let mut reader = open_reader(path)?;
        let mut h = Hasher::new();
        let mut header = [0u8; CROP_HEADER_LEN];
        read_hashed(&mut reader, &mut h, &mut header)
            .map_err(|e| corrupted(0, format!("read crop header failed: {e}")))?;

        if header[OFF_MAGIC..OFF_SCHEMA] != CROP_MAGIC {
            return Err(corrupted(
                0,
                "not a cropped dense blueprint (magic mismatch)",
            ));
        }
        let schema = u32::from_le_bytes(header[OFF_SCHEMA..OFF_KEEP_THROUGH].try_into().unwrap());
        if schema != CROP_SCHEMA_VERSION {
            return Err(CheckpointError::SchemaMismatch {
                expected: CROP_SCHEMA_VERSION,
                got: schema,
            });
        }
        let keep_through = street_from_u8(header[OFF_KEEP_THROUGH]).ok_or_else(|| {
            corrupted(
                OFF_KEEP_THROUGH as u64,
                format!("invalid keep_through {}", header[OFF_KEEP_THROUGH]),
            )
        })?;
        let has_regret = match header[OFF_HAS_REGRET] {
            0 => false,
            1 => true,
            v => {
                return Err(corrupted(
                    OFF_HAS_REGRET as u64,
                    format!("invalid has_regret {v}"),
                ))
            }
        };
        let rd = |off: usize| u64::from_le_bytes(header[off..off + 8].try_into().unwrap());

        let bucket_blake3: [u8; 32] = header[OFF_BUCKET_BLAKE3..OFF_ACTION_BLAKE3]
            .try_into()
            .unwrap();
        if bucket_blake3 != expected.bucket_table_blake3 {
            return Err(CheckpointError::BucketTableMismatch {
                expected: expected.bucket_table_blake3,
                got: bucket_blake3,
            });
        }
        let got = DenseLayoutFingerprint {
            bucket_table_blake3: bucket_blake3,
            num_nodes: rd(OFF_SRC_NUM_NODES),
            total_rows: rd(OFF_SRC_TOTAL_ROWS),
            total_slots: rd(OFF_SRC_TOTAL_SLOTS),
            action_count_blake3: header[OFF_ACTION_BLAKE3..CROP_HEADER_LEN]
                .try_into()
                .unwrap(),
        };
        if got != expected {
            return Err(corrupted(
                OFF_SRC_NUM_NODES as u64,
                format!(
                    "source layout fingerprint mismatch: file(nodes={}, rows={}, slots={}) vs \
                     game(nodes={}, rows={}, slots={})——crop 文件不是用这个树 / 抽象训出来的",
                    got.num_nodes,
                    got.total_rows,
                    got.total_slots,
                    expected.num_nodes,
                    expected.total_rows,
                    expected.total_slots,
                ),
            ));
        }
        let layout = KeptLayout::new(&indexer, keep_through);
        let declared = (rd(OFF_KEPT_NODES), rd(OFF_KEPT_ROWS), rd(OFF_KEPT_SLOTS));
        if declared != (layout.kept_nodes, layout.kept_rows, layout.kept_slots) {
            return Err(corrupted(
                OFF_KEPT_NODES as u64,
                format!(
                    "kept layout mismatch: file {declared:?} vs recomputed ({}, {}, {})",
                    layout.kept_nodes, layout.kept_rows, layout.kept_slots
                ),
            ));
        }

        let n_words = words_for_rows(layout.kept_rows);
        let rerr = |what: &'static str| {
            move |e: std::io::Error| corrupted(0, format!("read crop {what}: {e}"))
        };
        let regret_touched =
            read_words_hashed(&mut reader, &mut h, n_words).map_err(rerr("regret touched"))?;
        let strategy_touched =
            read_words_hashed(&mut reader, &mut h, n_words).map_err(rerr("strategy touched"))?;
        let regret = if has_regret {
            Some(read_f64s_hashed(&mut reader, &mut h, layout.kept_slots).map_err(rerr("regret"))?)
        } else {
            None
        };
        let strategy =
            read_f64s_hashed(&mut reader, &mut h, layout.kept_slots).map_err(rerr("strategy"))?;
        verify_trailer_and_eof(&mut reader, h, "crop file")?;

        Ok(Self {
            indexer: Arc::new(indexer),
            layout,
            keep_through,
            update_count: rd(OFF_UPDATE_COUNT),
            regret_touched,
            strategy_touched,
            regret,
            strategy,
        })
    }

    /// 保留到哪条街（含）。
    pub fn keep_through(&self) -> StreetTag {
        self.keep_through
    }

    /// 源 checkpoint 的 update 数。
    pub fn update_count(&self) -> u64 {
        self.update_count
    }

    /// 是否保留了 regret 值表。
    pub fn has_regret(&self) -> bool {
        self.regret.is_some()
    }

    /// `info` 所在节点的 `(kept_row, kept_slot_start, action_count)`；被裁掉 → `None`。
    fn locate(&self, info: InfoSetId) -> Option<(u64, usize, usize)> {
        let node_id = (info.raw() >> NLHE_V2_NODE_ID_SHIFT) as usize;
        let (row_base, slot_base) = self.layout.base[node_id]?;
        let m = self.indexer.node_meta(node_id as NodeId);
        let bucket = u64::from(info.bucket_id());
        debug_assert!(
            bucket < u64::from(m.bucket_count),
            "bucket_id {bucket} >= bucket_count {}（node {node_id}）",
            m.bucket_count
        );
        let n = usize::from(m.action_count);
        Some((
            row_base + bucket,
            (slot_base + bucket * u64::from(m.action_count)) as usize,
            n,
        ))
    }

    /// average strategy。`None` = 该信息集所在街已被裁掉（调用方须改走实时搜索等，**不可**
    /// 当作未访问）；`Some(vec![])` = 保留街上未访问；否则与原 trainer 同一
    /// `S(a) / Σ S`（Σ == 0 退化均匀）计算序列，逐位一致。
    pub fn average_strategy(&self, info: InfoSetId) -> Option<Vec<f64>> {
        let (row, start, n) = self.locate(info)?;
        if !(bit_get(&self.strategy_touched, row) || bit_get(&self.regret_touched, row)) {
            return Some(Vec::new());
        }
        let logical = &self.strategy[start..start + n];
        let total: f64 = logical.iter().copied().sum();
        Some(if total > 0.0 {
            logical.iter().map(|s| s / total).collect()
        } else {
            vec![1.0 / n as f64; n]
        })
    }

    /// 逐动作累计 regret（与 dense trainer `regret_for` 同约定：未 touch → 空 Vec）。
    /// `None` = 该街已裁掉，或 crop 时未保留 regret 表。
    pub fn regret_for(&self, info: InfoSetId) -> Option<Vec<f64>> {
        let regret = self.regret.as_ref()?;
        let (row, start, n) = self.locate(info)?;
        Some(if bit_get(&self.regret_touched, row) {
            regret[start..start + n].to_vec()
        } else {
            Vec::new()
        })
    }
}

fn read_f64s_hashed<R: Read>(r: &mut R, h: &mut Hasher, n: u64) -> std::io::Result<Vec<f64>> {
    let mut vals = vec![0f64; n as usize];
    let mut buf = vec![0u8; COPY_BUF_BYTES];
    for chunk in vals.chunks_mut(COPY_BUF_BYTES / 8) {
        let nbytes = chunk.len() * 8;
        read_hashed(r, h, &mut buf[..nbytes])?;
        for (v, b) in chunk.iter_mut().zip(buf[..nbytes].chunks_exact(8)) {
            *v = f64::from_le_bytes(b.try_into().unwrap());
        }
    }
    Ok(vals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstraction::bucket_table::{BucketConfig, BucketTable};
    use crate::core::rng::ChaCha20Rng;
    use crate::rules::config::TableConfig;
    use crate::training::monitor::StrategySnapshot;
    use crate::training::nlhe::pack_info_set_v2;
    use crate::training::nlhe_betting_tree::{all_1pot_6max, first_small_preopen_1pot_6max};
    use crate::training::nlhe_dense_trainer::DenseNlheEsMccfrTrainer;

    /// 小游戏：3 座 / 20BB / all-1pot N=2 / stub 200 桶（树小、能走到河牌）。
    fn small_game() -> SimplifiedNlheGame {
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::new(200, 200, 200).expect("200/200/200"),
        ));
        let (abs, rules) = all_1pot_6max(2);
        SimplifiedNlheGame::new_with_abstraction(table, TableConfig::n_max_at_bb(3, 20), abs, rules)
            .expect("small game")
    }

    /// 训练一个小 dense trainer：LCFR period 小 → 表处于 lazy global_scale != 1 状态，
    /// 覆盖「ckpt 写逻辑值 = raw × scale」与 trainer 查询现算 `raw × scale` 的一致性。
    fn trained(steps: u64) -> DenseNlheEsMccfrTrainer {
        let mut t = DenseNlheEsMccfrTrainer::new(small_game(), 7).with_lcfr_period(500);
        let mut rng = ChaCha20Rng::from_seed(0xC0_0001);
        for _ in 0..steps {
            t.step(&mut rng).expect("step");
        }
        t
    }

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dense_crop_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    fn bits_eq(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
    }

    /// 核心等价：对全树每个 (node, bucket)，保留街上 crop 查询 == 原 trainer 查询（逐位，
    /// average strategy + regret），裁掉的街返回 None。三种 keep_through 都验；并确认
    /// 各街确有被访问的行（否则等价性是空验）。
    #[test]
    fn crop_matches_trainer_bitwise() {
        let trainer = trained(3_000);
        let game = trainer.game();
        let dir = tmpdir("eq");
        let ckpt = dir.join("src.ckpt");
        trainer.save_checkpoint(&ckpt).expect("save");
        let tree = game.tree();
        let buckets = bucket_count_by_street(game);

        let mut touched_by_street = [0u64; 4];
        for id in 0..tree.num_nodes() as NodeId {
            let street = tree.node(id).street;
            for b in 0..buckets[street as usize] {
                let info = pack_info_set_v2(b, id, street);
                touched_by_street[street as usize] +=
                    u64::from(!trainer.average_strategy(info).is_empty());
            }
        }
        assert!(
            touched_by_street.iter().all(|&n| n > 0),
            "四条街都应有被访问的信息集（否则等价性是空验）: {touched_by_street:?}"
        );

        for keep in [StreetTag::Preflop, StreetTag::Flop, StreetTag::Turn] {
            let out = dir.join(format!("crop_{keep:?}.bin"));
            let opts = CropOptions {
                keep_through: keep,
                keep_regret: true,
            };
            let s = crop_dense_checkpoint(&ckpt, &out, game, opts).expect("crop");
            assert_eq!(s.source_update_count, 3_000);
            let bp = CroppedBlueprint::load(&out, game).expect("load crop");
            assert_eq!(bp.keep_through(), keep);
            assert!(bp.has_regret());
            for id in 0..tree.num_nodes() as NodeId {
                let street = tree.node(id).street;
                for b in 0..buckets[street as usize] {
                    let info = pack_info_set_v2(b, id, street);
                    if street as u8 <= keep as u8 {
                        let avg = bp.average_strategy(info).expect("kept street");
                        assert!(
                            bits_eq(&avg, &trainer.average_strategy(info)),
                            "avg 不一致 @ node {id} bucket {b} ({street:?})"
                        );
                        let reg = bp.regret_for(info).expect("kept street + regret kept");
                        assert!(
                            bits_eq(&reg, &StrategySnapshot::regret_for(&trainer, info)),
                            "regret 不一致 @ node {id} bucket {b} ({street:?})"
                        );
                    } else {
                        assert!(bp.average_strategy(info).is_none(), "{street:?} 应已裁掉");
                        assert!(bp.regret_for(info).is_none());
                    }
                }
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `keep_regret = false`：average strategy 仍逐位一致、regret 查询 None；文件尺寸
    /// 精确等于 header + 两 bitset + 单值表 + trailer（含 regret 时多一张值表）。
    #[test]
    fn strategy_only_and_exact_size() {
        let trainer = trained(1_000);
        let game = trainer.game();
        let dir = tmpdir("size");
        let ckpt = dir.join("src.ckpt");
        trainer.save_checkpoint(&ckpt).expect("save");

        let full = dir.join("full.crop");
        let lean = dir.join("lean.crop");
        let keep = |keep_regret| CropOptions {
            keep_through: StreetTag::Flop,
            keep_regret,
        };
        let sf = crop_dense_checkpoint(&ckpt, &full, game, keep(true)).expect("crop full");
        let sl = crop_dense_checkpoint(&ckpt, &lean, game, keep(false)).expect("crop lean");
        let fixed = (CROP_HEADER_LEN + TRAILER_LEN) as u64 + 2 * 8 * sf.kept_rows.div_ceil(64);
        assert_eq!(sf.output_bytes, fixed + 2 * 8 * sf.kept_slots);
        assert_eq!(sl.output_bytes, fixed + 8 * sl.kept_slots);
        assert!(sf.kept_slots < sf.source_slots, "应确实裁掉了转牌 / 河牌");

        let bp = CroppedBlueprint::load(&lean, game).expect("load lean");
        assert!(!bp.has_regret());
        let tree = game.tree();
        let root = tree.root_id();
        for b in 0..169 {
            let info = pack_info_set_v2(b, root, StreetTag::Preflop);
            assert!(bits_eq(
                &bp.average_strategy(info).expect("preflop kept"),
                &trainer.average_strategy(info)
            ));
            assert!(bp.regret_for(info).is_none(), "未保留 regret 表 → None");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 拒绝路径：① 源 ckpt 值区被改 1 byte → crop 失败且**不产出** dst；② crop 文件被改
    /// → load 失败；③ 用不同树（不同 reshape）的 game 去 crop / load → 指纹不符拒绝。
    #[test]
    fn rejects_tampered_and_mismatched() {
        let trainer = trained(500);
        let game = trainer.game();
        let dir = tmpdir("reject");
        let ckpt = dir.join("src.ckpt");
        trainer.save_checkpoint(&ckpt).expect("save");
        let opts = CropOptions {
            keep_through: StreetTag::Flop,
            keep_regret: true,
        };

        // ① 改源 ckpt 末尾附近（值区）一个 byte。
        let mut bytes = std::fs::read(&ckpt).expect("read");
        let pos = bytes.len() - TRAILER_LEN - 9;
        bytes[pos] ^= 0x01;
        let bad = dir.join("bad.ckpt");
        std::fs::write(&bad, &bytes).expect("write bad");
        let out_bad = dir.join("from_bad.crop");
        assert!(crop_dense_checkpoint(&bad, &out_bad, game, opts).is_err());
        assert!(!out_bad.exists(), "源校验失败不应留下输出文件");

        // ② 改 crop 文件一个值 byte。
        let out = dir.join("ok.crop");
        crop_dense_checkpoint(&ckpt, &out, game, opts).expect("crop");
        let mut cb = std::fs::read(&out).expect("read crop");
        let pos = cb.len() - TRAILER_LEN - 9;
        cb[pos] ^= 0x01;
        let bad_crop = dir.join("bad.crop");
        std::fs::write(&bad_crop, &cb).expect("write bad crop");
        assert!(CroppedBlueprint::load(&bad_crop, game).is_err());

        // ③ 不同树：同座位 / 码深 / 桶表，换 reshape。
        let table = Arc::new(BucketTable::stub_for_postflop(
            BucketConfig::new(200, 200, 200).expect("200/200/200"),
        ));
        let (abs, rules) = first_small_preopen_1pot_6max(2);
        let other = SimplifiedNlheGame::new_with_abstraction(
            table,
            TableConfig::n_max_at_bb(3, 20),
            abs,
            rules,
        )
        .expect("other game");
        assert!(crop_dense_checkpoint(&ckpt, &dir.join("x.crop"), &other, opts).is_err());
        assert!(CroppedBlueprint::load(&out, &other).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}

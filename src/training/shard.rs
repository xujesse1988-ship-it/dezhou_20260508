//! 阶段 5 紧凑 RegretTable 分片加载（API-560..API-579 / D-512 字面 path.md §5
//! 紧凑存储 + 分片加载门槛）。
//!
//! ## 分片协议（D-512 字面）
//!
//! - **shard count** = **256**（InfoSetId 高 8 bit 作为 shard key；stage 2
//!   D-218 InfoSetId bit 56..63 已是高位，shard 分布预期均匀）。
//! - **per-shard storage**：
//!   - **first usable 1B path**：全 256 shards 常驻 RAM（实测 RegretTable 总
//!     ~280 MB << c6a 64 GB），分片仅作 layout organization，**不**触发 disk I/O。
//!   - **production 10¹¹ path**（D-441-rev0）：预期 RegretTable 总 ~30-50 GB，
//!     单 host 64 GB 下走 mmap-backed `artifacts/shards/regret_t{traverser:02}
//!     _s{shard_id:03}.bin`（每 shard ~120 MiB）+ **LRU eviction** 限 **128
//!     shards in RAM**（80% RAM 留 traversal working set）。
//! - **eviction policy**：tracked last-access timestamp，evict 最早 unused shard
//!   走 `madvise(MADV_DONTNEED)` 让 OS reclaim。
//! - **hit/miss metrics**：`shard_hit_count` / `shard_miss_count` / `evict_count`
//!   / `mmap_resident_bytes` 进 metrics.jsonl（D-595 unique source）。
//!
//! ## 并发约束（D-512 字面）
//!
//! 单 traversal 内 InfoSet access pattern 由 ES-MCCFR 自然产生跨 shard 跳跃，
//! shard eviction 必须保证 in-flight traversal 的 shard pin（用 `Arc<RwLock>`
//! ref count，eviction 等待 0 reader）。
//!
//! ## A1 \[实现\] 状态
//!
//! 字段集 + 错误类型 + 入口签名 lock；方法体走 `unimplemented!()`。C2 \[实现\]
//! 落地真实 mmap-backed dispatch + LRU eviction + madvise。

#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use thiserror::Error;

/// API-562 — 单 traverser × 单 shard 的紧凑 RegretTable 子段。
///
/// 字段集与 [`crate::training::regret_compact::RegretTableCompact`] 同型 SoA
/// 三 Vec 布局；A1 \[实现\] scaffold 走标准 `Vec<u64>` / `Vec<[i16; 16]>` /
/// `Vec<f32>`；C2 \[实现\] 落地后切到 `memmap2::Mmap` zero-copy 视图（read-only
/// 路径下 mmap-backed，read-write 路径下 anonymous mmap 或 standard alloc）。
///
/// **§D-275-revM 评估**（stage 5 B2 \[实现\] 起步前）：`memmap2::Mmap::map`
/// API 内部 `unsafe`，调用方代码段无 `unsafe` 块，与 stage 1 D-275
/// `[lints.rust] unsafe_code = "forbid"` 约束不冲突（`forbid` 仅拦截调用方
/// `unsafe` 块）。如出现 false-positive 警告走 §X-revN carve-out。
pub struct RegretShard {
    /// 0-based traverser id（D-412 字面 6 traverser × 256 shard）。
    pub traverser: u8,
    /// 0-based shard id（D-512 字面 0..256）。
    pub shard_id: u8,
    /// 当前 shard 内 populated slot 数（key != `u64::MAX`）。
    pub key_count: u64,
    /// SoA keys（A1 走 owned Vec；C2 切 mmap-backed 后字段类型保持 — 实际是
    /// `memmap2::Mmap` 的 `&[u64]` view，B2/C2 \[实现\] 起步前 lock 类型）。
    pub keys: Vec<u64>,
    /// SoA payloads（同上）。
    pub payloads: Vec<[i16; 16]>,
    /// SoA scales（同上）。
    pub scales: Vec<f32>,
}

/// API-565 — shard loader 性能 / 资源统计。
///
/// 字段进 metrics.jsonl（D-595 unique source）。
#[derive(Clone, Copy, Debug, Default)]
pub struct ShardMetrics {
    /// cumulative load_shard 命中 resident 缓存的次数。
    pub hit_count: u64,
    /// cumulative miss + mmap-open 次数。
    pub miss_count: u64,
    /// cumulative LRU evict 次数。
    pub evict_count: u64,
    /// 当前 resident shard 实际驻留物理内存（`mmap_resident` 字段；查 RSS 子段）。
    pub mmap_resident_bytes: u64,
    /// 256 shard 全部 mmap 总文件 byte（含 cold）。
    pub mmap_total_bytes: u64,
}

/// API-560 — `ShardLoader` 内部状态。
///
/// **256 shard × 6 traverser × LRU 128 pin** 字面（D-512）。
///
/// # A1 \[实现\] 状态
///
/// 字段集字面锁；C2 \[实现\] 起步前消费全字段。`allow(dead_code)` 在 A1 stub
/// 阶段抑制 dead-code 警告。
#[allow(dead_code)]
pub struct ShardLoader {
    /// base directory（`artifacts/shards/`）。
    pub base_dir: PathBuf,
    /// shard 总数（默认 256，D-512 字面）。
    pub shard_count: u8,
    /// 同时 resident 上限（默认 128，D-512 字面 = 50% 的 shard count）。
    pub max_resident_shards: usize,
    /// 当前驻留的 shard `(traverser, shard_id) → Arc<RwLock<RegretShard>>`。
    pub(crate) resident: HashMap<(u8, u8), Arc<RwLock<RegretShard>>>,
    /// 每 shard 最后访问 timestamp（access_counter 单调递增）。
    pub(crate) last_access: HashMap<(u8, u8), u64>,
    /// 全局递增 access counter（每次 load_shard 命中 +1）。
    pub(crate) access_counter: AtomicU64,
    /// 资源 / 命中统计。
    pub(crate) metrics: ShardMetrics,
}

impl ShardLoader {
    /// API-560 — 构造空 loader。
    ///
    /// `base_dir` 不必存在；具体 shard 文件查不到时走 [`ShardError::NotFound`]。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。C2 \[实现\] 落地。
    pub fn new(
        base_dir: &Path,
        shard_count: u8,
        max_resident_shards: usize,
    ) -> Result<Self, ShardError> {
        let _ = (base_dir, shard_count, max_resident_shards);
        unimplemented!("stage 5 A1 scaffold — ShardLoader::new 落地于 C2 [实现]")
    }

    /// API-561 — 加载或返回已驻留的 shard。
    ///
    /// 1. 检查 resident → 若 hit 返回 `Arc<RwLock<...>>` + 更新 last_access。
    /// 2. miss 时 → 若 `resident.len() >= max_resident_shards` → [`Self::evict_lru`]。
    /// 3. mmap-open `base_dir/regret_t{traverser:02}_s{shard_id:03}.bin`。
    /// 4. 插入 resident + 更新 metrics。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。C2 \[实现\] 落地。
    pub fn load_shard(
        &mut self,
        traverser: u8,
        shard_id: u8,
    ) -> Result<Arc<RwLock<RegretShard>>, ShardError> {
        let _ = (traverser, shard_id);
        unimplemented!("stage 5 A1 scaffold — ShardLoader::load_shard 落地于 C2 [实现]")
    }

    /// API-562 — LRU evict。
    ///
    /// 找 `last_access` 最早 + `Arc::strong_count == 1`（ref_count == 0 reader）→
    /// `madvise(MADV_DONTNEED)` 让 OS reclaim → 从 resident 移除。
    /// `Arc<RwLock>` Drop 走标准路径，文件不删（mmap-only）。
    ///
    /// 返 `Some((traverser, shard_id))` 表示成功 evict，`None` 表示无可 evict
    /// 候选（全部 in-flight）。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。C2 \[实现\] 落地。
    pub fn evict_lru(&mut self) -> Option<(u8, u8)> {
        unimplemented!("stage 5 A1 scaffold — ShardLoader::evict_lru 落地于 C2 [实现]")
    }

    /// API-570 — pin 一个 shard 在 resident 内（保证在 caller drop `Arc` 之前
    /// 不会被 evict）。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。C2 \[实现\] 落地。
    pub fn pin_shard(
        &self,
        traverser: u8,
        shard_id: u8,
    ) -> Result<Arc<RwLock<RegretShard>>, ShardError> {
        let _ = (traverser, shard_id);
        unimplemented!("stage 5 A1 scaffold — ShardLoader::pin_shard 落地于 C2 [实现]")
    }

    /// API-565 — read-only metrics getter。
    pub fn metrics(&self) -> &ShardMetrics {
        &self.metrics
    }

    /// API-579 — metrics flush 到 JSONL（每 D-595 cadence 一行）。
    ///
    /// # A1 \[实现\] 状态
    ///
    /// `unimplemented!()` 占位。C2 \[实现\] 落地。
    pub fn flush_metrics_to_jsonl(&self, writer: &mut dyn io::Write) -> Result<(), io::Error> {
        let _ = writer;
        unimplemented!("stage 5 A1 scaffold — ShardLoader::flush_metrics_to_jsonl 落地于 C2 [实现]")
    }
}

/// API-564 — `(traverser, shard_id)` → 文件路径（D-512 字面命名约定）。
///
/// 格式：`{base_dir}/regret_t{traverser:02}_s{shard_id:03}.bin`。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位。C2 \[实现\] 落地。
pub fn shard_file_path(base_dir: &Path, traverser: u8, shard_id: u8) -> PathBuf {
    let _ = (base_dir, traverser, shard_id);
    unimplemented!("stage 5 A1 scaffold — shard_file_path 落地于 C2 [实现]")
}

/// API-565 — InfoSetId 高 8 bit 路由到 shard 编号（D-512 字面）。
///
/// # A1 \[实现\] 状态
///
/// `unimplemented!()` 占位。C2 \[实现\] 落地 `(info_set >> 56) as u8`。
pub fn shard_id_from_info_set(info_set: u64) -> u8 {
    let _ = info_set;
    unimplemented!("stage 5 A1 scaffold — shard_id_from_info_set 落地于 C2 [实现]")
}

/// API-565 — shard loader 全套错误。
///
/// 继承 stage 4 `TrainerError` / stage 2 `BucketTableError` 错误追加不删模式
/// （D-374）。
#[derive(Debug, Error)]
pub enum ShardError {
    /// shard 文件不存在 — base_dir / traverser / shard_id 任一不对。
    #[error("shard t={traverser} s={shard_id} not found at {path:?}")]
    NotFound {
        traverser: u8,
        shard_id: u8,
        path: PathBuf,
    },

    /// mmap 调用失败（典型：文件长度 != 期望 / file mode 不对 / OS resource
    /// 限制）。
    #[error("shard t={traverser} s={shard_id} mmap failed: {source}")]
    MmapFailed {
        traverser: u8,
        shard_id: u8,
        #[source]
        source: io::Error,
    },

    /// shard 文件 schema 字段与当前 binary 期望不匹配（继承 stage 4 D-549
    /// `ensure_trainer_schema` preflight 模式）。
    #[error("shard t={traverser} s={shard_id} schema mismatch")]
    SchemaMismatch { traverser: u8, shard_id: u8 },

    /// evict 被阻塞 — 目标 shard 仍有 in-flight reader 持有 `Arc<RwLock>`。
    #[error("evict blocked: shard t={traverser} s={shard_id} pinned by {reader_count} readers")]
    EvictBlocked {
        traverser: u8,
        shard_id: u8,
        reader_count: usize,
    },
}

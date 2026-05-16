# 阶段 5 API 规约（Authoritative spec for testers）

## 文档地位

本文档是阶段 5 [测试] agent 编写测试与 harness 时的**唯一权威 API 签名规约**。[实现] agent 必须按本文档签名落地；任何签名漂移需走 `API-NNN-revM` 修订流程 + 同 PR 更新 `tests/api_signatures.rs` trip-wire（继承 stage 1 §11 + stage 2 §11 + stage 3 §11 + stage 4 §11 模式）。

阶段 5 API 编号从 **API-500** 起，与 stage 1 API-NNN（API-001..API-013）+ stage 2 API-NNN（API-200..API-302）+ stage 3 API-NNN（API-300..API-392）+ stage 4 API-NNN（API-400..API-499）不冲突。stage 1 + 2 + 3 + 4 API 全集作为只读 spec 继承到 stage 5。

---

## Batch 1 范围声明

本 batch 1 commit 落地：

- API-500..API-509：preamble + 模块组织 + trait 扩展原则。
- API-590..API-599：测试 + harness 必需的 read-only inspection API（perf instrumentation / RSS readback / metrics.jsonl schema 扩展）。

API-510..API-589 全集（紧凑 RegretTable / Trainer extension / shard loader / pruning toggle 全套签名）batch 3 详化。

---

## 1. 模块组织与扩展原则（API-500..API-509）

| 编号 | API 签名 | 说明 |
|---|---|---|
| API-500 | 新 module `src/training/regret_compact.rs` | 紧凑 RegretTable 数据结构落地（D-510 + D-511 字面）。**不**替换 `src/training/regret.rs`（stage 3 D-321-rev2 既有 HashMap-backed `RegretTable` 维持作为 fallback + ablation baseline + stage 4 schema=2 checkpoint 加载路径需要）。stage 5 trainer dispatch 走 trait-based selection（`Trainer<G>` 内 RegretTable type 由 trainer variant 决定）。|
| API-501 | 新 module `src/training/quantize.rs` | f32 ↔ q15 quantization helper（D-511 字面）。pub fn `f32_to_q15(x: f32, scale: f32) -> i16` / `q15_to_f32(q: i16, scale: f32) -> f32` + per-row scale 计算策略。|
| API-502 | 新 module `src/training/shard.rs` | 紧凑 RegretTable 分片加载（D-512 字面）。pub struct `ShardLoader` + `load_shard(shard_id: u8) -> Result<RegretShard, ShardError>` + LRU eviction policy。|
| API-503 | 新 module `src/training/pruning.rs` | pruning + ε resurface 状态机（D-520 + D-521 字面）。pub struct `PruningState` + `should_prune(&self, info_set: InfoSet, action: usize) -> bool` + `resurface_pass(&mut self, rng: &mut dyn RngSource)`。|
| API-504 | 既有 module additive 扩展 — `src/training/trainer.rs` | Trainer trait 加 read-only `fn regret_table_compact(&self) -> Option<&RegretTableCompact>`（**default impl 返回 None**，stage 3 + stage 4 既有 trainer impl 默认 None；stage 5 EsMccfrLinearRmPlusCompact override 返 Some）。**约束**：既有 7 必实现方法签名 byte-equal 维持（stage 4 D2 + E2 lock）。|
| API-505 | 既有 module additive 扩展 — `src/training/checkpoint.rs` | `SCHEMA_VERSION` 常量 bump 2 → 3 + `HEADER_LEN` 可能 bump（具体 batch 3 lock）+ `Checkpoint::open` 走 schema_version dispatch 三路径（v1 / v2 / v3）。stage 4 D-549 trainer-aware `ensure_trainer_schema` preflight 扩展 stage 5 `TrainerVariant::EsMccfrLinearRmPlusCompact` expected=3 path。|
| API-506 | 既有 module additive 扩展 — `src/training/metrics.rs` | `TrainingMetrics` 加 `regret_table_section_bytes: u64` 字段 + `MetricsCollector::observe` 加估算逻辑（D-540 内存 SLO 测量路径）。既有 5-variant alarm dispatch 不动。|
| API-507 | 既有 module additive 扩展 — `tools/train_cfr.rs` | CLI 加 5 flag：`--compact-regret-table` boolean（default false，stage 5 enable）/ `--pruning-on` boolean（default false）/ `--pruning-threshold` f32 / `--resurface-period` u64 / `--shard-count` u8。stage 4 16 flag 不动 byte-equal 维持。|
| API-508 | 新 binary `tools/perf_baseline.rs` | c6a host 跑 perf baseline 测量 update/s + RSS + 3-trial min/mean/max 汇总（D-591 + D-592 字面 acceptance protocol 自动化）。CLI flag：`--game nlhe-6max` / `--trainer es-mccfr-linear-rm-plus-compact` / `--updates 1e8` / `--warm-up-update-count 5e7` / `--threads 32` / `--seed-list 42,43,44` / `--output-jsonl perf_baseline.jsonl`。|
| API-509 | trait 扩展原则 | 阶段 5 trait 扩展走**纯 additive** — 既有 trait 7 必实现方法签名 byte-equal 维持（stage 4 D2 + E2 lock）。新 trait method 走 default impl 让 stage 3 + stage 4 既有 impl 不需要改。**禁止**改既有 trait method 签名（D-507 字面 stage 1+2+3+4 baseline 维持的强约束面之一）。|

---

## 2. 紧凑 RegretTable + StrategyAccumulator API（API-510..API-529）

### Batch 2 lock — 紧凑 array 数据结构 + q15 quantization 全套签名

模块布局：`src/training/regret_compact.rs`（D-510 字面 SoA Robin Hood）+ `src/training/quantize.rs`（D-511 字面 q15 helper）+ `src/training/pruning.rs`（D-520 字面 inline pruning + D-521 resurface）。

```rust
// src/training/regret_compact.rs — pub struct + pub impl

pub struct RegretTableCompact<I: InfoSet> {
    keys: Vec<u64>,           // InfoSetId.to_u64(), u64::MAX = empty sentinel
    payloads: Vec<[i16; 16]>, // q15 quantized regret, padded 14→16 for AVX2
    scales: Vec<f32>,         // per-row scale factor
    len: usize,               // populated slot count
    capacity: usize,          // 2^N power-of-two
    _info_set_marker: PhantomData<I>,
}

// API-510
impl<I: InfoSet> RegretTableCompact<I> {
    pub fn with_initial_capacity_estimate(estimated_unique_info_sets: usize) -> Self;
}

// API-511
pub fn regret_at(&self, info_set: I, action: usize) -> f32;
// 内部: probe slot via FxHash → dequant q15 × scale → f32

// API-512
pub fn add_regret(&mut self, info_set: I, action: usize, delta: f32);
// 内部: probe-or-insert slot → quant delta to q15 → saturating_add → check row overflow → maybe row-renorm

// API-513
pub fn clamp_rm_plus(&mut self);
// 内部: in-place SIMD max(q15_lane, 0) over all slots, AVX2 path + scalar fallback

// API-514
pub fn scale_linear_lazy(&mut self, decay: f32);
// 内部: 仅 mutate scales[i] *= decay for all populated slots (D-511 lazy 路径, scale-only)

// API-515
pub fn len(&self) -> usize;
pub fn is_empty(&self) -> bool;

// API-516
pub fn section_bytes(&self) -> u64;
// 内部: (keys.len() × 8) + (payloads.len() × 32) + (scales.len() × 4) + metadata overhead
// 给 D-540 内存 SLO 测量路径 + metrics.jsonl regret_table_section_bytes 字段

// API-517
pub fn iter(&self) -> RegretTableCompactIter<'_, I>;
// 内部: 迭代非空 slot (keys[i] != u64::MAX) → 返回 (info_set, &[i16; 16], scale)

// API-518
pub fn renormalize_scales(&mut self);
// D-511 字面: 每 1e6 iter 触发 → 全表 scan → max(|q15|) 区间判断 → scale × 2 + q15 >> 1 或 scale / 2 + q15 << 1

// API-519
pub fn collision_metrics(&self) -> CollisionMetrics;
// 内部: 返 (max_probe_distance, avg_probe_distance, load_factor) - 给 B1 [测试] 用
pub struct CollisionMetrics {
    pub max_probe_distance: usize,
    pub avg_probe_distance: f32,
    pub load_factor: f32,
}
```

```rust
// src/training/quantize.rs — pub fn helper

// API-520
pub fn f32_to_q15(value: f32, scale: f32) -> i16;
// (value / scale × 32768).round().clamp(-32768, 32767) as i16
// scale == 0.0 时返回 0 (defensive, 应当不发生因为 row 至少有 1 个非零值时 scale > 0)

// API-521
pub fn q15_to_f32(q: i16, scale: f32) -> f32;
// (q as f32) × (scale / 32768.0)

// API-522
pub fn compute_row_scale(row_values: &[f32; 14]) -> f32;
// max(|row_values|.iter()) 或 0.0 如全 0
// 给 add_regret 路径下 row scale 初始化 / 重算

// API-523
pub fn quantize_row(row_values: &[f32; 14], scale: f32, out: &mut [i16; 16]);
// 14 个值走 f32_to_q15, padding[14..16] = i16::MIN

// API-524
pub fn dequantize_row(payload: &[i16; 16], scale: f32, out: &mut [f32; 14]);
// 14 个值走 q15_to_f32, 忽略 padding[14..16]

// API-525
pub fn dequantize_action(payload: &[i16; 16], scale: f32, action: usize) -> f32;
// 单 action q15_to_f32, 给 should_prune inline check 用
```

```rust
// StrategyAccumulator 同型 API（API-526..API-529）

// API-526
pub struct StrategyAccumulatorCompact<I: InfoSet> {
    // SoA: keys / payloads / scales, 同 RegretTableCompact 布局
}

impl<I: InfoSet> StrategyAccumulatorCompact<I> {
    pub fn with_initial_capacity_estimate(estimated_unique_info_sets: usize) -> Self;
    pub fn add_strategy_sum(&mut self, info_set: I, action: usize, delta: f32);
    pub fn average_strategy(&self, info_set: I, out: &mut [f32; 14]);
    pub fn scale_linear_lazy(&mut self, decay: f32);
    pub fn section_bytes(&self) -> u64;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self) -> StrategyAccumulatorCompactIter<'_, I>;
    pub fn renormalize_scales(&mut self);
}

// API-527 — pruning state 派生 query
// （pruning state 不单独存储，由 RegretTableCompact.regret_at 派生）

// API-528 — RegretTableCompact + StrategyAccumulatorCompact 共享 InfoSet derive
// 两者 keys 数组**不共享**（D-517 字面）；各 alloc 独立 capacity

// API-529 — 紧凑 array Drop + Clone 语义
// #[derive(Clone)] 走 Vec::clone × 3，capacity 保留 (D-519 字面)
// Drop 走标准 Vec::drop，无 unsafe
```

### Pruning 模块 API（API-530..API-539）

> Note: 编号 API-530..API-539 batch 2 落地（与 batch 3 详化的 Trainer extension API-540+ 不冲突重排）。

```rust
// src/training/pruning.rs

// API-530
pub struct PruningConfig {
    pub threshold: f32,           // default -300_000_000.0 (D-520 字面)
    pub resurface_period: u64,    // default 10_000_000 (D-521 字面)
    pub resurface_epsilon: f32,   // default 0.05 (D-521 字面)
    pub resurface_reset_value: f32, // default -150_000_000.0 (D-521 字面 = threshold × 0.5)
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            threshold: -300_000_000.0,
            resurface_period: 10_000_000,
            resurface_epsilon: 0.05,
            resurface_reset_value: -150_000_000.0,
        }
    }
}

// API-531
pub fn should_prune(table: &RegretTableCompact<I>, info_set: I, action: usize, cfg: &PruningConfig) -> bool;
// 内部: regret_at(info_set, action) < cfg.threshold

// API-532
pub fn resurface_pass<I: InfoSet>(
    table: &mut RegretTableCompact<I>,
    cfg: &PruningConfig,
    rng: &mut dyn RngSource,
    resurface_pass_id: u64,
) -> ResurfaceMetrics;
// 内部: 全表 scan → 每 pruned action (q15 < quantized_threshold) → rng.next_uniform_f32() < ε → q15 ← quantize(reset_value, scale)
// rng seed 走 master_seed.wrapping_add(0xDEAD_BEEF_CAFE_BABE * resurface_pass_id) (D-528 字面)

pub struct ResurfaceMetrics {
    pub scanned_action_count: u64,
    pub pruned_action_count: u64,
    pub reactivated_action_count: u64,
    pub wall_time: Duration,
}

// API-533..API-539 预留
// 候选: metrics.jsonl 写入 helper / pruning toggle status / resurface pass scheduler
```

---

## 3. Trainer extension + Checkpoint v3 API（API-540..API-559）

### Batch 3 lock

```rust
// src/training/trainer.rs — TrainerVariant 扩展

// API-540
pub enum TrainerVariant {
    VanillaCfr = 1,                   // stage 3 D-302
    EsMccfr = 2,                      // stage 3 D-301
    EsMccfrLinearRmPlus = 3,          // stage 4 D-400
    EsMccfrLinearRmPlusCompact = 4,   // stage 5 D-500 主线
}

impl TrainerVariant {
    pub const fn expected_schema_version(self) -> u8 {
        match self {
            Self::VanillaCfr | Self::EsMccfr => 1,
            Self::EsMccfrLinearRmPlus => 2,
            Self::EsMccfrLinearRmPlusCompact => 3,
        }
    }
}

// API-541
pub struct EsMccfrLinearRmPlusCompactTrainer<G: Game> {
    game: G,
    regret_tables: [RegretTableCompact<G::InfoSet>; 6],
    strategy_accums: [StrategyAccumulatorCompact<G::InfoSet>; 6],
    pruning_config: PruningConfig,
    shard_loader: Option<ShardLoader>,           // None = first usable single-host
    update_count: u64,
    warmup_complete: bool,
    resurface_pass_id: u64,
    metrics: TrainingMetrics,
    config: TrainerConfig,
}

impl<G: Game> EsMccfrLinearRmPlusCompactTrainer<G> {
    pub fn new(game: G, config: TrainerConfig) -> Self;
    pub fn with_initial_capacity_estimate(self, estimate: usize) -> Self;
    pub fn with_pruning_config(self, cfg: PruningConfig) -> Self;
    pub fn with_shard_loader(self, loader: ShardLoader) -> Self;
}

// API-542
impl<G: Game> Trainer<G> for EsMccfrLinearRmPlusCompactTrainer<G> {
    // 7 既有必实现方法签名 byte-equal 维持（stage 4 D2 + E2 lock）
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError>;
    fn step_parallel(&mut self, rng_pool: &mut [dyn RngSource], batch_size: usize) -> Result<(), TrainerError>;
    fn current_strategy_for_traverser(&self, traverser: usize, info_set: G::InfoSet, out: &mut [f32; 14]);
    fn average_strategy_for_traverser(&self, traverser: usize, info_set: G::InfoSet, out: &mut [f32; 14]);
    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError>;
    fn load_checkpoint(&mut self, path: &Path) -> Result<(), CheckpointError>;
    fn game_ref(&self) -> &G;
}

// API-543
impl<G: Game> EsMccfrLinearRmPlusCompactTrainer<G> {
    pub fn regret_table_compact(&self, traverser: usize) -> &RegretTableCompact<G::InfoSet>;
    pub fn strategy_accum_compact(&self, traverser: usize) -> &StrategyAccumulatorCompact<G::InfoSet>;
    pub fn pruning_config(&self) -> &PruningConfig;
    pub fn collision_metrics(&self, traverser: usize) -> CollisionMetrics;
    pub fn update_count(&self) -> u64;
    pub fn warmup_complete(&self) -> bool;
    pub fn resurface_pass_id(&self) -> u64;
    pub fn shard_stats(&self) -> Option<ShardStats>;
}

// API-544 — Trainer trait additive method (default None)
impl<G: Game, T: Trainer<G>> T {
    fn regret_table_compact_opt(&self) -> Option<&RegretTableCompact<G::InfoSet>> { None }
    fn strategy_accum_compact_opt(&self) -> Option<&StrategyAccumulatorCompact<G::InfoSet>> { None }
}
// EsMccfrLinearRmPlusCompactTrainer override 返 Some

// API-545..API-549 stage 5 trainer 独占辅助（保留扩展）
```

```rust
// src/training/checkpoint.rs — schema_version 2 → 3

// API-550
pub const SCHEMA_VERSION: u8 = 3;
pub const HEADER_LEN: usize = 192;
pub const STAGE3_HEADER_LEN: usize = 108;
pub const STAGE4_HEADER_LEN: usize = 128;

// API-551
#[repr(C)]
pub struct CheckpointHeaderV3 {
    pub magic: [u8; 8],                          // "PLBRSCKT" 字面
    pub schema_version: u8,                      // = 3
    pub trainer_variant: u8,                     // = TrainerVariant::EsMccfrLinearRmPlusCompact as u8
    pub info_set_id_layout_version: u8,          // = 1 (stage 2 D-218 维持)
    pub traverser_count: u8,                     // = 6 (stage 4 D-412)
    pub quant_bits: u8,                          // = 15 (q15 字面)
    pub padding_a: [u8; 3],                      // align
    pub capacity_estimate: u64,                  // per-table capacity (D-518)
    pub update_count: u64,                       // cumulative since first run
    pub warmup_complete: u8,                     // bool
    pub padding_b: [u8; 7],                      // align
    pub pruning_config_threshold: f32,           // D-520 字面
    pub pruning_config_resurface_period: u64,    // D-521
    pub pruning_config_resurface_epsilon: f32,
    pub pruning_config_resurface_reset: f32,
    pub resurface_pass_id: u64,                  // D-528
    pub naive_baseline_blake3: [u8; 32],         // D-548 baseline 锁定 + 跨 binary 拒绝
    pub body_blake3: [u8; 32],                   // D-563 self-consistency
    pub padding_c: [u8; remaining_to_192],       // align to HEADER_LEN
}

// API-552
impl Checkpoint {
    pub fn open(path: &Path) -> Result<Self, CheckpointError>;
    // 内部: 读 first 8 byte magic + 1 byte schema_version → dispatch 三路径
    //   schema = 1 → stage 3 path (108 byte header, EsMccfr / VanillaCfr body)
    //   schema = 2 → stage 4 path (128 byte header, EsMccfrLinearRmPlus body)
    //   schema = 3 → stage 5 path (192 byte header, EsMccfrLinearRmPlusCompact body)
    //   other → CheckpointError::SchemaVersionMismatch

    pub fn save_schema_v3(
        path: &Path,
        header: &CheckpointHeaderV3,
        regret_tables: &[RegretTableCompact<I>; 6],
        strategy_accums: &[StrategyAccumulatorCompact<I>; 6],
    ) -> Result<(), CheckpointError>;
    // 内部: 写 192 byte header + body 12 sub-region (6 traverser × 2 table)
    //   每 sub-region encoding = bincode + zstd level=3 compress
    //   sub-region 间用 4-byte magic 0xDEADBEEF 分隔
    //   body_blake3 在 header 落地前算完 + write
}

// API-553
pub fn ensure_trainer_schema(
    expected_variant: TrainerVariant,
    actual_schema: u8,
) -> Result<(), CheckpointError>;
// 内部: expected_variant.expected_schema_version() == actual_schema 否则 Err

// API-554..API-559 v3 body region encoding helper
pub(crate) fn encode_compact_region<I: InfoSet>(
    table: &RegretTableCompact<I>,
    writer: &mut impl Write,
) -> Result<(), CheckpointError>;
pub(crate) fn decode_compact_region<I: InfoSet>(
    reader: &mut impl Read,
) -> Result<RegretTableCompact<I>, CheckpointError>;
// 同型 strategy_accum encode/decode
```

---

## 4. Shard loader API（API-560..API-579）

### Batch 3 lock

```rust
// src/training/shard.rs

// API-560
pub struct ShardLoader {
    base_dir: PathBuf,
    shard_count: u8,                                  // = 256 (D-512 字面 高 8 bit InfoSetId)
    max_resident_shards: usize,                       // = 128 (D-512 字面 LRU pin 上限)
    resident: HashMap<(u8, u8), Arc<RwLock<RegretShard>>>,  // (traverser, shard_id) → mmap
    last_access: HashMap<(u8, u8), u64>,              // last access timestamp
    access_counter: AtomicU64,
    metrics: ShardMetrics,
}

impl ShardLoader {
    pub fn new(base_dir: &Path, shard_count: u8, max_resident_shards: usize) -> Result<Self, ShardError>;
}

// API-561
impl ShardLoader {
    pub fn load_shard(&mut self, traverser: u8, shard_id: u8) -> Result<Arc<RwLock<RegretShard>>, ShardError>;
    // 内部: 1. 检查 resident → 若 hit 返回 + 更新 last_access
    //       2. miss 时 → 若 resident.len() >= max_resident_shards → evict_lru
    //       3. mmap-open file `base_dir/regret_t{traverser:02}_s{shard_id:03}.bin`
    //       4. 插入 resident + 更新 metrics
}

// API-562
impl ShardLoader {
    pub fn evict_lru(&mut self) -> Option<(u8, u8)>;
    // 内部: 找 last_access 最早 + ref_count == 0 (Arc::strong_count == 1)
    //       madvise(MADV_DONTNEED) → 从 resident 移除
    //       Arc<RwLock> Drop 走标准路径, 文件不删（mmap-only）
}

// API-563
#[repr(C)]
pub struct RegretShard {
    pub traverser: u8,
    pub shard_id: u8,
    pub key_count: u64,
    pub keys: Mmap<u64>,         // memmap2 read-only mmap
    pub payloads: Mmap<[i16; 16]>,
    pub scales: Mmap<f32>,
}

// API-564
pub fn shard_file_path(base_dir: &Path, traverser: u8, shard_id: u8) -> PathBuf;
// 内部: base_dir.join(format!("regret_t{:02}_s{:03}.bin", traverser, shard_id))

// API-565..API-569
pub struct ShardMetrics {
    pub hit_count: u64,
    pub miss_count: u64,
    pub evict_count: u64,
    pub mmap_resident_bytes: u64,
    pub mmap_total_bytes: u64,
}

pub fn shard_id_from_info_set(info_set: u64) -> u8;
// 内部: (info_set >> 56) as u8 (D-512 字面高 8 bit)

#[derive(Debug, thiserror::Error)]
pub enum ShardError {
    #[error("shard {0} traverser {1} not found at {2:?}")]
    NotFound(u8, u8, PathBuf),
    #[error("shard {0} traverser {1} mmap failed: {2}")]
    MmapFailed(u8, u8, io::Error),
    #[error("shard {0} traverser {1} schema mismatch")]
    SchemaMismatch(u8, u8),
    #[error("evict blocked: shard {0} traverser {1} pinned by {2} readers")]
    EvictBlocked(u8, u8, usize),
}

// API-570..API-579 shard pin + Arc<RwLock> ref count 路径
impl ShardLoader {
    pub fn pin_shard(&self, traverser: u8, shard_id: u8) -> Result<Arc<RwLock<RegretShard>>, ShardError>;
    pub fn metrics(&self) -> &ShardMetrics;
    pub fn flush_metrics_to_jsonl(&self, writer: &mut impl Write) -> Result<(), io::Error>;
}
```

---

## 5. Pruning + resurface API（已在 §2 API-530..API-539 落地）

batch 2 已落地（D-520..D-529 字面）。具体签名见 §2 末尾 — `PruningConfig` / `should_prune` / `resurface_pass` / `ResurfaceMetrics`。**编号 API-530..API-539 batch 2 lock 后**与 §3 Trainer + Checkpoint API-540+ 不冲突重排。

---

## 6. 性能 instrumentation + 测试 harness API（API-580..API-599）

### Batch 3 lock — perf_baseline binary 全套

```rust
// tools/perf_baseline.rs — D-535 acceptance run host scheduling 主驱动

// API-580
pub fn main() -> Result<(), Box<dyn Error>>;
// CLI flag (16 total):
//   --game nlhe-6max                     (D-410)
//   --trainer es-mccfr-linear-rm-plus-compact  (API-540)
//   --abstraction pluribus-14            (D-420)
//   --bucket-table <path>                 v3 production 528 MiB
//   --naive-baseline <path>               tests/data/stage5_naive_baseline.json (D-547)
//   --seed-list 42,43,44                  3 seed (D-532 字面)
//   --run-wall-seconds 1800               30 min (D-538)
//   --warm-up-wall-seconds 300            5 min (D-538)
//   --updates-per-trial 0                 0 = wall-bound, else update-bound
//   --threads 32                          c6a 32 vCPU (D-506)
//   --parallel-batch-size 32              stage 4 §E-rev2 baseline (扩展到 64/128 由 D-515 阶段 1)
//   --pruning-on                          bool (D-525 default false 但 perf_baseline default true)
//   --output-jsonl perf_baseline.jsonl
//   --acceptance-target-update-per-s 200000  (D-530 字面)
//   --acceptance-target-memory-ratio 0.5     (D-540 字面)
//   --skip-host-preflight                 emergency override (default false)

// API-581
pub struct PerfBaselinePreflight {
    pub load_average_5m: f64,
    pub cpu_governor: String,
    pub cpu_freq_mhz_min: u64,
    pub cpu_freq_mhz_max: u64,
    pub freq_consistency_ok: bool,         // (max - min) / max < 0.05
}

pub fn preflight_check() -> Result<PerfBaselinePreflight, PreflightError>;
// 内部: D-537 字面 — uptime / cpupower frequency-info / /proc/cpuinfo MHz 一致性
// 任一 fail abort + 提示 user 修复

#[derive(Debug, thiserror::Error)]
pub enum PreflightError {
    #[error("load average {0} > 0.5, host not idle")]
    HostNotIdle(f64),
    #[error("cpu governor {0:?} != 'performance', run: cpupower frequency-set -g performance")]
    WrongGovernor(String),
    #[error("cpu freq inconsistent (min {0} MHz, max {1} MHz), turbo throttling suspected")]
    FreqInconsistent(u64, u64),
}

// API-582
pub struct TrialResult {
    pub seed: u64,
    pub trial_idx: u8,           // 0 = first attempt, 1 = retry (D-536)
    pub update_per_s_mean: f64,
    pub update_per_s_min_window: f64,
    pub update_per_s_max_window: f64,
    pub steady_state_seconds: f64,
    pub rss_bytes_peak: u64,
    pub regret_table_section_bytes_peak: u64,
    pub naive_baseline_ratio: f64,    // section_bytes / naive_baseline (D-548)
    pub passed: bool,
}

pub fn run_trial(
    seed: u64,
    cfg: &PerfBaselineConfig,
) -> Result<TrialResult, TrialError>;
// 内部: 启 EsMccfrLinearRmPlusCompactTrainer + 跑 wall-bound 30 min
//        收集 metrics.jsonl + 计算 steady-state slice (D-538)
//        D-536 retry: 若 result.update_per_s_mean < target → 重测 1 次

// API-583
pub struct AcceptanceSummary {
    pub trials: Vec<TrialResult>,        // 3+ trials with retries
    pub min_update_per_s: f64,
    pub mean_update_per_s: f64,
    pub max_update_per_s: f64,
    pub min_memory_ratio: f64,
    pub mean_memory_ratio: f64,
    pub max_memory_ratio: f64,
    pub slo_throughput_pass: bool,       // min ≥ 200K (D-539)
    pub slo_memory_pass: bool,           // mean ≤ 0.5 (D-542)
    pub naive_baseline_blake3: String,   // D-548 锁定来源
    pub git_sha: String,
    pub host_info: HostInfo,
}

pub fn aggregate_trials(trials: Vec<TrialResult>) -> AcceptanceSummary;
// D-539 字面: SLO PASS 判据 min(3 trials) ≥ target

// API-584
pub fn write_acceptance_jsonl(
    summary: &AcceptanceSummary,
    writer: &mut impl Write,
) -> Result<(), io::Error>;
// JSONL schema: 1 line per trial + 1 final line summary
// 各字段全 commit message + report 引用唯一来源 (D-595)

// API-585
pub struct HostInfo {
    pub instance_type: String,       // "c6a.8xlarge"
    pub vcpu_count: u32,             // 32
    pub cpu_model: String,           // "AMD EPYC 7R13"
    pub ram_gb: u32,
    pub kernel: String,
    pub rustc_version: String,
}

pub fn detect_host_info() -> HostInfo;
// 内部: /proc/cpuinfo + uname + rustc --version + AWS IMDS (instance-type)

// API-586..API-589 预留 — c6a host scheduling automation hooks
//   候选: --host-boot-aws-c6a-8xlarge (自动 boot via aws cli)
//        / --host-shutdown-on-completion
//        / --gh-release-upload-jsonl
//        / --slack-notify-webhook
```

### Stage 5 既有 trainer + metrics 扩展 API（API-590..API-599）

| 编号 | API 签名 | 说明 |
|---|---|---|
| API-590 | `TrainingMetrics::regret_table_section_bytes() -> u64` | read-only getter，D-540 + D-544 公式 |
| API-591 | `MetricsCollector::sample_throughput_window(&self, window: Duration) -> f64` | mid-run steady-state throughput 计算（D-538 / D-591 测试协议路径）|
| API-592 | `MetricsCollector::record_warm_up_complete(&mut self, update_count: u64)` | warm-up 5 min skip 边界标记（D-538 字面）|
| API-593 | metrics.jsonl schema 扩展 — 新字段 | `regret_table_section_bytes: u64` / `strategy_accum_section_bytes: u64` / `pruning_state_section_bytes: u64`（D-524 字面 = 0 不单独存储）/ `shard_hit_count: u64` / `shard_miss_count: u64` / `evict_count: u64` / `mmap_resident_bytes: u64` / `mmap_total_bytes: u64`（D-546 production 路径）/ `elapsed_wall_s: f64`（D-534）/ `update_per_s_window: f64`（D-534）/ `pruned_action_count: u64` / `pruned_action_ratio: f32` / `resurface_event_count: u64` / `resurface_reactivated_count: u64`（D-526 字面）。stage 4 既有字段 + 5-variant alarm dispatch byte-equal 维持。|
| API-594 | `tests/perf_slo.rs` 扩 stage 5 — 新增 `#[test] #[ignore]` 函数 | `stage5_compact_regret_table_throughput_c6a_32vcpu_geq_200k` / `stage5_compact_regret_table_memory_geq_50_percent_reduction` / `stage5_compact_regret_table_collision_metrics_within_bounds`（D-569）。所有 stage 5 SLO 测试**默认 #[ignore]**。|
| API-595 | `tests/api_signatures.rs` 扩 stage 5 | API-500..API-599 全套 trip-wire（A1 stub 返 `!`）|
| API-596 | `tests/checkpoint_v3_round_trip.rs` 新 integration crate | D-563 anchor 实施路径：写 → 读 → 重写 → BLAKE3 byte-equal；schema dispatch 三路径 (v1/v2/v3) 全覆盖；跨 binary 拒绝 mismatch 走 D-549 `ensure_trainer_schema` preflight |
| API-597 | `tests/stage5_anchors.rs` 新 integration crate | D-560..D-563 anchor 4 项实测覆盖：LBR 6-traverser ≤ 59,000 / baseline 3 类 mean 阈值 / Slumbot 95% CI overlap / round-trip BLAKE3 |
| API-598 | `tests/regret_table_compact_collision.rs` 新 integration crate | D-569 collision metrics anchor：1M warm-up + 10M steady-state 两次 snapshot；load_factor / max_probe_distance / avg_probe_distance 三阈值 |
| API-599 | 预留 | batch 4 详化（候选：c6a host scheduling automation hook test / NUMA topology assertion / cpu freq lock check test）|

---

## 已知未决项

| 编号 | 项 | 触发条件 / 决策时点 |
|---|---|---|
| API-505 HEADER_LEN bump | batch 3 lock = 192 byte（API-550 字面 24 byte 新增 + 40 byte 对齐 pad）| 本 commit batch 3 关闭 |
| API-540..API-589 全集 | batch 3 lock | 本 commit batch 3 关闭 |
| API-586..API-589 host scheduling automation | 取决于 stage 5 主线 c6a host on-demand 启停是否需要 binary 内自动化（vs 手动 ssh）| stage 5 F2 [实现] production 训练触发前评估 |
| API-599 | host preflight / NUMA topology assertion | batch 4 详化（如有则补，否则关闭）|
| API-508 `tools/perf_baseline.rs` 是否合并到 `tools/train_cfr.rs` | batch 3 lock = **不合并**（perf_baseline 是 acceptance run 专用 binary，独立 16 flag + preflight check + 3-trial aggregation，与 train_cfr 主训练循环职责分离）| 本 commit batch 3 关闭 |

---

## 修订历史

- **batch 1**（commit c2fa4f4）= API-500..API-509 + API-590..API-595 + 占位 API 编号落地。
- **batch 2**（commit 63154ec）= API-510..API-529 紧凑 RegretTable + q15 quantization + StrategyAccumulator 全套签名 + API-530..API-539 Pruning + resurface 签名 + §3-5 段范围 renumber。
- **batch 3**（本 commit）= API-540..API-559 Trainer extension + Checkpoint v3 schema（HEADER_LEN 128 → 192 + 8 new header fields + 6×2 sub-region encoding + body BLAKE3 self-consistency）+ API-560..API-579 Shard loader（256 shard mmap + LRU 128 pin + Arc<RwLock> ref count + madvise）+ API-580..API-589 perf_baseline binary（16 CLI flag + preflight check + 3-trial aggregate + AcceptanceSummary）+ API-590..API-599 既有 trainer metrics 扩展 + 3 新 integration test crate（checkpoint_v3_round_trip / stage5_anchors / regret_table_compact_collision）。

后续 API-NNN-revM 修订按 stage 1 §11 + stage 2 §11 + stage 3 §11 + stage 4 §11 同型 flow append。

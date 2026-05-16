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

**deferred to batch 3 详化**。skeleton：

- API-510 `RegretTableCompact::new(initial_capacity: usize) -> Self`
- API-511 `RegretTableCompact::regret_at(&self, info_set: InfoSet, action: usize) -> f32`
- API-512 `RegretTableCompact::add_regret(&mut self, info_set: InfoSet, action: usize, delta: f32)`
- API-513 `RegretTableCompact::clamp_rm_plus(&mut self)` — RM+ 路径
- API-514 `RegretTableCompact::scale_linear(&mut self, decay: f32)` — Linear discounting eager 路径（保留 stage 4 D-401-revM lazy 路径在 batch 2 评估）
- API-515 `RegretTableCompact::len() -> usize` / `is_empty() -> bool`
- API-516..API-519 q15 quantization helper API（继承 API-501）
- API-520..API-525 StrategyAccumulator 同型 API
- API-526..API-529 紧凑 RegretTable iteration API（dump / load / metrics）

---

## 3. Trainer extension + Checkpoint v3 API（API-530..API-549）

**deferred to batch 3 详化**。skeleton：

- API-530 `TrainerVariant::EsMccfrLinearRmPlusCompact` enum variant
- API-531 `EsMccfrTrainer::with_compact_regret_table(table: RegretTableCompact) -> Self` builder
- API-532 `Trainer::regret_table_compact(&self) -> Option<&RegretTableCompact>` default None override
- API-533..API-539 stage 5 trainer 独占方法（pruning state read / shard hot/cold stats / quantization scale dump）
- API-540 `Checkpoint::SCHEMA_VERSION = 3` 常量
- API-541 `Checkpoint::open` v3 dispatch path
- API-542 `EsMccfrTrainer::save_checkpoint` schema=3 path
- API-543..API-549 v3 header field + body sub-region encoding

---

## 4. Shard loader API（API-550..API-559）

**deferred to batch 3 详化**。skeleton：

- API-550 `ShardLoader::new(base_dir: &Path, shard_count: u8) -> Self`
- API-551 `ShardLoader::load_shard(&mut self, shard_id: u8) -> Result<&RegretShard, ShardError>`
- API-552 `ShardLoader::evict_lru(&mut self) -> Option<u8>`
- API-553..API-559 shard 持久化 file layout / hit/miss metrics

---

## 5. Pruning + resurface API（API-560..API-579）

**deferred to batch 3 详化**。skeleton：

- API-560 `PruningState::new(initial_capacity: usize) -> Self`
- API-561 `PruningState::should_prune(&self, info_set: InfoSet, action: usize) -> bool`
- API-562 `PruningState::mark_pruned(&mut self, info_set: InfoSet, action: usize)`
- API-563 `PruningState::resurface_pass(&mut self, rng: &mut dyn RngSource, threshold: f32, epsilon: f32)`
- API-564..API-569 pruning + warm-up boundary state / serialize/deserialize / metrics 接入
- API-570..API-579 ε resurface 周期 / 比例 / 全表扫描 schedule

---

## 6. 性能 instrumentation + 测试 harness API（API-580..API-599）

| 编号 | API 签名 | 说明 |
|---|---|---|
| API-580..API-589 | **deferred to batch 3 详化** | perf_baseline binary 内部 helper + 3-trial 汇总 + JSONL schema |
| API-590 | `TrainingMetrics::regret_table_section_bytes() -> u64` | read-only getter，D-540 内存 SLO 测量路径 |
| API-591 | `MetricsCollector::sample_throughput_window(&self, window: Duration) -> f64` | mid-run steady-state throughput 计算，D-591 测试协议路径 |
| API-592 | `MetricsCollector::record_warm_up_complete(&mut self, update_count: u64)` | warm-up 5 min skip 边界标记，D-592 字面 |
| API-593 | metrics.jsonl schema 扩展 — 新字段 | `regret_table_section_bytes: u64` / `strategy_accum_section_bytes: u64` / `pruning_state_section_bytes: u64` / `shard_hit_count: u64` / `shard_miss_count: u64`。继承 stage 4 既有 3 条曲线 proxy + 5-variant alarm dispatch byte-equal 维持。|
| API-594 | `tests/perf_slo.rs` 扩 stage 5 — 新增 `#[test] #[ignore]` 函数 | `stage5_compact_regret_table_thread_throughput_c6a_32vcpu_geq_200k` 等 SLO 断言。所有 stage 5 SLO 测试**默认 #[ignore]**，opt-in via `cargo test --release --test perf_slo -- --ignored`。|
| API-595 | `tests/api_signatures.rs` 扩 stage 5 | API-500..API-599 全套 trip-wire（A1 stub 返 `!`，错签名 silently compile fail）|
| API-596..API-599 | 预留 | batch 3 详化（候选：c6a host idle detection / cpu frequency lock check / NUMA topology asserttion 等）|

---

## 已知未决项

| 编号 | 项 | 触发条件 / 决策时点 |
|---|---|---|
| API-510..API-589 全集 | 紧凑 RegretTable / Trainer extension / shard loader / pruning toggle / pruning state 全套签名 | batch 3 详化时 lock |
| API-505 HEADER_LEN bump 数字 | 取决于 v3 header field 新增数 | batch 3 详化时 lock |
| API-507 CLI flag default 值 | 取决于 stage 5 主线 default-on / default-off 策略 | batch 3 详化时 lock |
| API-508 `tools/perf_baseline.rs` 是否合并到 `tools/train_cfr.rs` | 取决于 A1 [实现] scaffold 起步前评估 | A1 [实现] 起步前 batch 4 决定 |

---

## 修订历史

stage 5 A0 [决策] 起步 commit（本 commit）= API-500..API-509 + API-590..API-595 + 占位 API 编号 batch 1 落地。后续 API-NNN-revM 修订按 stage 1 §11 + stage 2 §11 + stage 3 §11 + stage 4 §11 同型 flow append。

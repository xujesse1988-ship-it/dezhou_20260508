# NLHE dense infoset table 方案草案（2026-05-26）

## 背景

当前 `RegretTable<I>` / `StrategyAccumulator<I>` 是通用 `HashMap<I, Vec<f64>>`
容器，按首次访问 lazy 初始化。这个设计对 Kuhn / Leduc / 泛型 `Game` 很稳，但在
默认 heads-up NLHE profile 下已经成为长期训练的主要内存和吞吐压力之一：

- `InfoSetId` 持续插入导致两个大 HashMap 增长。
- HashMap 扩容和 key/value 分散布局带来额外 RSS、cache miss 和 checkpoint 克隆成本。
- `step_parallel` merge 阶段仍要对每条 local delta 做 HashMap lookup。
- 长 run 记录里，100M infoset 后 RSS 稳态约 30 GB，final checkpoint 序列化峰值约
  46.8 GB。

当前 NLHE `InfoSetId` v2 已经具备 dense 化条件：

- `SimplifiedNlheGame::new` 启动时完整构建 public betting tree。
- 每个决策节点有稳定 `node_id`，由公开下注路径唯一确定。
- `InfoSetId` 高 26 bit 写入 `node_id`，低位保留 `bucket_id` / `street_tag`。
- 每个 `node_id` 的合法动作数由 betting tree 节点决定，训练全程稳定。

因此，NLHE 专用路径可以把 `InfoSetId` 映射为数组下标，避免在热路径上使用
HashMap。

## 目标

1. 降低 NLHE 长 run 的常驻内存和 checkpoint 峰值。
2. 降低 `current_strategy` / `accumulate` / merge 阶段的 HashMap lookup 成本。
3. 保留默认确定性训练语义：同 seed / 同版本下结果可复现。
4. 保留通用训练 API 对 Kuhn / Leduc / 未来 6-max 的兼容，不把核心 trait 改成
   heads-up only。
5. 以可回退方式上线：HashMap 路径继续存在，dense 路径先通过 CLI flag 或专用
   trainer opt-in。

## 非目标

- 不删除通用 `RegretTable<I>` / `StrategyAccumulator<I>`。
- 不改变 stage 1 / stage 2 的 `InfoAbstraction::map` contract。
- 不把 `Game::InfoSet`、`TableConfig.n_seats`、`SeatId` 等接口改成 heads-up 专属。
- 第一版不取消 `dispatch + local delta + deterministic merge` 并行结构。
- 第一版不追求 old checkpoint 与 dense checkpoint 的双向无损兼容；可以先支持
  HashMap checkpoint -> dense 加载，dense checkpoint 需要 schema bump。

## 当前规模估算

默认 profile：

| street | decision nodes | bucket count | infosets |
|---|---:|---:|---:|
| Preflop | 912 | 169 | 154,128 |
| Flop | 9,072 | 500 | 4,536,000 |
| Turn | 48,176 | 500 | 24,088,000 |
| River | 181,936 | 500 | 90,968,000 |
| Total | 240,096 | - | 119,746,128 |

如果采用固定 stride：

| layout | one table | regret + strategy |
|---|---:|---:|
| stride = 6 actions | 5.35 GiB | 10.70 GiB |
| stride = 8 actions | 7.14 GiB | 14.28 GiB |

更好的生产布局是按 node 的真实 `action_count` 做变长扁平数组：

```text
slot_base[node_id] = prefix_sum(bucket_count(node) * action_count(node))
slot(info, action_index) =
    slot_base[node_id] + bucket_id * action_count(node_id) + action_index
```

这样不需要 per-row `Vec`，也不会为 2-action / 3-action 节点浪费到 6 或 8 个
`f64`。具体内存要由 sizing 工具统计 `Σ bucket_count(node) * action_count(node)`
后给出。

visited bitset 很小：119.7M infoset 约 14.3 MiB / bitset。即使 regret 和
strategy 各维护一个 bitset，总量也不到 30 MiB。

## 数据布局方案

### 方案 A：full dense prealloc

训练开始时一次性分配：

```rust
struct DenseNlheTable {
    values: Vec<f64>,
    touched_rows: BitSet,
}
```

优点：

- 实现最直接，热路径下标计算后就是 slice 访问。
- 无 HashMap、无 page lookup、无运行期插入。
- checkpoint 可以直接写 raw f64 + bitset，避免排序和大规模 clone。

缺点：

- 训练开始就消耗完整内存。
- `strategy_sum` 访问比 regret 更稀疏，full dense 会为大量 0 行付费。
- `rescale_all` 若扫完整数组，会变成固定的内存带宽开销。

适用：第一版原型和 64 GB 级训练机。

### 方案 B：paged dense

把 dense index 空间切成固定大小 page，首次写入 page 时分配：

```text
global_slot -> page_id + offset
pages[page_id] -> Option<Box<[f64; PAGE_SLOTS]>>
```

优点：

- 仍然不需要 HashMap key；page table 是数组。
- 比 HashMap 更连续，且避免为从未访问的区域付费。
- `rescale_all` 可以只扫 allocated pages。

缺点：

- 热路径多一次 page lookup。
- 实现复杂度高于 full dense。
- checkpoint 要序列化 page bitmap + page payload。

适用：内存更紧的 32 GB 机器，或发现 `strategy_sum` 稀疏浪费明显时。

### 推荐落地顺序

先做 **full dense prealloc** 原型拿到真实 throughput / RSS / checkpoint 数据；
如果 full dense 在 32 GB 目标机器上仍偏紧，再把 `strategy_sum` 或两张表切到
paged dense。

## Indexer 设计

新增 NLHE 专用 indexer，不进入通用 `Game` trait：

```rust
struct NlheDenseIndexer {
    nodes: Vec<NlheDenseNodeMeta>,
    total_rows: u64,
    total_slots: u64,
}

struct NlheDenseNodeMeta {
    slot_base: u64,
    row_base: u64,
    bucket_count: u32,
    action_count: u8,
    street: StreetTag,
}
```

下标计算：

```text
node_id = info.raw() >> 38
bucket_id = info.bucket_id()
meta = nodes[node_id]

debug_assert bucket_id < meta.bucket_count
slot_start = meta.slot_base + bucket_id * meta.action_count
row_index = meta.row_base + bucket_id
```

`bucket_count` 来源：

- Preflop: 169
- Flop / Turn / River: `BucketTable::bucket_count(street_tag)`

`action_count` 来源：

- `PublicBettingTree::node(node_id).legal_actions.len()`
- 所有 bucket 共享同一个 node 的 action 顺序。

## Trainer 集成路线

不要第一步把 `EsMccfrTrainer<G>` 泛型化成 storage trait。更低风险的路线是新增
NLHE 专用 trainer 或内部 storage mode：

```text
HashMap path:
EsMccfrTrainer<SimplifiedNlheGame>

Dense path:
DenseNlheEsMccfrTrainer
```

或者在 CLI 层提供：

```text
tools/train_cfr --game nlhe --storage sparse-hashmap
tools/train_cfr --game nlhe --storage dense
tools/train_cfr --game nlhe --storage paged-dense
```

第一版建议：

- `EsMccfrTrainer<G>` 保持不动。
- 新增 `DenseNlheEsMccfrTrainer`，复制 NLHE ES-MCCFR recurse 的最小必要逻辑。
- 通过相同 seed 的短跑 checkpoint / strategy snapshot 与 HashMap 路径做对照。
- dense 路径稳定后，再评估是否抽象出内部 `RegretStorage` trait 复用代码。

这样可以避免一次性动到 Kuhn / Leduc / generic trainer 的签名测试。

## 并行语义

Dense array **不能自动取消** 当前的 `dispatch + local delta + merge` 结构。

原因：

- 多线程直接执行 `values[slot] += delta` 是非原子 read-modify-write，裸写会 data race。
- 即使用 atomic CAS 模拟 `f64` add，也会引入线程交错顺序差异，破坏 byte-equal
  确定性。
- 直接写主 regret 会让同一 batch 内后续 trajectory 读到部分更新后的 sigma，不再是
  pre-dispatch snapshot 语义。

第一版并行结构应保留：

```text
dispatch:
  worker read shared dense regret snapshot
  worker push local indexed delta

merge:
  main thread 按 tid 升序
  每个 worker 内按 push 顺序
  values[slot_start + a] += delta[a]
```

Local delta 从：

```text
(InfoSetId, SigmaVec)
```

改成：

```text
(slot_start: u64, row_index: u64, action_count: u8, SigmaVec)
```

如果 `slot_start` 已经足够定位表内 slice，`row_index` 只用于 touched bitset 和诊断。

可选实验路径：

| 模式 | 是否确定性 | 风险 | 备注 |
|---|---|---|---|
| deterministic local delta + merge | 是 | 低 | 默认路径 |
| shard-local merge | 是 | 中 | 按 slot range 分片归并，本质仍是 merge |
| Hogwild direct write | 否 | 高 | 仅作为 opt-in 质量实验 |
| atomic f64 add | 否/弱 | 高 | CAS 成本可能抵消省掉 merge 的收益 |
| double buffer delta table | 是 | 中 | 内存更高，periodic reduce |

结论：dense 第一版要优化的是 **merge 成本**，不是取消 merge。

## Checkpoint 方案

当前 checkpoint 写 `Vec<(InfoSet, Vec<f64>)>`，HashMap path 需要 clone + sort。
Dense path 应新增 schema：

```text
schema_version = 3
storage_kind = DenseNlheV1
indexer fingerprint:
  - bucket table blake3
  - action abstraction version / ratios
  - betting tree node count
  - per-node action_count hash
payload:
  - update_count / rng_state / lcfr metadata
  - regret_touched_rows bitset
  - strategy_touched_rows bitset
  - regret_values raw little-endian f64
  - strategy_values raw little-endian f64
```

兼容策略：

- dense trainer 可以加载旧 HashMap checkpoint：逐 entry 计算 slot 并填数组。
- 旧 HashMap trainer 不需要加载 dense checkpoint。
- schema bump 必须拒绝 layout fingerprint 不匹配的 checkpoint，避免把旧树或旧 bucket
  配置的数组误读成当前 profile。

后续可再做 sparse dense checkpoint：只写 touched rows 或 allocated pages，降低文件大小。

## Public query 语义

当前 `Trainer::current_strategy(info)` / `average_strategy(info)` 在 trainer 主表里没有该
infoset 时返回空 `Vec`，内部训练热路径则把未见 infoset 视为全 0 regret 并返回均匀
策略。

Dense path 如果 full prealloc，全 0 行无法区分“已分配但未访问”和“访问过但值为 0”。
因此需要 bitset：

- hot path `current_strategy_by_slot`：无需 touched，直接对全 0 regret 返回 uniform。
- public `current_strategy(info)`：若 regret/strategy 都未 touched，保持返回空 `Vec`。
- public `average_strategy(info)`：同理；若 touched 但 sum 为 0，返回 uniform。

这能保持现有外部测试和诊断工具语义。

## 实施阶段

### Phase 0：sizing / instrumentation

- 扩展 `tools/nlhe_betting_tree_sizing.rs`，输出：
  - `total_rows`
  - `total_slots_variable_action`
  - full dense memory estimate
  - fixed stride 6 / 8 memory estimate
  - per-street rows / slots
- 新增 indexer 单元测试：
  - 所有 node 的 `bucket_id` range 能成功映射。
  - `slot_start + action_count <= total_slots`。
  - 不同 `(node_id, bucket_id)` 映射到不同 row。
  - `action_count` 与 betting tree legal action 长度一致。

### Phase 1：dense table 原型

- 新增 `NlheDenseIndexer` 和 `DenseNlheTable`。
- 支持：
  - `current_strategy_smallvec_by_info`
  - `accumulate_by_slot`
  - `average_strategy_by_info`
  - `rescale_all`
- 不接 trainer，先用合成 delta 测试数值语义。

### Phase 2：DenseNlheEsMccfrTrainer

- 复制 NLHE ES-MCCFR recurse，改成 dense index hot path。
- local delta 存 `slot_start` 而不是 `InfoSetId`。
- 单线程 `step` 与 HashMap 路径短跑对照：
  - 固定 seed
  - 1K / 50K updates
  - snapshot probes average_strategy 一致或 byte-equal

### Phase 3：parallel dense path

- 实现 `step_parallel`：
  - worker 只读 shared dense regret。
  - local indexed delta append-only。
  - main thread deterministic playback merge。
- 对照 HashMap parallel 路径：
  - throughput
  - RSS
  - strategy snapshot
  - LBR proxy 小样本

### Phase 4：checkpoint v3

- dense raw checkpoint save/load。
- old HashMap checkpoint -> dense load。
- roundtrip 测试：
  - dense save/load 后 strategy snapshot 一致。
  - fingerprint 不匹配时明确报错。

### Phase 5：paged dense 评估

如果 full dense 内存仍不理想：

- 优先把 `strategy_sum` 改 paged dense。
- 再评估 regret 是否也需要 paged。
- 对比 page size：
  - 64 KiB slots
  - 256 KiB slots
  - 1 MiB slots

## 验证门槛

正确性：

- `cargo test --no-run`
- `cargo test nlhe_dense_indexer`
- `cargo test cfr_simplified_nlhe --release` 中与 storage 无关的测试继续通过。
- HashMap 与 dense 在固定 seed 短跑 strategy snapshot 一致。
- checkpoint roundtrip 一致。

性能：

- 1M updates，32 threads，B=128：
  - throughput 不低于 HashMap baseline。
  - RSS 峰值显著低于 HashMap path。
  - checkpoint wall time 显著下降。
- 100M updates：
  - 稳态 RSS 低于 30 GB。
  - final checkpoint 峰值低于 46.8 GB。
  - LBR proxy 不劣于同 update HashMap baseline 的统计误差范围。

确定性：

- 同 seed / 同 binary / 同 host 重跑 checkpoint hash 或 strategy snapshot byte-equal。
- parallel dense 默认路径不使用 atomic direct write 或 Hogwild。

## 风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| full dense 启动内存过高 | 32 GB 机器可能吃紧 | CLI flag opt-in；必要时 strategy_sum 先 paged dense |
| dense rescale 扫完整数组 | LCFR period boundary 变慢 | 记录 rescale wall；paged 模式只扫 allocated pages |
| checkpoint schema 复杂化 | 老工具无法读 dense ckpt | schema v3 明确 storage_kind；先改 reader / report 工具 |
| 代码复制 NLHE recurse | 维护成本上升 | 原型阶段接受；稳定后抽内部 storage trait |
| action abstraction 改变导致 index 错读 | checkpoint 静默错误 | layout fingerprint 必须包含 per-node action_count hash |
| 直接写数组诱惑大 | 破确定性或引入 data race | 默认只走 deterministic local delta + merge |

## 建议结论

这个方向对当前 heads-up NLHE 是可行的，并且比继续微优化 HashMap 更有上限。
推荐下一步先做 `Phase 0 + Phase 1`：拿到真实 `total_slots_variable_action` 和 dense
原型内存，再决定 full dense 是否足够，还是直接进入 paged dense。

并行层面不要第一步取消 `dispatch + merge`。先把 merge 从 HashMap playback 变成数组
下标 playback，保住确定性和训练语义；等 dense baseline 稳定后，再单独实验
Hogwild / shard merge 等更激进模式。

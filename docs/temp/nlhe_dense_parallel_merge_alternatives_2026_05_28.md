# Dense 并行训练去/省 merge 两个方案

写作背景：`docs/status_v2.md` §训练吞吐基线注「throughput 上限由 step_parallel
serial merge 卡死」；dense 后端 26.8k/s 稳态后不再衰，但 bet-size 扩张到 359.6M
infoset / 13.48 GiB 表后 merge 的 O(N×B) f64 加法会按比例放大，可能重新成为瓶颈。
本文档把
`docs/temp/nlhe_dense_infoset_table_plan_2026_05_26.md` §并行语义 里只列了一行的
两个候选——**lock-free atomic add**（plan 表中 "Hogwild! direct write" /
"atomic f64 add" 两行的工程合并，详 §A.0 术语澄清）与 **shard-local merge**——
展开成实现级方案，
包含改动文件、数据结构、LCFR / bitset / checkpoint 交互、测试改动、风险与预期收益。

不做决策。两方案并列。结尾给一份对比表 + "先做哪个" 的最小建议。

## 0. 共享前提

两方案共享下列前提，方案描述内不再重复：

- 改动主体在 `src/training/nlhe_dense.rs`（表层）与
  `src/training/nlhe_dense_trainer.rs`（trainer step_parallel）。
- `EsMccfrTrainer<G>`（HashMap path）**不动**，本文档只覆盖
  `DenseNlheEsMccfrTrainer`。HashMap path 走原 `step_parallel` 提供
  byte-equal 对照锚（`tests/dense_nlhe_trainer.rs::dense_step_parallel_byte_equal_hashmap`
  仍然是 dense 默认路径的合同；新增路径不破坏它，新路径走自己的
  flag / 新 entry point）。
- 单线程 `step` 路径与 `recurse_es_dense` **完全不改**；两方案只动
  `step_parallel` / `recurse_es_dense_parallel`。
- 仓库 `unsafe_code = "forbid"`，整篇方案不引入 unsafe；用
  `std::sync::atomic` 已有的安全原语 + 现有 rayon。
- 仓库已有 `AtomicU64` 使用先例（`src/training/nlhe.rs:31`
  `SimplifiedNlheState::info_set_cache`），可参照同型 import。
- LCFR period rescale 行为以「period boundary 后下一批 update 看到的逻辑表」
  byte-equal 不再要求，但 **logical 语义**（`Σ R⁺ → σ`、`Σ S → avg_strategy`）必须
  在 1‰ 内匹配 HashMap path 短跑 + 在 100M smoke run 匹配
  `run_dense_lcfr_100m` LBR 1,143 ± 87 这一档（见 docs/status_v2.md）。

---

## 方案 A — Lock-free atomic add

### A.0 术语澄清：严格 Hogwild! vs lock-free atomic

口语里两个词常混用，工程语境下应该区分：

| 名字 | 写操作 | 内存模型 | Rust 可行性 |
|---|---|---|---|
| 严格 Hogwild!（Niu et al. 2011 原版）| `*p += delta` 裸写 | 靠硬件 8-byte aligned store 原子性（x86 / ARM64 保证）| **需 unsafe**，本仓库 `unsafe_code = "forbid"` 禁掉 |
| Lock-free atomic add（本方案）| `cell.fetch_update(\|b\| Some((from_bits(b) + d).to_bits()))` CAS loop | 形式化 atomic，CAS 冲突自动重试 | `Vec<AtomicU64>` + bit-cast，**safe Rust** |
| 原生 atomic fetch_add | `cell.fetch_add(delta, Relaxed)` 单指令 | atomic | 仅整数类型（u64/i64）；**f64 无此 API** |

Niu 2011 论文标题就是「Hogwild!: A Lock-Free Approach to Parallelizing SGD」——
**论文里 Hogwild! ≡ lock-free**，两个词同义。但工程实务中：

- 「Hogwild!」 = 裸写 / 接受 torn write，最激进
- 「lock-free atomic」 = 用 `std::sync::atomic` 保单 cell 写原子，仍允许多 worker
  顺序非确定

仓库 lint `forbid unsafe` 砍掉严格 Hogwild! 这一档，所以**本方案只能走 lock-free
atomic via CAS loop**。性能对比：

- 严格 Hogwild! 单 cell 写 = 单条 mov 指令；CAS loop 在无冲突时 = 1 LL/SC
  pair，几个 cycle 多开销；CAS 失败重试时再翻倍。
- NLHE 119M slot / 0.06% revisit prob 下冲突极稀，两者实测应几乎不可分
  （差 < 2%）。
- **正确性上 CAS 更强**：单 cell 写永远是合法 delta 子集之和，无 torn write
  风险（x86 上其实也无，但 Rust 内存模型不允许这条假设）。

下文「方案 A」/ 「lock-free atomic add」/「CAS path」三个名字等义。

### A.1 核心思想

worker 直接对主 `regret` / `strategy_sum` 表做 lock-free atomic f64 add（CAS
loop），**完全去掉 local delta + merge 阶段**。trajectory 之间 σ 读看到的是当下
表（不再是 pre-dispatch snapshot）。多 worker 同一 slot 写有 race，但因为只做
加法、加法可交换、且每次 CAS 把 cell 写成「之前值 + delta」整体原子可见，**最终
值是某种合法 delta interleaving 的和**，CFR/MCCFR 收敛性沿用 Hogwild! 论文
（Niu et al. 2011，"A Lock-Free Approach"）+ 后续 MCCFR 经验（Pluribus /
Libratus 据传走同型 lock-free accum）。

### A.2 数据结构改动

`src/training/nlhe_dense.rs::DenseNlheTable`：

```rust
// 当前
pub struct DenseNlheTable {
    indexer: Arc<NlheDenseIndexer>,
    global_scale: f64,
    values: Vec<f64>,
    touched_rows: TouchedRows,
}

// 方案 A 后
pub struct DenseNlheTable {
    indexer: Arc<NlheDenseIndexer>,
    global_scale: AtomicU64,          // f64 bits via to_bits / from_bits
    values: Vec<AtomicU64>,           // 与 Vec<f64> 同 size（每元素 8 byte）
    touched_rows: TouchedRows,         // 改为 Vec<AtomicU64> 见 §A.6
}
```

`Vec<AtomicU64>` 与 `Vec<f64>` 在内存上等价（都是 8-byte aligned u64 数组），
RSS 不变。`AtomicU64` 不是 `Clone`，所以 `DenseNlheTable` 失去 derive Clone
（当前也没有 derive，影响为零）。

### A.3 热路径改动

#### A.3.1 atomic f64 add helper（私有）

```rust
#[inline]
fn atomic_f64_add(cell: &AtomicU64, delta: f64) {
    // 严格 Hogwild!（Niu et al. 2011）在 x86 上走 `*p += δ` 裸写，靠硬件 8-byte
    // aligned store 原子性。Rust 内存模型不允许这条假设（且需 unsafe），改用
    // CAS loop = lock-free atomic add，单 cell 写永远是合法 delta 子集之和。
    // Relaxed 即可：CFR 收敛只关心最终值，不依赖 happens-before。
    let _ = cell.fetch_update(
        Ordering::Relaxed,
        Ordering::Relaxed,
        |bits| {
            let v = f64::from_bits(bits);
            Some((v + delta).to_bits())
        },
    );
    // fetch_update 内部 CAS 失败会自动重试到成功；不可能返回 Err（closure 恒
    // 返回 Some）。无 ABA 风险：f64 加法是单步业务原子单位。
}
```

#### A.3.2 `accumulate_by_slot` 改写

```rust
pub fn accumulate_by_slot(&self, slot_start: u64, row_index: u64, delta: &[f64]) {
    let start = slot_start as usize;
    let scale_bits = self.global_scale.load(Ordering::Relaxed);
    let scale = f64::from_bits(scale_bits);
    if scale == 1.0 {
        for (i, &d) in delta.iter().enumerate() {
            atomic_f64_add(&self.values[start + i], d);
        }
    } else {
        let inv = 1.0 / scale; // 不写成 d/scale 让 hot path 少一次除法
        for (i, &d) in delta.iter().enumerate() {
            atomic_f64_add(&self.values[start + i], d * inv);
        }
    }
    self.touched_rows.set(row_index); // §A.6 改 atomic word OR
}
```

注意签名从 `&mut self` 改为 `&self`：rayon worker 可以共享 `&DenseNlheTable`
不需要任何外层锁。

#### A.3.3 `current_strategy_smallvec_at` 改写

```rust
pub(crate) fn current_strategy_smallvec_at(
    &self,
    slot_start: u64,
    action_count: usize,
) -> SigmaVec {
    let n = action_count;
    let start = slot_start as usize;
    let scale = f64::from_bits(self.global_scale.load(Ordering::Relaxed));
    let mut positives: SigmaVec = SigmaVec::with_capacity(n);
    let mut sum = 0.0_f64;
    for i in 0..n {
        let raw = f64::from_bits(self.values[start + i].load(Ordering::Relaxed));
        let logical = raw * scale;
        let r_plus = if logical > 0.0 { logical } else { 0.0 };
        positives.push(r_plus);
        sum += r_plus;
    }
    if sum > 0.0 {
        for p in &mut positives { *p /= sum; }
        positives
    } else {
        SigmaVec::from_elem(1.0 / n as f64, n)
    }
}
```

`average_strategy_by_info` / `row_sum_by_info` 同型改 `load(Relaxed) + from_bits`。

### A.4 trainer step_parallel 改写

`src/training/nlhe_dense_trainer.rs::step_parallel` 收缩成（新 recurse helper
统一走 `_lockfree` 后缀，与现有 `_parallel` 同级区分；标识符与术语「lock-free
atomic」一致，文档内任何提到 "Hogwild!" 都是论文概念引用，不再作为代码命名）：

```rust
pub fn step_parallel(
    &mut self,
    rng_pool: &mut [Box<dyn RngSource>],
    n_threads: usize,
    batch_per_worker: usize,
) -> Result<(), TrainerError> {
    let n_active = n_threads.min(rng_pool.len());
    if n_active == 0 || batch_per_worker == 0 { return Ok(()); }
    let active_pool = &mut rng_pool[..n_active];
    let n_players = self.game.n_players() as u64;
    let base = self.update_count;
    let game = &self.game;
    let regret = &self.regret;          // &DenseNlheTable
    let strategy = &self.strategy_sum;  // &DenseNlheTable
    active_pool.par_iter_mut().enumerate().for_each(|(tid, rng_slot)| {
        let rng = rng_slot.as_mut();
        for batch_idx in 0..batch_per_worker {
            let traj = batch_idx as u64 * n_active as u64 + tid as u64;
            let traverser = ((base + traj) % n_players) as PlayerId;
            let root = game.root(rng);
            recurse_es_dense_lockfree(root, traverser, 1.0, regret, strategy, rng);
        }
    });
    self.update_count += (n_active as u64) * (batch_per_worker as u64);
    self.maybe_lcfr_rescale_lockfree();   // §A.5
    Ok(())
}
```

`recurse_es_dense_lockfree` 与现有 `recurse_es_dense_parallel` 几乎同型，差别：

- σ 读：直接 `regret.current_strategy_smallvec_at(slot_start, n)`（load 当下值，
  不读 thread-local snapshot）。
- regret / strategy_sum 写：直接 `regret.accumulate_by_slot(...)`
  / `strategy.accumulate_by_slot(...)`（&self 入口）。
- 不再持有 `DenseLocalDelta`；签名删两个 local 参数。

### A.5 LCFR rescale 处理

period boundary 需要把所有 worker 暂停、改 `global_scale`、再启动。当前
`step_parallel` 是同步入口（rayon par_iter 完成后才到 rescale），所以**rescale
天然在所有 worker 结束之后调用**——这一前提不变。

但有一个新问题：`step_parallel` 一次产 `n_active × batch_per_worker` 个 update，
可能跨 period boundary。原 deterministic merge 版本，本批 delta 全用 pre-rescale
scale 累积，rescale 在 update_count 增加后调用一次性补齐；lock-free 版本下，
worker 持续读 `global_scale.load(Relaxed)`，**一个 worker 在 batch 中途读到的
scale 仍然是本批开始时的值**（rescale 还没发生），所以这条不变量保持。

但若有人把 `step_parallel` 拆细到「每 N 个 update 强行 rescale 一次」，会有半
worker 看旧 scale / 半看新 scale 的 race。**约束**：保持 rescale 只在
`step_parallel` 末尾调用一次（与现有结构一致），不允许 worker 内部 trip rescale。

`maybe_lcfr_rescale_lockfree`：

```rust
fn maybe_lcfr_rescale_lockfree(&mut self) {
    let Some(period) = self.lcfr_period_size else { return; };
    let target = self.update_count / period;
    while self.lcfr_periods_completed < target {
        let n = self.lcfr_periods_completed + 1;
        let factor = (n as f64) / ((n + 1) as f64);
        if self.lcfr_rescale_regret { self.regret.rescale_all_atomic(factor); }
        self.strategy_sum.rescale_all_atomic(factor);
        self.lcfr_periods_completed = n;
    }
}
```

`rescale_all_atomic`：因为 worker 全部 join，可以独占地走 CAS：

```rust
pub fn rescale_all_atomic(&self, factor: f64) {
    let cur = f64::from_bits(self.global_scale.load(Ordering::Relaxed));
    let next = cur * factor;
    if factor > 0.0 && next.is_finite() && next != 0.0 {
        self.global_scale.store(next.to_bits(), Ordering::Relaxed);
        return;
    }
    // 异常因子：materialize（eager 扫表 ×factor，scale 复位 1.0）。
    // 同样是 worker join 后调用，可以走 plain load → multiply → store 不
    // 必 CAS，因为没人写。
    let scale = f64::from_bits(self.global_scale.load(Ordering::Relaxed));
    let combined = scale * factor;
    for cell in &self.values {
        let v = f64::from_bits(cell.load(Ordering::Relaxed)) * combined;
        cell.store(v.to_bits(), Ordering::Relaxed);
    }
    self.global_scale.store(1.0_f64.to_bits(), Ordering::Relaxed);
}
```

### A.6 touched_rows 处理

`TouchedRows::set` 必须线程安全。改动：

```rust
struct TouchedRows {
    words: Vec<AtomicU64>,
    len: u64,
}

impl TouchedRows {
    fn set(&self, idx: u64) {
        let word = (idx / 64) as usize;
        let bit = 1u64 << (idx % 64);
        self.words[word].fetch_or(bit, Ordering::Relaxed);
    }
    fn get(&self, idx: u64) -> bool {
        let word = (idx / 64) as usize;
        let bit = 1u64 << (idx % 64);
        self.words[word].load(Ordering::Relaxed) & bit != 0
    }
}
```

`words()` / `words_mut()`（checkpoint save/load 用）需要在 worker 全 join 后
调用——这一前提与现有 checkpoint 调用路径一致（`save_checkpoint` / `load`
都不在 `step_parallel` 期间调）。

### A.7 Checkpoint 影响

`nlhe_dense_checkpoint.rs` 保存的是 logical f64 vector，物理底层换成
`Vec<AtomicU64>` 后：

- save：把 `cell.load(Relaxed) * scale` 流式写出（每 chunk 内逐元素 from_bits
  → 乘 scale → to_le_bytes）。逻辑等价当前实现。
- load：逐元素 read le_bytes → from_le_bytes → 直接 store；新表 scale = 1.0。

checkpoint 二进制 layout 不变 → 不需要 bump schema_version 3 / `DenseLayoutFingerprint`
不需要新字段。但 magic 是否 bump（识别"这是 lock-free atomic 产物" vs
deterministic merge 产物）需要决策——blueprint 内容上无法区分，可以不 bump，
把 checkpoint 当"任一 dense path 产物"。

### A.8 BLAKE3 byte-equal 对照失效

`tests/cfr_simplified_nlhe.rs::simplified_nlhe_es_mccfr_fixed_seed_repeat_3_times_blake3_identical_1m_update`
钉死跨 run 字节相等。Lock-free atomic add 路径**不可能**通过它——CAS race
顺序不确定，每次 worker 间到达同一 cell 的 interleaving 不同。两条出路：

1. **路径分流**：anchor 测试只跑 deterministic merge path（即现状 default），
   lock-free atomic 走 opt-in flag `with_lockfree_parallel()`；anchor 不接此
   路径，`nlhe_h3_report --dense` 之类下游工具看 path 选择自动走对应 trainer。
2. **统计等价对照**：新增 anchor 跑 N 次 lock-free atomic，比对 strategy
   snapshot 余弦相似度 > 0.999、L∞ 偏差 < 1e-3。N=3 / 100k update 短跑。

建议两条都要：1 保护现有 anchor 永远不被破坏，2 给 lock-free 路径自己的回归门槛。

### A.9 测试改动

新增 `tests/dense_nlhe_trainer_lockfree.rs`（与现有 `dense_nlhe_trainer.rs`
平级）：

| Test | 内容 | 通过门槛 |
|---|---|---|
| `lockfree_smoke_no_panic` | 10k update × 8 thread × B=16，trainer 不 panic | run 完不 abort |
| `lockfree_avg_strategy_close_to_hashmap` | 100k update 同 seed pool，与 HashMap 路径同 trainer 比 avg_strategy L∞ | L∞ < 5e-3 在 traverser 已访问 infoset 集合 |
| `lockfree_lbr_no_regression_short` | 1M update LCFR period=100k，LBR proxy 计算与 deterministic dense 同档 | LBR ratio in [0.7, 1.4]（短跑噪声大，宽松门槛） |
| `lockfree_self_consistency` | 同 trainer 跑 100k 后跑 average_strategy 全 probe，检查 `Σ_a avg(a) ≈ 1.0`（允许 1e-9 误差） | sum 偏 < 1e-9 |

长跑回归（ignored）：

| Test | 内容 |
|---|---|
| `lockfree_lcfr_100m_lbr_match_run_dense_lcfr_100m` | 100M LCFR period=1M LBR proxy，目标 ≈ 1,143 ± 87（dense baseline）|

`tests/dense_nlhe_trainer.rs::dense_step_parallel_byte_equal_hashmap` **保持不动
且必须继续过**：保护 deterministic path。

### A.10 性能预期

CAS loop 单 retry cost 几十 cycle，目标 profile 119M slot × 0.06% revisit prob
（per merge 阶段 estimation）下命中率极低，期望 ≤ 1.1× CAS retry。

但 **cache-line 抢占**才是 lock-free atomic / Hogwild! 实战瓶颈：

- 决策节点 action_count avg 2.516，单 infoset row 占 ~20 byte，比 cache line
  小，**邻居 row 共享 cache line**。多 worker 写邻居行会有 false sharing。
- preflop 169 row + flop 9072 节点共享 hot rows，preflop strategy_sum row
  在每 trajectory 都被写 → 100% cache line 争抢。
- HashMap path 因为每 entry 独立 alloc 反而避开 false sharing。

预期净效果：32 thread 在 c6a.8xlarge 上从 26.8k/s → **乐观 50–80k/s（去 merge
吃满 trajectory 并发），悲观 25k/s（false sharing 吃光收益）**。**必须先做
microbench 才能下结论**，bench 候选位置 `benches/stage3.rs::dense_step_parallel_lockfree`。

bet-size 扩张到 359.6M infoset 后，平均 revisit prob 进一步降低 + 表更大
slot 更稀疏 → cache line 抢占应更轻，方案 A 收益对扩张更友好。

### A.11 已知偏离 / 风险

| 风险 | 描述 | 缓解 |
|---|---|---|
| BLAKE3 anchor 失效 | 无法跨 run 字节相等 | 路径分流（§A.8）|
| LCFR rescale 期间 worker 持有旧 scale | 现行结构下 rescale 在 worker join 后，已天然避免 | 不允许 worker 内部触发 rescale（lint / 代码 review）|
| f64 NaN 透过 CAS | `+= 0.0/0.0` 后 cell 永久 NaN | NaN delta 是上游 trainer bug；保留现有 debug_assert |
| `global_scale` 趋零 underflow | 长 run rescale 累积 N!/(N+1)! 趋 0 | 已有 `next.is_finite() && next != 0.0` 校验，trip eager materialize |
| `Vec<AtomicU64>` allocator 行为 | 与 `Vec<f64>` 等价（同 size / align）| 无（Rust 标准库保证）|
| CAS retry 风暴 | 高争抢 cell（preflop 169 row）上 CAS 失败重试堆积 | 1) preflop row 共 169 × 6 = 1014 slot 极少；2) 真出问题可对极热 row 走 thread-local accumulator + 周期 flush（混合模式，本文档未展开）|
| Pluribus 论文未明确 lock-free 设计 | 经验外推 | 短跑 anchor + LBR baseline 对照（§A.9）|

---

## 方案 B — shard-local merge

### B.1 核心思想

保留 local delta + merge 结构（**byte-equal 保住**），但 merge 从「main thread
serial playback」改为「rayon par_iter over shard，每 shard 内部串行 playback」。
shard 按 `slot_start` 区间切，shard 之间 slot 不重叠 → 跨 shard 并行写主表零
race。

merge 复杂度从 O(N_delta) serial 改为 O(N_delta / S) parallel + O(S) overhead，
S = shard 数。

### B.2 shard 切分策略

`total_slots` 在生产 profile 是 310M（当前）/ 833M（bet-size 扩张后），按
shard_size = 1M slot 切：

- 310M / 1M = 310 shard（当前）
- 833M / 1M = 833 shard（扩张后）

shard 边界对齐 `node_id`，因为：

1. infoset 的 `slot_start` 由 `slot_base[node_id] + bucket * action_count` 决定，
   `slot_start + action_count` 永远不跨 `slot_base[node_id + 1]`（同节点行不跨界）。
2. shard 边界放在 `slot_base[*]` 上，单 worker 一条 trajectory 经过的所有
   infoset，每个独立落进恰好 1 shard——push 时按 slot_start `/ shard_stride`
   一次性分流。

但 node 数 240k 远 > shard 数 833，shard 不必每节点一个。简化：固定 shard 个数
S = 64（或 8 × n_threads），按 `total_slots / S` 等分；shard_id =
`slot_start / shard_size`。**不需要** 与 node 边界对齐——单 infoset row 长度 ≤ 7
slot，跨界概率 ≈ 7 / shard_size = 7e-6，可接受 ±1 shard 漂移；漂移不影响
byte-equal（shard 内仍按 push 顺序串行），只影响 worker 内部分桶决策。

更严格的对齐策略：

```rust
let shard_size = total_slots.div_ceil(NUM_SHARDS);
let shard_id_of = |slot_start: u64| (slot_start / shard_size) as usize;
```

跨界 slot 直接进 `shard_id_of(slot_start)` 那个 shard（按起点判断），不拆。

### B.3 数据结构改动

`DenseLocalDelta` → `ShardedLocalDelta`：

```rust
pub(crate) struct ShardedLocalDelta {
    // 每 shard 一个 entries vec；shard_id_of(slot_start) 决定 push 进哪个。
    shards: Vec<Vec<(u64, u64, SigmaVec)>>,
}

impl ShardedLocalDelta {
    pub(crate) fn new(num_shards: usize) -> Self {
        Self { shards: (0..num_shards).map(|_| Vec::new()).collect() }
    }
    pub(crate) fn push(&mut self, shard_id: usize, slot_start: u64, row_index: u64, delta: SigmaVec) {
        self.shards[shard_id].push((slot_start, row_index, delta));
    }
}
```

每 worker 持有 `(ShardedLocalDelta, ShardedLocalDelta)`，regret + strategy_sum
各一份。

### B.4 trainer step_parallel 改写

```rust
const NUM_SHARDS: usize = 64;  // tunable，可走 CLI flag

pub fn step_parallel(&mut self, rng_pool: &mut [Box<dyn RngSource>], n_threads: usize, batch_per_worker: usize) {
    // ... 同当前实现，dispatch 部分把 DenseLocalDelta::new() 换成
    // ShardedLocalDelta::new(NUM_SHARDS)；
    // recurse_es_dense_parallel 内 push 时按 shard_id 分流（§B.5）。

    let shard_size = self.regret.indexer().total_slots().div_ceil(NUM_SHARDS as u64);

    let deltas: Vec<(ShardedLocalDelta, ShardedLocalDelta)> = active_pool
        .par_iter_mut().enumerate()
        .map(|(tid, rng_slot)| { /* 同当前，把 DenseLocalDelta → ShardedLocalDelta */ })
        .collect();

    // 关键改动：merge 走 par_iter over shard_id。
    let regret = &mut self.regret;       // 注意：需要 split_at_mut by shard
    let strategy = &mut self.strategy_sum;

    // values: Vec<f64> 没有原生 split-by-shard，但 par_chunks_mut 可以；
    // 每 shard 独立 mut slice 给 rayon worker。bitset 同型 split。
    (0..NUM_SHARDS).into_par_iter().for_each(|shard_id| {
        let start = (shard_id as u64) * shard_size;
        let end = ((shard_id as u64 + 1) * shard_size).min(regret.num_slots() as u64);

        // 关键：跨 shard 写入主表需要 unsafe split 或独立 slice cell。
        // §B.6 详述。
        for (local_r, _) in &deltas {
            for &(slot_start, row_index, ref delta) in &local_r.shards[shard_id] {
                // playback 进主表 [start..end] 区间
                regret_shard_accumulate(slot_start, row_index, delta);
            }
        }
        // strategy_sum 同型
    });

    self.update_count += (n_active as u64) * (batch_per_worker as u64);
    self.maybe_lcfr_rescale();
}
```

### B.5 worker push 分流

`recurse_es_dense_parallel` 内：

```rust
// 当前
local_regret.push(slot.slot_start, slot.row_index, delta);

// shard 版本
let shard_id = (slot.slot_start / shard_size) as usize;
local_regret.push(shard_id, slot.slot_start, slot.row_index, delta);
```

`shard_size` 通过闭包捕获或参数传入。worker 内仍按 DFS 顺序 push 到对应
shard——shard 内顺序保持 deterministic。

### B.6 主表 split-by-shard 的 Rust 借用问题

`forbid unsafe` 下，把 `Vec<f64>` 切成 N 个 disjoint `&mut [f64]` slice 给
rayon worker 用，有两条路：

**B.6.a — `rayon::slice::ParallelSliceMut::par_chunks_mut`**

```rust
let chunks: Vec<&mut [f64]> = regret.values_mut()  // 需要新增 pub(crate) 入口
    .par_chunks_mut(shard_size as usize)
    .collect();
// chunks[shard_id] 就是该 shard 的 slice
```

`par_chunks_mut` 自身用了 unsafe split，但它在 rayon crate 内，**调用方代码**
不出现 unsafe。lint `forbid` 只作用于本 crate，可以用。

**B.6.b — 提供 sharded accumulator API**

把分片逻辑封装进 `DenseNlheTable`：

```rust
impl DenseNlheTable {
    /// 把表按 shard 切，对每 shard 调用 closure（rayon par_iter 内调用）。
    pub fn par_apply_shards<F>(&mut self, num_shards: usize, f: F)
    where F: Fn(usize, &mut [f64], &mut [u64], u64, u64) + Sync + Send,
          // (shard_id, values_slice, touched_words_slice, slot_start_base, slot_end)
    {
        let shard_size = self.values.len().div_ceil(num_shards);
        // bitset 边界对齐：每 shard 包含 shard_size / 64 个 word（前提：
        // shard_size % 64 == 0；选 NUM_SHARDS 时强制 64 整除）
        // ... rayon par_chunks_mut over self.values
    }
}
```

trainer 端：

```rust
self.regret.par_apply_shards(NUM_SHARDS, |shard_id, vals, touched, base_slot, _end| {
    for (local_r, _) in &deltas {
        for &(slot_start, row_index, ref delta) in &local_r.shards[shard_id] {
            let local_start = (slot_start - base_slot) as usize;
            // values[local_start..local_start+delta.len()] += delta[*]
            // touched: 类似 (row_index - base_row) bit set
        }
    }
});
```

bitset 切分要求 `shard_size % 64 == 0`；选 NUM_SHARDS 让
`total_slots / NUM_SHARDS` 64 整除（或在 indexer 建表时 round up 到 64 的
倍数，浪费 ≤ 64 个 slot 可忽略）。

推荐 B.6.b：封装更清晰，trainer 不接触 raw slice。

### B.7 LCFR rescale 处理

**完全不变**：merge 结束后调 `maybe_lcfr_rescale`，与现状一致。`rescale_all`
的 `&mut self` 入口也不变（worker join 后独占）。无新风险。

### B.8 checkpoint 影响

**零影响**。`DenseNlheTable` 的物理 layout（`Vec<f64>`）不变；
`raw_values` / `touched_words` 入口不变；save / load 二进制 byte-equal。

### B.9 BLAKE3 byte-equal 处理

要求 merge 阶段 shard 内 playback 顺序与 deterministic merge 路径**完全一致**：

- 当前路径：`for (tid in 0..n_active) for (entry in worker[tid].entries) accumulate`
- shard 路径：`for (shard in shards par) for (tid in 0..n_active) for (entry in worker[tid].shards[shard_id]) accumulate`

worker 内 push 顺序是 DFS deterministic，所以同 `(shard, tid, push_idx)` 三元组
下 entry 顺序固定。跨 shard 并行不影响**单个 cell** 的 f64 加法序列（每 cell
只属于一个 shard）→ byte-equal 保持。

**注意**：shard_size 必须 deterministic（`total_slots.div_ceil(NUM_SHARDS)`
是纯函数），NUM_SHARDS 是常量。如果做成 CLI flag tunable，必须把
NUM_SHARDS 进 checkpoint metadata + BLAKE3 anchor 测试固定一个值。

**测试**：`tests/dense_nlhe_trainer.rs::dense_step_parallel_byte_equal_hashmap`
保持过——这是 shard 方案的 must-have，回归立即 reject。

### B.10 测试改动

| Test | 内容 | 通过门槛 |
|---|---|---|
| 现有 `dense_step_parallel_byte_equal_hashmap` | 不动 | 仍然 byte-equal HashMap |
| 新 `shard_merge_byte_equal_pre_shard` | 同 trainer 切 / 不切 shard 两路 N=10k update，逐 cell `to_bits` 相等 | f64 bits 全等 |
| 新 `shard_merge_throughput_bench`（benches/）| n_threads=32 / NUM_SHARDS ∈ {1,8,32,64,256}，记 step_parallel wall | 不验断言，纯 perf 曲线 |

短跑无新行为差异，所以不需要 LBR 对照。

### B.11 性能预期

merge 当前 c6a.8xlarge 32 thread / B=128 实测占比未直接测出，但 plan §并行语义
里把 「serial merge」明确列为 throughput 上限。粗算：

- 一次 `step_parallel` 产 32 × 128 = 4096 update；每 update DFS 访问 ~30
  decision node（NLHE avg trajectory depth），共 ~120k push；每 push 一次
  f64 vector add（avg 2.5 slot）→ ~300k f64 add per `step_parallel`。
- 当前 serial merge：~300k f64 add @ ~5 ns/add ≈ 1.5 ms / step_parallel call。
- worker compute（recurse）：26.8k/s × 4096 update / call ≈ 152 ms / call
  on dense path。
- merge 占 ~1%——**当前 dense 100M profile 下 merge 远不是瓶颈**。

bet-size 扩张到 359.6M / 13.48 GiB 后，节点数 +3×、avg action 3-4、merge 总
add 数 ~1M / call ≈ 5 ms，worker compute 也 ~3× = 450 ms / call，merge 仍占 ~1%。

**结论**：shard-local merge 在当前规模 / 扩张规模都不解决主要瓶颈，**预期收益
< 5%**。它的价值在保 byte-equal 的前提下提供一条「serial merge 万一变瓶颈」的
逃生通道，不在当下吞吐。

### B.12 风险

| 风险 | 描述 | 缓解 |
|---|---|---|
| 收益 < 5% | merge 不是当前瓶颈 | 先 bench 量化 merge 占比再决定是否实现 |
| par_apply_shards 引入 internal unsafe（rayon 的）| 仓库 lint 不被触发，但审计面增 | 锁 rayon 版本，代码 review |
| bitset 64-bit word 对齐 | shard_size 必须 64 整除 | indexer 建表 round-up，浪费 < 64 slot |
| 跨 shard 漂移（单 row 跨 shard 边界）| 不发生：row 长 ≤ 7 < shard_size 1M | N/A |

---

## 对比

四档候选（包含被 lint 砍掉的严格 Hogwild! 作为参考基线）：

| 维度 | 现状（deterministic merge）| 严格 Hogwild!（torn write OK）| 方案 A（lock-free atomic CAS）| 方案 B（shard merge）|
|---|---|---|---|---|
| BLAKE3 byte-equal | ✅ | ❌ | ❌ | ✅ |
| HashMap path byte-equal 对照 | ✅ | ❌ | ❌ 走路径分流 | ✅ |
| **可在本仓库实现** | ✅ | ❌ 需 unsafe（lint forbid）| ✅ | ✅ |
| merge 阶段并行 | ❌ serial | n/a 无 merge | n/a 无 merge | ✅ par over shard |
| 单 cell 写原子性 | n/a（serial 阶段）| 硬件 aligned mov（论文假设）| CAS loop（Rust 内存模型保证）| n/a（shard 内 serial）|
| LCFR rescale 复杂度 | trivial | trivial | trivial（worker join 后单独 store）| trivial |
| checkpoint 兼容 | baseline | n/a | 物理 layout 同 / atomic load → f64 写出 | 完全相同 |
| Rust 改动量 | n/a | n/a | 中：表层全换 AtomicU64 / 新 recurse / 新 anchor | 小：merge 段加 par_chunks，worker push 加 shard_id |
| 当前 profile 预期收益 | baseline | 30–200%（理论上限）| 30–200%（CAS retry 比严格版差 <2%）| < 5% |
| 扩张 profile 预期收益 | baseline | 同当前或更好 | 同当前或更好 | < 5% |
| 主要风险 | n/a | unsafe / lint 不通过 | cache-line false sharing 吃光收益 | 收益不够大不值得做 |

**实际只剩三档可选**：现状 / 方案 A / 方案 B。严格 Hogwild! 列在表里只是为了
说明「方案 A 与论文原版的差距 ≈ CAS retry 的开销，在 NLHE 稀疏冲突下基本可忽略」，
不是真正候选。

### 建议路径（不做决策，只列）

如果当前真正的瓶颈是 worker compute（trajectory DFS）而不是 merge——`status_v2.md`
的 26.8k/s 稳态曲线高度暗示如此——那两方案都不是头号杠杆。先做：

1. `perf record` 跑一次 dense step_parallel，看 merge 阶段实际占百分之几。
2. 若 merge > 10%：先做方案 B（保 byte-equal，改动小），有 5–10% wall 收益。
3. 若 merge < 5% 且 worker compute 占 > 80%：方案 A 才有意义——但它在
   c6a.8xlarge 32 thread 下能否真的 break 26.8k/s ceiling，必须先做 prototype
   microbench 验证，不能纸面推断（false sharing 在 NLHE 119M slot 上的真实
   行为只能实测）。
4. 任何决策前，写一个 `--parallel-mode {deterministic, sharded, lockfree}` CLI
   flag，三条路径并存，让 bench 在同 trainer 上对照。

### 待决问题

- NUM_SHARDS 选多少（B 方案）：固定 64 还是 8 × n_threads 还是 CLI。
- Lock-free atomic（A 方案）是否做 statistical anchor（§A.8 第 2 条），还是只靠 LBR baseline 对照。
- merge perf 实测谁来做：现有 c6a.8xlarge 32 vCPU box 是否还在；不在则按
  feedback `high_perf_host_on_demand` 先与用户确认预算 + 起 host。

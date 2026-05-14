# 阶段 4 API 规范

## 文档地位

本文档锁定阶段 4（6-max NLHE Blueprint 训练）公开的 Rust API surface（trait / struct / enum / 公开方法签名）。一旦 commit，后续步骤（A1 / B1 / B2 / ... / F3）的所有 agent 必须严格按此签名实现 / 测试。

任何 API 修改必须：
1. 在本文档以 `API-NNN-revM` 形式追加新条目（不删除原条目）
2. 必要时 bump `Checkpoint.schema_version`（API-440 stage 4 schema_version=2，stage 3 schema_version=1 → stage 4 schema_version=2 升级 — 不向前兼容）或 `HandHistory.schema_version`（继承 stage 1 D-101）或 `BucketTable.schema_version`（继承 stage 2 D-240）或 stage 2 `InfoSetId` 64-bit layout（API-423 InfoSet bit 扩展 14-action mask）
3. 同步更新 `tests/api_signatures.rs`（trip-wire），否则 `cargo test --no-run` fail
4. 通知所有正在工作的 agent

阶段 4 API 编号从 **API-400** 起，与 stage 1（API-001..API-013）+ stage 2（API-200..API-302）+ stage 3（API-300..API-392）不冲突。stage 1 + stage 2 + stage 3 API surface 作为只读契约继承到 stage 4，未在本文档覆盖部分以 `pluribus_stage1_api.md` + `pluribus_stage2_api.md` + `pluribus_stage3_api.md` 为准。

---

## 1. EsMccfrTrainer 扩展（Linear MCCFR + RM+，`module: training::trainer`）

### `EsMccfrTrainer::with_linear_rm_plus` builder（API-400）

stage 3 `EsMccfrTrainer` builder 扩展：通过 `with_linear_rm_plus()` 方法切到 stage 4 Linear MCCFR + RM+ 模式。Stage 3 字面 `EsMccfrTrainer::new(...)` 路径维持 stage 3 standard CFR + RM 不变；stage 4 路径**显式**通过 builder 切入。

```rust
impl<G: Game> EsMccfrTrainer<G> {
    /// stage 4 D-400 Linear MCCFR + RM+ 模式（D-401 Linear discounting + D-402 RM+ clamp + D-403 Linear weighted strategy sum）
    /// stage 3 `EsMccfrTrainer::new(...)` 返回的 trainer 调用本方法切到 stage 4 模式
    /// 切换后 update_count 维持，后续 step() 走 stage 4 路径
    /// warm-up phase（D-409）按 `warmup_complete_at` 配置自动切换 stage 3 → stage 4 路径
    pub fn with_linear_rm_plus(mut self, warmup_complete_at: u64) -> Self {
        self.config.linear_weighting_enabled = true;
        self.config.rm_plus_enabled = true;
        self.config.warmup_complete_at = warmup_complete_at;
        self
    }
}
```

**API-400 不变量**：
- `with_linear_rm_plus()` 切换之前累积的 regret / strategy_sum 保留不动（warmup phase 1M update 走 stage 3 standard CFR + RM 路径 byte-equal 保持，**stage 3 BLAKE3 anchor 1M update × 3 不变量在 stage 4 warmup phase 必须重现一致**）。
- 切换后下一次 `step()` 起触发 D-409 warm-up phase 检查：`update_count < warmup_complete_at` 走 stage 3 路径，`update_count >= warmup_complete_at` 走 stage 4 路径。
- **Deterministic 切换边界**：切换点 update_count = `warmup_complete_at` 的那一个 step 必须 byte-equal across multiple runs。`warmup_complete` 状态进 checkpoint header（API-440 字段，D-446 字面）。

### `TrainerConfig` 扩展（API-401）

```rust
pub struct TrainerConfig {
    // stage 3 字段继承
    pub n_threads: u8,
    pub checkpoint_interval: u64,
    pub metrics_interval: u64,

    // stage 4 新增字段
    pub linear_weighting_enabled: bool,    // D-401 Linear discounting on/off
    pub rm_plus_enabled: bool,             // D-402 RM+ clamp on/off
    pub warmup_complete_at: u64,           // D-409 warm-up phase 长度（默认 1_000_000）
    pub decay_strategy: DecayStrategy,     // D-401-revM eager / lazy decay 选型
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum DecayStrategy {
    /// stage 4 A0 默认 — 每 iter 起始扫全表应用 decay factor；性能开销估计 ~30% 单线程
    EagerDecay,
    /// stage 4 D-401-revM 候选 — 每 entry 存 (value, last_update_count_t) tuple；query 时延迟应用
    /// 实现复杂度高但 throughput 收益 30%+；B2 [实现] 起步前 evaluate
    LazyDecay,
}
```

### `current_strategy` / `average_strategy` query 接口（API-402）

stage 4 RegretTable / StrategyAccumulator query 走 stage 3 D-328 路径。Linear weighting + RM+ 在 trainer step() 内部 in-place 应用，query 接口签名不变（继承 stage 3 API-320 / API-321）。

```rust
// 继承 stage 3 API-320 不变
impl<G: Game> RegretTable<G> {
    pub fn current_strategy(&self, info_set: &G::InfoSet, n_actions: usize) -> Vec<f64>;
}
impl<G: Game> StrategyAccumulator<G> {
    pub fn average_strategy(&self, info_set: &G::InfoSet, n_actions: usize) -> Vec<f64>;
}
```

### `Trainer::current_strategy_for_traverser`（API-403，6-traverser routing）

```rust
impl<G: Game> Trainer<G> for EsMccfrTrainer<G> {
    // 继承 stage 3 trait surface

    /// stage 4 新增 — 6-traverser routing（D-412 / D-414）
    /// `traverser` ∈ [0, n_players)，返回该 traverser 视角下的 current strategy
    /// Kuhn / Leduc / SimplifiedNlheGame 路径不实现（n_players = 2 时 traverser=0/1 走 stage 3 路径）
    /// NlheGame6 必须实现
    fn current_strategy_for_traverser(
        &self,
        traverser: PlayerId,
        info_set: &G::InfoSet,
    ) -> Vec<f64> {
        // default impl: route to stage 3 single-traverser path（n_players = 2 时退化）
        self.current_strategy(info_set)
    }

    /// stage 4 新增 — 6-traverser average strategy routing
    fn average_strategy_for_traverser(
        &self,
        traverser: PlayerId,
        info_set: &G::InfoSet,
    ) -> Vec<f64> {
        self.average_strategy(info_set)
    }
}
```

---

## 2. NlheGame6（`module: training::nlhe_6max`）

### `NlheGame6` struct（API-410）

stage 3 `Game` trait 的第 4 个 impl（继承 KuhnGame / LeducGame / SimplifiedNlheGame）。

```rust
pub struct NlheGame6 {
    bucket_table: Arc<BucketTable>,              // stage 2 BucketTable（D-424 v3 production artifact）
    action_abstraction: PluribusActionAbstraction, // stage 4 14-action（D-420）
    config: TableConfig,                         // stage 1 TableConfig（n_seats=6 + 100 BB starting stack）
}

impl NlheGame6 {
    /// stage 4 lock D-424 — bucket_table 必须是 v3 production artifact (BLAKE3 = 67ee5554...)
    /// schema_version 必须 = 2（stage 2 D-244 字面）
    pub fn new(bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        // 校验 BucketTable.schema_version == 2 (D-424)
        // 校验 BucketTable.cluster_config == (500, 500, 500)
        // 校验 bucket_table_blake3 == expected v3 anchor
        // ...
    }
}

// Action 类型：14-action AbstractAction（D-420，复用 stage 2 trait + 新增 PluribusActionAbstraction impl）
pub type NlheGame6Action = stage2::AbstractAction;

// InfoSet 类型：继承 stage 2 64-bit InfoSetId + D-423 14-action mask 扩展
// API-423 lock 复用 stage 2 IA-007 reserved 14 bits 区域
pub type NlheGame6InfoSet = stage2::InfoSetId;

impl Game for NlheGame6 {
    type State = NlheGame6State;
    type Action = NlheGame6Action;
    type InfoSet = NlheGame6InfoSet;

    // D-411 GameVariant 新枚举值
    const VARIANT: GameVariant = GameVariant::Nlhe6Max;

    fn n_players(&self) -> usize { 6 } // D-410 6-player NLHE

    fn bucket_table_blake3(&self) -> [u8; 32] {
        self.bucket_table.content_hash()
    }

    // ... 其它 trait method
}

pub struct NlheGame6State {
    pub game_state: stage1::GameState,             // stage 1 GameState n_seats=6
    pub action_history: smallvec::SmallVec<[NlheGame6Action; 32]>,
}
```

### `GameVariant` enum 扩展（API-411）

stage 3 `GameVariant` enum 追加第 4 个变体（继承 stage 3 D-373-rev1 enum tag 模式）：

```rust
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
#[repr(u8)]
pub enum GameVariant {
    Kuhn = 0,
    Leduc = 1,
    SimplifiedNlhe = 2,
    Nlhe6Max = 3,    // stage 4 新增（API-411）
}

impl GameVariant {
    pub fn from_u8(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Kuhn),
            1 => Some(Self::Leduc),
            2 => Some(Self::SimplifiedNlhe),
            3 => Some(Self::Nlhe6Max),  // stage 4 新增
            _ => None,
        }
    }
}
```

### 6-traverser routing（API-412）

```rust
impl NlheGame6 {
    /// stage 4 D-412 — 6-traverser alternating；返回 iter t 上的 traverser index
    pub fn traverser_at_iter(t: u64) -> PlayerId {
        (t % 6) as PlayerId
    }

    /// stage 4 D-412 多线程并发 — base_update_count + tid 路由
    pub fn traverser_for_thread(base_update_count: u64, tid: usize) -> PlayerId {
        ((base_update_count + tid as u64) % 6) as PlayerId
    }
}
```

### `NlheGame6` 与 `SimplifiedNlheGame` 退化路径（API-413）

stage 4 HU 退化路径走 `NlheGame6::new(bucket_table)` 配 `n_seats=2`：

```rust
impl NlheGame6 {
    /// stage 4 D-416 — HU 退化路径 (n_seats=2)
    /// stage 3 SimplifiedNlheGame 路径上的 BLAKE3 anchor 必须 byte-equal 维持
    /// （stage 3 1M update × 3 BLAKE3 anchor 在 stage 4 commit 必须不退化）
    pub fn new_hu(bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        let mut config = TableConfig::default();
        config.n_seats = 2;  // D-416 HU 退化
        Self::with_config(bucket_table, config)
    }

    pub fn with_config(bucket_table: Arc<BucketTable>, config: TableConfig) -> Result<Self, TrainerError> {
        // ...
    }
}
```

---

## 3. PluribusActionAbstraction（`module: abstraction::action_pluribus`）

### `PluribusActionAbstraction` struct（API-420）

stage 2 `ActionAbstraction` trait 的第 2 个 impl（继承 stage 2 `DefaultActionAbstraction` 5-action 作为 ablation baseline）。

```rust
pub struct PluribusActionAbstraction;

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum PluribusAction {
    Fold = 0,
    Check = 1,
    Call = 2,
    Raise05Pot = 3,    // 0.5 × pot
    Raise075Pot = 4,   // 0.75 × pot
    Raise1Pot = 5,     // 1 × pot
    Raise15Pot = 6,    // 1.5 × pot
    Raise2Pot = 7,     // 2 × pot
    Raise3Pot = 8,     // 3 × pot
    Raise5Pot = 9,     // 5 × pot
    Raise10Pot = 10,   // 10 × pot
    Raise25Pot = 11,   // 25 × pot
    Raise50Pot = 12,   // 50 × pot
    AllIn = 13,        // 14th action
}

impl PluribusAction {
    /// stage 4 D-420 Action enumeration 固定顺序
    pub const N_ACTIONS: usize = 14;

    /// 14-action 集合迭代 — 固定顺序（D-420 deterministic order）
    pub fn all() -> [Self; 14] {
        [
            Self::Fold, Self::Check, Self::Call,
            Self::Raise05Pot, Self::Raise075Pot, Self::Raise1Pot,
            Self::Raise15Pot, Self::Raise2Pot, Self::Raise3Pot,
            Self::Raise5Pot, Self::Raise10Pot, Self::Raise25Pot, Self::Raise50Pot,
            Self::AllIn,
        ]
    }

    /// raise multiplier ∈ [0.5, 0.75, 1, 1.5, 2, 3, 5, 10, 25, 50]; non-raise = None
    pub fn raise_multiplier(self) -> Option<f64> {
        match self {
            Self::Raise05Pot => Some(0.5),
            Self::Raise075Pot => Some(0.75),
            Self::Raise1Pot => Some(1.0),
            Self::Raise15Pot => Some(1.5),
            Self::Raise2Pot => Some(2.0),
            Self::Raise3Pot => Some(3.0),
            Self::Raise5Pot => Some(5.0),
            Self::Raise10Pot => Some(10.0),
            Self::Raise25Pot => Some(25.0),
            Self::Raise50Pot => Some(50.0),
            _ => None,
        }
    }
}

impl stage2::ActionAbstraction for PluribusActionAbstraction {
    type Action = PluribusAction;

    fn actions(&self, state: &stage1::GameState) -> Vec<Self::Action> {
        // D-420：基于 state 的 legal 14-action 子集
        // D-422：raise size 走 stage 1 GameState::apply byte-equal 验证
        let mut legal = Vec::with_capacity(14);
        for action in PluribusAction::all() {
            if self.is_legal(&action, state) {
                legal.push(action);
            }
        }
        legal
    }

    fn n_actions(&self) -> usize { PluribusAction::N_ACTIONS }
}
```

### `legal_actions` 子集计算（API-421）

```rust
impl PluribusActionAbstraction {
    /// D-420 + D-422：判定 PluribusAction 在 state 上是否 legal
    /// 走 stage 1 GameState 的 betting state + stack / pot 计算
    pub fn is_legal(&self, action: &PluribusAction, state: &stage1::GameState) -> bool {
        match action {
            PluribusAction::Fold => state.can_fold(),
            PluribusAction::Check => state.can_check(),
            PluribusAction::Call => state.can_call(),
            PluribusAction::AllIn => state.can_all_in(),
            action if action.raise_multiplier().is_some() => {
                let mult = action.raise_multiplier().unwrap();
                let raise_to = self.compute_raise_to(state, mult);
                state.can_raise_to(raise_to)
            }
            _ => unreachable!(),
        }
    }

    /// D-420 raise size 计算：raise_to = current_bet + multiplier × pot_size
    /// 不满足 min raise（stage 1 D-033）的 raise size legal_actions 内自动剔除
    /// 超过 stack 的 raise size 自动转 all_in（与 stage 1 D-022 字面继承）
    fn compute_raise_to(&self, state: &stage1::GameState, multiplier: f64) -> ChipAmount {
        let pot = state.pot_size();
        let current_bet = state.current_bet();
        let raise_delta = ChipAmount::from_f64((pot.0 as f64 * multiplier) as u64);
        current_bet + raise_delta
    }
}
```

### `legal_actions` mask 编码（API-423）

InfoSetId 14-bit availability mask（D-423 lock 复用 stage 2 IA-007 reserved 14 bits）：

```rust
pub struct InfoSetId {
    inner: u64,
}

// 64-bit layout (stage 2 D-218 继承 + stage 4 D-423 mask 区域 lock)：
//
// bits 0..6:    actor       (6-bit, stage 2)
// bits 6..12:   street_tag  (6-bit, stage 2)
// bits 12..18:  stage 3 D-317-rev1 6-bit mask (deprecated in stage 4 NlheGame6 路径)
// bits 12..33:  bucket_id   (21-bit, stage 2)
// bits 33..63:  stage 2 IA-007 reserved → stage 4 D-423 14-bit mask 占 bits 33..47 + reserved 16 bits
// bit  63:      mode_flag   (stage 2)
//
// stage 4 NlheGame6 路径：bits 33..47 = 14-action availability mask（D-423 lock）
// SimplifiedNlheGame 路径：bits 12..18 = 6-bit mask（stage 3 D-317-rev1 维持）

impl InfoSetId {
    /// stage 4 API-423 — 14-action mask 设置
    pub fn with_14action_mask(self, mask: u16) -> Self {
        debug_assert!(mask < (1 << 14));
        let cleared = self.inner & !(0x3FFFu64 << 33);
        Self { inner: cleared | ((mask as u64) << 33) }
    }

    pub fn legal_actions_mask_14(&self) -> u16 {
        ((self.inner >> 33) & 0x3FFF) as u16
    }
}
```

---

## 4. 6-traverser RegretTable / StrategyAccumulator（`module: training::regret`）

### `RegretTable` 6-traverser 扩展（API-430）

```rust
/// stage 4 EsMccfrTrainer 持有 6 套独立 RegretTable（D-412 + D-438）
/// 单 trainer 内部数组 [RegretTable<G>; 6]，编译期固定 6 长度
impl<G: Game> EsMccfrTrainer<G> {
    /// stage 4 6-traverser 内部 storage
    /// pub(crate) since stage 4 trainer 内部实现细节，外部走 API-403 routing
    pub(crate) fn regret_tables(&self) -> &[RegretTable<G>; 6] {
        &self.regret_tables
    }

    pub(crate) fn strategy_accumulators(&self) -> &[StrategyAccumulator<G>; 6] {
        &self.strategy_accumulators
    }
}

// RegretTable 接口签名继承 stage 3 API-320 不变
// 在 stage 4 NlheGame6 路径上 6 套 RegretTable 各自独立
```

### `RegretTable::into_inner` 6-traverser 扩展（API-431）

stage 3 `RegretTable::into_inner` 用于 thread-local merge（继承 stage 3 D-321-rev2 batch merge 模式扩展到 6-traverser）：

```rust
impl<G: Game> RegretTable<G> {
    /// stage 4 多线程 thread-local accumulator merge 入口（继承 stage 3 D-321-rev2 模式）
    /// 多线程并发场景下每 thread 持有独立 RegretTable[traverser]，scope 结束后 main thread merge
    pub(crate) fn into_inner(self) -> HashMap<G::InfoSet, Vec<f64>> {
        self.inner
    }
}
```

### `RegretTable::peak_rss_bytes`（API-432）

```rust
impl<G: Game> RegretTable<G> {
    /// stage 4 D-431 RSS 监控 — 当前 RegretTable 内部 HashMap 估算字节占用
    /// 用于 TrainingMetrics::peak_rss_bytes 累计（API-470）
    pub fn estimated_memory_bytes(&self) -> u64 {
        let n_entries = self.inner.len() as u64;
        let bytes_per_entry = std::mem::size_of::<G::InfoSet>() as u64
            + (PluribusAction::N_ACTIONS as u64 * 8); // 14 × f64 = 112 byte
        n_entries * (bytes_per_entry + 24) // HashMap overhead estimate
    }
}
```

---

## 5. Checkpoint v2 Schema（`module: training::checkpoint`）

### `Checkpoint` v2 binary layout（API-440）

stage 4 schema_version=2，128-byte header（stage 3 96-byte + 32-byte 扩展）：

```text
Checkpoint v2 binary layout (128-byte header + bincode body + 32-byte trailer)：

Offset  Size  Field              Content
------  ----  ----------------   ----------------------------------------------
0       8     magic              b"PLCKPT\0\0"
8       4     schema_version     u32 = 2 (stage 4 升级 stage 3 = 1，不向前兼容)
12      1     trainer_variant    u8 ∈ {0=VanillaCFR, 1=ESMccfr, 2=ESMccfrLinearRmPlus}
13      1     game_variant       u8 ∈ {0=Kuhn, 1=Leduc, 2=SimplifiedNlhe, 3=Nlhe6Max}
14      1     traverser_count    u8 = 1 (stage 3) / 6 (stage 4 NlheGame6)
15      1     linear_weighting   u8 ∈ {0=off, 1=on}
16      1     rm_plus            u8 ∈ {0=off, 1=on}
17      1     warmup_complete    u8 ∈ {0=in_warmup, 1=complete}
18      6     pad_a              all-zero
24      8     update_count       u64
32      32    rng_state          ChaCha20 state, 继承 stage 1 D-228
64      32    bucket_table_blake3 BucketTable content_hash (Kuhn/Leduc = all-zero)
96      8     regret_offset      u64 — bincode-serialized regret_tables 起始偏移
104     8     strategy_offset    u64 — bincode-serialized strategy_accumulators 起始偏移
112     16    pad_b              all-zero
128     N     body               bincode-serialized [RegretTable<G>; T] + [StrategyAccumulator<G>; T]
                                 where T = traverser_count (1 or 6)
N+128   32    trailer_blake3     BLAKE3(file[0..N+128])
```

### `Checkpoint` 字段扩展（API-441）

```rust
pub struct Checkpoint {
    // stage 3 字段继承
    pub schema_version: u32,                    // stage 4 = 2
    pub trainer_variant: TrainerVariant,
    pub game_variant: GameVariant,
    pub update_count: u64,
    pub rng_state: [u8; 32],
    pub bucket_table_blake3: [u8; 32],

    // stage 4 新增字段
    pub traverser_count: u8,                    // 1 (stage 3) / 6 (stage 4 NlheGame6)
    pub linear_weighting_enabled: bool,
    pub rm_plus_enabled: bool,
    pub warmup_complete: bool,
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
#[repr(u8)]
pub enum TrainerVariant {
    VanillaCFR = 0,
    ESMccfr = 1,
    ESMccfrLinearRmPlus = 2,    // stage 4 新增（D-449）
}
```

### `Checkpoint::save_v2` 实现（API-442）

```rust
impl Checkpoint {
    /// stage 4 D-353 atomic write-to-temp + rename
    /// stage 3 Checkpoint::save 路径维持，stage 4 新增 schema_version=2 codec
    pub fn save_v2<G: Game>(
        &self,
        regret_tables: &[RegretTable<G>; 6],
        strategy_accumulators: &[StrategyAccumulator<G>; 6],
        path: &Path,
    ) -> Result<(), CheckpointError> {
        // 1. open <path>.tmp
        // 2. write 128-byte header
        // 3. bincode-serialize 6 RegretTable + 6 StrategyAccumulator
        // 4. compute trailer BLAKE3
        // 5. fsync
        // 6. rename <path>.tmp → <path>
        // ...
    }

    /// stage 3 Checkpoint::open 路径扩展 — 自动 dispatch schema_version 1 vs 2
    pub fn open<G: Game>(path: &Path) -> Result<(Self, [RegretTable<G>; 6], [StrategyAccumulator<G>; 6]), CheckpointError> {
        // 1. read 8-byte magic
        // 2. read 4-byte schema_version → dispatch v1 (stage 3) / v2 (stage 4)
        // 3. v1 path:  return stage 3 single-traverser arrays wrapped in [_; 6] with traverser_count=1
        // 4. v2 path:  full 6-traverser parse
        // ...
    }
}
```

### `CheckpointError` 扩展（API-443）

stage 3 `CheckpointError` 5 variant 继承不变；stage 4 不新增 variant（schema_version mismatch 走既有 `SchemaMismatch`，trainer_variant mismatch 走既有 `TrainerMismatch`）。

```rust
// stage 3 CheckpointError 5 variant 继承不变（API-313 stage 3 字面）
// stage 4 字面继承，不扩展
```

---

## 6. LBR Evaluator（`module: training::lbr`）

### `LbrEvaluator` struct（API-450）

stage 4 新增（D-450 / D-453 Rust 自实现）：

```rust
pub struct LbrEvaluator<G: Game> {
    trainer: Arc<EsMccfrTrainer<G>>,
    action_set_size: usize,     // 14 for NlheGame6 (D-456 14-action)
    myopic_horizon: u8,         // 1 (D-455 lock myopic horizon = 1)
}

impl<G: Game> LbrEvaluator<G> {
    /// stage 4 D-450 — 创建 LBR evaluator 对一个特定 traverser
    pub fn new(
        trainer: Arc<EsMccfrTrainer<G>>,
        action_set_size: usize,
        myopic_horizon: u8,
    ) -> Result<Self, TrainerError> {
        if action_set_size != 14 && action_set_size != 5 {
            return Err(TrainerError::PreflopActionAbstractionMismatch);
        }
        Ok(Self { trainer, action_set_size, myopic_horizon })
    }

    /// stage 4 D-452 — 对一个 LBR-player 计算 1000 hand 上的 LBR 上界 mbb/g
    /// `lbr_player` ∈ [0, n_players)
    /// `n_hands` 通常 1000（D-452）
    /// `rng` 显式注入（继承 stage 1 D-027 / D-050）
    pub fn compute(
        &self,
        lbr_player: PlayerId,
        n_hands: u64,
        rng: &mut dyn RngSource,
    ) -> Result<LbrResult, TrainerError> {
        // 1. for each hand:
        //    a. deal state
        //    b. for each lbr_player decision point:
        //       - enumerate 14 actions
        //       - for each action: compute EV (assuming blueprint after this point)
        //       - take max EV action
        //    c. lbr_value_hand = max EV across decision points
        // 2. return (mean(lbr_value_hands), standard_error(lbr_value_hands))
        // ...
    }

    /// stage 4 D-459 — 6-traverser average LBR
    /// 计算每 traverser 的 LBR mbb/g（D-414 6 traverser 独立），返回 average
    pub fn compute_six_traverser_average(
        &self,
        n_hands_per_traverser: u64,
        rng: &mut dyn RngSource,
    ) -> Result<SixTraverserLbrResult, TrainerError> {
        // ...
    }
}
```

### `LbrResult` struct（API-451）

```rust
pub struct LbrResult {
    pub lbr_player: PlayerId,
    pub lbr_value_mbbg: f64,             // LBR upper bound (mbb/g)
    pub standard_error_mbbg: f64,
    pub n_hands: u64,
    pub computation_seconds: f64,
}

pub struct SixTraverserLbrResult {
    pub per_traverser: [LbrResult; 6],
    pub average_mbbg: f64,
    pub max_mbbg: f64,                   // 6-traverser 最大 LBR（D-459 §carve-out 锚点）
    pub min_mbbg: f64,
}
```

### `lbr_compute` CLI 入口（API-452）

```rust
// tools/lbr_compute.rs
/// CLI: cargo run --release --bin lbr_compute -- \
///     --checkpoint PATH --n-hands 1000 --traverser 0 --rng-seed S
fn main() -> Result<(), Box<dyn Error>> {
    // ...
}
```

### OpenSpiel LBR sanity check（API-457）

```rust
impl<G: Game> LbrEvaluator<G> {
    /// stage 4 D-457 — F3 [报告] 一次性接入 OpenSpiel `algorithms/exploitability_descent.py` 对照
    /// 输出 OpenSpiel-compatible policy 到 `path`，由 Python script 消费
    pub fn export_policy_for_openspiel(&self, path: &Path) -> Result<(), TrainerError> {
        // ...
    }
}
```

---

## 7. Slumbot Bridge & Head-to-Head Evaluation（`module: training::slumbot_eval`）

### `SlumbotBridge` struct（API-460）

stage 4 D-460 / D-461 HU NLHE 评测对手 bridge：

```rust
pub struct SlumbotBridge {
    http_client: reqwest::blocking::Client,
    api_endpoint: String,           // "http://www.slumbot.com/api/..."
    api_key: Option<String>,        // 可选，若 Slumbot 需要
    timeout: std::time::Duration,
}

impl SlumbotBridge {
    pub fn new(api_endpoint: String) -> Self {
        // ...
    }

    /// stage 4 D-460 — 单 hand 评测：blueprint 与 Slumbot 对战 1 手
    /// blueprint 视角 mbb 净收益（duplicate dealing 走 D-461 协议）
    pub fn play_one_hand(
        &mut self,
        blueprint: &impl Trainer<NlheGame6>,
        seed: u64,
    ) -> Result<SlumbotHandResult, TrainerError> {
        // 1. POST /new_hand to Slumbot
        // 2. 协议双向：blueprint act → POST blueprint_action → Slumbot reply → blueprint act ...
        // 3. terminal: Slumbot 返回 outcome
        // 4. mbb 净收益 = chip_delta / 1.0 (BB unit) × 1000
        // ...
    }
}
```

### `Head2HeadResult` struct（API-461）

```rust
pub struct Head2HeadResult {
    pub mean_mbbg: f64,
    pub standard_error_mbbg: f64,
    pub confidence_interval_95: (f64, f64),  // (low, high)
    pub n_hands: u64,
    pub duplicate_dealing: bool,             // D-461 duplicate dealing on/off
    pub blueprint_seed: u64,
    pub wall_clock_seconds: f64,
}
```

### `evaluate_vs_slumbot` 评测入口（API-462）

```rust
impl SlumbotBridge {
    /// stage 4 D-461 — 100K 手评测（D-460 协议 + duplicate dealing + 重复 5 次 mean）
    pub fn evaluate_blueprint(
        &mut self,
        blueprint: &impl Trainer<NlheGame6>,
        n_hands: u64,                // 通常 100_000
        master_seed: u64,            // D-468 master seed
        duplicate_dealing: bool,
    ) -> Result<Head2HeadResult, TrainerError> {
        // ...
    }
}
```

### Slumbot 不可用 fallback（API-463）

```rust
/// stage 4 D-463-revM — Slumbot API 不可用时 fallback 到 OpenSpiel-trained HU baseline
/// 决策时机：F2 [实现] 起步前评估，A0 lock 主路径 Slumbot 在线
pub struct OpenSpielHuBaseline {
    policy_path: PathBuf,
}

impl OpenSpielHuBaseline {
    pub fn play_one_hand(
        &mut self,
        blueprint: &impl Trainer<NlheGame6>,
        seed: u64,
    ) -> Result<HuHandResult, TrainerError> {
        // ...
    }
}
```

---

## 8. Monitoring Metrics（`module: training::metrics`）

### `TrainingMetrics` struct（API-470）

stage 4 D-470 / D-471 / D-472 / D-473 监控接口：

```rust
pub struct TrainingMetrics {
    pub update_count: u64,
    pub wall_clock_seconds: f64,

    // D-470 average regret growth rate
    pub avg_regret_growth_rate: f64,         // max_I R̃_t(I) / sqrt(T)
    pub regret_growth_trend_up_count: u8,    // 连续多少个采样点呈 trend up（≥5 = P0 告警）

    // D-471 策略 entropy
    pub policy_entropy: f64,                  // H(σ_t) averaged over reachable InfoSets

    // D-472 动作概率震荡幅度
    pub policy_oscillation: f64,              // Σ |σ_t - σ_{t-10⁵}|

    // D-431 RSS 监控
    pub peak_rss_bytes: u64,

    // D-478 EV sanity check
    pub ev_sum_residual: f64,                 // |Σ_traverser EV(traverser)| (zero-sum check)

    // alarms
    pub last_alarm: Option<TrainingAlarm>,
}
```

### `TrainingAlarm` enum（API-471）

```rust
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum TrainingAlarm {
    /// D-470 — average regret growth trend up ≥ 5 个采样点（P0 阻塞）
    RegretGrowthTrendUp { trend_up_count: u8, last_sample_t: u64 },
    /// D-471 — entropy 回升 ≥ 5% 连续 3 采样点（warn）
    EntropyRising { delta_pct: f64 },
    /// D-472 — oscillation 增加 ≥ 5 采样点（warn）
    OscillationTrendUp,
    /// D-431 — RSS 超 limit（P0 阻塞）
    OutOfMemory { rss_bytes: u64, limit_bytes: u64 },
    /// D-478 — EV sum residual 超容差（P0 阻塞）
    EvSumViolation { residual: f64, tolerance: f64 },
}
```

### `EsMccfrTrainer::metrics`（API-472）

```rust
impl<G: Game> EsMccfrTrainer<G> {
    /// stage 4 D-473 — 公开 read-only metrics 接口
    /// trainer 不主动 abort；CLI / 用户根据 metrics 决策
    pub fn metrics(&self) -> &TrainingMetrics {
        &self.metrics
    }
}
```

### `MetricsCollector` 内部状态（API-473）

```rust
pub(crate) struct MetricsCollector {
    last_avg_regret: f64,
    last_entropy: f64,
    last_strategy_snapshot: HashMap<InfoSetId, Vec<f64>>,
    history_of_regret_growth: smallvec::SmallVec<[f64; 16]>,
    sample_interval: u64,
    last_sample_t: u64,
}

impl MetricsCollector {
    /// 每 sample_interval（D-476 默认 10⁵）update 调用一次，更新 TrainingMetrics 字段
    pub fn observe(
        &mut self,
        trainer: &impl Trainer<NlheGame6>,
        rng: &mut dyn RngSource,
        metrics: &mut TrainingMetrics,
    ) -> Result<(), TrainerError> {
        // ...
    }
}
```

### JSONL log 输出（API-474）

```rust
/// stage 4 D-474 — JSONL 行格式训练日志
/// 每 10⁵ update 一行 JSON 写入 --log-file PATH（默认 stdout）
pub fn write_metrics_jsonl<W: io::Write>(
    writer: &mut W,
    metrics: &TrainingMetrics,
) -> io::Result<()> {
    serde_json::to_writer(writer, metrics)?;
    writeln!(writer)?;
    Ok(())
}
```

---

## 9. Baseline Opponents（`module: training::baseline_eval`）

### `Opponent6Max` trait（API-480）

stage 4 D-480 / D-483 — 3 类 baseline opponent 实现该 trait：

```rust
pub trait Opponent6Max {
    /// stage 4 D-483 — decision point 上选 1 个 AbstractAction
    /// `rng` 显式注入（继承 stage 1 D-027 / D-050）
    fn act(
        &mut self,
        state: &stage1::GameState,
        legal_actions: &[PluribusAction],
        rng: &mut dyn RngSource,
    ) -> PluribusAction;

    /// 标识 baseline 名称（用于 eval result 输出）
    fn name(&self) -> &'static str;
}
```

### `RandomOpponent` impl（API-481）

```rust
pub struct RandomOpponent;

impl Opponent6Max for RandomOpponent {
    fn act(&mut self, _state: &stage1::GameState, legal_actions: &[PluribusAction], rng: &mut dyn RngSource) -> PluribusAction {
        let idx = (rng.next_u64() as usize) % legal_actions.len();
        legal_actions[idx]
    }

    fn name(&self) -> &'static str { "random" }
}
```

### `CallStationOpponent` impl（API-482）

```rust
pub struct CallStationOpponent;

impl Opponent6Max for CallStationOpponent {
    fn act(&mut self, _state: &stage1::GameState, legal_actions: &[PluribusAction], rng: &mut dyn RngSource) -> PluribusAction {
        // D-480 ② — 99% call/check, 1% random (avoid 死局)
        let dice = rng.next_u64() % 100;
        if dice < 1 {
            let idx = (rng.next_u64() as usize) % legal_actions.len();
            return legal_actions[idx];
        }
        // 优先 call / check
        if legal_actions.contains(&PluribusAction::Call) {
            return PluribusAction::Call;
        }
        if legal_actions.contains(&PluribusAction::Check) {
            return PluribusAction::Check;
        }
        // 都不可，fold
        PluribusAction::Fold
    }

    fn name(&self) -> &'static str { "call_station" }
}
```

### `TagOpponent` impl（API-483）

```rust
pub struct TagOpponent {
    preflop_top_range_pct: u8,   // D-480 ③ 默认 20% top range
    postflop_cbet_pct: u8,       // 默认 70% c-bet rate
}

impl Opponent6Max for TagOpponent {
    fn act(&mut self, state: &stage1::GameState, legal_actions: &[PluribusAction], rng: &mut dyn RngSource) -> PluribusAction {
        // D-480 ③ — preflop top 20% raise，其它 fold；postflop 70% c-bet
        // ...
    }

    fn name(&self) -> &'static str { "tag" }
}
```

### `evaluate_vs_baseline` 评测入口（API-484）

```rust
/// stage 4 D-481 — 1M 手 baseline sanity 评测
pub fn evaluate_vs_baseline<G, O>(
    blueprint: &impl Trainer<G>,
    opponent: &mut O,
    n_hands: u64,                 // 通常 1_000_000
    master_seed: u64,
    rng: &mut dyn RngSource,
) -> Result<BaselineEvalResult, TrainerError>
where
    G: Game,
    O: Opponent6Max,
{
    // ...
}

pub struct BaselineEvalResult {
    pub mean_mbbg: f64,
    pub standard_error_mbbg: f64,
    pub n_hands: u64,
    pub opponent_name: String,
    pub blueprint_seats: Vec<usize>,    // 4 或 5 seats (D-481)
    pub opponent_seats: Vec<usize>,     // 2 或 1 seats
}
```

---

## 10. Performance SLO + Training CLI（`tools/train_cfr.rs` 扩展，`module: training::perf`）

### `train_cfr` CLI 扩展（API-490）

stage 4 D-372 stage 3 CLI 扩展：

```text
cargo run --release --bin train_cfr -- \
    --game {kuhn,leduc,nlhe-simplified,nlhe-6max} \
    --trainer {vanilla,es-mccfr,es-mccfr-linear-rm-plus} \
    --abstraction {default,pluribus-14} \
    --iter N \
    --seed S \
    --checkpoint-dir DIR \
    [--resume PATH] \
    [--checkpoint-every N] \
    [--keep-last N] \
    [--warmup-update-count N]         # stage 4 D-409，默认 1_000_000
    [--metrics-interval N]            # stage 4 D-476，默认 100_000
    [--log-file PATH]                 # stage 4 D-474 JSONL log
    [--alarm-dump-dir PATH]           # stage 4 D-479
    [--max-rss-bytes N]               # stage 4 D-431，默认 32 GiB
    [--abort-on-alarm {none,p0,all}]  # stage 4 D-473，trainer 不主动 abort；CLI 决定
```

### `PerfSloHarness` stage 4 扩展（API-491）

```rust
// tests/perf_slo.rs::stage4_*
// stage 4 D-490 SLO harness — 继承 stage 1 / 2 / 3 模式

/// stage 4 D-490 — 单线程 release ≥ 5K update/s（NlheGame6 Linear MCCFR + RM+）
#[test]
#[ignore]
fn stage4_nlhe_6max_single_thread_throughput_ge_5k_update_per_s() {
    // ...
}

/// stage 4 D-490 — 4-core release ≥ 15K update/s
#[test]
#[ignore]
fn stage4_nlhe_6max_four_core_throughput_ge_15k_update_per_s() {
    // ...
}

/// stage 4 D-490 — 32-vCPU release ≥ 20K update/s
#[test]
#[ignore]
fn stage4_nlhe_6max_32vcpu_throughput_ge_20k_update_per_s() {
    // ...
}

/// stage 4 D-454 — LBR computation P95 < 30 s for 1000 hand × 6 traverser
#[test]
#[ignore]
fn stage4_lbr_computation_p95_under_30s() {
    // ...
}

/// stage 4 D-485 — baseline sanity 1-2 min wall time for 3 baseline × 3 seed
#[test]
#[ignore]
fn stage4_baseline_eval_under_2min() {
    // ...
}
```

### 24h continuous run harness（API-497）

```rust
// tests/training_24h_continuous.rs
// stage 4 D-461 / D-497 — 24h 连续运行 fuzz

#[test]
#[ignore]
fn stage4_six_max_24h_no_crash() {
    // D-461 — 24h wall time / 固定 seed / 每 10⁶ update metrics / 每 10⁸ update checkpoint
    // 验收：无 panic / NaN / inf / RSS 增量 < 5 GB / 全部 checkpoint round-trip BLAKE3 byte-equal
    // ...
}

/// stage 4 D-498 — nightly fuzz wrapper（连续 7 天，每天 1 次 24h run）
#[test]
#[ignore]
fn stage4_seven_day_nightly_fuzz_no_crash() {
    // ...
}
```

### `bench` stage 4 扩展（API-499）

```rust
// benches/stage4.rs
// stage 4 D-496 — 3 bench group

fn bench_nlhe_6max_es_mccfr_linear_rm_plus_update(c: &mut Criterion) { ... }
fn bench_lbr_compute_1000_hand(c: &mut Criterion) { ... }
fn bench_baseline_eval_1000_hand(c: &mut Criterion) { ... }

criterion_group!(stage4_bench,
    bench_nlhe_6max_es_mccfr_linear_rm_plus_update,
    bench_lbr_compute_1000_hand,
    bench_baseline_eval_1000_hand,
);
criterion_main!(stage4_bench);
```

---

## 11. 模块导出（`lib.rs` + `Cargo.toml`）

### `src/lib.rs` 顶层 re-export（API-498）

```rust
pub mod training {
    // stage 3 继承
    pub mod game;
    pub mod kuhn;
    pub mod leduc;
    pub mod nlhe;                  // SimplifiedNlheGame (stage 3)
    pub mod regret;
    pub mod trainer;
    pub mod sampling;
    pub mod best_response;
    pub mod checkpoint;

    // stage 4 新增
    pub mod nlhe_6max;             // NlheGame6 (D-410 / D-411)
    pub mod lbr;                   // LbrEvaluator (D-450 / D-453)
    pub mod slumbot_eval;          // SlumbotBridge (D-460 / D-461)
    pub mod baseline_eval;         // Opponent6Max + 3 impls (D-480 / D-483)
    pub mod metrics;               // TrainingMetrics (D-470 / D-471 / D-472)
}

pub mod abstraction {
    // stage 2 继承 + stage 4 新增 PluribusActionAbstraction
    pub mod action_pluribus;       // D-420 (14-action)
    // ... stage 2 既有 modules
}

// stage 3 既有 re-export 继承
pub use training::game::{Game, NodeKind, PlayerId, GameVariant};
pub use training::trainer::{Trainer, VanillaCfrTrainer, EsMccfrTrainer, TrainerConfig, TrainerError, TrainerVariant};
pub use training::regret::{RegretTable, StrategyAccumulator};
pub use training::checkpoint::{Checkpoint, CheckpointError};

// stage 4 新增 re-export
pub use training::nlhe_6max::{NlheGame6, NlheGame6State, NlheGame6Action, NlheGame6InfoSet};
pub use training::lbr::{LbrEvaluator, LbrResult, SixTraverserLbrResult};
pub use training::slumbot_eval::{SlumbotBridge, Head2HeadResult, OpenSpielHuBaseline};
pub use training::baseline_eval::{Opponent6Max, RandomOpponent, CallStationOpponent, TagOpponent, BaselineEvalResult};
pub use training::metrics::{TrainingMetrics, TrainingAlarm};
pub use abstraction::action_pluribus::{PluribusActionAbstraction, PluribusAction};
```

### `Cargo.toml` stage 4 新增依赖（API-499，D-373-rev3）

```toml
[dependencies]
# stage 1 + stage 2 + stage 3 继承
blake3 = "1"
memmap2 = "0.9"
serde = { version = "1", features = ["derive"] }
bincode = "1.3"
tempfile = "3"
smallvec = "1"        # stage 3 E2-rev1 D-373-rev2

# stage 4 新增 — D-373-rev3 lock
rayon = "1"           # 真并发 ES-MCCFR (stage 3 E2-rev1 隐式依赖 → stage 4 显式)
reqwest = { version = "0.11", features = ["blocking", "json"] }   # D-463 Slumbot HTTP bridge
serde_json = "1"      # D-474 JSONL log

# stage 4 候选 evaluated in batch 2-4 / B2：
# fxhash = "0.2"      # D-430-revM FxHashMap 替代（A0 lock 不引入，B2 起步前 evaluate）
# tokio = { version = "1", features = ["rt", "macros"] }   # 若 Slumbot HTTP 需要 async（A0 lock blocking）
```

### `Cargo.toml` 新增 bin 入口（API-499 续）

```toml
[[bin]]
name = "train_cfr"           # stage 3 既有，stage 4 扩展 CLI flags（API-490）
path = "tools/train_cfr.rs"

[[bin]]
name = "lbr_compute"         # stage 4 新增（D-450 / API-452）
path = "tools/lbr_compute.rs"

[[bin]]
name = "eval_blueprint"      # stage 4 新增（D-461 / D-481 整合 Slumbot + baseline 评测）
path = "tools/eval_blueprint.rs"

[[bench]]
name = "stage4"              # stage 4 新增 bench group（D-496 / API-499）
harness = false
```

---

## 12. 与 stage 1 / 2 / 3 类型的桥接

### stage 1 `GameState` 桥接（API-492）

stage 4 `NlheGame6` 继承 stage 3 `SimplifiedNlheGame` 路径上 stage 1 `GameState` 桥接（API-390）扩展到 n_seats=6：

```rust
impl NlheGame6 {
    /// stage 4 D-410 — 走 stage 1 GameState n_seats=6 默认 multi-seat 分支
    pub fn root_state(&self, rng: &mut dyn RngSource) -> NlheGame6State {
        let game_state = stage1::GameState::new_with_config(&self.config, rng);
        NlheGame6State { game_state, action_history: SmallVec::new() }
    }

    /// stage 4 D-413 — actor_at_seat 桥接（trainer 内部 player_index 与 物理 SeatId 解耦）
    pub fn actor_at_seat(state: &NlheGame6State, seat_id: SeatId) -> PlayerId {
        // stage 1 GameState::actor_at_seat 桥接
        // ...
    }
}
```

### stage 2 `InfoSetId` 桥接（API-493）

```rust
impl NlheGame6 {
    /// stage 4 D-423 — 14-action mask 区域 bits 33..47 编码
    pub fn info_set(state: &NlheGame6State, actor: PlayerId) -> InfoSetId {
        let base = stage2::InfoSetId::from_state(&state.game_state, actor);
        let mask = Self::compute_14action_mask(&state.game_state);
        base.with_14action_mask(mask)
    }

    /// 14-action availability mask 计算（D-420 + D-423）
    fn compute_14action_mask(state: &stage1::GameState) -> u16 {
        // 14 bit per action availability
        // ...
    }
}
```

### stage 2 `ActionAbstraction` 桥接（API-494）

```rust
impl Game for NlheGame6 {
    fn legal_actions(state: &Self::State) -> Vec<Self::Action> {
        // D-420 — 走 PluribusActionAbstraction::actions(&state.game_state)
        let abstraction = PluribusActionAbstraction;
        abstraction.actions(&state.game_state)
            .into_iter()
            .map(NlheGame6Action::from)
            .collect()
    }
}
```

### stage 3 `EsMccfrTrainer` 桥接（API-495）

```rust
// stage 4 EsMccfrTrainer<NlheGame6> 内部数组扩展
// stage 3 EsMccfrTrainer<SimplifiedNlheGame> 路径 byte-equal 维持
// stage 4 路径 trainer.regret_tables: [RegretTable<NlheGame6>; 6]

impl EsMccfrTrainer<NlheGame6> {
    /// stage 4 D-412 + D-438 — 6-traverser routing
    pub fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        let traverser = (self.update_count % 6) as PlayerId;
        self.step_internal(traverser, rng)?;
        self.update_count += 1;
        self.maybe_observe_metrics(rng)?;
        Ok(())
    }

    pub fn step_parallel(&mut self, rngs: &mut [Box<dyn RngSource>], n_threads: u8) -> Result<(), TrainerError> {
        // D-412 多线程并发 — base_update_count + tid 路由
        // D-321-rev2 rayon thread pool + append-only delta merge 继承
        // ...
    }
}
```

---

## 13. 端到端示例（doc-test 占位）

### NlheGame6 Linear MCCFR + RM+ 1K iter（API-411 doc-test）

```rust
/// stage 4 端到端 doc-test — Linear MCCFR + RM+ on NlheGame6 1K iter
///
/// ```rust
/// use poker::training::{EsMccfrTrainer, NlheGame6, RegretTable};
/// use poker::abstraction::BucketTable;
/// use poker::rng::ChaCha20Rng;
/// use std::sync::Arc;
///
/// # let bucket_table = Arc::new(BucketTable::open_v3_anchor_for_test()?);
/// let game = NlheGame6::new(bucket_table)?;
/// let mut rng = ChaCha20Rng::from_seed([42u8; 32]);
///
/// let mut trainer = EsMccfrTrainer::new(game, 1)?
///     .with_linear_rm_plus(1_000_000);    // D-409 warm-up 1M update
///
/// for _ in 0..1_000 {
///     trainer.step(&mut rng)?;
/// }
///
/// assert_eq!(trainer.update_count(), 1_000);
/// let metrics = trainer.metrics();
/// println!("avg_regret_growth={}", metrics.avg_regret_growth_rate);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// **注**：doc-test 走 `#[ignore]` opt-in（依赖 bucket table v3 artifact，cargo test default 不触发）；
/// 与 stage 3 SimplifiedNlheGame doc-test 同型策略（D-378 字面继承）。
pub struct DocTestNlheGame6;
```

### Checkpoint v2 round-trip（API-441 doc-test）

```rust
/// stage 4 端到端 doc-test — Checkpoint schema_version=2 round-trip
///
/// ```rust
/// use poker::training::{Checkpoint, EsMccfrTrainer, NlheGame6, RegretTable, GameVariant, TrainerVariant};
///
/// // 训练 N update → 保存 → 加载 → 继续 M update → BLAKE3 byte-equal
/// # let mut trainer = build_test_trainer()?;
/// for _ in 0..100 { trainer.step(&mut rng)?; }
/// trainer.save_checkpoint(Path::new("/tmp/blueprint_v2.ckpt"))?;
///
/// let restored = EsMccfrTrainer::<NlheGame6>::load_checkpoint(Path::new("/tmp/blueprint_v2.ckpt"))?;
/// assert_eq!(restored.update_count(), 100);
/// assert_eq!(restored.config().linear_weighting_enabled, true);
/// assert_eq!(restored.config().rm_plus_enabled, true);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct DocTestCheckpointV2;
```

### LBR computation（API-451 doc-test）

```rust
/// stage 4 端到端 doc-test — LBR computation
///
/// ```rust
/// use poker::training::lbr::LbrEvaluator;
/// # let trainer = Arc::new(build_first_usable_blueprint()?);
/// let evaluator = LbrEvaluator::new(trainer, 14, 1)?;
///
/// let mut rng = ChaCha20Rng::from_seed([1u8; 32]);
/// let result = evaluator.compute(0, 1000, &mut rng)?;
///
/// // stage 4 first usable LBR < 200 mbb/g 阈值
/// assert!(result.lbr_value_mbbg < 200.0);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct DocTestLbrCompute;
```

---

## 14. API 修改流程

继承 stage 1 + stage 2 + stage 3 API 修改流程：

1. 任何 API 修改在本文档以 `API-NNN-revM` 形式追加新条目（不删除原条目）+ `pluribus_stage4_api.md` §修订历史追加 entry。
2. 必要时 bump `Checkpoint.schema_version`（API-440 stage 4 schema_version=2）或继承的 stage 1 / 2 / 3 schema_version。
3. 同步更新 `tests/api_signatures.rs`（trip-wire），否则 `cargo test --no-run` fail。
4. 跨 stage 边界 API 修改（如 API-423 修改 stage 2 `InfoSetId` 64-bit layout）需要：(a) 在被修改 stage 的 api.md 同 PR 追加 API-NNN-revM；(b) 在 stage 1 / stage 2 / stage 3 测试套件全 0 failed byte-equal 维持；(c) 用户书面授权（与 stage 3 D-022b-rev1 / stage 3 D-321-rev2 同型跨 stage carve-out 模式）。
5. 通知所有正在工作的 agent。

### 修订历史

- **2026-05-14（A0 [决策] 起步 batch 5 落地）**：stage 4 A0 [决策] 起步 batch 5 落地 `docs/pluribus_stage4_api.md`（本文档）骨架 — API-400..API-499 全 API surface + §1 EsMccfrTrainer Linear+RM+ 扩展 + §2 NlheGame6 + §3 PluribusActionAbstraction + §4 6-traverser RegretTable + §5 Checkpoint v2 schema + §6 LbrEvaluator + §7 SlumbotBridge + §8 TrainingMetrics + §9 Opponent6Max + 3 baseline + §10 SLO + CLI + §11 lib.rs + Cargo.toml + §12 stage 1/2/3 桥接 + §13 端到端 doc-test + §14 API 修改流程 + §15 与决策文档对应关系。**核心 API 锁**：(a) **API-400 / API-401** `EsMccfrTrainer::with_linear_rm_plus(warmup_complete_at)` builder + `TrainerConfig` 4 个新字段（D-401 / D-402 / D-409）；(b) **API-410 / API-411 / API-413** `NlheGame6` impl Game trait + `GameVariant::Nlhe6Max` 4th variant + `new_hu()` HU 退化路径；(c) **API-420 / API-421** `PluribusActionAbstraction` + `PluribusAction` 14-variant enum + 14-action `legal_actions` 子集；(d) **API-423** `InfoSetId::with_14action_mask` bits 33..47 14-bit mask 区域（D-423 lock 复用 stage 2 IA-007 reserved）；(e) **API-440 / API-441** Checkpoint v2 128-byte header + 8 个新字段 (`schema_version: u32 = 2 / trainer_variant: u8 / traverser_count: u8 / linear_weighting: u8 / rm_plus: u8 / warmup_complete: u8 / regret_offset: u64 / strategy_offset: u64`)；(f) **API-450..API-452** `LbrEvaluator` Rust 自实现 + `LbrResult` / `SixTraverserLbrResult` + `lbr_compute` CLI；(g) **API-460 / API-461** `SlumbotBridge` HTTP + `Head2HeadResult` + duplicate dealing；(h) **API-470 / API-471 / API-472** `TrainingMetrics` 9 字段 + `TrainingAlarm` 5 variant + JSONL log；(i) **API-480..API-484** `Opponent6Max` trait + 3 baseline impl (Random / CallStation / TAG) + `evaluate_vs_baseline` 1M 手协议；(j) **API-490 / API-499** `train_cfr` CLI 11 个 stage 4 新 flag + `stage4_*` SLO harness + benches/stage4.rs 3 bench group + `Cargo.toml` stage 4 新增 3 crate（rayon / reqwest / serde_json）。本节首条由 stage 4 A0 [决策] batch 5 commit 落地，与 `pluribus_stage4_validation.md` §修订历史 + `pluribus_stage4_decisions.md` §修订历史 + `pluribus_stage4_workflow.md` §修订历史 + `CLAUDE.md` "stage 4 A0 起步 batch 1-5 closed" 状态翻面同步。

---

## 15. 与决策文档的对应关系

| API 号段 | 决策号段 | 说明 |
|---|---|---|
| API-400..API-409 | D-400..D-409 | EsMccfrTrainer Linear+RM+ 扩展 + warm-up + TrainerConfig 字段 |
| API-410..API-419 | D-410..D-419 | NlheGame6 impl Game + 6-traverser routing + HU 退化路径 |
| API-420..API-429 | D-420..D-429 | PluribusActionAbstraction + 14-action enum + InfoSetId 14-bit mask |
| API-430..API-439 | D-430..D-439 | 6-traverser RegretTable + into_inner + RSS 监控 |
| API-440..API-449 | D-440..D-449 | Checkpoint v2 schema 128-byte header + TrainerVariant::ESMccfrLinearRmPlus |
| API-450..API-459 | D-450..D-459 | LbrEvaluator Rust 自实现 + LBR result + OpenSpiel sanity export |
| API-460..API-469 | D-460..D-469 | SlumbotBridge HTTP + Head2HeadResult + duplicate dealing + fallback baseline |
| API-470..API-479 | D-470..D-479 | TrainingMetrics 9 字段 + TrainingAlarm 5 variant + JSONL log |
| API-480..API-489 | D-480..D-489 | Opponent6Max trait + 3 baseline + 1M 手协议 |
| API-490..API-499 | D-490..D-499 | SLO + CLI + 24h continuous + 7-day fuzz + Cargo.toml 依赖 |

---

## 参考资料

- Pluribus 主论文：https://noambrown.github.io/papers/19-Science-Superhuman.pdf §2 Algorithm / §S2 / §S4
- Brown, Sandholm, "Solving Imperfect-Information Games via Discounted Regret Minimization"（AAAI 2019）— Linear CFR API 表达模式
- Tammelin et al. 2015 IJCAI — RM+ API 表达模式
- Lisý & Bowling 2017 — LBR API 表达模式
- OpenSpiel `algorithms/exploitability_descent.py` — LBR Python reference
- Slumbot API endpoints：http://www.slumbot.com/api/
- 阶段 3 API 规范：`pluribus_stage3_api.md`（API-300..API-392）— stage 4 字面继承
- 阶段 2 API 规范：`pluribus_stage2_api.md`（API-200..API-302）— ActionAbstraction trait + InfoSetId 字面继承
- 阶段 1 API 规范：`pluribus_stage1_api.md`（API-001..API-013）— GameState / RngSource 字面继承

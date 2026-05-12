# 阶段 3 API 规范

## 文档地位

本文档锁定阶段 3（MCCFR 小规模验证）公开的 Rust API surface（trait / struct / enum / 公开方法签名）。一旦 commit，后续步骤（A1 / B1 / B2 / ... / F3）的所有 agent 必须严格按此签名实现 / 测试。

任何 API 修改必须：
1. 在本文档以 `API-NNN-revM` 形式追加新条目（不删除原条目）
2. 必要时 bump `Checkpoint.schema_version`（API-350）或 `HandHistory.schema_version`（继承 stage 1 D-101，仅当 stage 3 修改影响序列化时触发）
3. 同步更新 `tests/api_signatures.rs`（trip-wire），否则 `cargo test --no-run` fail
4. 通知所有正在工作的 agent

阶段 3 API 编号从 **API-300** 起，与 stage 1（API-001..API-013）+ stage 2（API-200..API-302）不冲突。stage 1 + stage 2 API surface 作为只读契约继承到 stage 3，未在本文档覆盖部分以 `pluribus_stage1_api.md` + `pluribus_stage2_api.md` 为准。

---

## 1. Game trait（`module: training::game`）

### `Game` trait（API-300）

通用游戏抽象，让 `Trainer<G: Game>` 在 Kuhn / Leduc / 简化 NLHE 上同型工作。

```rust
pub trait Game {
    /// 完整游戏状态（含 chance + decision history）
    type State: Clone + Send + Sync;
    /// game-specific action（Kuhn: { Check, Bet, Call, Fold }; Leduc: { Check, Bet, Call, Fold, Raise }; 简化 NLHE: AbstractAction from stage 2）
    type Action: Clone + Copy + Send + Sync + Eq + std::fmt::Debug;
    /// game-specific InfoSet id（Kuhn/Leduc 用 stage 3 独立编码；简化 NLHE 继承 stage 2 InfoSetId）
    type InfoSet: Clone + Send + Sync + Eq + std::hash::Hash + std::fmt::Debug;

    /// 玩家数（Kuhn/Leduc/简化 NLHE 全部 = 2）
    fn n_players(&self) -> usize;

    /// 初始状态（含 deal chance node 已完成；后续 chance node 在 next 内部触发）
    fn root(&self, rng: &mut dyn RngSource) -> Self::State;

    /// 当前节点角色：Chance / Player(PlayerId) / Terminal
    fn current(state: &Self::State) -> NodeKind;

    /// 当前 InfoSet（actor 视角，含 actor 私有信息 + 公开历史）
    /// 仅当 current(state) == Player(_) 时有意义；Chance / Terminal 调用 panic
    fn info_set(state: &Self::State, actor: PlayerId) -> Self::InfoSet;

    /// 当前节点合法 action 列表（D-318：Kuhn/Leduc 直接返回 game-specific 枚举；简化 NLHE 走 stage 2 ActionAbstraction）
    fn legal_actions(state: &Self::State) -> Vec<Self::Action>;

    /// 执行 action 转移状态；chance node 走 chance_distribution + rng 采样；decision node 直接 apply
    fn next(state: Self::State, action: Self::Action, rng: &mut dyn RngSource) -> Self::State;

    /// chance node 上的离散分布（仅 chance node 调用，Player/Terminal panic）
    /// 返回 `(action, probability)` 二元对；Σ probability = 1.0
    fn chance_distribution(state: &Self::State) -> Vec<(Self::Action, f64)>;

    /// terminal payoff（D-316 chip 净收益直接当 utility）
    /// 仅 current(state) == Terminal 时有意义；Chance/Player 调用 panic
    fn payoff(state: &Self::State, player: PlayerId) -> f64;
}

pub type PlayerId = u8; // 0-indexed; 2-player game player ∈ {0, 1}

pub enum NodeKind {
    Chance,
    Player(PlayerId),
    Terminal,
}
```

**不变量（API-300 invariants）**：
- `n_players(&self) >= 2`（CFR 在 1-player 退化为单 player MDP，不属于 stage 3 范围）。
- `chance_distribution(state)` 的所有概率严格 `> 0.0`（零概率 outcome 应从分布中剔除而不是保留）；Σ probability = 1.0 ± 1e-12。
- `info_set(state, actor)` 必须是 actor 视角下信息的**完整 hash**：相同 actor 在不同 hidden state（如对手手牌）下必须可能产生不同 InfoSet id，但对 actor 可观察的所有信息必须确定性。
- `legal_actions(state)` 顺序必须**确定性**且与 `RegretTable` `Vec<f64>` 索引一一对应（D-324 action_count 训练全程恒定）。
- `next(state, action, rng)` 在 decision node 上**不消费** RNG（pure transition）；仅 chance node 消费 RNG（D-308 sample-1 路径）。

### `KuhnGame`（API-301）

```rust
pub struct KuhnGame; // Zero-sized, infallible

#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq)]
pub enum KuhnAction {
    Check,
    Bet,
    Call,
    Fold,
}

#[derive(Clone, Eq, Hash, Debug, PartialEq)]
pub struct KuhnInfoSet {
    pub actor: PlayerId,
    pub private_card: u8, // ∈ {11, 12, 13} per D-310 Kuhn rules
    pub history: KuhnHistory, // enum: Empty / Check / Bet / CheckBet (player 1 view) / ...
}

#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq)]
pub enum KuhnHistory {
    Empty,            // P1 to act
    Check,            // P2 to act after P1 check
    Bet,              // P2 to act after P1 bet
    CheckBet,         // P1 to act after P1 check, P2 bet
}

impl Game for KuhnGame {
    type State = KuhnState;
    type Action = KuhnAction;
    type InfoSet = KuhnInfoSet;
    // ... methods per Game trait
}

pub struct KuhnState {
    pub cards: [u8; 2],       // [P1 card, P2 card]
    pub history: KuhnHistory,
    pub terminal_payoffs: Option<[f64; 2]>, // Some when terminal
}
```

### `LeducGame`（API-302）

```rust
pub struct LeducGame; // Zero-sized

#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq)]
pub enum LeducAction {
    Check,
    Bet,
    Call,
    Fold,
    Raise,
}

#[derive(Clone, Eq, Hash, Debug, PartialEq)]
pub struct LeducInfoSet {
    pub actor: PlayerId,
    pub private_card: u8,        // ∈ {0..6} encoding {J♠, J♥, Q♠, Q♥, K♠, K♥}
    pub public_card: Option<u8>, // None preflop; Some(0..6) postflop
    pub street: LeducStreet,     // Preflop / Postflop
    pub history: LeducHistory,
}

#[derive(Clone, Copy, Eq, Hash, Debug, PartialEq)]
pub enum LeducStreet { Preflop, Postflop }

// LeducHistory: 编码每条街的 voluntary action 序列；最多 2 raise (D-311)
pub type LeducHistory = smallvec::SmallVec<[LeducAction; 8]>;

impl Game for LeducGame {
    type State = LeducState;
    type Action = LeducAction;
    type InfoSet = LeducInfoSet;
    // ... methods per Game trait
}

pub struct LeducState {
    pub cards: [u8; 2],
    pub public_card: Option<u8>,
    pub street: LeducStreet,
    pub history: LeducHistory,
    pub committed: [u32; 2],
    pub terminal_payoffs: Option<[f64; 2]>,
}
```

### `SimplifiedNlheGame`（API-303）

```rust
pub struct SimplifiedNlheGame {
    bucket_table: Arc<BucketTable>, // stage 2 BucketTable，构造时载入
    config: TableConfig,             // stage 1 TableConfig（2-player + 100 BB starting stack）
}

impl SimplifiedNlheGame {
    pub fn new(bucket_table: Arc<BucketTable>) -> Result<Self, TrainerError> {
        // 校验 BucketTable.schema_version == 1 或 2 (D-314-rev1/rev2 lock 时确定)
        // 校验 BucketConfig == (500, 500, 500) (stage 2 默认)
        // ...
    }
}

// Action = stage 2 AbstractAction（D-318：5-action 直接当 Game::Action 返回）
pub type SimplifiedNlheAction = AbstractAction;

// InfoSet = stage 2 InfoSetId（D-317：简化 NLHE 继承 stage 2 64-bit InfoSetId）
pub type SimplifiedNlheInfoSet = InfoSetId;

impl Game for SimplifiedNlheGame {
    type State = SimplifiedNlheState;
    type Action = SimplifiedNlheAction;
    type InfoSet = SimplifiedNlheInfoSet;
    // ... methods per Game trait
}

pub struct SimplifiedNlheState {
    pub game_state: GameState, // stage 1 GameState
    pub action_history: Vec<SimplifiedNlheAction>,
}
```

---

## 2. Trainer trait（`module: training::trainer`）

### `Trainer` trait（API-310）

```rust
pub trait Trainer<G: Game> {
    /// 执行 1 iter 训练（Vanilla CFR）或 1 update（ES-MCCFR）
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError>;

    /// 当前 InfoSet 上的 current strategy（regret matching）
    fn current_strategy(&self, info_set: &G::InfoSet) -> Vec<f64>;

    /// 当前 InfoSet 上的 average strategy（strategy_sum 归一化）
    fn average_strategy(&self, info_set: &G::InfoSet) -> Vec<f64>;

    /// 已完成 iter / update 数（Vanilla CFR: iter；ES-MCCFR: per-player update）
    fn update_count(&self) -> u64;

    /// 写出 checkpoint（D-353 write-to-temp + atomic rename）
    fn save_checkpoint(&self, path: &Path) -> Result<(), CheckpointError>;

    /// 从 checkpoint 恢复（D-350 schema 校验 + D-352 trailer BLAKE3）
    fn load_checkpoint(path: &Path, game: G) -> Result<Self, CheckpointError>
    where
        Self: Sized;
}
```

### `VanillaCfrTrainer`（API-311）

```rust
pub struct VanillaCfrTrainer<G: Game> {
    pub(crate) game: G,
    pub(crate) regret: RegretTable<G::InfoSet>,
    pub(crate) strategy_sum: StrategyAccumulator<G::InfoSet>,
    pub(crate) iter: u64,
    pub(crate) rng_substream_seed: [u8; 32], // D-335 sub-stream root seed
}

impl<G: Game> VanillaCfrTrainer<G> {
    pub fn new(game: G, master_seed: u64) -> Self { /* ... */ }
}

impl<G: Game> Trainer<G> for VanillaCfrTrainer<G> {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        // 每 step 执行 1 iter（遍历完整博弈树 n_players 次，每次 1 traverser）
        // D-300 详解伪代码
    }
    // ... other methods
}
```

### `EsMccfrTrainer`（API-312）

```rust
pub struct EsMccfrTrainer<G: Game> {
    pub(crate) game: G,
    pub(crate) regret: RegretTable<G::InfoSet>, // 可能为 thread-safe wrapper，由 D-321 决定
    pub(crate) strategy_sum: StrategyAccumulator<G::InfoSet>,
    pub(crate) update_count: u64,
    pub(crate) rng_substream_seed: [u8; 32],
}

impl<G: Game> EsMccfrTrainer<G> {
    pub fn new(game: G, master_seed: u64) -> Self { /* ... */ }

    /// 多线程并发 step（D-321 thread-safety 模型决定具体实现）
    pub fn step_parallel(&mut self, rng_pool: &mut [Box<dyn RngSource>], n_threads: usize)
        -> Result<(), TrainerError> { /* ... */ }
}

impl<G: Game> Trainer<G> for EsMccfrTrainer<G> {
    fn step(&mut self, rng: &mut dyn RngSource) -> Result<(), TrainerError> {
        // 每 step 执行 1 update（D-307 alternating traverser）
        // D-301 详解伪代码
    }
    // ... other methods
}
```

### `TrainerError` enum（API-313）

```rust
#[derive(Debug, thiserror::Error)]
pub enum TrainerError {
    #[error("info_set {info_set:?} action_count mismatch: expected {expected}, got {got}")]
    ActionCountMismatch {
        info_set: String, // Debug-formatted G::InfoSet
        expected: usize,
        got: usize,
    },
    #[error("training process RSS {rss_bytes} exceeded limit {limit}")]
    OutOfMemory { rss_bytes: u64, limit: u64 },
    #[error("bucket table schema {got} not supported (expected {expected})")]
    UnsupportedBucketTable { expected: u32, got: u32 },
    #[error("regret matching probability sum {got} out of tolerance {tolerance}")]
    ProbabilitySumOutOfTolerance { got: f64, tolerance: f64 },
    #[error("checkpoint error: {0}")]
    CheckpointError(#[from] CheckpointError),
}
```

`TrainerError` 5 类对应 D-324 / D-325 / D-323 (bucket table version) / D-330 / D-351 propagation。继承 stage 1 + stage 2 错误追加不删模式。

---

## 3. RegretTable & StrategyAccumulator（`module: training::regret`）

### `RegretTable`（API-320）

```rust
pub struct RegretTable<I: Eq + std::hash::Hash + Clone> {
    inner: HashMap<I, Vec<f64>>, // D-320 内部存储
    n_actions_index: HashMap<I, usize>, // D-324 action_count 缓存
}

impl<I: Eq + std::hash::Hash + Clone> RegretTable<I> {
    pub fn new() -> Self {
        RegretTable {
            inner: HashMap::new(),
            n_actions_index: HashMap::new(),
        }
    }

    /// 获取 InfoSet 上的 regret vec 引用；首次访问时 lazy 分配（D-323）
    pub fn get_or_init(&mut self, info_set: I, n_actions: usize) -> &mut Vec<f64> {
        // D-324: 校验 n_actions 与已分配 vec 长度一致；不一致 panic / return Err
        // ...
    }

    /// 计算 current_strategy：regret matching + 退化均匀分布（D-303 + D-331）
    pub fn current_strategy(&self, info_set: &I, n_actions: usize) -> Vec<f64> {
        match self.inner.get(info_set) {
            None => vec![1.0 / n_actions as f64; n_actions], // 未访问 InfoSet
            Some(r) => {
                let r_plus: Vec<f64> = r.iter().map(|&x| x.max(0.0)).collect();
                let denom: f64 = r_plus.iter().sum();
                if denom > 0.0 {
                    r_plus.iter().map(|&x| x / denom).collect()
                } else {
                    vec![1.0 / n_actions as f64; n_actions]
                }
            }
        }
    }

    /// 累积 regret（D-305 标准 CFR update）
    pub fn accumulate(&mut self, info_set: I, delta: &[f64]) {
        // D-324 校验 + 加和
    }

    /// 已访问 InfoSet 数（监控用）
    pub fn len(&self) -> usize { self.inner.len() }
}
```

### `StrategyAccumulator`（API-321）

```rust
pub struct StrategyAccumulator<I: Eq + std::hash::Hash + Clone> {
    inner: HashMap<I, Vec<f64>>,
    n_actions_index: HashMap<I, usize>,
}

impl<I: Eq + std::hash::Hash + Clone> StrategyAccumulator<I> {
    pub fn new() -> Self { /* ... */ }

    /// 累积 strategy_sum：S(I, a) += π_traverser × σ(I, a) (D-304)
    pub fn accumulate(&mut self, info_set: I, weighted_strategy: &[f64]) { /* ... */ }

    /// 计算 average_strategy（D-304 标准累积 / Σ_b S(I, b)）
    pub fn average_strategy(&self, info_set: &I, n_actions: usize) -> Vec<f64> { /* ... */ }

    pub fn len(&self) -> usize { self.inner.len() }
}
```

### 不变量（API-320 / API-321 invariants）

- `get_or_init(I, n)` / `accumulate(I, delta)` 在同 `I` 上 `n` / `delta.len()` 必须一致，否则 panic（D-324）。
- `current_strategy(I, n) / average_strategy(I, n)` 返回 `Vec<f64>` 长度 = `n`，sum = 1.0 ± 1e-9（D-330）。
- 已访问 InfoSet 后 `len()` 单调非降。

---

## 4. BestResponse trait（`module: training::best_response`）

### `BestResponse` trait（API-340）

```rust
pub trait BestResponse<G: Game> {
    /// 计算 best response 对应 player 视角下的 EV + one-hot strategy
    /// 对手策略由 RegretTable.average_strategy 提供
    fn compute(
        game: &G,
        opponent_strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
        target_player: PlayerId,
    ) -> (HashMap<G::InfoSet, Vec<f64>>, f64);
}
```

### `KuhnBestResponse`（API-341）+ `LeducBestResponse`（API-342）

```rust
pub struct KuhnBestResponse;

impl BestResponse<KuhnGame> for KuhnBestResponse {
    fn compute(
        game: &KuhnGame,
        opponent_strategy: &dyn Fn(&KuhnInfoSet, usize) -> Vec<f64>,
        target_player: PlayerId,
    ) -> (HashMap<KuhnInfoSet, Vec<f64>>, f64) {
        // D-340 full-tree backward induction
        // ...
    }
}

pub struct LeducBestResponse;

impl BestResponse<LeducGame> for LeducBestResponse {
    fn compute(/* ... */) -> (HashMap<LeducInfoSet, Vec<f64>>, f64) {
        // D-341 same backward induction, polynomial in InfoSet count
    }
}
```

### Exploitability 辅助函数（API-343）

```rust
/// 计算 game 上的 exploitability（D-340 / D-341）
/// = (BR_0(σ_1) + BR_1(σ_0)) / 2，单位 chip/game
pub fn exploitability<G, BR>(
    game: &G,
    strategy: &dyn Fn(&G::InfoSet, usize) -> Vec<f64>,
) -> f64
where
    G: Game,
    BR: BestResponse<G>,
{
    let (_, br_0_value) = BR::compute(game, strategy, 0);
    let (_, br_1_value) = BR::compute(game, strategy, 1);
    (br_0_value + br_1_value) / 2.0
}
```

---

## 5. Checkpoint（`module: training::checkpoint`）

### `Checkpoint` struct（API-350）

```rust
pub struct Checkpoint {
    pub schema_version: u32,
    pub trainer_variant: TrainerVariant,
    pub game_variant: GameVariant,
    pub update_count: u64,
    pub rng_state: [u8; 32],         // ChaCha20Rng 内部 state（继承 stage 1）
    pub bucket_table_blake3: [u8; 32], // 简化 NLHE 非零；Kuhn/Leduc 全零
    pub regret_table_bytes: Vec<u8>,   // bincode-serialized
    pub strategy_sum_bytes: Vec<u8>,   // bincode-serialized
}

#[repr(u8)]
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum TrainerVariant {
    VanillaCFR = 0,
    ESMccfr = 1,
}

#[repr(u8)]
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum GameVariant {
    Kuhn = 0,
    Leduc = 1,
    SimplifiedNlhe = 2,
}

impl Checkpoint {
    /// 写出 checkpoint 到 `path`（D-353 write-to-temp + atomic rename）
    pub fn save(&self, path: &Path) -> Result<(), CheckpointError> { /* ... */ }

    /// 从 `path` 加载（D-352 eager BLAKE3 校验）
    pub fn open(path: &Path) -> Result<Self, CheckpointError> { /* ... */ }
}
```

### 二进制 schema（API-350 binary layout）

| 字段 | 起始偏移 | 长度 | 编码 |
|---|---|---|---|
| `magic` | 0 | 8 | `b"PLCKPT\0\0"` |
| `schema_version` | 8 | 4 | u32 LE |
| `trainer_variant` | 12 | 1 | u8 |
| `game_variant` | 13 | 1 | u8 |
| `pad` | 14 | 6 | 0 |
| `update_count` | 20 | 8 | u64 LE |
| `rng_state` | 28 | 32 | bytes |
| `bucket_table_blake3` | 60 | 32 | bytes |
| `regret_table_offset` | 92 | 8 | u64 LE（≥ 100） |
| `strategy_sum_offset` | 100 | 8 | u64 LE |
| **(header end)** | 108 | — | — |
| `regret_table_body` | `regret_table_offset` | varies | bincode 1.x serialized HashMap |
| `strategy_sum_body` | `strategy_sum_offset` | varies | bincode 1.x serialized HashMap |
| `trailer_blake3` | `len - 32` | 32 | bytes |

**header alignment**：8 byte aligned，header 实际 108 byte（不再 pad to 112，bincode body 起点 = `regret_table_offset` 字段值，由写入器精确控制）。

**body 顺序**：D-327 bincode serialize HashMap，按 `InfoSet` Debug 排序后顺序写入（确保 BLAKE3 byte-equal across hosts）。

### `CheckpointError` enum（API-351）

```rust
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("checkpoint file not found: {path:?}")]
    FileNotFound { path: PathBuf },
    #[error("checkpoint schema mismatch: expected {expected}, got {got}")]
    SchemaMismatch { expected: u32, got: u32 },
    #[error("checkpoint trainer mismatch: expected {expected:?}, got {got:?}")]
    TrainerMismatch {
        expected: (TrainerVariant, GameVariant),
        got: (TrainerVariant, GameVariant),
    },
    #[error("checkpoint bucket_table BLAKE3 mismatch: expected {expected:02x?}, got {got:02x?}")]
    BucketTableMismatch { expected: [u8; 32], got: [u8; 32] },
    #[error("checkpoint corrupted at offset {offset}: {reason}")]
    Corrupted { offset: u64, reason: String },
}
```

继承 stage 2 `BucketTableError` 5 类形态。

---

## 6. Sampling helpers（`module: training::sampling`）

### RNG sub-stream 派生（API-330）

```rust
/// 6 个 op_id 表项（D-335）
pub const OP_KUHN_DEAL: u64 = 0x03_00;
pub const OP_LEDUC_DEAL: u64 = 0x03_01;
pub const OP_NLHE_DEAL: u64 = 0x03_02;
pub const OP_OPP_ACTION_SAMPLE: u64 = 0x03_10;
pub const OP_CHANCE_SAMPLE: u64 = 0x03_11;
pub const OP_TRAVERSER_TIE: u64 = 0x03_20;

/// SplitMix64 finalizer（继承 stage 1 D-228 + stage 2 cluster::rng_substream 模式）
pub fn derive_substream_seed(master_seed: u64, op_id: u64, iter: u64) -> [u8; 32] {
    // SplitMix64 finalizer × 4 → 32 byte ChaCha20Rng seed
    // ...
}
```

### Discrete distribution sampling（API-331）

```rust
/// 在 (action, probability) 列表上采样 1 个 outcome
/// rng 消费 1 次 next_u64；浮点 CDF binary search
pub fn sample_discrete<A: Copy>(
    distribution: &[(A, f64)],
    rng: &mut dyn RngSource,
) -> A {
    // D-336 实现
    // sum_check: Σ probability = 1.0 ± 1e-12
    // ...
}
```

---

## 7. 训练 CLI（`tools/train_cfr.rs`）

### CLI 入口（API-370）

```text
cargo run --release --bin train_cfr -- [OPTIONS]

OPTIONS:
    --game {kuhn,leduc,nlhe}       (required) D-372 game selection
    --trainer {vanilla,es-mccfr}   (optional) 默认按 game 自动推断
    --iter N                       (required for Kuhn/Leduc) iter 数
    --updates N                    (required for nlhe) update 数
    --seed S                       (optional, default 0) master seed
    --checkpoint-dir DIR           (optional, default ./artifacts/) checkpoint 输出目录
    --resume PATH                  (optional) 从 checkpoint 恢复
    --checkpoint-every N           (optional) 自动 checkpoint 频率（默认 D-355）
    --keep-last N                  (optional, default 5) backup 保留数（D-359）
    --bucket-table PATH            (required for nlhe) BucketTable artifact 路径
    --threads N                    (optional, default 1) 多线程并发数（仅 ES-MCCFR）
    --quiet                        (optional) 静默 progress log
```

### 进度日志（API-371）

stderr 实时输出（继承 stage 2 `train_bucket_table.rs` 模式）：

```text
[INFO] training game=Kuhn trainer=VanillaCFR seed=42
[INFO] iter 1000 / 10000 elapsed=0.10s exploitability_estimate=0.05
[INFO] iter 2000 / 10000 elapsed=0.21s exploitability_estimate=0.02
...
[INFO] checkpoint saved: artifacts/kuhn_vanilla_cfr_seed_42_iter_10000.ckpt
[INFO] training complete: 10000 iter, elapsed 1.05s
```

简化 NLHE 多线程模式：

```text
[INFO] training game=SimplifiedNlhe trainer=ESMccfr seed=42 threads=4
[INFO] update 10M / 100M elapsed=215.3s throughput=46500 update/s avg_regret=12.4
[INFO] update 25M / 100M elapsed=540.1s throughput=46300 update/s avg_regret=15.1
...
```

---

## 8. 模块导出（`lib.rs` + `Cargo.toml`）

### `src/lib.rs` 顶层 re-export（API-380）

```rust
pub mod training;

pub use training::{
    Trainer, VanillaCfrTrainer, EsMccfrTrainer, TrainerError,
    Game, NodeKind, PlayerId,
    KuhnGame, KuhnAction, KuhnInfoSet, KuhnHistory,
    LeducGame, LeducAction, LeducInfoSet, LeducStreet,
    SimplifiedNlheGame,
    RegretTable, StrategyAccumulator,
    BestResponse, KuhnBestResponse, LeducBestResponse, exploitability,
    Checkpoint, CheckpointError, TrainerVariant, GameVariant,
};
```

### `Cargo.toml` 新增依赖（D-373 锁定 3 个）

```toml
[dependencies]
# ... stage 1 + stage 2 既有依赖（blake3 / memmap2 / serde / thiserror 等）

# stage 3 新增
bincode = "1.3"
tempfile = "3"
# thread-safety: 在 D-321 batch 3 [实现] 之前 lock；候选 parking_lot / dashmap / crossbeam
# (commented placeholder — A0 batch 5 不预提交)
# parking_lot = "0.12"
```

### `Cargo.toml` 新增 bin 入口

```toml
[[bin]]
name = "train_cfr"
path = "tools/train_cfr.rs"
```

---

## 9. 与 stage 1 / stage 2 类型的桥接

### stage 1 `GameState` 桥接（API-390）

`SimplifiedNlheGame::State` 内部 wrap stage 1 `GameState`：

```rust
pub struct SimplifiedNlheState {
    pub game_state: GameState,                  // stage 1 read-only consumer
    pub action_history: Vec<AbstractAction>,    // stage 2 AbstractAction 累积
}
```

`Game::next` 内部调用 stage 1 `GameState::apply_action`（继承 stage 1 API-002）：

```rust
fn next(state: SimplifiedNlheState, action: SimplifiedNlheAction, rng: &mut dyn RngSource) -> SimplifiedNlheState {
    let concrete_action = action.to_concrete(&state.game_state); // stage 2 AbstractAction → Action
    let new_game_state = state.game_state.clone().apply_action(concrete_action, rng).expect("legal");
    SimplifiedNlheState {
        game_state: new_game_state,
        action_history: { let mut h = state.action_history; h.push(action); h },
    }
}
```

### stage 2 `InfoSetId` 桥接（API-391）

`SimplifiedNlheGame::info_set` 调用 stage 2 `PreflopLossless169` / `PostflopBucketAbstraction`：

```rust
fn info_set(state: &SimplifiedNlheState, actor: PlayerId) -> InfoSetId {
    let street = state.game_state.current_street();
    if street == Street::Preflop {
        PreflopLossless169::info_set_id(&state.game_state, actor)
    } else {
        PostflopBucketAbstraction::info_set_id(&state.game_state, actor, &bucket_table)
    }
}
```

### stage 2 `ActionAbstraction` 桥接（API-392）

`SimplifiedNlheGame::legal_actions` 调用 stage 2 `DefaultActionAbstraction::actions`：

```rust
fn legal_actions(state: &SimplifiedNlheState) -> Vec<SimplifiedNlheAction> {
    DefaultActionAbstraction::actions(&state.game_state).into_vec()
}
```

stage 2 5-action 顺序（D-209 deterministic）直接作为 `RegretTable` `Vec<f64>` 索引（D-324 action_count 全程恒定）。

---

## 10. 端到端示例（doc-test 占位）

### Kuhn 10 iter Vanilla CFR（API-378 doc-test）

```rust
use poker::training::*;
use poker::core::ChaCha20Rng;

fn main() {
    let game = KuhnGame;
    let mut trainer = VanillaCfrTrainer::new(game, /* master_seed */ 42);
    let mut rng = ChaCha20Rng::seed_from_u64(42);

    for _ in 0..10 {
        trainer.step(&mut rng).unwrap();
    }

    // 查询 player 0 的 InfoSet `(card=K, history=Empty)` 的 average strategy
    let info_set = KuhnInfoSet {
        actor: 0,
        private_card: 13,
        history: KuhnHistory::Empty,
    };
    let strategy = trainer.average_strategy(&info_set);
    assert_eq!(strategy.len(), 2); // {Check, Bet}
    assert!((strategy.iter().sum::<f64>() - 1.0).abs() < 1e-9);
}
```

### Checkpoint round-trip（API-378 doc-test）

```rust
let game = KuhnGame;
let mut trainer = VanillaCfrTrainer::new(game, 42);
let mut rng = ChaCha20Rng::seed_from_u64(42);

// 训练 5 iter
for _ in 0..5 { trainer.step(&mut rng).unwrap(); }

// 保存 checkpoint
let checkpoint_path = std::env::temp_dir().join("kuhn_ckpt_5iter.bin");
trainer.save_checkpoint(&checkpoint_path).unwrap();

// 加载 checkpoint + 继续训练 5 iter
let mut loaded = VanillaCfrTrainer::<KuhnGame>::load_checkpoint(&checkpoint_path, KuhnGame).unwrap();
let mut rng_loaded = ChaCha20Rng::seed_from_u64(42); // RNG state 由 checkpoint 恢复，这里不重复 seed
for _ in 0..5 { loaded.step(&mut rng_loaded).unwrap(); }

// 与不中断对照训练 10 iter byte-equal
let game2 = KuhnGame;
let mut trainer2 = VanillaCfrTrainer::new(game2, 42);
let mut rng2 = ChaCha20Rng::seed_from_u64(42);
for _ in 0..10 { trainer2.step(&mut rng2).unwrap(); }

// 比较 regret_table BLAKE3
assert_eq!(loaded.regret_blake3(), trainer2.regret_blake3());
```

---

## 11. API 修改流程

继承 `pluribus_stage1_api.md` §11 + `pluribus_stage2_api.md` §9 修改流程：

1. 在本文档以 `API-NNN-revM` 形式追加新条目（不删除原条目）
2. 同步更新 `tests/api_signatures.rs`（trip-wire），否则 `cargo test --no-run` fail
3. 必要时 bump `Checkpoint.schema_version`（D-350）
4. 通知所有正在工作的 agent

### 修订历史

- **2026-05-12（A0 [决策] 起步 batch 5 落地）**：stage 3 A0 [决策] 起步 batch 5 落地 `docs/pluribus_stage3_api.md`（本文档）骨架 + API-300..API-392 全 API surface。本节首条由 stage 3 A0 [决策] batch 5 commit 落地，与 `pluribus_stage3_validation.md` §修订历史 + `pluribus_stage3_decisions.md` §修订历史同步。
    - §1 Game trait: API-300 `Game` trait 通用签名 + 3 个关联类型（State / Action / InfoSet）+ 6 个方法 + `NodeKind` / `PlayerId` 辅助 + 5 条不变量；API-301 `KuhnGame` + `KuhnAction` + `KuhnInfoSet` + `KuhnHistory` + `KuhnState`；API-302 `LeducGame` + `LeducAction` + `LeducInfoSet` + `LeducStreet` + `LeducHistory` + `LeducState`；API-303 `SimplifiedNlheGame` + `SimplifiedNlheState` + type alias `SimplifiedNlheAction = AbstractAction` + `SimplifiedNlheInfoSet = InfoSetId`。
    - §2 Trainer trait: API-310 `Trainer<G: Game>` trait 6 方法（`step` / `current_strategy` / `average_strategy` / `update_count` / `save_checkpoint` / `load_checkpoint`）；API-311 `VanillaCfrTrainer<G>`；API-312 `EsMccfrTrainer<G>` + `step_parallel`；API-313 `TrainerError` 5 variant。
    - §3 RegretTable & StrategyAccumulator: API-320 `RegretTable<I>` HashMap-backed lazy init；API-321 `StrategyAccumulator<I>` 同型；3 条不变量。
    - §4 BestResponse: API-340 `BestResponse<G>` trait；API-341 `KuhnBestResponse` full-tree BR；API-342 `LeducBestResponse` 同型；API-343 `exploitability<G, BR>` 辅助函数。
    - §5 Checkpoint: API-350 `Checkpoint` struct + 二进制 schema（108 byte header + bincode body + 32 byte BLAKE3 trailer）+ `save` / `open` API；API-351 `CheckpointError` 5 variant。
    - §6 Sampling helpers: API-330 `derive_substream_seed` + 6 个 `OP_*` const；API-331 `sample_discrete` 离散采样。
    - §7 训练 CLI: API-370 CLI flag definition；API-371 进度日志格式。
    - §8 模块导出: API-380 `src/lib.rs` re-export 列表 + `Cargo.toml` 3 个新增依赖。
    - §9 桥接: API-390 stage 1 `GameState` wrap；API-391 stage 2 `InfoSetId` 桥接；API-392 stage 2 `ActionAbstraction` 桥接。
    - §10 端到端示例 API-378 doc-test 2 个占位。

---

## 12. 与决策文档的对应关系

| API 编号 | 对应决策 | 备注 |
|---|---|---|
| API-300 `Game` trait | D-312 | trait 抽象层 |
| API-301 `KuhnGame` | D-310 | Kuhn 规则 |
| API-302 `LeducGame` | D-311 | Leduc 规则 |
| API-303 `SimplifiedNlheGame` | D-313 + D-314 (deferred) | 简化 NLHE 范围 + bucket table 依赖 deferred |
| API-310 `Trainer` trait | D-371 | trait surface |
| API-311 `VanillaCfrTrainer` | D-300 | Vanilla CFR for Kuhn/Leduc |
| API-312 `EsMccfrTrainer` | D-301 + D-321 (deferred) | ES-MCCFR + thread-safety deferred |
| API-313 `TrainerError` | D-324 / D-325 / D-330 | 5 类错误 |
| API-320 `RegretTable` | D-320 / D-323 / D-328 | HashMap + lazy + query |
| API-321 `StrategyAccumulator` | D-322 / D-328 | 独立结构 + query |
| API-330 `derive_substream_seed` | D-335 | RNG sub-stream + 6 op_id |
| API-331 `sample_discrete` | D-336 / D-337 | CDF binary search |
| API-340 `BestResponse` trait | D-344 | trait 输出 (strategy, value) |
| API-341 `KuhnBestResponse` | D-340 | full-tree BR |
| API-342 `LeducBestResponse` | D-341 | backward induction |
| API-343 `exploitability` | D-340 / D-341 | (BR_0 + BR_1) / 2 |
| API-350 `Checkpoint` schema | D-350 | 108 byte header + bincode body + BLAKE3 trailer |
| API-351 `CheckpointError` | D-351 | 5 类错误 |
| API-370 `train_cfr` CLI | D-372 | CLI flags |
| API-371 进度日志 | D-355 | auto-save 频率 |
| API-378 doc-test | D-378 | 端到端示例 |
| API-380 lib.rs re-export | D-374 / D-376 | 公开 API surface |
| API-390 / API-391 / API-392 桥接 | stage 1 API-002 / stage 2 API-215 / D-318 | stage 1 + stage 2 类型边界 |

---

## 参考资料

- `pluribus_stage1_api.md` — stage 1 API surface（GameState / HandEvaluator / HandHistory / RngSource 接口锁定）
- `pluribus_stage2_api.md` — stage 2 API surface（ActionAbstraction / InfoAbstraction / EquityCalculator / BucketTable 接口锁定）
- `pluribus_stage3_decisions.md` — stage 3 D-300..D-379 全决策
- `pluribus_stage3_validation.md` — stage 3 path.md §阶段 3 字面 5 条门槛 anchor
- Zinkevich et al. 2007 (CFR) / Lanctot et al. 2009 (MCCFR) — 算法定义参考

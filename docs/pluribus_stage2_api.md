# 阶段 2 API 契约

## 文档地位

本文档定义阶段 2（抽象层）所有公开类型与方法的契约。**A1 步骤的代码骨架必须严格匹配本文档**。

- 测试 agent 在 B1 / C1 / D1 / E1 / F1 写测试时，**只依赖**本文档定义的 API 与阶段 1 `pluribus_stage1_api.md` 定义的 API。
- 实现 agent 在 A1 / B2 / C2 / D2 / E2 / F2 写产品代码时，**不得偏离**本文档签名（除非走 `API-NNN-revM` 修订流程修改本文档）。
- 任何在实现过程中发现的 API 不足或歧义，必须先在本文档追加 `API-NNN-revM` 条目，再实施。

阶段 2 API 编号从 **API-200** 起，与阶段 1 `API-001..API-099` 不冲突。阶段 1 API 全集 + `API-NNN-revM` 修订作为只读 spec 继承到阶段 2，未在本文档显式扩展的部分以 `pluribus_stage1_api.md` 为准。任何 stage 2 [实现] agent 发现 stage 1 API 不够用 → 走 stage 1 `API-NNN-revM` 修订流程，**不允许**直接在本文档覆盖 stage 1 API。

所有签名为 Rust 风格。语义说明放在签名后的注释或下方文字。

---

## 1. Action abstraction（`module: abstraction::action`）

### AbstractAction / AbstractActionSet / ActionAbstractionConfig

```rust
/// 抽象动作。pot ratio 编码进 `Bet` / `Raise` 变体；apply 时取 `to`。
///
/// `Bet` 与 `Raise` 在构造时由 stage 1 `LegalActionSet`（LA-002 互斥）选定：
/// 本下注轮无前序 bet ⇒ `Bet`，已有前序 bet ⇒ `Raise`。该拆分让
/// `to_concrete()` 无状态可调用（见 §7），同时 D-212 `betting_state`
/// 字段在 `Bet` 与 `Raise` 之间的转移无歧义（`Bet` 把 `Open` 推进到
/// `FacingBetNoRaise`；`Raise` 把任何状态推进到 `FacingRaise{1,2,3+}`）。
///
/// `ratio_label` 仅作为 InfoSet 编码区分性（D-207 / D-209），不参与 apply 计算。
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AbstractAction {
    Fold,
    Check,
    Call { to: ChipAmount },
    /// 本下注轮无前序 bet（`legal_actions().bet_range.is_some()`）。
    Bet { to: ChipAmount, ratio_label: BetRatio },
    /// 本下注轮已有前序 bet（`legal_actions().raise_range.is_some()`）。
    Raise { to: ChipAmount, ratio_label: BetRatio },
    AllIn { to: ChipAmount },
}

/// pot ratio 标签的整数编码，避免 `f64` 进入 `Eq` / `Hash`。
///
/// 内部存 `ratio × 1000` 的 `u32`（D-200 默认值：`Half = 500`、`Full = 1000`）。
/// `ActionAbstractionConfig` 接受 `f64` 输入但内部规整为该整数表示。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct BetRatio(u32);

impl BetRatio {
    pub const HALF_POT: BetRatio = BetRatio(500);
    pub const FULL_POT: BetRatio = BetRatio(1000);
    pub fn from_f64(ratio: f64) -> Option<BetRatio>;
    pub fn as_milli(self) -> u32; // 返回内部整数表示
}

/// 抽象动作集合输出。顺序固定为 D-209：
/// `[Fold?, Check?, Call?, Bet(0.5×pot)? | Raise(0.5×pot)?, Bet(1.0×pot)? | Raise(1.0×pot)?, AllIn?]`
/// `?` 表示不存在则跳过；同一 ratio 槽位 `Bet` 与 `Raise` 互斥（由 stage 1 LA-002 保证）。
#[derive(Clone, Debug)]
pub struct AbstractActionSet {
    actions: Vec<AbstractAction>,
}

impl AbstractActionSet {
    pub fn iter(&self) -> std::slice::Iter<'_, AbstractAction>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn contains(&self, action: AbstractAction) -> bool;
    pub fn as_slice(&self) -> &[AbstractAction];
}

/// `ActionAbstractionConfig`：raise size 集合（D-202）。
/// `raise_pot_ratios` 长度 ∈ [1, 14]，每个元素 ∈ (0.0, +∞)。
#[derive(Clone, Debug)]
pub struct ActionAbstractionConfig {
    pub raise_pot_ratios: Vec<BetRatio>,
}

impl ActionAbstractionConfig {
    /// 默认 5-action 配置：`[BetRatio::HALF_POT, BetRatio::FULL_POT]`。
    pub fn default_5_action() -> ActionAbstractionConfig;

    /// 自定义构造。长度 / 范围越界返回 `ConfigError`。
    pub fn new(raise_pot_ratios: Vec<f64>) -> Result<ActionAbstractionConfig, ConfigError>;

    pub fn raise_count(&self) -> usize;
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("raise_pot_ratios length out of range: expected [1, 14], got {0}")]
    RaiseCountOutOfRange(usize),
    #[error("raise pot ratio not positive finite: {0}")]
    RaiseRatioInvalid(f64),
    /// `BucketConfig::new` 越界：每条街 bucket 数应 ∈ [10, 10_000]（D-214）。
    #[error("bucket count out of range for {street:?}: expected [10, 10_000], got {got}")]
    BucketCountOutOfRange { street: StreetTag, got: u32 },
}
```

### ActionAbstraction trait + DefaultActionAbstraction

```rust
pub trait ActionAbstraction: Send + Sync {
    /// 给定当前 GameState，返回抽象动作集合（D-200..D-209 全部 fallback 已应用）。
    fn abstract_actions(&self, state: &GameState) -> AbstractActionSet;

    /// off-tree action 映射（D-201 PHM stub；stage 2 仅占位实现，
    /// stage 6c 完整数值验证）。
    ///
    /// `real_to` 是对手实际下注的 `to` 字段（绝对金额，与 stage 1
    /// `Action::Bet/Raise { to }` 同语义）。
    fn map_off_tree(&self, state: &GameState, real_to: ChipAmount) -> AbstractAction;

    /// 配置只读访问。
    fn config(&self) -> &ActionAbstractionConfig;
}

pub struct DefaultActionAbstraction { /* opaque */ }

impl DefaultActionAbstraction {
    pub fn new(config: ActionAbstractionConfig) -> DefaultActionAbstraction;
    pub fn default_5_action() -> DefaultActionAbstraction;
}

impl ActionAbstraction for DefaultActionAbstraction { /* ... */ }
```

### AbstractAction / AbstractActionSet 不变量

实现 agent 必须保证、测试 agent 在 invariant suite 中验证：

- AA-001 顺序固定（D-209）：`abstract_actions(state).iter()` 输出顺序为 `[Fold?, Check?, Call?, Bet(0.5×pot)? | Raise(0.5×pot)?, Bet(1.0×pot)? | Raise(1.0×pot)?, AllIn?]`，`?` 表示不存在则跳过；同一 ratio 槽位 `Bet` 与 `Raise` 互斥（由 stage 1 LA-002 保证，本不变量直接继承）。
- AA-002 `Fold` 与 `Check` 互斥（D-204）：当 `state.legal_actions().check == true` 时 `Fold` 不出现在抽象动作集合。
- AA-003 `Bet/Raise(x×pot)` fallback（D-205）：bet vs raise 由 stage 1 `LegalActionSet`（LA-002）选定：`bet_range.is_some()` ⇒ 输出 `Bet`，`raise_range.is_some()` ⇒ 输出 `Raise`。若 `x×pot < min_to`，输出 `Bet { to = min_to }` / `Raise { to = min_to }`（不剔除）；若 `x×pot >= committed_this_round + stack`，输出 `AllIn { to = committed_this_round + stack }`。
- AA-004 折叠去重（D-206）：抽象动作集合中不同 `AbstractAction` 实例的 `to` 字段必须互不相等（除 `Fold` / `Check` 不带 `to`）。
- AA-005 集合非空：当 `state.current_player().is_some()` 时，`abstract_actions(state).len() >= 1`（至少有 `Fold` 或 `Check` 之一，AA-002 保证不会同时空）。
- AA-006 集合空：当 `state.current_player().is_none()`（terminal / all-in 跳轮）时，`abstract_actions(state).is_empty() == true`。
- AA-007 deterministic：同 `(GameState, ActionAbstractionConfig)` 重复调用 `abstract_actions` 1,000,000 次结果完全相同（含 `Vec` 内 byte-equal）。
- AA-008 `effective_stack` 计算（D-208）：`effective_stack = min(actor.stack, max(opp.stack for opp in still_active_opps))`，仅排除 `Folded` 状态。

---

## 2. Information abstraction（`module: abstraction::info` / `preflop` / `postflop`）

### InfoSetId

```rust
/// 复合 InfoSet id。低位编码与 D-215 / D-216 一致，**preflop / postflop 共享同一 64-bit layout**。
///
/// 字段顺序（低位起）：
/// - bit  0..24: `bucket_id`         (24 bit；preflop = hand_class_169 ∈ 0..169；postflop = BucketTable::lookup 返回 cluster id ∈ 0..bucket_count(street))
/// - bit 24..28: `position_bucket`   ( 4 bit；0..n_seats-1，支持 2..=9 桌大小)
/// - bit 28..32: `stack_bucket`      ( 4 bit；0..4 = D-211 5 桶；postflop 沿用 preflop 起手值)
/// - bit 32..35: `betting_state`     ( 3 bit；0..4 = D-212 5 状态 enum 值)
/// - bit 35..38: `street_tag`        ( 3 bit；0..3 = Preflop/Flop/Turn/River；preflop 显式编码 0 不靠零启发式)
/// - bit 38..64: `reserved`          (26 bit；必须为 0)
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct InfoSetId(u64);

impl InfoSetId {
    pub fn raw(self) -> u64;
    pub fn street_tag(self) -> StreetTag;
    pub fn position_bucket(self) -> u8;
    pub fn stack_bucket(self) -> u8;
    pub fn betting_state(self) -> BettingState;
    pub fn bucket_id(self) -> u32;
}

/// 当前下注轮的合法动作集语义（D-212）。preflop 与 postflop 共用同一枚举。
///
/// 该字段直接决定 actor 的合法动作集——`Open` 局面 actor 可 `Check / Bet`，
/// `FacingBetNoRaise` 局面 actor 必须 `Fold / Call / Raise`，二者**不同**；
/// 仅以 raise count = 0 编码会让两类局面同 InfoSetId 但合法动作集不同，
/// CFR regret 矩阵跨 GameState 错位（F17 修复）。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum BettingState {
    /// preflop: BB 在 limpers / walks 后有 check option；
    /// postflop: 本街无 voluntary bet。
    Open = 0,
    /// preflop: 非 BB 位首次面对 BB 强制下注（无 voluntary raise）；
    /// postflop: 本街已有 opening bet 但无 raise。
    FacingBetNoRaise = 1,
    /// 本下注轮已发生 1 次 voluntary raise（含 incomplete short all-in）。
    FacingRaise1 = 2,
    FacingRaise2 = 3,
    /// ≥ 3 次 voluntary raise 吸收。
    FacingRaise3Plus = 4,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum StreetTag {
    Preflop = 0,
    Flop = 1,
    Turn = 2,
    River = 3,
}
```

### InfoAbstraction trait

```rust
pub trait InfoAbstraction: Send + Sync {
    /// `(GameState, hole_cards)` → InfoSet id。
    ///
    /// 整条调用路径**禁止浮点**（D-273 / D-252）；postflop 走 mmap bucket lookup
    /// 命中整数 bucket id；preflop 走组合 lookup 表。
    fn map(&self, state: &GameState, hole: [Card; 2]) -> InfoSetId;
}
```

### PreflopLossless169

```rust
pub struct PreflopLossless169 { /* opaque */ }

impl PreflopLossless169 {
    pub fn new() -> PreflopLossless169;

    /// 169 lossless 等价类编号（D-217）：
    /// - `0..13` = pocket pairs（22, 33, ..., AA 升序）
    /// - `13..91` = suited（按高牌主排序、低牌副排序：32s 起，AKs 终）
    /// - `91..169` = offsuit（同顺序）
    pub fn hand_class(&self, hole: [Card; 2]) -> u8;

    /// 169 类总 hole 计数：pairs 6 / suited 4 / offsuit 12，总和 1326。
    pub fn hole_count_in_class(class: u8) -> u8;
}

impl Default for PreflopLossless169 { /* ... */ }

impl InfoAbstraction for PreflopLossless169 {
    /// preflop 路径：`(hand_class_169, position_bucket, stack_bucket, betting_state)`
    /// 复合到 `InfoSetId`（D-215 统一 64-bit 编码，`bucket_id = hand_class_169`，
    /// `street_tag = StreetTag::Preflop`）。
    fn map(&self, state: &GameState, hole: [Card; 2]) -> InfoSetId;
}
```

### PostflopBucketAbstraction

```rust
pub struct PostflopBucketAbstraction {
    table: BucketTable,
    /* canonical id 计算缓存等内部字段 */
}

impl PostflopBucketAbstraction {
    /// 从 mmap-loaded BucketTable 构造。
    pub fn new(table: BucketTable) -> PostflopBucketAbstraction;

    /// 仅对 flop / turn / river 街生效；preflop 应走 PreflopLossless169。
    pub fn bucket_id(&self, state: &GameState, hole: [Card; 2]) -> u32;

    pub fn config(&self) -> BucketConfig;
}

impl InfoAbstraction for PostflopBucketAbstraction {
    /// postflop 路径：`(street, board, hole) → bucket_id`（mmap），与 preflop key
    /// 字段（position / stack / betting_state / street_tag）合并到 `InfoSetId`
    /// （D-215 统一 64-bit 编码；`bucket_id` 由 `BucketTable::lookup` 命中得到）。
    fn map(&self, state: &GameState, hole: [Card; 2]) -> InfoSetId;
}
```

### Information abstraction 不变量

- IA-001 preflop 169 全覆盖（D-217）：枚举全部 1326 起手牌 → 169 类一一映射；每类 hole 计数与组合数学一致（pairs 6 / suited 4 / offsuit 12 / 总和 1326）。
- IA-002 preflop key 区分性（validation §2）：同一 `hand_class_169` 在不同 `(position, stack, betting_state)` 下产出**不同** `InfoSetId.raw()`，碰撞率 0%。`betting_state` 5 状态展开（含 `Open` 与 `FacingBetNoRaise` 区分）保证 BB-after-limp 与 first-in-non-BB 不混入同一 InfoSet（F17）。
- IA-003 postflop bucket id 范围：`PostflopBucketAbstraction::bucket_id(...)` 返回值 ∈ `[0, BucketConfig.{street})`，且必须满足 `< 2^24`（D-215 `bucket_id` 字段宽度上限）。
- IA-004 deterministic：`map(state, hole)` 重复 1,000,000 次结果 byte-equal。
- IA-005 postflop 不依赖 preflop key（D-219）：`PostflopBucketAbstraction::bucket_id(state, hole)` 输出仅依赖 `(street, board, hole)`，不依赖 `(position, stack, betting_state)`。
- IA-006 街隔离：`map(state, hole)` 必须根据 `state.street()` 选择 preflop 或 postflop 路径；`Showdown` 街是 InfoSet 不可达状态（无 actor 决策点），`map` 在 `state.is_terminal() == true` 时返回上一条 betting round 的 `InfoSetId`（即 river 街最后一次决策点的编码），调用方一般不会触发该路径。
- IA-007 InfoSetId reserved 位为零（D-215）：`InfoSetId.raw()` 的 bit 38..64（26 bit）必须全为 0；任一非零 bit 写入是 P0 阻塞 bug。`tests/info_id_encoding.rs` 全枚举 typical state space 断言此不变量。

---

## 3. Equity calculator（`module: abstraction::equity`）

```rust
/// Equity 计算 trait。**仅离线 clustering 训练路径** 使用；运行时映射禁止触发
/// （D-225）。`f64` 出现在本 trait 是显式允许的——本路径在 `abstraction::equity`
/// / `abstraction::cluster` 子模块，与 `abstraction::map` 子模块（禁浮点，D-252）
/// 物理隔离。
pub trait EquityCalculator: Send + Sync {
    /// hand-vs-uniform-random-hole equity。
    /// 返回值 ∈ [0.0, 1.0]，必须 finite（D-224）。
    fn equity(&self, hole: [Card; 2], board: &[Card], rng: &mut dyn RngSource) -> f64;

    /// EHS²（potential-aware 二阶矩，D-223）。
    /// 返回值 ∈ [0.0, 1.0]。river 状态退化为 `equity²`。
    fn ehs_squared(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> f64;

    /// OCHS 向量。长度 = `n_opp_clusters`（D-222 默认 8）。
    /// 每维 ∈ [0.0, 1.0]，必须 finite。
    fn ochs(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Vec<f64>;
}

/// Monte Carlo equity 实现。基于 stage 1 `HandEvaluator`（`pluribus_stage1_api.md` §6）。
pub struct MonteCarloEquity {
    iter: u32,
    n_opp_clusters: u8,
    /* opaque：HandEvaluator 引用、OCHS opponent cluster 中心、缓存等 */
}

impl MonteCarloEquity {
    /// 默认配置：`iter = 10_000`、`n_opp_clusters = 8`（D-220 / D-222）。
    pub fn new(evaluator: std::sync::Arc<dyn HandEvaluator>) -> MonteCarloEquity;

    /// 自定义 iter（CI 短测试可降到 1,000；clustering 训练必须用默认 10k）。
    pub fn with_iter(self, iter: u32) -> MonteCarloEquity;

    /// 自定义 OCHS opponent cluster 数（stage 2 默认 8；stage 4 消融可调）。
    pub fn with_opp_clusters(self, n: u8) -> MonteCarloEquity;

    pub fn iter(&self) -> u32;
    pub fn n_opp_clusters(&self) -> u8;
}

impl EquityCalculator for MonteCarloEquity { /* ... */ }
```

### Equity calculator 不变量

- EQ-001 反对称容差（D-220a）：`|equity(A, B | board) + equity(B, A | board) - 1| ≤ 0.005`（`iter = 10_000`）；`iter = 1_000` 时容差 0.02。
- EQ-002 范围：`equity / ehs_squared / ochs[i]` 全部 ∈ [0.0, 1.0] 且 finite（D-224）；任何 NaN / Inf 视为 P0 阻塞 bug。
- EQ-003 EHS² rollout（D-227）：**采样口径**——outer 是 "已知我方 hole + 当前 board" 视角下未发**公共牌**枚举；对手 hole 不在 outer 维度，而在 inner equity 内部 Monte Carlo（uniform over remaining cards 排除我方 hole + 完整 board）。**rollout 数**：river 状态 outer = 0 rollout，EHS² 退化为 `inner_EHS²`（inner equity 仍走 D-220 默认 iter Monte Carlo）；turn 状态 outer = **46 张**未发 river 卡全枚举（52 - 2 hole - 4 board，确定性，无 outer RNG）；flop 状态 outer = **`C(47, 2) = 1081` 个 (turn, river) 无序对**全枚举（52 - 2 hole - 3 board = 47 张未发，选 2，确定性）。outer enumeration 不消耗 RngSource；inner equity 在每个 outer 评估点走 Monte Carlo（消耗 RngSource，sub-stream seed 由 outer enumeration index 决定保证 byte-equal）。
- EQ-004 OCHS 向量长度：`ochs(...).len() == self.n_opp_clusters() as usize`。
- EQ-005 deterministic：同 `(hole, board, rng_seed, iter, n_opp_clusters)` 重复调用结果 byte-equal。

---

## 4. Bucket table（`module: abstraction::bucket_table`）

### BucketConfig

```rust
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct BucketConfig {
    pub flop: u32,
    pub turn: u32,
    pub river: u32,
}

impl BucketConfig {
    /// stage 2 默认验收配置（D-213）。
    pub const fn default_500_500_500() -> BucketConfig;

    /// 校验每条街 ∈ [10, 10_000]（D-214）。
    pub fn new(flop: u32, turn: u32, river: u32) -> Result<BucketConfig, ConfigError>;
}
```

### BucketTable

```rust
/// mmap-backed bucket lookup table（D-240..D-249）。
///
/// 文件 layout（D-244；80-byte 定长 header + 变长 body + 32-byte trailer，
/// 全部 little-endian；reader 通过 header §⑨ 偏移表定位变长段，不依赖前段累积 size）：
///
/// ```text
/// // ===== header (80 bytes, 8-byte aligned) =====
/// offset 0x00: magic: [u8; 8] = b"PLBKT\0\0\0"                        // D-240
/// offset 0x08: schema_version: u32 LE = 1                             // D-240
/// offset 0x0C: feature_set_id: u32 LE = 1 (EHS² + OCHS(N=8))          // D-240
/// offset 0x10: bucket_count_flop:  u32 LE                             // D-214
/// offset 0x14: bucket_count_turn:  u32 LE
/// offset 0x18: bucket_count_river: u32 LE
/// offset 0x1C: n_canonical_flop:   u32 LE                             // D-218 / F13
/// offset 0x20: n_canonical_turn:   u32 LE
/// offset 0x24: n_canonical_river:  u32 LE
/// offset 0x28: n_dims:             u8                                 // D-221 (=9)
/// offset 0x29: pad:                [u8; 7] = 0                        // 8-byte align
/// offset 0x30: training_seed:      u64 LE                             // D-237
/// offset 0x38: centroid_metadata_offset: u64 LE                       // F13 (绝对偏移)
/// offset 0x40: centroid_data_offset:     u64 LE
/// offset 0x48: lookup_table_offset:      u64 LE
/// // ===== body (变长，按 header 偏移定位) =====
/// // centroid_metadata (3 streets × n_dims × (min: f32, max: f32))
/// // centroid_data     (3 streets × bucket_count(street) × n_dims × u8)  // D-241 / D-236b 重编号顺序
/// // lookup_table:
/// //   preflop:  [u32 LE; 1326]                                       // D-239 / D-245
/// //   flop:     [u32 LE; n_canonical_flop]
/// //   turn:     [u32 LE; n_canonical_turn]
/// //   river:    [u32 LE; n_canonical_river]
/// // ===== trailer (32 bytes) =====
/// // blake3: [u8; 32] = BLAKE3(file_body[..len-32])                   // D-243
/// ```
///
/// reader 必须按 §⑨ 偏移表定位变长段（不允许 const-bake 段 size 推算），
/// 任何 offset 越界 / 不递增 / 不 8-byte 对齐均视为 `BucketTableError::Corrupted`。
pub struct BucketTable { /* opaque：mmap 内部状态、缓存元数据 */ }

impl BucketTable {
    /// **eager 校验**：mmap → 读 header → 校验 schema_version / feature_set_id /
    /// 文件总大小 → 计算 BLAKE3 trailer → 比对 → 任一失败立即返回错误。
    /// 全 5 类错误路径见 BucketTableError。
    pub fn open(path: &std::path::Path) -> Result<BucketTable, BucketTableError>;

    /// `(street, board_canonical_id, hole_canonical_id) → bucket_id`。
    /// preflop 街忽略 `board_canonical_id` 参数（传 0）。
    /// `hole_canonical_id` 越界时返回 None。
    ///
    /// **接口接 `StreetTag`（不接 stage 1 `Street`）**——`StreetTag` 仅含 4 个 betting
    /// 街变体（`Preflop / Flop / Turn / River`），不含 `Showdown`。caller 必须在
    /// 调用前把 `Street::Showdown` 局面分流（Showdown 不存在 InfoSet 决策点，调用
    /// `lookup` 是语义错误）。该约束让 `bucket_count` / `lookup` / `BucketConfig` /
    /// `ConfigError::BucketCountOutOfRange` 全部用同一个 `StreetTag` 类型，避免
    /// `Street` ↔ `StreetTag` 反复转换的 spec drift（F9 修复）。
    pub fn lookup(
        &self,
        street: StreetTag,
        board_canonical_id: u32,
        hole_canonical_id: u32,
    ) -> Option<u32>;

    pub fn schema_version(&self) -> u32;
    pub fn feature_set_id(&self) -> u32;
    pub fn config(&self) -> BucketConfig;
    pub fn training_seed(&self) -> u64;

    /// 每条街 bucket 数；`StreetTag::Preflop` 固定返回 169。
    pub fn bucket_count(&self, street: StreetTag) -> u32;

    /// 文件 BLAKE3 自校验值（D-243）。同 mmap 加载后 byte-equal。
    pub fn content_hash(&self) -> [u8; 32];
}
```

### BucketTableError

```rust
#[derive(Debug, thiserror::Error)]
pub enum BucketTableError {
    #[error("bucket table file not found: {path:?}")]
    FileNotFound { path: std::path::PathBuf },

    #[error("bucket table schema mismatch: expected {expected}, got {got}")]
    SchemaMismatch { expected: u32, got: u32 },

    #[error("bucket table feature_set_id mismatch: expected {expected}, got {got}")]
    FeatureSetMismatch { expected: u32, got: u32 },

    /// mmap 边界越界 / 文件被截断 / header 字段声明的 size 与实际文件不符。
    #[error("bucket table size mismatch: expected {expected} bytes, got {got}")]
    SizeMismatch { expected: u64, got: u64 },

    /// magic bytes 错误 / BLAKE3 trailer 不匹配 / 字段越界 / 内部不一致。
    #[error("bucket table corrupted at offset {offset}: {reason}")]
    Corrupted { offset: u64, reason: String },
}
```

### BucketTable 不变量

- BT-001 magic bytes（D-240）：`open` 必须校验 `file[0..8] == b"PLBKT\0\0\0"`；不匹配返回 `BucketTableError::Corrupted { offset: 0, reason: "magic bytes mismatch" }`。
- BT-002 schema_version 拒绝路径（D-246）：v1 reader 必须显式拒绝 `schema_version > 1` 文件，返回 `BucketTableError::SchemaMismatch { expected: 1, got: <found> }`。
- BT-003 feature_set_id 拒绝路径：`open` 校验 `feature_set_id` 是否在当前 reader 支持的集合内；不支持返回 `BucketTableError::FeatureSetMismatch`。stage 2 默认 reader 支持 `{1}`（EHS² + OCHS(N=8)）。
- BT-004 BLAKE3 trailer eager 校验（D-243）：`open` 必须计算 `BLAKE3(file_body[..len-32])` 并与 `file_body[len-32..len]` 比对；不匹配返回 `BucketTableError::Corrupted { offset: len-32, reason: "blake3 trailer mismatch" }`。
- BT-005 bucket id 范围：`lookup(street, ...)` 返回 `Some(bucket_id)` 时 `bucket_id < bucket_count(street)`；preflop `bucket_id < 169`。
- BT-006 deterministic：同 mmap 文件多次 `lookup(...)` 调用结果 byte-equal；`content_hash()` 多次调用结果完全相同。
- BT-007 byte flip 安全（validation §5）：任意单字节翻转后 `open()` 必须返回 `BucketTableError::*` 而非 panic（除非翻转的是 padding，由 BLAKE3 trailer 检测）。`tests/bucket_table_corruption.rs` 100k 次 byte flip 0 panic。变长段绝对偏移表（D-244 §⑨）让 reader 在 byte-flip 命中 size 字段时也能从偏移读 bound 而非累积 size 推算 → 不会出现 mmap 越界 panic。
- BT-008 header 偏移表完整性（D-244 §⑨；F13 修复）：`centroid_metadata_offset` / `centroid_data_offset` / `lookup_table_offset` 必须严格递增、每个 ≥ 80（header end）、每个 ≤ `len - 32`（trailer start）、每个 8-byte 对齐；任一违反返回 `BucketTableError::Corrupted { offset: <field offset>, reason: "section offset invariant violated" }`。`bucket_count(street) > 10_000` / `n_canonical(street) > 2^24` / `n_dims != 9 (for feature_set_id=1)` 同样返回 `Corrupted`。

---

## 5. 训练 CLI（`tools/train_bucket_table.rs`）

```rust
/// CLI entry point。从 RngSource seed → equity Monte Carlo → k-means clustering →
/// 写出 mmap bucket table 二进制文件。
///
/// 用法（伪代码）：
/// ```bash
/// cargo run --release --bin train_bucket_table -- \
///     --seed 0xCAFEBABE \
///     --flop 500 --turn 500 --river 500 \
///     --output artifacts/bucket_table_{git_short}_{config}.bin
/// ```
pub fn main() -> Result<(), TrainError>;

#[derive(Debug, thiserror::Error)]
pub enum TrainError {
    #[error("invalid CLI args: {0}")]
    InvalidArgs(String),
    #[error("clustering failed: {0}")]
    ClusteringFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("bucket table error: {0}")]
    BucketTable(#[from] BucketTableError),
}
```

CLI 不属公开 API surface（不被 `lib.rs` re-export）；签名变化不需要 `API-NNN-revM`，由 stage 2 [实现] / [测试] agent 在 PR 中自由迭代。但 **CLI 输入参数 → 输出 bucket table** 的契约（同 `(seed, BucketConfig)` 输出 byte-equal）受 D-237 / D-243 约束。

---

## 6. 模块导出（顶层 `lib.rs` 追加）

```rust
// 阶段 1 既有（不动，仅作上下文）
pub mod core;
pub mod rules;
pub mod eval;
pub mod history;
pub mod error;

// 阶段 2 新增 ★
pub mod abstraction;

// 阶段 1 既有 re-export 不动；以下为阶段 2 顶层 re-export（D-253）
pub use abstraction::action::{
    AbstractAction, AbstractActionSet, ActionAbstraction, ActionAbstractionConfig,
    BetRatio, ConfigError, DefaultActionAbstraction,
};
pub use abstraction::info::{BettingState, InfoAbstraction, InfoSetId, StreetTag};
pub use abstraction::preflop::PreflopLossless169;
pub use abstraction::postflop::PostflopBucketAbstraction;
pub use abstraction::equity::{EquityCalculator, MonteCarloEquity};
pub use abstraction::bucket_table::{BucketConfig, BucketTable, BucketTableError};
```

`abstraction::cluster` / `abstraction::feature` / `abstraction::map` 子模块**不**顶层 re-export（D-254 内部子模块隔离）；`abstraction::map` 子模块通过 `PreflopLossless169::map` / `PostflopBucketAbstraction::map` 间接对外，`cluster` 仅由 `tools/train_bucket_table.rs` 引用。**例外**（D-228 公开 contract）：`abstraction::cluster::rng_substream` 模块顶层暴露 `derive_substream_seed(master_seed, op_id, sub_index) -> u64` 函数 + 全部 `op_id` 命名常量（`OCHS_WARMUP` / `CLUSTER_MAIN_FLOP / TURN / RIVER` / `KMEANS_PP_INIT_*` / `EMPTY_CLUSTER_SPLIT_*` / `EQUITY_MONTE_CARLO` / `EHS2_INNER_EQUITY_*` / `OCHS_FEATURE_INNER`），便于 `tests/clustering_determinism.rs` 等 [测试] 独立构造 sub-stream 验证 byte-equal。该 sub-module 走 `pub use abstraction::cluster::rng_substream;` 从 `abstraction::mod.rs` 暴露，但内部 k-means / EMD / 特征计算实现仍保持模块私有。

---

## 7. 与阶段 1 类型的桥接

阶段 2 不修改 stage 1 类型；通过以下便捷函数桥接：

```rust
impl InfoSetId {
    /// 便捷构造：从 GameState + hole + 抽象层 → InfoSetId。
    /// 等价于 `abs.map(state, hole)`，仅作为 driver 代码的 ergonomic helper。
    pub fn from_game_state<A: InfoAbstraction>(
        state: &GameState,
        hole: [Card; 2],
        abs: &A,
    ) -> InfoSetId;
}

impl AbstractAction {
    /// `AbstractAction` → 实际可 apply 的 `Action`（stage 1 类型）。**无状态**——
    /// `AbstractAction::Bet` / `Raise` 在构造时已由 stage 1 `LegalActionSet` 区分，
    /// 转换无歧义。映射规则：
    /// - `Fold`            → `Action::Fold`
    /// - `Check`           → `Action::Check`
    /// - `Call { .. }`     → `Action::Call`（stage 1 `Action::Call` 不带 `to`，跟注金额由
    ///                        state machine 推导，等同于 stage 1 §2 行为）
    /// - `Bet { to, .. }`  → `Action::Bet { to }`
    /// - `Raise { to, .. }`→ `Action::Raise { to }`
    /// - `AllIn { .. }`    → `Action::AllIn`（stage 1 状态机自动归一化，`to` 字段作为
    ///                        InfoSet 编码标签即可丢弃）
    pub fn to_concrete(self) -> Action;
}
```

这些桥接函数仅做字段提取 / 转换，不引入语义层。stage 4 CFR driver 通过 `AbstractAction::to_concrete().apply(&mut state)?` 路径在 mock 抽象树上前进；该路径无需 `&GameState` 入参，由 `AbstractAction` 的 `Bet` / `Raise` 拆分自身保证语义正确。

---

## 8. 端到端示例（doc test 占位）

A1 [实现] 落以下 doc test 占位（`unimplemented!()` 的代码块仍可通过 `cargo doc`，但 `cargo test --doc` 在 B2 [实现] 完整后启用）：

```rust
/// # Example（B2 / C2 完成后启用）
///
/// ```ignore
/// use poker::*;
/// use std::sync::Arc;
///
/// let evaluator: Arc<dyn HandEvaluator> = Arc::new(NaiveHandEvaluator::new());
/// let action_abs = DefaultActionAbstraction::default_5_action();
/// let preflop_abs = PreflopLossless169::new();
/// let bucket_table = BucketTable::open("artifacts/bucket_table_demo.bin")?;
/// let postflop_abs = PostflopBucketAbstraction::new(bucket_table);
///
/// let config = TableConfig::default_6max_100bb();
/// let state = GameState::new(&config, /* seed = */ 42);
///
/// // Action abstraction
/// let actions: AbstractActionSet = action_abs.abstract_actions(&state);
/// assert!(!actions.is_empty());
///
/// // Information abstraction（preflop 路径）
/// let hole = state.players()[0].hole_cards.unwrap();
/// let info_id: InfoSetId = preflop_abs.map(&state, hole);
/// assert_eq!(info_id.street_tag(), StreetTag::Preflop);
/// ```
```

---

## 9. API 修改流程

继承阶段 1 §11 API-100 ~ API-102 流程：

- **API-300** 任何对本文档已定义签名的修改必须在本文档以追加 `API-NNN-revM` 条目记录，**不删除原条目**
- **API-301** 修改若影响 `BucketTable` 二进制兼容性，必须 bump `BucketTable.schema_version` 并提供升级器（继承 stage 1 API-101 精神，`BucketTable.schema_version` 替代 `HandHistory.schema_version`）
- **API-302** 修改 PR 必须经过决策者 review；测试 agent 与实现 agent 同时被 cc

阶段 1 API（API-001..API-099）的修改仍走 `pluribus_stage1_api.md` §11 流程，**不**走本文档的 API-300。

### 修订历史

阶段 2 实施过程中的 API 修订按时间线追加到本节，遵循阶段 1 §11 修订历史 同样 "追加不删" 约定。

格式参考 stage 1 `API-001-rev1`（`HandHistory::replay` / `replay_to` 返回类型由 `Result<_, RuleError>` 改为 `Result<_, HistoryError>`）。

（本节首条由 stage 2 [实现] agent 在 B2 / C2 / D2 / E2 / F2 任一阶段发现 API 不够用、需要修订时填入。stage 2 A0 关闭时本节为空。）

---

## 10. 与决策文档的对应关系

本文档每个类型 / 字段 / 不变量都可追溯到 `pluribus_stage2_decisions.md` 中的某条 `D-NNN`。如发现不一致，以 `pluribus_stage2_decisions.md` 为准，本文档同步修正。

| 本文档段落 / 类型 | 关联决策（stage 2） | 关联决策（stage 1，只读继承） |
|---|---|---|
| §1 `AbstractAction` / `AbstractActionSet` / `ActionAbstractionConfig` | D-200 ~ D-209 | D-026（无浮点的运行时映射约束） |
| §1 `BetRatio` 整数化 | D-200 / D-202 / D-207 | D-026（避免 `f64` 进入 `Eq`/`Hash`） |
| §1 `DefaultActionAbstraction::abstract_actions` 不变量（AA-001..AA-008） | D-200 ~ D-209 / D-273 | D-026 / D-027 |
| §1 `map_off_tree` PHM stub | D-201 | — |
| §2 `InfoSetId` 统一 64-bit layout + `BettingState` enum + `StreetTag` enum | D-212 / D-215 / D-216 | — |
| §2 `PreflopLossless169` | D-210 / D-211 / D-212 / D-215 / D-217 | — |
| §2 `PostflopBucketAbstraction` | D-213 / D-214 / D-215 / D-216 / D-218 / D-219 | — |
| §2 IA-002 betting_state 区分 BB-after-limp vs first-in-non-BB | D-212 / D-215 | — |
| §2 IA-005 postflop 不依赖 preflop key | D-219 | — |
| §2 IA-007 reserved 位为零 | D-215 | — |
| §3 `EquityCalculator` / `MonteCarloEquity` | D-220 / D-220a / D-221 / D-222 / D-223 / D-224 / D-227 | API-005 `RngSource`（继承）；§6 `HandEvaluator`（继承） |
| §3 EQ-001 反对称容差 | D-220 / D-220a | — |
| §3 EQ-003 EHS² rollout | D-227 | — |
| §6 `abstraction::cluster::rng_substream` 公开 contract（`derive_substream_seed` + op_id 表）| D-228 | D-028（stage 1 deck-dealing 协议同型）/ D-027 / D-050（显式 RngSource） |
| §4 `BucketTable` 文件 layout（含 80-byte header 偏移表）| D-240 ~ D-249（含 D-236b 重编号后的 lookup 写入顺序）| D-053（BLAKE3）；D-061 ~ D-066（schema 兼容精神） |
| §4 `BucketConfig` + `ConfigError::BucketCountOutOfRange` | D-213 / D-214 | — |
| §4 `BucketTableError` 5 类 | D-247 | API §8 `RuleError` / `HistoryError` 同型 |
| §4 BT-007 byte flip 安全 | D-243 / D-247 | API §10 PB-002 from_proto 拒绝路径同型 |
| §4 BT-008 header 偏移表完整性 | D-244 §⑨ | — |
| §5 `tools/train_bucket_table.rs` | D-237 / D-242 | — |
| §6 `lib.rs` 顶层 re-export | D-253 / D-254 | API §9（继承） |
| §7 `InfoSetId::from_game_state` / `AbstractAction::to_concrete` | D-272（不修改 stage 1 API） | API §1（`Card` / `ChipAmount`）/ §2（`Action`）/ §4（`GameState`） |
| §8 doc test 占位 | D-273 / API-302 | — |

---

## 参考资料

- 阶段 2 决策记录：`pluribus_stage2_decisions.md`
- 阶段 2 验收门槛：`pluribus_stage2_validation.md`
- 阶段 2 实施流程：`pluribus_stage2_workflow.md`
- 阶段 1 API 契约（只读继承）：`pluribus_stage1_api.md`
- 阶段 1 决策记录（只读继承）：`pluribus_stage1_decisions.md`
- prost / thiserror（继承 stage 1）：https://github.com/tokio-rs/prost / https://github.com/dtolnay/thiserror
- memmap2（D-255 stage 2 新增）：https://github.com/RazrFalcon/memmap2-rs
- BLAKE3（D-243 自校验）：https://github.com/BLAKE3-team/BLAKE3

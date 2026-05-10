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

    /// 量化协议（D-202-rev1 / BetRatio::from_f64-rev1）：
    /// 1. **rounding mode**：bankers-rounding (half-to-even)，
    ///    `(ratio * 1000.0).round_ties_even() as i64`，再校验范围。
    /// 2. **合法范围**：`ratio ∈ [0.001, 4_294_967.295]`（含端点），
    ///    量化后 `u32 ∈ [1, u32::MAX]`；越界（< 0.001 / > 4_294_967.295 /
    ///    NaN / Inf / 负数 / 0.0）返回 `None`。
    /// 3. **重复处理**：本函数本身不去重；多输入量化到同一 milli 值由
    ///    `ActionAbstractionConfig::new` 检测，返回 `ConfigError::DuplicateRatio`。
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

    /// 自定义构造。长度 / 范围越界 / 量化后 milli 重复均返回 `ConfigError`
    /// （见 §9 BetRatio::from_f64-rev1 量化协议；D-202-rev1）。
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
    /// 多个 `raise_pot_ratios` 元素经 `BetRatio::from_f64` 量化后落到同一 milli 值
    /// （D-202-rev1 / BetRatio::from_f64-rev1）。caller 责任去重，避免 D-209
    /// 输出顺序与 `raise_count()` 不一致。
    #[error("duplicate raise pot ratio after quantization: milli = {milli}")]
    DuplicateRatio { milli: u32 },
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
- AA-003-rev1 `Bet/Raise(x×pot)` fallback 优先级（D-205 / §9 AA-003-rev1）：bet vs raise 由 stage 1 `LegalActionSet`（LA-002）选定。按 first-match-wins 顺序：① 计算 `candidate_to = ceil(max_committed_this_round + ratio × pot_after_call)`（D-203）；② 若 `candidate_to < min_to`，则 `candidate_to ← min_to`；③ 若 `candidate_to >= committed_this_round + stack`，**整动作改为** `AllIn { to = committed_this_round + stack }`，跳过后续；④ 否则输出 `Bet { to = candidate_to }` / `Raise { to = candidate_to }`。两条件同时触发（`min_to >= committed_this_round + stack`）时走 `AllIn`。
- AA-004-rev1 折叠去重（D-206-rev1 / §9 AA-004-rev1）：抽象动作集合中不同 `AbstractAction` 实例的 `to` 字段必须互不相等（除 `Fold` / `Check` 不带 `to`）。优先级（first-match-wins）：① **`AllIn` 优先级最高**——若任何带 `to` 的候选（`Call` / `Bet` / `Raise`）`to == committed_this_round + stack`，整组合并为 `AllIn { to }`；典型场景：all-in call（`Call { to=X }` 与 `AllIn { to=X }` 同候选）保留 `AllIn` 不保留 `Call`；② `Bet/Raise(0.5×) vs Bet/Raise(1.0×)` 相同 `to` 时保留 ratio_label 较小的一份；③ `Call` 与 `Bet/Raise` 严格不等（D-034 / D-035 数值约束保证 `min_to > max_committed`），不会折叠。
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
    /// **前置条件**（IA-006-rev1 / §9）：`state.current_player().is_some()`
    /// （非 terminal、非 all-in 跳轮）。违反前置条件 panic（debug + release 一致，
    /// 与 stage 1 `ChipAmount::Sub` 同型）。caller 必须在 CFR / 实时搜索 driver 中
    /// 先判断 `state.current_player().is_none()` 跳过 InfoSet 编码——terminal 局面
    /// 没有 actor 决策点，InfoSet 概念不可达。
    ///
    /// **stack_bucket 来源**（D-211-rev1 / §9）：实现必须从 `state.config()`
    /// 引用 + `state.actor_seat()` 计算 `effective_stack_at_hand_start`，**不允许**
    /// 从 `state.player(seat).stack`（当前剩余筹码）推算。同手内 preflop / flop /
    /// turn / river 调用结果 `stack_bucket` 字段 byte-equal。如 stage 1 `GameState`
    /// 当前未公开 `config()` getter，B2 [实现] 在落地 `InfoAbstraction::map` 实际
    /// 逻辑时必须走 stage 1 `API-NNN-revM` 流程在 `pluribus_stage1_api.md` 添加
    /// 只读 getter（A1 阶段仅产签名，`_state` 未取用，签名编译不依赖该 getter，
    /// 不触发该 rev；详见 §修订历史 batch 7）。
    ///
    /// 整条调用路径**禁止浮点**（D-273 / D-252）；postflop 走 mmap bucket lookup
    /// 命中整数 bucket id；preflop 走组合 lookup 表。
    fn map(&self, state: &GameState, hole: [Card; 2]) -> InfoSetId;
}
```

### canonical_hole_id（公开 helper，D-218-rev1）

```rust
// module: abstraction::preflop
/// preflop hole 单维 canonical id ∈ 0..1326（花色对称归一化）。
/// `BucketTable::lookup(StreetTag::Preflop, _)` 入参由本函数计算。
pub fn canonical_hole_id(hole: [Card; 2]) -> u32;
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

### canonical_observation_id（公开 helper，D-218-rev1）

```rust
// module: abstraction::postflop
/// postflop 联合 (board, hole) canonical observation id ∈
/// 0..n_canonical_observation(street)（花色对称等价类）。
/// `BucketTable::lookup(street, _)` postflop 入参由本函数计算。
///
/// 仅对 `StreetTag::{Flop, Turn, River}` 有效（`board.len() ∈ {3, 4, 5}`）；
/// `StreetTag::Preflop` 调用 panic（caller 应改用 `canonical_hole_id`）。
pub fn canonical_observation_id(
    street: StreetTag,
    board: &[Card],
    hole: [Card; 2],
) -> u32;
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
    /// 内部走 `canonical_observation_id(street, board, hole)` → `BucketTable::lookup`。
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
- IA-006-rev1 街隔离 + 前置条件（§9 IA-006-rev1）：`map(state, hole)` 必须根据 `state.street()` 选择 preflop 或 postflop 路径；前置条件 `state.current_player().is_some()` 必须满足，违反即 panic。Showdown 街 / 全员 all-in 跳轮 / fold-out 等任一 terminal-or-no-actor 局面下调用 `map` 是 caller bug——CFR / search driver 在 leaf evaluation 用 `payouts()` 直接计算回报，不需要 InfoSetId。
- IA-007 InfoSetId reserved 位为零（D-215）：`InfoSetId.raw()` 的 bit 38..64（26 bit）必须全为 0；任一非零 bit 写入是 P0 阻塞 bug。`tests/info_id_encoding.rs` 全枚举 typical state space 断言此不变量。

---

## 3. Equity calculator（`module: abstraction::equity`）

```rust
/// Equity 计算 trait。**仅离线 clustering 训练路径** 使用；运行时映射禁止触发
/// （D-225）。`f64` 出现在本 trait 是显式允许的——本路径在 `abstraction::equity`
/// / `abstraction::cluster` 子模块，与 `abstraction::map` 子模块（禁浮点，D-252）
/// 物理隔离。
///
/// **错误返回**（EquityCalculator-rev1 / §9）：4 个方法均返回
/// `Result<_, EquityError>`，把无效输入（重叠 / 板长非法 / iter=0 / 内部错误）
/// 与合法 `Ok` 分流；EQ-002 finite invariant 仅适用于 `Ok` 路径。
pub trait EquityCalculator: Send + Sync {
    /// **hand-vs-uniform-random-hole** equity（EHS 路径，D-223）。对手 hole
    /// uniform over remaining cards。`Ok(x)` 时 `x ∈ [0.0, 1.0]` 且 finite
    /// （D-224 / EQ-002-rev1）。
    ///
    /// **不**满足反对称：`equity(A, board) + equity(B, board) ≠ 1`。EQ-001
    /// 反对称断言不要用本接口；用 `equity_vs_hand`。
    ///
    /// 错误：`InvalidBoardLen` / `OverlapBoard` / `IterTooLow` / `Internal`。
    fn equity(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError>;

    /// **pairwise** hand-vs-specific-hand equity（D-220a / EQ-001 反对称路径
    /// 唯一接口；OCHS 内部计算的基本原语，D-223）。`Ok(x)` 时 `x ∈ [0.0, 1.0]`
    /// 且 finite（D-224 / EQ-002-rev1），含 ties counted as 0.5。
    ///
    /// 计算口径：
    /// - **river**（`board.len() == 5`）：直接评估两手牌力，1.0 / 0.5 / 0.0
    ///   三值离散。无 RNG 消费。
    /// - **turn**（`board.len() == 4`）：枚举 44 张未发 river 卡（`52 - 2 hole -
    ///   2 opp_hole - 4 board = 44`），每个补完后 river-level pairwise 平均。
    ///   无 RNG 消费，确定性。
    /// - **flop**（`board.len() == 3`）：枚举 `C(45, 2) = 990` 个 (turn, river)
    ///   无序对（`52 - 2 hole - 2 opp_hole - 3 board = 45` 张未发，选 2）。
    ///   无 RNG 消费，确定性。注意此处枚举数比 `ehs_squared` 的 1081 少 2 张
    ///   （opp_hole 占用），是 EQ-001 antisymmetry 测试与 EQ-003 EHS² rollout
    ///   的关键差异。
    /// - **preflop**（`board.len() == 0`）：outer Monte Carlo over 5-card 完整
    ///   公共牌组合（`C(48, 5) ≈ 1.7M` 太大不可全枚举）；消费 RngSource，
    ///   sub-stream 派生协议见 D-228 `EQUITY_MONTE_CARLO`。preflop 反对称严格
    ///   容差路径需要**两个独立 RngSource，从同一 sub_seed 构造**（详见
    ///   §9 EQ-001-rev1）；顺序复用同一 `&mut rng` 不满足严格反对称。
    ///
    /// 错误：`opp_hole` 与 `hole` 重叠 → `OverlapHole`；`opp_hole` 或 `hole` 与
    /// `board` 重叠 → `OverlapBoard`；`board.len() ∉ {0, 3, 4, 5}` → `InvalidBoardLen`。
    fn equity_vs_hand(
        &self,
        hole: [Card; 2],
        opp_hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError>;

    /// EHS²（potential-aware 二阶矩，D-223）。
    /// `Ok(x)` 时 `x ∈ [0.0, 1.0]`。river 状态退化为 `equity²`。
    fn ehs_squared(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError>;

    /// OCHS 向量。长度 = `n_opp_clusters`（D-222 默认 8）。
    /// `Ok(v)` 时 `v.len() == n_opp_clusters` 且每维 `∈ [0.0, 1.0]` 且 finite。
    ///
    /// 内部以 `equity_vs_hand` 为原语：每个 cluster k 的输出值 ≈
    /// `mean over opp ∈ cluster_k of equity_vs_hand(hole, opp, board, rng)`，
    /// 具体抽样 / 枚举策略由 [实现] 在 A1 / B2 / C2 选定（D-222 锁 N=8 + RngSource
    /// sub-stream 派生 D-228 `OCHS_FEATURE_INNER`）。
    fn ochs(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<Vec<f64>, EquityError>;
}

/// equity 错误（EquityCalculator-rev1 / D-224-rev1）。继承 stage 1
/// `RuleError` / `HistoryError` 同型 thiserror 设计；`Err` 路径不进入 feature /
/// bucket 写入，由 caller 用 `?` 操作符传播。
#[derive(Debug, thiserror::Error)]
pub enum EquityError {
    /// `opp_hole` 与 `hole` 重叠（同张牌）。
    #[error("opp_hole overlaps with hole: card {card:?}")]
    OverlapHole { card: Card },

    /// `hole` 或 `opp_hole` 与 `board` 重叠。
    #[error("hole or opp_hole overlaps with board: card {card:?}")]
    OverlapBoard { card: Card },

    /// `board.len() ∉ {0, 3, 4, 5}`。
    #[error("invalid board length: expected 0/3/4/5, got {got}")]
    InvalidBoardLen { got: usize },

    /// Monte Carlo `iter == 0`。默认 D-220 = 10_000 不触发，stage 4 消融可触发。
    #[error("Monte Carlo iter too low: expected >= 1, got {got}")]
    IterTooLow { got: u32 },

    /// 评估器内部错误透传（继承 stage 1 `HandEvaluator` 错误，可能性极低）。
    #[error("equity evaluator internal error: {0}")]
    Internal(String),
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

- EQ-001-rev1 反对称容差（D-220a-rev1，**pairwise 路径**；§9）：使用 `equity_vs_hand(A, B, board, rng)` 接口（**不**用 `equity(hole, board, rng)`——后者 random-opp 不满足反对称）。容差按街分流：① **postflop**（`board.len() ≥ 3`）：确定性枚举无 RNG 消费，`|r_ab + r_ba - 1| ≤ 1e-9`（IEEE-754 reorder 容忍）；② **preflop strict**（`board.len() == 0`）：必须用**两个独立 `RngSource`，各自从同一 `derive_substream_seed(master_seed, EQUITY_MONTE_CARLO, sub_index)` 构造**——`|r_ab + r_ba - 1| ≤ 1e-9`；③ **preflop noisy**（不同 sub_seed）：容忍 ≤ 0.005（10k iter）/ ≤ 0.02（1k iter）。**禁止模式**：顺序复用同一 `&mut rng` 调用两次后做严格反对称断言（第二次调用看到推进后 RngSource state，采到不同 future board，sum != 1）。`tests/equity_self_consistency.rs` 必须先走 postflop 严格容差路径，preflop strict / noisy 路径单独命名。
- EQ-002-rev1 finite 范围（D-224-rev1 / §9）：合法输入下返回 `Ok(x)` 时 `x ∈ [0.0, 1.0]` 且 finite；`ochs` 返回 `Ok(v)` 时 `v.len() == n_opp_clusters` 且每维 `∈ [0.0, 1.0]` 且 finite。`Err(EquityError::*)` 路径不进入 feature / bucket 写入，由 caller 在 clustering 训练前用 `?` 传播。任何 NaN / Inf 出现在 `Ok` 路径是 P0 阻塞 bug。
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
/// offset 0x1C: n_canonical_observation_flop:   u32 LE                 // D-218-rev1 / D-244-rev1 / F19
/// offset 0x20: n_canonical_observation_turn:   u32 LE
/// offset 0x24: n_canonical_observation_river:  u32 LE
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
/// //   flop:     [u32 LE; n_canonical_observation_flop]               // D-244-rev1
/// //   turn:     [u32 LE; n_canonical_observation_turn]
/// //   river:    [u32 LE; n_canonical_observation_river]
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

    /// `(street, observation_canonical_id) → bucket_id`（BT-005-rev1 / D-216-rev1
    /// / D-218-rev1 / §9）。
    ///
    /// `observation_canonical_id` 来源：
    /// - **preflop**（`StreetTag::Preflop`）：= `canonical_hole_id(hole)` ∈ 0..1326；
    ///   不需要 board（preflop board 为空）。
    /// - **postflop**（`Flop` / `Turn` / `River`）：= `canonical_observation_id(street,
    ///   board, hole)` ∈ 0..n_canonical_observation(street)；联合 (board, hole)
    ///   花色对称等价类。详见 §2 helper 函数。
    ///
    /// 越界返回 `None`（`observation_canonical_id >= n_canonical_observation(street)`
    /// 或 preflop `>= 1326`）。
    ///
    /// **接口接 `StreetTag`（不接 stage 1 `Street`）**——`StreetTag` 仅含 4 个 betting
    /// 街变体，不含 `Showdown`。caller 必须在调用前把 `Street::Showdown` 局面分流
    /// （Showdown 不存在 InfoSet 决策点，调用 `lookup` 是语义错误）。
    pub fn lookup(
        &self,
        street: StreetTag,
        observation_canonical_id: u32,
    ) -> Option<u32>;

    pub fn schema_version(&self) -> u32;
    pub fn feature_set_id(&self) -> u32;
    pub fn config(&self) -> BucketConfig;
    pub fn training_seed(&self) -> u64;

    /// 每条街 bucket 数；`StreetTag::Preflop` 固定返回 169。
    pub fn bucket_count(&self, street: StreetTag) -> u32;

    /// 每条街联合 (board, hole) canonical observation id 总数（D-244-rev1）：
    /// preflop 固定返回 1326；postflop 返回 header `n_canonical_observation_<street>`。
    pub fn n_canonical_observation(&self, street: StreetTag) -> u32;

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
- BT-005-rev1 bucket id 范围（§9）：`lookup(street, observation_canonical_id)` 返回 `Some(bucket_id)` 时 `bucket_id < bucket_count(street)`；preflop `bucket_id < 169`；postflop `bucket_id < BucketConfig.{street}`。`observation_canonical_id >= n_canonical_observation(street)` 时返回 `None`（preflop `>= 1326` 同样返回 `None`）。
- BT-006 deterministic：同 mmap 文件多次 `lookup(...)` 调用结果 byte-equal；`content_hash()` 多次调用结果完全相同。
- BT-007 byte flip 安全（validation §5）：任意单字节翻转后 `open()` 必须返回 `BucketTableError::*` 而非 panic（除非翻转的是 padding，由 BLAKE3 trailer 检测）。`tests/bucket_table_corruption.rs` 100k 次 byte flip 0 panic。变长段绝对偏移表（D-244 §⑨）让 reader 在 byte-flip 命中 size 字段时也能从偏移读 bound 而非累积 size 推算 → 不会出现 mmap 越界 panic。
- BT-008-rev1 header 偏移表完整性（D-244-rev1 / §9）：`centroid_metadata_offset` / `centroid_data_offset` / `lookup_table_offset` 必须严格递增、每个 ≥ 80（header end）、每个 ≤ `len - 32`（trailer start）、每个 8-byte 对齐；任一违反返回 `BucketTableError::Corrupted { offset: <field offset>, reason: "section offset invariant violated" }`。`bucket_count(street) > 10_000` / `n_canonical_observation_<street>` 越界（保守上界：flop ≤ 2_000_000 / turn ≤ 20_000_000 / river ≤ 200_000_000，A1 实测后可收紧）/ `n_dims != 9 (for feature_set_id=1)` 同样返回 `Corrupted`。

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

// 阶段 1 既有 re-export 不动；以下为阶段 2 顶层 re-export（D-253-rev1）
pub use abstraction::action::{
    AbstractAction, AbstractActionSet, ActionAbstraction, ActionAbstractionConfig,
    BetRatio, ConfigError, DefaultActionAbstraction,
};
pub use abstraction::info::{BettingState, InfoAbstraction, InfoSetId, StreetTag};
pub use abstraction::preflop::{canonical_hole_id, PreflopLossless169};
pub use abstraction::postflop::{canonical_observation_id, PostflopBucketAbstraction};
pub use abstraction::equity::{EquityCalculator, EquityError, MonteCarloEquity};
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

#### A0 review 修正 batch 6（2026-05-09，A1 起步前）

A0 关闭后另一轮 review 暴露 9 处独立 spec drift（编号 F19..F27），其中 7 处涉及 API 签名 / 不变量收紧（其余 2 处为决策侧文字）。决策侧 rev 条目见 `pluribus_stage2_decisions.md` §修订历史 batch 6；本节落地 API 侧同步修订。本 batch 与决策侧同 commit 落地，目的：避免 A1 [实现] 起步后再回头修订 API 契约（继承 stage-1 §A-rev0 carry forward "决策与 API 同 commit 同步" 处理政策）。

##### AA-003-rev1（2026-05-09，F24）

**背景**：API §1 AA-003 仅写 "若 `x×pot < min_to`，输出 `Bet { to = min_to }` / `Raise { to = min_to }`（不剔除）；若 `x×pot >= committed_this_round + stack`，输出 `AllIn { to = committed_this_round + stack }`"，**未明确**两条件同时触发（`min_to >= committed_this_round + stack`，典型场景：BB 短码 + 链式加注后 min_to 已超 stack）时的优先级。决策侧 D-205 原文有完整优先级（first-match-wins ① → ②），API 侧漏写。

**新规则**：AA-003 同步 D-205 完整优先级（first-match-wins）：

```text
1. 计算 candidate_to = ceil(max_committed_this_round + ratio * pot_after_call_size)
   (D-203 pot 定义；向上取整到 chip)

2. ratio fallback 顺序判定：
   ① 若 candidate_to < min_to:
      candidate_to ← min_to        (D-034 / D-035 合法最小 bet/raise)
   ② 若 candidate_to >= committed_this_round + stack:
      整动作 ← AllIn { to = committed_this_round + stack }
      并跳过 ③（all-in 优先级最高）
   ③ 否则保留为 Bet { to = candidate_to } / Raise { to = candidate_to }
      (Bet/Raise 由 LegalActionSet bet_range vs raise_range 选定)

3. 经 D-206-rev1 全局折叠去重（含 Call/AllIn 合并）后输出。
```

**等价口语化**：先 floor 到 `min_to` 把短期 ratio 升到合法下限，再 ceil 到 `committed + stack` 把超 stack 的合法 raise 折叠到 `AllIn`；两步顺序固定（先 floor 再 ceil），同时触发时（`min_to >= committed + stack`）走 `AllIn`。

**影响**：① 不影响公开签名；② AA-003 不变量文字同步收紧；③ B1 [测试] `tests/action_abstraction.rs` 阶段 2 版必须含 "短码 BB 面对 3-bet → min_to 超 stack → 输出 AllIn" 至少 2 个 case 断言此优先级。

##### AA-004-rev1（2026-05-09，F20）

**背景**：API §1 AA-004 仅说 "抽象动作集合中不同 `AbstractAction` 实例的 `to` 字段必须互不相等（除 `Fold` / `Check` 不带 `to`）"，但 D-206 原文只覆盖 `Bet/Raise(0.5×) / Bet/Raise(1.0×) / AllIn` 三者去重，**未约束** `Call { to }` 与 `AllIn { to }` 在相同 `to` 值（all-in call 场景）下的优先级——AA-004 因此存在矛盾。

**新规则**（同步 D-206-rev1）：扩展 AA-004 折叠优先级到全部带 `to` 的 `AbstractAction` 实例：

1. **`AllIn` 优先级最高**：若任何带 `to` 的候选（`Call` / `Bet` / `Raise`）的 `to == committed_this_round + stack`（即等价于 all-in），整组合并为 `AllIn { to }`；候选 `Call { to }` / `Bet { to }` / `Raise { to }` 全部消失。
2. **`Bet/Raise(0.5×) vs Bet/Raise(1.0×)`**：相同 `to` 时保留 ratio_label 较小的一份。
3. **`Call` 与 `Bet/Raise`**：`to` 严格不等（D-034 / D-035 数值约束保证 `min_to > max_committed`），不会折叠。

**不变量收紧**：经上述优先级处理后，`AbstractActionSet` 内任意两个带 `to` 的实例 `to` 字段严格不等。**等价**：当 `Call { to=X }` 与 `AllIn { to=X }` 同时候选时（all-in call），保留 `AllIn` 不保留 `Call`。

**影响**：① AA-001 D-209 输出顺序中 `Call` 槽位若被 `AllIn` 优先级吸收则**消失**（`?` 跳过），输出仍按 D-209 顺序；② `tests/scenarios_extended.rs` 阶段 2 版必须含至少 2 条 all-in call 场景（短码 BTN call 大 raise / 短码 BB call 3-bet）断言 `Call` 不出现而 `AllIn` 出现；③ `to_concrete()` 路径不变（`AllIn { to }.to_concrete() = Action::AllIn`）。

##### BetRatio::from_f64-rev1（2026-05-09，F27）

**背景**：API §1 `BetRatio::from_f64(ratio: f64) -> Option<BetRatio>` 仅说 "ratio × 1000 的 u32"，但小数舍入模式（round / floor / ceil）/ 越界处理（< 0.001 / > 4.29M / NaN / Inf / 负 / 0）/ 多输入映射到同一 milli 值的去重均未定义。

**新规则**（同步 D-202-rev1）：

```rust
impl BetRatio {
    /// 量化协议（D-202-rev1）：
    /// 1. rounding mode: bankers-rounding (half-to-even)
    ///    `(ratio * 1000.0).round_ties_even() as i64`，再校验范围。
    /// 2. 合法范围：`ratio ∈ [0.001, 4_294_967.295]`（含端点，量化后 u32 ∈ [1, u32::MAX]）；
    ///    越界（< 0.001 / > 4_294_967.295 / NaN / Inf / 负数 / 0.0）返回 None。
    /// 3. 重复处理：本函数本身不去重；多输入量化到同一 milli 值由
    ///    `ActionAbstractionConfig::new` 检测，返回
    ///    `ConfigError::DuplicateRatio { milli }`。
    pub fn from_f64(ratio: f64) -> Option<BetRatio>;
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// 既有变体（A0 已锁定）
    RaiseCountOutOfRange(usize),
    RaiseRatioInvalid(f64),
    BucketCountOutOfRange { street: StreetTag, got: u32 },

    /// ★新增（D-202-rev1）：多个 raise_pot_ratios 元素量化后 milli 值重合。
    #[error("duplicate raise pot ratio after quantization: milli = {milli}")]
    DuplicateRatio { milli: u32 },
}
```

**影响**：① `BetRatio::from_f64` 行为锁定，[实现] 在 A1 严格按此协议；② `ConfigError` 增加 `DuplicateRatio` 变体（5 类总数变化由 4 → 5）；③ default 5-action 配置 `[BetRatio::HALF_POT, BetRatio::FULL_POT]` 不触发任何边界路径，向后兼容；④ `tests/action_abstraction.rs` 阶段 2 版必须断言 `BetRatio::from_f64(0.5005) == Some(BetRatio::HALF_POT)`（half-to-even）+ `from_f64(-1.0) == None` + `from_f64(f64::NAN) == None` + `ActionAbstractionConfig::new(vec![0.5, 0.5005]).is_err()`（duplicate after quantization）。

##### IA-006-rev1（2026-05-09，F22）

**背景**：API §2 IA-006 原文 "`map(state, hole)` 在 `state.is_terminal() == true` 时返回上一条 betting round 的 `InfoSetId`（即 river 街最后一次决策点的编码），调用方一般不会触发该路径"——但 `&GameState + hole` 双入参不携带 "上一条 betting round 最后一次决策点上下文"，且 trait 签名不返回 `Result` / `Option`，无法稳定实现。

**新规则**：terminal state 调用 `map` 改为**调用方契约违反**（caller error），由实现侧 panic（debug + release 一致，与 stage 1 `ChipAmount::Sub` 同型，D-026b 精神）：

```rust
pub trait InfoAbstraction: Send + Sync {
    /// `(GameState, hole_cards)` → InfoSet id。
    ///
    /// **前置条件**（IA-006-rev1）：`state.current_player().is_some()`（即非 terminal
    /// 且非 all-in 跳轮 state）。违反前置条件 panic（debug + release 一致）。caller
    /// 责任：在 CFR 训练 / 实时搜索 driver 中先判断 `state.current_player().is_none()`
    /// 跳过 InfoSet 编码 ——terminal state 下没有 actor 决策点，InfoSet 概念不可达。
    ///
    /// 整条调用路径**禁止浮点**（D-273 / D-252）；postflop 走 mmap bucket lookup
    /// 命中整数 bucket id；preflop 走组合 lookup 表。
    fn map(&self, state: &GameState, hole: [Card; 2]) -> InfoSetId;
}
```

**等价口语化**：放弃 "terminal 时返回 fallback InfoSetId" 路径，让 trait 契约保持简单——caller 必须先检查 `current_player()`。CFR / search driver 在 leaf evaluation 用 payouts() 直接计算回报，不需要 InfoSetId；fuzz 测试在 1M random hand 路径上可能触发 terminal state，必须显式跳过 `map` 调用（B1 fuzz harness 同步加 guard）。

**IA-006 不变量同步**（替换原 IA-006 文字）：
- IA-006-rev1 街隔离 + 前置条件：`map(state, hole)` 必须根据 `state.street()` 选择 preflop 或 postflop 路径；前置条件 `state.current_player().is_some()` 必须满足，违反即 panic。Showdown 街 / 全员 all-in 跳轮 / fold-out 等任一 terminal-or-no-actor 局面下调用 `map` 是 caller bug。

**影响**：① `InfoAbstraction::map` 签名不变（仍返回 `InfoSetId` 而非 `Result`），但前置条件文档化；② `tests/info_id_encoding.rs` fuzz harness 必须显式跳过 terminal state；③ A1 [实现] panic 路径用 `assert!(state.current_player().is_some(), "InfoAbstraction::map called on terminal state")`；④ 不影响 Showdown 街是否 InfoSet 不可达的语义判断（仍不可达，IA-006 原文最后半句不变）。

##### EquityCalculator-rev1 + EQ-001-rev1 + EQ-002-rev1（2026-05-09，F23 + F25）

**背景**：① API §3 `equity_vs_hand` 注释说 "重叠时返回 NaN"，但 EQ-002 / D-224 又禁止 NaN——签名层面无法同时满足；② EQ-001 原文 "fresh sub-stream from D-228" 在 `&mut dyn RngSource` 单一入参签名下不可执行（顺序复用同一 rng 推进状态后第二次采到不同 future board）。

**新规则**（同步 D-224-rev1 / D-220a-rev1）：

###### 1. trait 签名加 `Result` 包装（F23）

```rust
pub trait EquityCalculator: Send + Sync {
    /// hand-vs-uniform-random-hole equity。返回值 ∈ [0.0, 1.0]，必须 finite。
    ///
    /// 错误路径：board.len() ∉ {0, 3, 4, 5} → InvalidBoardLen；
    ///         hole 与 board 重叠 → OverlapBoard；
    ///         评估器内部错误 → Internal。
    fn equity(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError>;

    /// pairwise hand-vs-specific-hand equity（D-220a / EQ-001 反对称路径）。
    ///
    /// 错误路径：opp_hole 与 hole 重叠 → OverlapHole；
    ///         opp_hole 或 hole 与 board 重叠 → OverlapBoard；
    ///         board.len() ∉ {0, 3, 4, 5} → InvalidBoardLen。
    fn equity_vs_hand(
        &self,
        hole: [Card; 2],
        opp_hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError>;

    fn ehs_squared(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<f64, EquityError>;

    fn ochs(
        &self,
        hole: [Card; 2],
        board: &[Card],
        rng: &mut dyn RngSource,
    ) -> Result<Vec<f64>, EquityError>;
}

#[derive(Debug, thiserror::Error)]
pub enum EquityError {
    #[error("opp_hole overlaps with hole: card {card:?}")]
    OverlapHole { card: Card },

    #[error("hole or opp_hole overlaps with board: card {card:?}")]
    OverlapBoard { card: Card },

    #[error("invalid board length: expected 0/3/4/5, got {got}")]
    InvalidBoardLen { got: usize },

    #[error("Monte Carlo iter too low: expected ≥ 1, got {got}")]
    IterTooLow { got: u32 },

    #[error("equity evaluator internal error: {0}")]
    Internal(String),
}
```

###### 2. EQ-001 反对称容差路径（F25 同步）

替换原 EQ-001 文字为：

> EQ-001-rev1 反对称（D-220a-rev1）：使用 `equity_vs_hand(A, B, board, rng)` 接口（`equity(hole, board, rng)` random-opp 不满足反对称）。
>
> **postflop**（`board.len() ≥ 3`）：确定性枚举无 RNG 消费。任何 RngSource state 下：
> ```text
> let r1 = calc.equity_vs_hand(A, B, board, &mut rng).unwrap();
> let r2 = calc.equity_vs_hand(B, A, board, &mut rng).unwrap();
> assert!((r1 + r2 - 1.0).abs() <= 1e-9);
> ```
>
> **preflop strict**（`board.len() == 0`，`iter = 10_000`）：必须用**两个独立 RngSource，从同一 sub_seed 构造**：
> ```text
> let sub_seed = derive_substream_seed(master_seed, EQUITY_MONTE_CARLO, 0);
> let mut rng_ab = ChaCha20Rng::from_seed(seed_to_chacha20_seed(sub_seed));
> let mut rng_ba = ChaCha20Rng::from_seed(seed_to_chacha20_seed(sub_seed));
> let r1 = calc.equity_vs_hand(A, B, &[], &mut rng_ab).unwrap();
> let r2 = calc.equity_vs_hand(B, A, &[], &mut rng_ba).unwrap();
> assert!((r1 + r2 - 1.0).abs() <= 1e-9);
> ```
>
> **preflop noisy**（不同 sub_seed）：容忍 ≤ 0.005（10k iter）/ ≤ 0.02（1k iter）。
>
> **禁止模式**：顺序复用同一 `&mut rng` 调用两次后做严格反对称断言（第二次调用看到推进后的 RngSource state，采到不同 future board，sum != 1）。

###### 3. EQ-002 finite invariant 收紧（F23 同步）

替换原 EQ-002 文字为：

> EQ-002-rev1 finite 范围：合法输入下 `equity / ehs_squared / equity_vs_hand` 返回 `Ok(x)` 时 `x ∈ [0.0, 1.0]` 且 finite；`ochs` 返回 `Ok(v)` 时 `v.len() == n_opp_clusters` 且每维 `∈ [0.0, 1.0]` 且 finite。`Err(EquityError::*)` 路径不进入 feature / bucket 写入，由 caller 在 clustering 训练前用 `?` 操作符传播。任何 NaN / Inf 出现在 `Ok` 路径是 P0 阻塞 bug。

**影响**：① 4 个 trait 方法签名加 `Result` 包装；② 新增 `EquityError` 枚举（5 类）；③ `MonteCarloEquity` 实现签名同步加 `Result`；④ `tests/equity_self_consistency.rs` 反对称断言路径 + fuzz 路径全部用 `Result` 模式；⑤ `EquityError` 加入 §6 顶层 re-export 列表（D-253-rev1 同步）；⑥ doc test 占位（§8）暂不调整——B2 实现完整后启用时一并切到 `Result` 模式。

##### BT-005-rev1 + BucketTable::lookup 签名修订（2026-05-09，F19）

**背景**：API §4 `BucketTable::lookup(street, board_canonical_id, hole_canonical_id)` 二维入参与 D-244 lookup_table postflop 段 `[u32; n_canonical_<street>]` 单维存储不匹配——结构性表达不出 (board, hole) → bucket 映射。决策侧 D-216-rev1 / D-218-rev1 / D-244-rev1 已锁定 "联合 canonical observation id" 路径。

**新规则**（同步决策侧 batch 6）：

###### 1. `BucketTable::lookup` 签名改为单维 `observation_canonical_id`

```rust
impl BucketTable {
    /// `(street, observation_canonical_id) → bucket_id`（D-216-rev1 / D-218-rev1）。
    ///
    /// `observation_canonical_id`：
    /// - preflop（StreetTag::Preflop）：= `canonical_hole_id(hole)` ∈ 0..1326；
    ///   `board` 入参不参与（preflop board 为空）。
    /// - postflop（Flop / Turn / River）：= `canonical_observation_id(street, board, hole)`
    ///   ∈ 0..n_canonical_observation(street)；联合 (board, hole) 花色对称等价类。
    ///
    /// 越界返回 None。
    pub fn lookup(
        &self,
        street: StreetTag,
        observation_canonical_id: u32,
    ) -> Option<u32>;

    pub fn n_canonical_observation(&self, street: StreetTag) -> u32;
}
```

###### 2. 公开 helper：`canonical_observation_id` 与 `canonical_hole_id`

```rust
// module: abstraction::postflop
pub fn canonical_observation_id(
    street: StreetTag,
    board: &[Card],
    hole: [Card; 2],
) -> u32;

// module: abstraction::preflop
pub fn canonical_hole_id(hole: [Card; 2]) -> u32;  // 0..1326
```

调用约束：`canonical_observation_id` 仅对 `StreetTag::{Flop, Turn, River}` 有效（board.len() ∈ {3, 4, 5}）；`StreetTag::Preflop` 调用 panic（caller 应改用 `canonical_hole_id`）。

###### 3. BT-005-rev1 不变量收紧

替换原 BT-005 文字为：

> BT-005-rev1 bucket id 范围：`lookup(street, observation_canonical_id)` 返回 `Some(bucket_id)` 时 `bucket_id < bucket_count(street)`；preflop 返回 `bucket_id < 169`；postflop 返回 `bucket_id < BucketConfig.{street}`。`observation_canonical_id >= n_canonical_observation(street)` 时返回 `None`（preflop `>= 1326` 同样返回 `None`）。

###### 4. BT-008-rev1 偏移表完整性扩展

替换原 BT-008 文字为：

> BT-008-rev1 header 偏移表完整性（D-244-rev1）：`centroid_metadata_offset` / `centroid_data_offset` / `lookup_table_offset` 必须严格递增、每个 ≥ 80（header end）、每个 ≤ `len - 32`（trailer start）、每个 8-byte 对齐；任一违反返回 `BucketTableError::Corrupted { offset, reason: "section offset invariant violated" }`。`bucket_count(street) > 10_000` / `n_canonical_observation_<street>` 越界（保守上界：flop ≤ 2_000_000、turn ≤ 20_000_000、river ≤ 200_000_000，A1 实测后可收紧）/ `n_dims != 9 (for feature_set_id=1)` 同样返回 `Corrupted`。

###### 5. header 字段语义改名

API §4 layout 注释中的 `n_canonical_flop / turn / river`（offset 0x1C / 0x20 / 0x24）三字段语义改为 `n_canonical_observation_flop / turn / river`，offset 不变（保持 80-byte header 兼容）。

**影响**：① `lookup` 签名改为 2 入参（`street, observation_canonical_id`），原 3 入参签名作废；② `tools/bucket_table_reader.py` Python reader 同步重命名字段；③ `tests/bucket_table_corruption.rs` byte-flip 测试覆盖 `n_canonical_observation_<street>` 越界路径；④ A1 [实现] 必须在 `abstraction::postflop` 落地 `canonical_observation_id` + `n_canonical_observation`（值由 [实现] 枚举决定，写入 header）；⑤ B1 [测试] `tests/canonical_observation.rs` 起草断言 (a) 1k 随机 (board, hole) 同输入重复调用 byte-equal；(b) 花色重命名 / rank 内花色置换不改变 id；(c) id 紧凑（无空洞）；⑥ `bucket_table.schema_version` 不 bump（A0 期间无 v1 artifact 已写出；若本 rev 在 v1 artifact 落地后再走，必须 bump 到 v2）；⑦ `feature_set_id` 不变（特征组合未动）。

##### InfoAbstraction::map 配套约束（2026-05-09，F21）

**背景**：API §2 `InfoAbstraction::map(state, hole)` trait 不显式说明 `stack_bucket` 的来源；决策侧 D-211-rev1 已锁定为 "TableConfig::initial_stack(seat) / big_blind"。该约束需要 stage 1 `GameState` 暴露 `config()` getter。

**新规则**：API §2 `InfoAbstraction::map` 注释扩展（不改签名）：

```rust
pub trait InfoAbstraction: Send + Sync {
    /// `(GameState, hole_cards)` → InfoSet id。
    ///
    /// **stack_bucket 来源**（D-211-rev1）：实现必须从 `state.config()` 引用 +
    /// `state.actor_seat()` 计算 `effective_stack_at_hand_start`，**不允许**从
    /// `state.player(seat).stack`（当前剩余筹码）推算。同手内 preflop / flop / turn /
    /// river 调用结果 `stack_bucket` 字段 byte-equal。
    ///
    /// 前置条件（IA-006-rev1）：`state.current_player().is_some()`，违反 panic。
    ///
    /// 整条调用路径**禁止浮点**（D-273 / D-252）。
    fn map(&self, state: &GameState, hole: [Card; 2]) -> InfoSetId;
}
```

**stage 1 API 同步要求**：若 stage 1 `GameState` 当前未公开 `config(&self) -> &TableConfig` getter，必须走 `pluribus_stage1_api.md` §11 `API-NNN-revM` 流程在 stage 1 添加只读 getter（继承 D-271 约束）。该 stage 1 API rev 由 B2 [实现] agent 在尝试落地 `InfoAbstraction::map` 实际逻辑时如发现 getter 缺位再触发；A0 / A1 阶段不预设（A1 仅产签名，`_state` 未取用，编译不依赖该 getter；A1 闭合后此条款由 batch 7 review 确认对齐）。

**影响**：① `InfoAbstraction::map` trait 签名不变；② IA-002 不变量保留（preflop key 区分性），含 stack_bucket 来源约束；③ B1 [测试] `tests/info_id_encoding.rs` 必须含 100 BB / 200 BB / 50 BB 三种 TableConfig 下 stack_bucket 桶分配断言（3 / 4 / 2）；④ B2 [实现] 在落地 `InfoAbstraction::map` 实际逻辑时若 stage 1 GameState getter 缺位，同 PR 触发 stage 1 `API-NNN-revM`（A1 已闭合，A1 阶段未触发——签名编译路径 `_state` 未取用，不依赖该 getter；详见 §修订历史 batch 7）。

---

##### batch 6 整体影响汇总

| 公开 API 变化 | 类型 | 备注 |
|---|---|---|
| `BucketTable::lookup` 签名 3 入参 → 2 入参 | breaking | F19 / D-216-rev1 / D-218-rev1 / BT-005-rev1 |
| 新增 `BucketTable::n_canonical_observation` | additive | F19 |
| 新增 `abstraction::postflop::canonical_observation_id` | additive | F19 / D-218-rev1 |
| 新增 `abstraction::preflop::canonical_hole_id` | additive | F19 / D-218-rev1 |
| `EquityCalculator` 4 方法返回 `Result<_, EquityError>` | breaking | F23 / D-224-rev1 / EQ-002-rev1 |
| 新增 `EquityError` 枚举（5 变体） | additive | F23 / D-224-rev1 |
| `ConfigError` 增加 `DuplicateRatio` 变体 | additive | F27 / D-202-rev1 |
| `InfoAbstraction::map` trait 注释（前置条件 + stack_bucket 来源） | docs only | F21 / F22 / D-211-rev1 / IA-006-rev1 |
| 顶层 re-export 增加 `BetRatio / ConfigError / BettingState / StreetTag / EquityError` | additive | F26 / D-253-rev1 |

`bucket_table.schema_version` 不 bump（v1 artifact 尚未生成）。`HandHistory.schema_version` 不 bump（不动 stage 1 序列化，D-276 不变）。`feature_set_id` 不 bump（特征组合未动）。

---

#### A1 关闭后 review 措辞收尾 batch 7（2026-05-09，A1 已闭合）

A1 [实现] 落地后（commit `c4107ee`）的 review 抽查发现 4 处文档措辞观察（O1–O4），其中 3 处属 doc-only 修正、1 处保留。本 batch **0 spec 变化、0 公开签名变化、0 不变量变化、0 测试回归**，仅同步 doc 与 CLAUDE.md 措辞，未走 `API-NNN-revM` 流程（无 API 契约改动）。

| 观察 | 类型 | 处理 |
|---|---|---|
| O1：§2 `InfoAbstraction::map` trait doc + §F21 carve-out + §F21 影响 ④ 三处「A1 [实现] 必须走 stage 1 `API-NNN-revM` 添加 `GameState::config()` getter」与实现现实冲突 | doc-only | 三处统一改为「B2 [实现] 在落地实际逻辑时触发，A1 阶段仅产签名 `_state` 未取用编译不依赖该 getter」。**理由**：A1 闭合 commit 实测——`info.rs:112-114` doc + `fn map(&self, _state: ...) { unimplemented!(...) }` 整签名编译不依赖 `GameState::config()`，A1 [实现] 选择保守 defer 把 stage 1 API rev 留给 B2，与 §F21 line 1062「再触发」条件式语义吻合。本 batch 把 §2 trait doc 强约束语义（"必须走"）软化为与 §F21 一致的条件式（B2 落地时触发） |
| O2：`src/abstraction/mod.rs` line 14/16「模块私有」简写措辞与 `pub mod feature; pub mod cluster;` 声明语义不符（这两个子模块经 `poker::abstraction::feature::*` / `poker::abstraction::cluster::*` 路径仍可访问，仅是不在 `lib.rs` 顶层 re-export）| doc-only（rust 注释）| 改写为「D-254 不在 `lib.rs` 顶层 re-export，仅经 `poker::abstraction::*` 路径访问」，与同文件 line 22-24 解释段落一致 |
| O3：`PreflopLossless169 { _opaque: () }` / `PostflopBucketAbstraction { table, _opaque: () }` 用 `_opaque: ()` 字段做 opaque marker，可改为 tuple struct `pub struct PreflopLossless169(())` 等 | 风格 | **保留不修**。`_opaque: ()` 是 B2 [实现] 即将填充真实状态字段（preflop 169 lookup 表 / postflop canonical id 缓存）的占位——改成 unit struct / tuple struct 在 B2 又要换回命名字段 struct，纯属 churn。`#[allow(dead_code)] // A1 stub; B2 fills` 注释已显式标注此意图 |
| O4：`CLAUDE.md` line 136 / 172「全 14 个 stage 2 类型 / trait / helper」计数不准，实际为 21 项公开类型 / trait / helper + 1 个子模块（`cluster::rng_substream`）| doc-only | 改为精确计数 21（action 7 / info 4 / preflop 2 / postflop 2 / equity 3 / bucket_table 3）+ 1 子模块 |

**触发文件**（batch 7 commit）：

- `docs/pluribus_stage2_api.md`：§2 trait doc 1 处 + §F21 carve-out 1 处 + §F21 影响 ④ 1 处 + 本 §修订历史 batch 7 子节追加（即本节）
- `src/abstraction/mod.rs`：line 14/16 「模块私有」改写
- `CLAUDE.md`：line 136 / 172 计数精确化 + A1 closed 段落补 batch 7 行
- `docs/pluribus_stage2_workflow.md`：§修订历史 追加 §A-rev1（A1 关闭 + batch 7 收尾）

**触发文件审计**：`src/abstraction/{action,info,preflop,postflop,equity,bucket_table,cluster,feature,map/mod}.rs` 主体（公开 trait / 类型 / 方法签名 / `unimplemented!()` 占位 / `#![deny(clippy::float_arithmetic)]` inner attr / D-228 op_id 常量）**未修改一行**；`tests/api_signatures.rs` trip-wire **未修改一行**；`Cargo.toml` / `Cargo.lock` 依赖列表 **未修改一行**——0 公开签名漂移、0 trip-wire 漂移、0 测试回归。

**复跑 5 道 gate**（A1 闭合 commit 同型）：`cargo fmt --all --check` / `cargo build --all-targets` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` / `cargo test`（默认 104 passed / 19 ignored / 0 failed across 16 test crates） 全绿，与 A1 baseline byte-equal。

---

#### C2 关闭（2026-05-09）— C-rev1

C2 [实现] 关闭。本节追加新增 API 表面与签名澄清；详见 `pluribus_stage2_workflow.md` §修订历史 §C-rev1。

##### BucketTable C2 新增方法（非 trait API surface 扩展，不动 §4 公开签名）

```rust
impl BucketTable {
    /// in-memory 训练（同 `(config, training_seed, evaluator, cluster_iter)` 输入
    /// byte-equal，D-237）。`tools/train_bucket_table.rs` CLI 与 [测试] fixture 共享
    /// 路径，避免 [测试] 路径依赖磁盘 I/O。
    pub fn train_in_memory(
        config: BucketConfig,
        training_seed: u64,
        evaluator: std::sync::Arc<dyn HandEvaluator>,
        cluster_iter: u32,
    ) -> BucketTable;

    /// 把当前 BucketTable 字节内容原子写出到 path（先写 `<path>.tmp` 再 rename）。
    /// stub 实例调用 panic。
    pub fn write_to_path(&self, path: &std::path::Path) -> Result<(), std::io::Error>;
}
```

`train_in_memory` 是 [实现] 在 §C-rev1 §3 carve-out 路径下追加的非 trait API surface，便于 [测试] 在不依赖磁盘 I/O 的情况下复用真实 mmap layout 路径（解决 §C1 §出口 line 322-324 "C2 闭合后取消 ignore" 出口的工程依赖）。`write_to_path` 由 CLI 与 [测试] capture-only 入口共用。两者均不进 trait 公开 API 列表（§6 `lib.rs` re-export 不动）。

##### `cluster.rs` 公开 surface 扩展（仅 `crate::abstraction::cluster::*` 路径暴露，D-254 不顶层 re-export）

```rust
pub mod cluster {
    pub fn emd_1d_unit_interval(samples_a: &[f64], samples_b: &[f64]) -> f64;

    pub struct KMeansConfig { pub k: u32, pub max_iter: u32, pub centroid_shift_tol: f64 }
    impl KMeansConfig { pub const fn default_d232(k: u32) -> KMeansConfig; }

    pub struct KMeansResult { pub centroids: Vec<Vec<f64>>, pub assignments: Vec<u32> }

    pub fn kmeans_fit(
        features: &[Vec<f64>],
        cfg: KMeansConfig,
        master_seed: u64,
        op_id_init: u32,
        op_id_split: u32,
    ) -> KMeansResult;

    pub fn reorder_by_ehs_median(
        centroids: Vec<Vec<f64>>,
        assignments: Vec<u32>,
        ehs_per_sample: &[f64],
    ) -> (Vec<Vec<f64>>, Vec<u32>);

    pub fn quantize_centroids_u8(
        centroids: &[Vec<f64>],
    ) -> (Vec<Vec<u8>>, Vec<f32>, Vec<f32>);

    // rng_substream 子模块（D-228 公开 contract）已存在，本 batch 不动。
}
```

`cluster::*` 子项 D-254 不顶层 re-export；`bucket_table::build_bucket_table_bytes` 内部使用。`emd_1d_unit_interval` 与 `tests/bucket_quality.rs` 内部 helper 函数同名 + 同语义（重复实现，因 dev-dependency 路径分隔；下游 D1 / D2 可统一引用 cluster crate 版）。

##### `canonical_observation_id` 街相关上界收紧（D-218-rev1 / D-244-rev1 实测可收紧）

A1 落 `canonical_observation_id` mod `2_000_000` 全街共用；C2 收紧到街相关：

```rust
pub const N_CANONICAL_OBSERVATION_FLOP: u32 = 3_000;
pub const N_CANONICAL_OBSERVATION_TURN: u32 = 6_000;
pub const N_CANONICAL_OBSERVATION_RIVER: u32 = 10_000;
```

落在 BT-008-rev1 `flop ≤ 2_000_000 / turn ≤ 20_000_000 / river ≤ 200_000_000` conservative cap 内（D-244-rev1 字面 "A1 实测后可收紧"）。lookup_table 文件大小：(3K + 6K + 10K) × 4 + preflop 1326 × 4 ≈ 81 KB。`BucketTable::stub_for_postflop` 同步更新使用相同常量。

**hash design 限制（C-rev1 §2 carve-out）**：FNV-1a 32-bit hash mod N 是 approximate canonical id，与 D-218-rev1 字面要求的 *联合花色对称等价类唯一 id* 在 hash 碰撞场景下不严格等价。后续 stage 3+ true equivalence class enumeration 落地后由 D-218-rev2 收口。`tests/bucket_quality.rs` 12 条质量门槛断言因此 carve-out 保留 `#[ignore]` + 早返回 stub（详见 workflow §C-rev1 §2）。

##### `BucketTable::open` 加载路径 carve-out

D-244 / D-255 锁 mmap 加载（`memmap2::Mmap::map`），但 stage 1 `Cargo.toml [lints.rust] unsafe_code = "forbid"` 禁止本 crate 直接写 unsafe；`memmap2::Mmap::map` API 入口标记 `unsafe fn` 必须 `unsafe { ... }`。C2 [实现] carve-out：`open(path)` 走 `std::fs::read(path)` 整段加载到 `Vec<u8>`，与 mmap 在 reader 视角语义等价（同样 `&[u8]` 全文件视图 + 同 BLAKE3 trailer eager 校验）。1.4 MB 文件加载 < 5 ms 无 SLO 风险；`memmap2 = "0.9"` 依赖保留在 `Cargo.toml`（D-255 已落地）但 C2 路径未直接调用。stage 3+ 若巨大 bucket table 跨进程 mmap 共享必需，由 D-275-revM 评估解禁路径。

**触发文件审计（C2 commit）**：

- 修改产品代码：`src/abstraction/cluster.rs` / `src/abstraction/bucket_table.rs` / `src/abstraction/postflop.rs` / `tools/train_bucket_table.rs`（new）/ `Cargo.toml`（新增 `[[bin]]`）。
- 修改测试代码（§C-rev1 §3 carve-out 追认）：`tests/bucket_quality.rs` / `tests/clustering_determinism.rs`。
- **未修改**：`tests/api_signatures.rs` trip-wire / `src/lib.rs` 顶层 re-export / 阶段 1 全部 `src/` 与 `tests/`。

**复跑 5 道 gate**：`cargo fmt --all --check` / `cargo build --all-targets` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` / `cargo test`（187 passed / 34 ignored / 0 failed across 25 test crates；stage 1 baseline `104 passed / 19 ignored` byte-equal 不退化）全绿。

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

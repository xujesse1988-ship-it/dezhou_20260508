# 阶段 1 API 契约

## 文档地位

本文档定义阶段 1 所有公开类型与方法的契约。**A1 步骤的代码骨架必须严格匹配本文档**。

- 测试 agent 在 B1 / C1 / D1 / E1 / F1 写测试时，**只依赖**本文档定义的 API。
- 实现 agent 在 A1 / B2 / C2 / D2 / E2 / F2 写产品代码时，**不得偏离**本文档签名（除非走决策修改流程修改本文档）。
- 任何在实现过程中发现的 API 不足或歧义，必须先在本文档追加 `API-NNN-revM` 条目，再实施。

所有签名为 Rust 风格。语义说明放在签名后的注释或下方文字。

---

## 1. 基础类型（`module: core`）

### Card / Rank / Suit

```rust
/// 整数后备的扑克牌。0..52 范围。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Card(u8);

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum Rank {
    Two = 0, Three, Four, Five, Six, Seven, Eight, Nine, Ten,
    Jack, Queen, King, Ace, // Ace = 12
}

impl Rank {
    /// 从 0..=12 的 u8 值还原 Rank；超出范围返回 None。
    pub fn from_u8(value: u8) -> Option<Rank>;
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum Suit {
    Clubs = 0, Diamonds, Hearts, Spades,
}

impl Suit {
    /// 从 0..=3 的 u8 值还原 Suit；超出范围返回 None。
    pub fn from_u8(value: u8) -> Option<Suit>;
}

impl Card {
    /// 构造一张牌。
    pub const fn new(rank: Rank, suit: Suit) -> Card;
    pub fn rank(self) -> Rank;
    pub fn suit(self) -> Suit;
    /// 0..52 的稳定数值表示。
    pub fn to_u8(self) -> u8;
    pub fn from_u8(value: u8) -> Option<Card>;
}
```

**语义**：
- `Card::to_u8` 编码：`rank * 4 + suit`，保证跨平台稳定。
- 比较 `Rank` 时 `Two < Three < ... < Ace`。
- `Suit` 不参与强度比较（NLHE 无花色优劣）。

### ChipAmount

```rust
/// 整数筹码。1 chip = 1/100 BB（见 D-020）。
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ChipAmount(pub u64);

impl ChipAmount {
    pub const ZERO: ChipAmount = ChipAmount(0);
    pub const fn new(chips: u64) -> ChipAmount;
    pub fn as_u64(self) -> u64;
}

// 标准算术：Add / Sub / AddAssign / SubAssign / Mul<u64> 必须实现，且
// 仅走整数路径，禁止浮点。Sub / SubAssign 在下溢时 debug 与 release 都 panic
// （见 D-026b）；需要 saturating 语义的调用方必须显式用 checked_sub。
```

### Street / Position / SeatId

```rust
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum Street {
    Preflop = 0,
    Flop = 1,
    Turn = 2,
    River = 3,
    Showdown = 4,
}

/// 6-max 标准位置。仅当桌面 = 6 人时使用此名称；其他桌大小用 SeatId 与按钮相对位置表达。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Position {
    BTN, SB, BB, UTG, MP, CO,
}

/// 座位号 0..n_seats。按桌面物理座位编号，不随按钮变化。
///
/// **方向约定（D-029）**：`SeatId(k+1 mod n_seats)` 是 `SeatId(k)` 的左邻。
/// 按钮轮转（D-032）、盲注推导（D-022b / D-032）、odd chip 分配（D-039）、
/// showdown 顺序（D-037）中"向左" / "按钮左侧" 均按此理解。
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct SeatId(pub u8);
```

### Player

```rust
#[derive(Clone, Debug)]
pub struct Player {
    pub seat: SeatId,
    pub stack: ChipAmount,            // 当前剩余筹码（不含本街已投入）
    pub committed_this_round: ChipAmount, // 本下注轮已投入金额
    pub committed_total: ChipAmount,  // 本手全部下注轮累计已投入金额
    pub hole_cards: Option<[Card; 2]>,    // None 表示尚未发或已弃
    pub status: PlayerStatus,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum PlayerStatus {
    Active,    // 在牌局中、未弃、未 all-in
    AllIn,
    Folded,
    SittingOut, // 阶段 1 不使用，但保留枚举
}
```

---

## 2. 动作（`module: rules::action`）

```rust
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Action {
    Fold,
    Check,
    Call,
    /// 当前下注轮无前序 bet 时的下注。`to` = 该玩家本轮投入总额（绝对值）。
    Bet { to: ChipAmount },
    /// 前序已有 bet 时的加注。`to` = 该玩家本轮投入总额（绝对值）。
    Raise { to: ChipAmount },
    /// 全部剩余筹码。状态机内部归一化为 Bet/Raise/Call。
    AllIn,
}
```

**语义**：
- `Bet { to }` 与 `Raise { to }` 的 `to` 都是该玩家本下注轮投入的**绝对总额**（包含此动作之前已投入的盲注 / call / 之前被加注的额度）。换言之，应用动作后该玩家的 `committed_this_round` 必须严格等于 `to`。
- 从玩家筹码中实际扣除的金额 = `to - player.committed_this_round_before_action`（即"差额"），不是 `to` 本身。
- 伪代码：

  ```text
  fn apply_bet_or_raise(player, to):
      delta = to - player.committed_this_round   // 必须 > 0
      assert delta <= player.stack                 // 否则 InsufficientStack
      player.stack            -= delta
      player.committed_this_round = to             // 由原本的某值变为 to
      player.committed_total  += delta
      pot                     += delta
  ```

- 完整数值例子（`SB=50, BB=100, 6-max`，preflop 第一次行动）：BTN 起始 stack = 10,000，盲注阶段未投入 → `committed_this_round_before = 0`。BTN 选择 `Raise { to = 300 }`：
    - `delta = 300 - 0 = 300`
    - 从 BTN.stack 扣 300 → BTN.stack = 9,700
    - BTN.committed_this_round = 300
    - BTN.committed_total = 300
    - pot 中此前已有 SB(50) + BB(100) = 150；执行后 pot = 450
    - 注意 `to=300` 是 BTN 本轮投入总额；BTN 之后 SB 想跟注的话，`Call` 对应的实际扣款 = `300 - 50 = 250`（SB 已投 50）。
- AllIn 是便利变体，状态机执行时根据当前局面归一化为对应的 Bet/Raise/Call。**HandHistory 中存储归一化后的最终动作**（见 §5），保证回放无歧义。
- `Action::AllIn` 错误路径：玩家 `stack == 0` 时 `apply(AllIn)` 返回 `RuleError::InsufficientStack`；`0 < stack < min_bet_or_raise_delta` 时 `AllIn` 仍然合法（"under min" / incomplete raise，按 D-033 不重开 raise option），归一化为对应的 Bet/Raise/Call，且 `to = committed_this_round_before + stack`。

### LegalActionSet

```rust
#[derive(Clone, Debug)]
pub struct LegalActionSet {
    pub fold: bool,
    pub check: bool,
    pub call: Option<ChipAmount>,                  // 跟注所需金额（绝对，不是差额）
    pub bet_range: Option<(ChipAmount, ChipAmount)>,    // (min_to, max_to)
    pub raise_range: Option<(ChipAmount, ChipAmount)>,  // (min_to, max_to)
    pub all_in_amount: Option<ChipAmount>,         // 全 all-in 时的等效 to 值
}
```

**语义**：
- 每条字段独立，`None` 表示该动作不合法。
- `bet_range` 与 `raise_range` 互斥：本轮无前序 bet 时只能 `bet`，有前序 bet 时只能 `raise`。
- `min_to` 含 short all-in 不重开 raise 的约束（见 D-033）。

### LegalActionSet 不变量（实现 agent 必须保证、测试 agent 在 invariant suite 中验证）

- LA-001 `check` 与 `call` 互斥：当前下注轮 `committed_this_round` 与 `max_committed_this_round` 相等时只能 `check`（`call = None`）；不等时只能 `call`（`check = false`）。等价表述：`check && call.is_some()` 永远为 false。
- LA-002 `bet_range` 与 `raise_range` 互斥：本轮 `max_committed_this_round == 0`（无前序 bet）时 `raise_range = None`；`max_committed_this_round > 0` 时 `bet_range = None`。等价表述：`bet_range.is_some() && raise_range.is_some()` 永远为 false。
- LA-003 `fold` 永远合法（除非 `current_player == None`），即 `current_player().is_some() => fold == true`。
- LA-004 `call` 与 `check` 至少有一个真：当 `current_player().is_some()` 时，`check || call.is_some()` 必须为 true。
- LA-005 `bet_range` 的 `min_to` ≥ `BB`（首次开局，对应 D-034）；`raise_range` 的 `min_to` 满足 D-035 链式 min raise 约束。
- LA-006 `bet_range` / `raise_range` 的 `max_to` ≤ `committed_this_round + stack`（即玩家不可下注超出剩余筹码 + 本轮已投入额）。
- LA-007 `all_in_amount` 当且仅当 `stack > 0` 时为 `Some`；其值 = `committed_this_round + stack`。
- LA-008 当 `current_player() == None`（terminal / all-in 跳轮）时，所有字段必须为 `false` / `None`（"空集合"）。

---

## 3. 桌面配置（`module: rules::config`）

```rust
#[derive(Clone, Debug)]
pub struct TableConfig {
    pub n_seats: u8,                       // 2..=9，默认 6
    pub starting_stacks: Vec<ChipAmount>,  // 长度 = n_seats
    pub small_blind: ChipAmount,           // 默认 50 chips
    pub big_blind: ChipAmount,             // 默认 100 chips
    pub ante: ChipAmount,                  // 默认 0
    pub button_seat: SeatId,               // 起始按钮位
}

impl TableConfig {
    /// 6-max 100BB 的默认配置：6 座、起始 100BB、SB=50、BB=100、ante=0、按钮在座位 0。
    pub fn default_6max_100bb() -> TableConfig;
}
```

---

## 4. 游戏状态（`module: rules::state`）

```rust
pub struct GameState {
    /* 内部字段不公开 */
}

impl GameState {
    /// 初始化一手新牌（生产路径）。
    ///
    /// 内部以 `ChaCha20Rng::from_seed(seed)` 构造 rng，按 D-028 发牌协议抽牌、布盲、
    /// 按钮位由 config 指定。`HandHistory.seed` 自动记为该 `seed`，`replay()` 即可复现。
    pub fn new(config: &TableConfig, seed: u64) -> GameState;

    /// 初始化一手新牌（测试 / fuzz 路径）。
    ///
    /// 注入自定义 `RngSource`，典型用于 stacked deck（构造指定牌序，参见 D-028）。
    /// `seed` 仅作为 `HandHistory.seed` 的标签写入，**不参与发牌**；调用方需自负 rng 与 seed
    /// 的语义一致性 —— 若期望 `replay()` 能复现，则注入的 rng 必须等价于 `ChaCha20Rng::from_seed(seed)`，
    /// 否则 `replay()` 在底牌 / 公共牌校验阶段会返回 `HistoryError::ReplayDiverged`。
    /// 推荐：在 fuzz / 单元测试中使用 stacked rng + 固定 sentinel seed（如 `0`），并不要求 replay 复现。
    pub fn with_rng(config: &TableConfig, seed: u64, rng: &mut dyn RngSource) -> GameState;

    /// 当前要行动的玩家。手牌结束 / 全员 all-in 跳轮时返回 None。
    pub fn current_player(&self) -> Option<SeatId>;

    /// 当前合法动作集合。无玩家行动时返回空集合。
    pub fn legal_actions(&self) -> LegalActionSet;

    /// 应用一个动作。失败时返回错误，状态不改变。
    pub fn apply(&mut self, action: Action) -> Result<(), RuleError>;

    /// 当前下注街。
    pub fn street(&self) -> Street;

    /// 当前桌面公共牌（Flop=3, Turn=4, River=5）。
    pub fn board(&self) -> &[Card];

    /// 当前总 pot（含主池 + 所有 side pot）。
    pub fn pot(&self) -> ChipAmount;

    /// 当前所有玩家状态快照（按 SeatId 排序）。
    pub fn players(&self) -> &[Player];

    /// 牌局是否结束（已 showdown 或全员弃牌）。
    pub fn is_terminal(&self) -> bool;

    /// 终局每个玩家的净收益（正 = 赢、负 = 输）。仅 is_terminal 后有效。
    pub fn payouts(&self) -> Option<Vec<(SeatId, i64)>>;

    /// 当前 hand history 的引用，可随时序列化或回放。
    pub fn hand_history(&self) -> &HandHistory;

    /// 当前 `TableConfig` 的只读引用（API-004-rev1，stage 2 B2 触发；
    /// stage 2 `InfoAbstraction::map` 按 D-211-rev1 需要 `TableConfig::initial_stack(seat)`
    /// 计算 `stack_bucket`，不允许从 `player(seat).stack`（当前剩余筹码）反推）。
    pub fn config(&self) -> &TableConfig;
}
```

**关键不变量**（实现 agent 必须保证、测试 agent 应在 invariant suite 中验证）：

- I-001 任意时刻 `sum(player.stack) + pot() = sum(starting_stacks)`。`TableConfig.starting_stacks` 是发盲注 / ante **之前** 的座位栈（D-024）；引擎在开手时把盲注 / ante 从对应座位的 `stack` 转入 pot，转移过程总量守恒，因此本等式在牌局任意时刻、任意街、任意 apply 前后都必须成立。
- I-002 任意 `Player.stack >= 0`（用 `u64` 表达自然成立，但减法路径必须有下溢检查）
- I-003 任意一手内不出现重复 Card
- I-004 每个 betting round 结束时，所有 `Active` 状态玩家的 `committed_this_round` 相等
- I-005 `apply` 失败时 `GameState` 不变
- I-006 全员 all-in（除 ≤1 名 Active 外）后 `current_player` 必为 None
- I-007 终局必有获胜者（pot 必有归属）

---

## 5. Hand history（`module: history`）

### HandHistory 结构

```rust
#[derive(Clone, Debug)]
pub struct HandHistory {
    pub schema_version: u32,           // 当前固定为 1
    pub config: TableConfig,
    pub seed: u64,                     // 用于复现的初始 seed
    pub actions: Vec<RecordedAction>,  // 按发生顺序
    pub board: Vec<Card>,              // 0..=5 张
    pub hole_cards: Vec<Option<[Card; 2]>>, // 长度 = n_seats
    pub final_payouts: Vec<(SeatId, i64)>,  // 净收益
    pub showdown_order: Vec<SeatId>,        // 摊牌顺序，最后激进者在前
}

#[derive(Clone, Debug)]
pub struct RecordedAction {
    pub seq: u32,                  // 全手内单调递增
    pub seat: SeatId,
    pub street: Street,
    pub action: Action,            // AllIn 已归一化为 Bet/Raise/Call
    pub committed_after: ChipAmount, // 该 seat 在本街（= self.street）的投入总额；语义见下
}
```

**语义**：
- `actions` 中的 `Action::AllIn` **不应出现**；状态机在写入 hand history 时把 AllIn 归一化为对应的 `Call` / `Bet { to }` / `Raise { to }`，便于无歧义回放。
- `seq` 字段保证全序，回放时按 `seq` 顺序重放。
- `committed_after` 取**该动作 apply 完成、本街 `committed_this_round` 尚未被街转换重置之前**的值。换言之：
    - 对**未触发街转换**的动作：等价于 apply 后 `player.committed_this_round` 的值。
    - 对**触发街转换**的动作（即本街最后一个动作）：等价于"如果本街不重置，apply 后 `player.committed_this_round` 应有的值"。这恰好等于该动作收尾时该 seat 在本街已贡献给 pot 的累计金额。
- 上述定义保证 `committed_after` 在回放（含 `replay_to`）时可被独立校验，不依赖于"街转换 reset 是否已发生"的内部时序。
- 关于 `Action` 各变体的 `committed_after` 取值：
    - `Action::Fold` / `Action::Check`：`committed_after` = 该 seat 进入本动作前的 `committed_this_round`（本动作不改变投入额）。
    - `Action::Call` / `Action::Bet { to }` / `Action::Raise { to }`：`committed_after` = `to`。

### Roundtrip 接口

```rust
impl HandHistory {
    /// 序列化为 protobuf 字节（schema_version=1）。
    pub fn to_proto(&self) -> Vec<u8>;

    /// 从 protobuf 字节反序列化。错误情况见 HistoryError。
    pub fn from_proto(bytes: &[u8]) -> Result<HandHistory, HistoryError>;

    /// 完整回放：从 seed + actions 重建终局 GameState。
    /// 终局状态必须与原始记录完全一致（board, hole_cards, payouts）。
    ///
    /// 错误类型为 `HistoryError`（见 §8 与 §11 API-001-rev1）：
    /// - 记录的动作序列在某个位置违反规则 → `HistoryError::Rule { index, source }`
    /// - 重新发牌结果与记录的 `board` / `hole_cards` 不一致 → `HistoryError::ReplayDiverged`
    pub fn replay(&self) -> Result<GameState, HistoryError>;

    /// 部分回放：应用 `actions[0..action_index]` 后的中间状态（即"前 action_index 个动作已应用"）。
    /// `action_index = 0` 表示"刚发完手牌、未行动"；`action_index = actions.len()` 等同 replay()。
    /// 错误类型说明见 `replay`。
    pub fn replay_to(&self, action_index: usize) -> Result<GameState, HistoryError>;

    /// hand history 的内容指纹。BLAKE3(self.to_proto())。
    /// 由于 to_proto 是 deterministic（见 §10 PB-003），content_hash 跨平台稳定，
    /// 适合用于 D-051 跨平台一致性验收与 fuzz roundtrip 比对。
    pub fn content_hash(&self) -> [u8; 32];
}
```

**回放与街转换时序**：

- 公共牌（flop / turn / river）的发牌**不占** `actions` 序列中的位置，不产生 `RecordedAction`。
- 当一个 betting round 的最后一个动作（如 BB check 结束 preflop）被 `apply` 后：
    1. 状态机内部在该 `apply` 调用内先把所有玩家的 `committed_this_round` 重置为 0、收入 pot；
    2. 然后从 deck 抽取下街公共牌追加到 `board`；
    3. 然后切换 `street` 字段；
    4. 然后定位下街第一个行动者（postflop = SB 起，preflop = UTG 起）。
- 因此 `replay_to(k)` 返回的 `GameState` 满足：第 `k` 个动作已应用，若该动作恰为某街最后一个动作，则下街公共牌已发、`street` 已切换、`current_player` 指向下街第一个行动者；若 `k = 0` 则 `street == Preflop`、`board == []`、`current_player` 为 UTG（6-max）。
- 例：`actions.len() = 8`，第 5 个动作触发 preflop 结束 → `replay_to(5)` 的状态：preflop 5 个动作已应用、flop 3 张已在 `board` 中、`street == Flop`、`current_player` 为 SB 之后第一个 active 玩家。

**全员 all-in 跳轮（D-036）的多街快进时序**：

- 当某个 `apply` 调用结束后剩余 `Active` 玩家 ≤ 1 名（其余皆 `AllIn` 或 `Folded`），状态机在**同一 apply 调用内**连续执行：依次发完所有未发的公共牌（直到 `board.len() == 5`） → 切 `street = Showdown` → 计算 `payouts` → 设 `current_player() = None`、`is_terminal() = true`。
- 该过程不产生新的 `RecordedAction`（公共牌发牌本就不占 actions 序列）。`HandHistory.actions` 长度只反映触发该跳轮的最后一个玩家动作。
- `replay_to(k)` 行为：若第 k 个动作触发了"全员 all-in"分支，返回的 `GameState` 已处于 Showdown 状态、5 张 board 已发完、`is_terminal == true`；`replay_to(actions.len())` 在所有情形下都等价于 `replay()`。
- 与 D-036 / I-006 一致：跳轮的边界判定基于 apply 完成后的 `Active` 计数，不依赖 apply 中的中间状态。

---

## 6. 评估器（`module: eval`）

```rust
/// 不透明手牌强度。数值越大越强；同值代表同强度（split pot）。
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash, Debug)]
pub struct HandRank(pub u32);

impl HandRank {
    pub fn category(self) -> HandCategory;
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum HandCategory {
    HighCard, OnePair, TwoPair, Trips, Straight, Flush,
    FullHouse, Quads, StraightFlush, RoyalFlush,
}

/// 评估器接口。同一 trait 同时支持 5/6/7-card。
pub trait HandEvaluator: Send + Sync {
    fn eval5(&self, cards: &[Card; 5]) -> HandRank;
    fn eval6(&self, cards: &[Card; 6]) -> HandRank;
    fn eval7(&self, cards: &[Card; 7]) -> HandRank;
}
```

**语义**：
- `eval6` / `eval7` 必须返回所有 5-card 子集中最强的 `HandRank`。
- 三个接口对相同 5-card 输入必须返回相同 `HandRank`。
- 同 `HandCategory` 内 `HandRank` 数值可比较；`HandCategory(A) > HandCategory(B)` 时 `eval(A).0 > eval(B).0` 必须成立。
- 不要求 `HandRank` 数值跨不同 evaluator 实现一致；只要求**同一 evaluator 内部全序稳定**。

---

## 7. 随机源（`module: core::rng`）

```rust
/// 显式注入的随机源。所有用到随机数的地方都必须接受 &mut dyn RngSource，
/// 禁止使用全局 rng。
///
/// `Send` 约束：阶段 1 多线程模拟（D-054）要求 RngSource 可在线程间转移；
/// 实现方必须满足 `Send`。`Sync` 不强制（每线程持有独占 rng）。
pub trait RngSource: Send {
    fn next_u64(&mut self) -> u64;
}

/// 标准实现：基于 ChaCha20，seed-determined。
pub struct ChaCha20Rng { /* opaque */ }
impl ChaCha20Rng {
    pub fn from_seed(seed: u64) -> Self;
}
impl RngSource for ChaCha20Rng { /* ... */ }

/// 适配器：把任意 `rand::RngCore` 包装成 `RngSource`。
/// 不使用 blanket impl（会与具名实现冲突，且无法附加 Send 约束）。
pub struct RngCoreAdapter<R: rand::RngCore + Send>(pub R);

impl<R: rand::RngCore + Send> RngSource for RngCoreAdapter<R> { /* ... */ }

impl<R: rand::RngCore + Send> RngCoreAdapter<R> {
    pub fn from_rng_core(inner: R) -> Self { RngCoreAdapter(inner) }
}
```

**语义**：
- `from_seed` 必须确定性：相同 seed 在所有平台上产生相同序列（`ChaCha20` 算法保证）。
- 禁止使用 `OsRng` / `thread_rng()` 等系统熵源进入规则引擎或评估器。
- 任何 `rand::RngCore` 实现需要包成 `RngCoreAdapter` 才能注入；这避免了 blanket impl 与具名 `ChaCha20Rng` 之间的 conflicting impl，并保证 `Send` 约束。
- **发牌协议（参见决策 D-028）**：`GameState::new` 与 `GameState::with_rng` 调用 `RngSource` 的方式（消费几次 `next_u64`、每次 mod 多少、Fisher-Yates 步长、deck 索引到底牌 / 公共牌的映射）由 D-028 严格定义并作为 API 契约对外公开。这使得测试 / fuzz 代码可以构造 stacked `RngSource` 实现来产生指定牌序（B1 fixed scenario 主要靠这个写），无需依赖任何实现内部细节。`with_rng` 的 stacked 用法不要求传入的 rng 与 `ChaCha20Rng::from_seed(seed)` 一致，但此时 `replay()` 不保证可复现（详见 §4 `with_rng` 注释）。

---

## 8. 错误类型（`module: error`）

```rust
#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    #[error("not the current player's turn")]
    NotPlayerTurn,
    #[error("hand already terminated")]
    HandTerminated,
    /// 动作种类与当前下注轮状态不匹配。典型情形：本轮已有 bet 时收到 `Bet`（应为 `Raise`）；
    /// 本轮无 bet 时收到 `Raise` 或 `Call`（应为 `Bet` 或 `Check`）；本轮已有 bet 时收到 `Check`。
    /// `reason` 为 `&'static str`，限定使用预定义的几种字面量，避免字符串拼接进入热路径。
    #[error("wrong action for state: {action:?} ({reason})")]
    WrongActionForState { action: Action, reason: &'static str },
    /// 前序为 incomplete raise / short all-in，按 D-033 不重开 raise option，
    /// 但当前玩家尝试 `Raise`（无论是 `Action::Raise` 显式 raise，还是 `AllIn` 归一化后构成 raise）。
    #[error("raise option not reopened (previous raise was incomplete / short all-in)")]
    RaiseOptionNotReopened,
    /// raise 加注差额小于本轮最大有效加注差额（D-035），或首次 bet/raise 小于 BB（D-034）。
    #[error("min raise violation: required to >= {required:?}, got to = {got:?}")]
    MinRaiseViolation { required: ChipAmount, got: ChipAmount },
    /// `to` 字段本身越界：`to <= committed_this_round_before`（动作扣款 ≤ 0），
    /// 或 `to > committed_this_round_before + stack`（超出剩余筹码）。
    #[error("invalid amount: {0:?}")]
    InvalidAmount(ChipAmount),
    /// 玩家剩余 stack 不足以执行该动作的扣款（典型见 `AllIn` 在 `stack == 0` 时被调用）。
    #[error("insufficient stack")]
    InsufficientStack,
    /// 兜底变体：上述具名变体未覆盖、但实现 / 测试代码确实需要拒绝的情况。
    /// **新增违规类型时优先升级为具名变体**，不要长期依赖该兜底（测试 agent 在 invariant
    /// suite 中不应基于 `reason` 字符串内容做断言）。
    #[error("illegal action: {reason}")]
    IllegalAction { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("schema version mismatch: found {found}, supported {supported}")]
    SchemaVersionMismatch { found: u32, supported: u32 },
    #[error("corrupted history: {0}")]
    Corrupted(String),
    #[error("invalid protobuf: {0}")]
    InvalidProto(String),
    #[error("replay diverged at action index {index}: {reason}")]
    ReplayDiverged { index: usize, reason: String },
    /// 记录的动作序列在 `actions[index]` 处被规则引擎拒绝（典型见 corrupted
    /// history、跨版本不兼容残余、或上游写入端 bug）。`source` 携带底层
    /// `RuleError`，可通过 `std::error::Error::source()` 链式访问；外层
    /// `HistoryError` 表明该错误发生在 history replay 上下文（API-001-rev1）。
    #[error("replay action {index} rejected by rule engine")]
    Rule {
        index: usize,
        #[source]
        source: RuleError,
    },
}
```

---

## 9. 模块导出（顶层 `lib.rs`）

```rust
pub mod core;
pub mod rules;
pub mod eval;
pub mod history;
pub mod error;

// 顶层 re-export
pub use core::{Card, Rank, Suit, ChipAmount, Street, Position, SeatId, Player, PlayerStatus};
pub use core::rng::{RngSource, ChaCha20Rng, RngCoreAdapter};
pub use rules::action::{Action, LegalActionSet};
pub use rules::config::TableConfig;
pub use rules::state::GameState;
pub use eval::{HandEvaluator, HandRank, HandCategory};
pub use history::{HandHistory, RecordedAction};
pub use error::{RuleError, HistoryError};
```

---

## 10. proto 定义（`proto/hand_history.proto`）

```proto
syntax = "proto3";
package poker.v1;

message HandHistory {
    uint32 schema_version = 1;     // = 1
    TableConfig config = 2;
    uint64 seed = 3;
    repeated RecordedAction actions = 4;
    repeated uint32 board = 5;     // 每张牌 = Card.to_u8()
    repeated HoleCards hole_cards = 6;
    repeated Payout final_payouts = 7;
    repeated uint32 showdown_order = 8;
}

message TableConfig {
    uint32 n_seats = 1;
    repeated uint64 starting_stacks = 2;
    uint64 small_blind = 3;
    uint64 big_blind = 4;
    uint64 ante = 5;
    uint32 button_seat = 6;
}

message RecordedAction {
    uint32 seq = 1;
    uint32 seat = 2;
    Street street = 3;             // 显式 enum，见下方 Street 定义
    ActionKind kind = 4;
    uint64 to = 5;                 // 见下方 to 字段语义
    uint64 committed_after = 6;
}

enum ActionKind {
    // proto3 默认值哨兵：字段缺失或被截断时静默解码为 0，因此 0 必须保留为
    // UNSPECIFIED，由 PB-002 在 from_proto 阶段拒绝。任何合法 RecordedAction
    // 的 kind 不得为此值。
    ACTION_KIND_UNSPECIFIED = 0;
    FOLD = 1;
    CHECK = 2;
    CALL = 3;
    BET = 4;
    RAISE = 5;
    // AllIn 在写入时已归一化为 CALL/BET/RAISE，proto 中不出现
}

enum Street {
    // proto3 默认值哨兵，理由同上。任何合法 RecordedAction.street 与顶层
    // 字段都不得为此值（见 PB-002）。
    STREET_UNSPECIFIED = 0;
    PREFLOP = 1;
    FLOP = 2;
    TURN = 3;
    RIVER = 4;
    SHOWDOWN = 5;
}

message HoleCards {
    bool present = 1;
    uint32 c0 = 2;
    uint32 c1 = 3;
}

message Payout {
    uint32 seat = 1;
    sint64 amount = 2;
}
```

### proto 字段不变量

- PB-001 `RecordedAction.to` 字段语义按 `kind` 区分：
    - `kind == FOLD` → `to` 必须为 `0`；非 0 视为 corrupted。
    - `kind == CHECK` → `to` 必须为 `0`；非 0 视为 corrupted。
    - `kind == CALL` / `BET` / `RAISE` → `to` 必须 `> 0`；为 0 视为 corrupted。
- PB-002 `from_proto` 在校验阶段必须执行下列检查，任一失败即返回 `HistoryError::Corrupted`：
    - 按 PB-001 检查每条 `RecordedAction.to` 与 `kind` 的一致性，错误信息 `"action {seq}: to field violates kind invariant"`。
    - 拒绝 `RecordedAction.kind == ACTION_KIND_UNSPECIFIED`，错误信息 `"action {seq}: kind is UNSPECIFIED (proto3 default sentinel — likely missing or corrupted field)"`。
    - 拒绝 `RecordedAction.street == STREET_UNSPECIFIED`，错误信息 `"action {seq}: street is UNSPECIFIED"`。
    - 拒绝任何顶层 `Street` 字段（如未来 schema 新增）出现 `STREET_UNSPECIFIED`，错误信息按字段名给出。
- PB-003 `to_proto()` 输出必须 deterministic：相同 `HandHistory` 在所有平台上产生 byte-equal 字节流。`prost` 默认行为已满足该要求（字段按 tag 升序、map 字段在阶段 1 schema 中不出现）；任何 PR 引入非 deterministic 序列化路径必须 reject。
- PB-004 proto 端 `Street` / `ActionKind` 与 Rust 端的对应通过 `to_proto` / `from_proto` 中的显式转换函数完成，**不要求 discriminant 数值一致**：proto 端从 1 起始以保留 0 为 `*_UNSPECIFIED` 哨兵，Rust 端 `Street` 仍从 0 起始保持 API 友好。任何一侧增删枚举条目必须同步另一侧并 bump `HandHistory.schema_version`。当前映射表：

  | proto `Street` | Rust `Street` |
  |---|---|
  | `STREET_UNSPECIFIED = 0` | （仅出现在 PB-002 拒绝路径，无 Rust 对应） |
  | `PREFLOP = 1` | `Preflop = 0` |
  | `FLOP = 2` | `Flop = 1` |
  | `TURN = 3` | `Turn = 2` |
  | `RIVER = 4` | `River = 3` |
  | `SHOWDOWN = 5` | `Showdown = 4` |

  proto `ActionKind` 与 Rust `Action` 的对应：`ACTION_KIND_UNSPECIFIED = 0` 仅出现在 PB-002 拒绝路径；`FOLD = 1` / `CHECK = 2` / `CALL = 3` / `BET = 4` / `RAISE = 5` 分别对应 `Action::Fold` / `Action::Check` / `Action::Call` / `Action::Bet { to }` / `Action::Raise { to }`（`Action::AllIn` 在写入时已归一化为后三者之一，proto 中不出现）。

---

## 11. API 修改流程

- API-100 任何对本文档已定义签名的修改必须在本文档以追加 `API-NNN-revM` 条目记录，**不删除原条目**
- API-101 修改若影响 protobuf 兼容性，必须 bump `HandHistory.schema_version` 并提供升级器
- API-102 修改 PR 必须经过决策者 review；测试 agent 与实现 agent 同时被 cc

### 修订历史

- **API-001-rev1** (2026-05-07)：`HandHistory::replay` / `HandHistory::replay_to`
  返回类型由 `Result<GameState, RuleError>` 改为 `Result<GameState, HistoryError>`，
  并在 `HistoryError` 中新增 `Rule { index, source: RuleError }` 变体包裹底层
  `RuleError`。
  - **背景**：原签名与文档中已存在的 `HistoryError::ReplayDiverged` 错误类型分裂
    —— replay 失败的两种主要语义（动作序列违法 vs 重新发牌结果与记录不一致）
    被强行拆分到 `RuleError` 与 `HistoryError` 两个根类型，调用方需要分别捕获。
  - **理由**：replay 失败语义统一属于 history 域（"这条记录不可信任地重建"），
    而非 live-play 域（`apply` 仍然返回 `RuleError`）。`HistoryError::Rule`
    通过 `#[source]` 暴露底层 `RuleError`，调用方仍可链式访问根因。
  - **影响**：
    - 不影响 protobuf schema（不 bump `HandHistory.schema_version`）。
    - 不影响 `apply` / `legal_actions` 等 live-play API 签名。
    - A1 骨架已按本 rev1 实现；B2 起的实现 agent 直接按新签名展开。
    - B1 / C1 测试 agent 编写 replay 相关断言时，以 `HistoryError` 模式匹配。
  - **撤销条件**：若后续发现 history 与 rule 错误必须分离传递（如供 CFR 训练
    时的精细错误分类），可走 API-001-rev2 重拆。

- **API-004-rev1** (2026-05-09)：`GameState` 新增 `config(&self) -> &TableConfig`
  只读 getter（additive；不修改任何既有签名）。
  - **背景**：stage 2 D-211-rev1 锁定 `InfoAbstraction::map` 必须从
    `TableConfig::initial_stack(seat) / big_blind` 计算 `stack_bucket`，
    不允许从 `player(seat).stack`（当前剩余筹码）反推。stage 1 §4 `GameState`
    既有 getter（`street` / `board` / `pot` / `players` / `is_terminal` /
    `payouts` / `hand_history`）未暴露 `config`；`hand_history().config` 是
    克隆而非引用，热路径上每次 `map` 调用克隆 `TableConfig`（含
    `Vec<ChipAmount>` `starting_stacks`）开销不可接受。
  - **理由**：纯 additive 改动——`GameState.config: TableConfig` 内部字段早已
    存在（私有字段，§4 文档未公开），本 rev 仅添加只读 getter 暴露引用，
    不改变任何既有不变量、错误路径、proto schema 或行为语义。
  - **影响**：
    - 不影响 protobuf schema（不 bump `HandHistory.schema_version`）。
    - 不影响 stage 1 既有测试 / SLO（`tests/api_signatures.rs` 既有 stage 1
      assertions 不引用 `config()`，所以不需要修改 trip-wire；stage 2 trip-wire
      在 stage 2 B1 [测试] 加入时再覆盖）。
    - 触发条件（stage 2 §修订历史 §F21 carve-out）：B2 [实现] 在落地
      `InfoAbstraction::map` 实际逻辑时若 stage 1 `GameState::config()` getter
      缺位，同 PR 触发本 rev。
    - 后向兼容：所有依赖 stage 1 API 的 stage 1 测试 / 工具继续编译通过；
      `GameState` 字段添加新方法不破坏其它 impl。
  - **stage 2 配套**：本 rev 由 stage 2 B2 [实现] 触发，但 stage 2 §F21
    不需要再起一条 `pluribus_stage2_api.md` rev 条目——stage 2 `InfoAbstraction::map`
    签名不变，只是 trait doc 中 "B2 [实现] 触发 stage 1 API rev" 条款此 commit
    落地。

---

## 12. 与决策文档的对应关系

本文档每个类型 / 字段都可追溯到 `pluribus_stage1_decisions.md` 中的某条 D-NNN 决策。如发现不一致，以 `pluribus_stage1_decisions.md` 为准，本文档同步修正。

| 本文档类型 | 关联决策 |
|---|---|
| 整体技术栈选型（语言 / 测试 / proto / pyo3） | D-001 ~ D-008 |
| Crate 布局与模块边界（§9 模块导出） | D-010 ~ D-013 |
| `ChipAmount` | D-020 ~ D-026, D-026b |
| `ChipAmount` Sub 下溢 panic | D-026b |
| `payouts()` 返回 `i64` / 绝对筹码 `u64` 区分 | D-025b |
| `TableConfig` 默认值 / `default_6max_100bb` | D-021 ~ D-024, D-022b, D-030, D-031 |
| `Action::AllIn` 归一化 | D-033, D-042, D-061 ~ D-064 |
| min-raise 链式 / short all-in 不重开 | D-033 ~ D-035 |
| 全员 all-in 跳轮（I-006） | D-036 |
| `payouts()` / odd chip / 摊牌顺序 / side pot / uncalled | D-037 ~ D-041 |
| `string bet` 禁用（单次 Action 调用） | D-042 |
| 时间限制接口字段保留 | D-043 |
| `RngSource` / `ChaCha20Rng` / `RngCoreAdapter` | D-027, D-050 |
| `GameState::new(config, seed)` / `with_rng(config, seed, rng)` 发牌协议 | D-028 |
| `SeatId` 左邻方向约定 | D-029 |
| 跨平台 / 多线程一致性（`content_hash` / deterministic proto） | D-051 ~ D-054 |
| `HandHistory.schema_version` / proto 路径 / Python 绑定 | D-060 ~ D-066 |
| `HandEvaluator` 三接口 / `HandRank` / `HandCategory` | D-070 ~ D-075 |
| 交叉验证规模与参考实现引用 | D-080 ~ D-085 |

---

## 参考资料

- 阶段 1 决策记录：`pluribus_stage1_decisions.md`
- 阶段 1 验收门槛：`pluribus_stage1_validation.md`
- 阶段 1 实施流程：`pluribus_stage1_workflow.md`
- prost：https://github.com/tokio-rs/prost
- thiserror：https://github.com/dtolnay/thiserror

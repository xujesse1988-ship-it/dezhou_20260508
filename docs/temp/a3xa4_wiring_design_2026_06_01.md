# A3×A4 接进生产 PublicBettingTree 的实现设计（2026-06-01）

对应 `betting_history_abstraction_options_2026_05_31.md` 待办 (i)①。redirect 探针
（commit `6e6acac`，`tools/nlhe_betting_tree_sizing.rs`）只在 sizing 工具的 `walk` 里
**数节点**，不产出可训练的 game。本文定义把同一套规则搬进生产
`src/training/nlhe_betting_tree.rs`（建树）+ `src/training/nlhe.rs`（训练适配层
`Game::legal_actions`）的实现契约，**先定契约再写码**。

用户已选「先出实现设计再写码」：本文不改核心代码，只给落地蓝图 + 验收标准。

---

## 0. 规则放哪层 —— 最重要的正确性决定

**A3×A4 = 改策略空间（抽象 / betting tree），不改规则引擎。**

- **改**：`nlhe_betting_tree.rs`（建树时过滤动作）、`nlhe.rs`（运行期 `legal_actions`
  从树派生 + 抽象来源）。
- **绝不碰**：`src/rules/state.rs` 的 `GameState::legal_actions()`、side pot、showdown、
  `payouts()`。
- **后果**：规则引擎仍是**完整 6-max**，A3×A4 只是让 betting tree **不枚举**被砍的树枝
  → **S1 的 PokerKit 跨验证不受任何影响**（`project_6max_rules_validated`）。
- doc §A4 行 209「作为 position-asymmetric 的 `legal_actions()` 限制」指的是**训练适配层**
  `Game::legal_actions`（`nlhe.rs:449`），**不是**规则层 `GameState::legal_actions`
  （`state.rs:252`）。这点必须钉死，否则会误改规则引擎、破 S1。

为什么 A4「改游戏」却不用改规则引擎：redirect 砍掉的是 Check/Call 这两条**树枝**，
被训练器选中的动作（Fold / Raise / AllIn）照常 `game_state.apply()`，side pot / showdown
逻辑对实际发生的动作照常算。规则引擎只当**机制**（apply + 算收益），策略空间由树定义——
这正是「抽象」的标准含义。

---

## 1. A3×A4 规则的精确定义（逐字对齐 probe，勿改语义）

三个过滤，全部作用在 `abs.abstract_actions(state)` 之后、建 node 之前：

### 1a. 菜单（A3 first_small 的一半）
`StreetActionAbstraction::per_street([{1.0}, {0.5,1.0}, {0.5,1.0}, {0.5,1.0}])`
（`action.rs:534`）。preflop 单大档、postflop 含 0.5 小注。**这部分已有现成 API**。

### 1b. drop_small_reraise（A3 first_small 的另一半，无状态、逐动作）
删掉 `AbstractAction::Raise { ratio_label == BetRatio::HALF_POT }`（`action.rs:57`）。
即 0.5pot 只许作**开池 Bet**，任何 **re-raise 一律 1pot**。probe 出处
`nlhe_betting_tree_sizing.rs:438-444`。

> ⚠ 菜单（1a）与 drop 标志（1b）**必须配对**：只设菜单不设 drop = 全程 `{0.5,1}` 含
> 0.5 re-raise（= 224 GiB 那版，§A3）；只设 drop 不设菜单 = postflop 无 0.5、drop 空转。
> 故建议用**一个 profile 构造器**同时产出菜单 + rules，杜绝错配（见 §4）。

### 1c. width_redirect = N（A4 closing-action 优先，需线程化 `entrants`）
每个决策节点：第 (N+1) 个 **entrant** 不能被动进场（删 `Check` + `Call`，只剩
Fold 或 squeeze）。probe 出处 `nlhe_betting_tree_sizing.rs:415-459`，逐字搬：

```
e        = entrants.count_ones()
actor_in = (entrants >> actor) & 1 == 1
stay_e   = if actor_in { e } else { e + 1 }
block    = stay_e > N           // block 时删 Check / Call；Raise/AllIn/Fold 永不 gate
```

`entrants`（u16 bitmask，按 seat 索引）沿每条边更新（`:509-513`）：
**Fold → 清 actor 位；其它任何动作（含 Check）→ 置 actor 位**。跨街不清零。
N = 255 = 关。

**不变量**（probe 实测全过，§A3×A4 2026-06-01 校验三过）：
- postflop 决策节点最大在场（Active∪AllIn）**== N**；
- postflop 在场 **> N 的决策节点 == 0**（>N 只可能是多人 all-in 跑马，无 postflop 下注、
  不贡献决策节点）。
- HU self-check 仍守 240,096 节点（rules 全关时与历史 byte-equal）。

---

## 2. 树 ↔ 运行期一致性契约 —— F17-free 的关键

`D-318`（`nlhe.rs:80`）要求 **tree 的 `legal_actions` tag 顺序 == 运行期
`Game::legal_actions` 输出**，否则 regret 向量下标与 tree child 下标错位。今天两边都调
`nlhe_action_abstraction().abstract_actions(state)`、靠「同一计算」隐式相等。**加 filter
后这个隐式约定会破**：若只在建树加 filter，运行期仍返回全集，`next()`
（`nlhe.rs:483` 的 `position()`）会 panic「CFR 走了 tree 外动作」，且 regret 表多出被砍的槽。

**契约：运行期 `legal_actions` 改成「从树节点派生」**：

```
fn legal_actions(state):
    full = nlhe_action_abstraction().abstract_actions(&state.game_state)   // 规范 D-209 序
    node = state.tree.node(state.current_node_id)
    return full.filter(|a| node.legal_actions.contains(AbstractActionTag::of(a)))  // 保序
```

性质：
- 树是**唯一真相源**，运行期与树**一致 by construction**（filter 后必是 node tag 的子集
  且同序 → child 下标对齐）。
- 运行期**不需重算 `entrants`**——redirect 在建树时已 baked 进 node.legal_actions。
- **保 node_id（perfect recall）→ F17-free by construction**：每个 node 自带唯一
  legal_actions，info_set 用 node_id（`pack_info_set_v2`，`nlhe.rs:106`），两个同局面不同
  路径天然是不同 node、不会撞同 key 不同动作集（与 B3 摘要 key 那 100 万违例正相反）。
- `next()`（`nlhe.rs:464-526`）**逻辑不变**：仍按 tag 找 `edge_idx`；因运行期只产出树里有的
  tag，`position()` 必命中。

**等价性验收**：rules 全关（默认 HU 路径）时，derive-from-tree 必须与现行
`abstract_actions(state)` **byte-equal**（filter 退化为恒等，因 node.legal_actions ==
abstract_actions tags）。须有断言守住（见 §5）。

---

## 3. 建树 walk 的改动（`nlhe_betting_tree.rs`）

现 `walk(state, parent, action_from_parent, abs)`（`:117`）→ 加两样：

1. **`entrants: u16`** 参数（root 传 0），递归时按 §1c 更新。
2. **rules 配置**（见 §4），在 `legal_set` 取出后、`push` node 前套 §1b+§1c 的 filter；
   `node.legal_actions` / `children` 用 **filter 后**的集合。

新增构造入口（**不动** `build` / `build_with_abstraction` 默认行为，守 240,096 /
719,764 测试 byte-equal）：

```
PublicBettingTree::build_with_rules(config, abs, rules) -> PublicBettingTree
// build_with_abstraction(config, abs) == build_with_rules(config, abs, Rules::default())
```

建树内顺带 `debug_assert` §1c 不变量（walk 有 `state`，`live_count(state)` 现成）：
postflop 决策节点 `live_count(state) <= N`。把 probe 的 `redirect_postflop_over_n == 0`
变成生产侧编译期可查的断言。

---

## 4. N 参数化接口

```
// nlhe_betting_tree.rs（或 abstraction 层）
struct BettingAbstractionRules {
    drop_small_reraise: bool,   // §1b
    width_redirect: u8,         // §1c，255 = 关
}
impl Default → { drop_small_reraise: false, width_redirect: 255 }   // = 现行为
```

菜单（§1a）走已有的 `StreetActionAbstraction::per_street`。为杜绝 §1b 注里的菜单/标志错配，
建议**一个 profile 构造器同时产出二者**：

```
fn first_small_6max(width_redirect: u8) -> (StreetActionAbstraction, BettingAbstractionRules)
//   = (per_street([{1},{0.5,1},{0.5,1},{0.5,1}]),
//      Rules{ drop_small_reraise:true, width_redirect })
```

与 probe env 的对应（便于 cross-check 复算）：`FIRST_SMALL=1` ↔ 菜单 first_small +
`drop_small_reraise=true`；`WIDTH_REDIRECT=N` ↔ `width_redirect=N`。`N` 留参数，
**不阻塞**待办 (i)② 量「≤N-way 丢多少 EV」定 N（N=3 是甜点，先按 3 接、可改）。

---

## 5. 验收 / 测试计划（正确性闸门）

全部在 vultr 跑（`feedback_tests_on_vultr`；本机仅 build/fmt/clippy）。

1. **节点数 cross-check**（新 `#[ignore]` release 测试，套 `nlhe_betting_tree.rs:229`
   现有 `per_street_target_tree_node_count_matches_sizing_tool` 的路子）：
   `build_with_rules(default_6max_100bb(), first_small_6max(N))` 的 `tree.num_nodes()`
   == probe `FIRST_SMALL=1 WIDTH_REDIRECT=N` 的 `decision_nodes`。**精确值已跑 probe 取定**
   （vultr `6e6acac`，2026-06-01）：**N=3 = 1,154,822**（infoset@200 230.5M / depth 25 /
   redirect 415 restricted、postflop max live 3、>N 0）、**N=2 = 78,852**（infoset 15.6M /
   depth 17 / max live 2、>N 0）。两条独立代码路径（builder vs sizing 工具）对得上 = 接对了。
2. **守默认路径 byte-equal**：现有 `default_tree_node_count_unchanged`（240,096）+
   `per_street_...`（719,764）必须仍绿 —— rules 默认值不得改旧行为。
3. **运行期 derive-from-tree 等价**：默认 HU 路径上，新 `legal_actions` 与旧
   `abstract_actions(state)` 对每个可达 node **逐元素相等**（§2 等价性）。可在
   `tests/nlhe_infoset_semantics.rs` 旁加或新测试钉。
4. **不变量断言**：§3 的 postflop `live_count <= N` debug_assert 在 cross-check 建树时
   不 panic（= probe redirect 不变量在生产复现）。

---

## 6. 分阶段落地（本步 vs 下游）

- **本步（betting-tree 层，已实现）**：§3 建树（`build_with_rules` + `walk` 线程化
  `entrants` + 不变量 `debug_assert`）+ §4 `BettingAbstractionRules` / `first_small_6max`
  profile + §5 cross-check 测试。产物 = 生产能建 1,154,822(N=3) / 78,852(N=2) 节点的
  6-max A3×A4 capped 树，cross-check 钉死 == probe 真值。
  **§2 运行期 derive-from-tree 推迟到 P4**：本步 `SimplifiedNlheGame` 仍建默认 HU 树，§2 在
  默认树下是恒等过滤（纯 CFR 热路径开销）且**无可测对象**（无 A3×A4 game 可跑），落 P4 与
  n_seats 一起做、一并 perf-tune。本步未碰 `nlhe.rs`。
- **P4（已实现，commit `08b3edc`）**：`SimplifiedNlheGame` 去 HU 硬编码 —— 新增
  `new_with_abstraction(bucket_table, config, abstraction, rules)`，`new` 委托（HU 默认、
  byte-equal）；Game/State 携带 `abs: Arc<StreetActionAbstraction>`（`legal_actions` 用本
  game 的 abs，非全局硬编码）；`n_players()` → `config.n_seats`；**§2 derive-from-tree 落地**
  （`legal_actions` 过滤到 tree node tag，HU len 相等走 fast-path = byte-equal）；`info_set`
  `n_seats>2` 早返回 uncached 分支（2-slot u64 cache 容不下 6 座，HU 分支逐字不动；multiway
  cache 落 post-S3），位置仍由 node_id 内化 → 6-max 无碰撞、`pack_info_set_v2` 不改。smoke
  测试：6-max A3×A4 game 构造 + 轨迹走到 terminal + 6 座 payoff 守恒 + 树==78,852；HU 全绿
  （lib 52/0、`nlhe_infoset_semantics` T1、全 integration ok）。
  **桶 caveat**：6-max 暂用 HU 单对手 equity 桶占位 → 可构造 + 机制跑通，但有意义训练待 S3。
- **下游 P5 = S3**：6-max 多路 equity 桶（`equity.rs` 假设 1 对手，`six_max_nlhe_target.md`
  §S3）。**这是训练前真正的 gate**，全项目最大未知数，独立立项。

本步**不依赖** P4/P5：cross-check 测试直接建树验 `num_nodes()`，不经
`SimplifiedNlheGame`、不碰桶。运行期 `legal_actions` 的 derive-from-tree 改动先落，
默认 rules 下被现有 HU 测试覆盖（§5.2/5.3）。

---

## 7. 风险 / 边界

- **BB-check 算 entrant**（probe 副作用①，§A3×A4 2026-06-01）：超员 limped 池里 BB 的
  「过牌看 flop」= 第 (N+1) entrant → 被禁 → 只能 squeeze/fold（失去免费 flop）。这是
  「≤N 见 flop + 先到先得」的应有之义，**逐字搬 probe**（非 fold 即置位），别自作主张放过 BB。
- **多人 all-in 跑马越 N**：redirect 不 gate Raise/AllIn，逻辑上 shove 能让在场 > N；
  probe 实测这种线**无 postflop 下注、0 决策节点**。§3 的 debug_assert 守这条（应不触发）。
- **node_id 位宽富余**：1.15M < 2^21 ≪ 2^26（`pack_info_set_v2` 的 `NLHE_V2_NODE_ID_BITS`，
  `nlhe.rs:102,111`）。内存压力在 infoset@200 = 230.5M / dense 8.04 GiB（训练机内存，
  §A3×A4），不是 node_id 容量。
- **菜单/drop 错配**（§1b 注）：用 §4 profile 构造器规避。

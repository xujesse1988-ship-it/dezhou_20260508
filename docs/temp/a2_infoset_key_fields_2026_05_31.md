# A2 infoset key 字段详解（transposition 折叠的精确局面 key）

工作笔记。`docs/temp/betting_history_abstraction_options_2026_05_31.md` §A2 的展开稿，
**不改原文档**。回答："A2 的 infoset key 包括哪些字段，每个字段什么含义。"

A2 = 把 betting **树**折成 **DAG**：不再用 `node_id`（动作路径，完美回忆，
`src/training/nlhe_betting_tree.rs:6-9`，路径单射测试 `:296-308`）做下注维度，改用
**精确局面**做 key，让"殊途同归到同一局面"的不同动作序列共享一个 infoset。

## 0. 判断标准（每个字段要不要进 key，只看这一条）

> **A2 key = 续局博弈动态的充分统计量。**
> 两个状态 key 相等 ⟺ 从这里往下的子博弈**逐节点合法动作相同、逐叶收益相同**。
> 某字段**改变续局动态** → 必须进 key；**不改变动态**（只改 range、或只是信息记录）→ 不进。

注意 A2 是"对**博弈动态**无损、对 **range** 有损"——见 §3.3。它不是免费午餐，仍是对下注历史的
imperfect recall，只是粒度最细（不分桶）的那一档。理论与业界定位见原文档 §B3 + 参考。

## 1. 整体结构：infoset = 私有 + 公共

CFR 的 infoset 是**对某个行动玩家**而言的 = 私有信息 + 公共信息。本仓库现在就是这结构：
`pack_info_set_v2(hand_bucket, node_id, street_tag)`（`src/training/nlhe.rs:106`）。A2 只动公共那一半。

| 半边 | 现在 (v2) | A2 |
|---|---|---|
| 私有牌 | `bucket_id` + `street_tag` | **不变** |
| 公共下注 | `node_id`（动作路径，完美回忆） | **精确局面规范化 key** |

## 2. 私有牌部分（不变，2 字段）

| 字段 | 位 | 含义 | 来源 |
|---|---|---|---|
| `bucket_id` | 24 | 行动者**自己手牌**的强度桶。preflop = 169 无损 hand class；postflop = k-means 桶 id | `src/abstraction/info.rs:16`；postflop 走 `BucketTable::lookup` |
| `street_tag` | 3 | Preflop / Flop / Turn / River | `info.rs:100` |

infoset 里"对手看不见"的部分。A2 不碰。下面全是公共部分。

## 3. 公共下注部分（A2 的核心）= 精确局面规范化

把 `GameState`（`src/rules/state.rs:34-75`）里**决定续局动态**的字段抽出来、按 button 归一化后
拼成 key。分三类：必须显式编码、可派生（故意不进）、故意排除。

### 3.1 必须显式编码的字段

| 字段 | 来源 (`state.rs`) | 含义 | 决定续局的什么 |
|---|---|---|---|
| **`actor`**（相对 button） | `current_player` `:247` | **现在该谁动**（归一化成离 button 的相对位 0..5 / HU 0..1） | 谁行动 = 谁的合法动作、位置优劣、下一个轮到谁。同筹码不同 actor = 不同 infoset |
| **每个在场座位的 `committed_total`**（相对 button 排列） | `Player.committed_total` `:563` | 该座位**整手累计投入** | ① 底池 = Σ committed_total（`:331`）；② **side pot 分层**与摊牌归属（`contribution_levels`/`compute_payouts` `:973,846`）= 所有叶子收益 |
| **每个在场座位的 `committed_this_round`** | `Player.committed_this_round` `:562` | 该座位**本街已投入** | ① 当前注级 `max_committed_this_round`（`:750`）；② 各家**欠注** = 能否 check / call 多少；③ 本轮是否结束（`next_player_needing_action_from` `:722`） |
| **每个座位的 `status`** | `Player.status` `:565` | Active / Folded / AllIn | **谁还在牌局、谁还能动**。Folded 出局且不争底池；AllIn 争池但不再行动 |
| **`last_full_raise_size`** | `:66,500,513` | **最近一次足额加注的幅度** | **最小加注额** `min_to = max_committed + last_full_raise_size`（`:500`）→ 决定 `raise_range` 下界 |
| **`raise_option_open`**（在场座位 bitmask） | `:64,284` | 每家**还有没有"可加注"权** | NLHE"面对**不足额** all-in 不重开加注权"规则（D-033-rev1）：某些局面 actor 只能 fold/call、不能 re-raise（`:286,758`） |

要点：

- **`committed_total` 与 `committed_this_round` 都要、不能只留一个**：total 管底池/side pot/收益
  （跨街累计），this_round 管"当前轮谁欠谁"（每条街开头清零 `:712`）。从 total 反推不出 this_round
  的拆分，二者都得有。等价写法 = `(committed_prev = total − this_round, this_round)`。
- **`status` 里 Folded 必须显式、AllIn 可派生**：AllIn ⟺ stack==0 ⟺ committed_total==起始码量（可推）；
  但**弃牌玩家也有非零 committed**，从 committed 看不出它弃了 → "在场/弃牌"位掩码硬要。AllIn 那位
  顺手一起编码省得推。
- **必须"每个座位"，不是只看 actor**：A2 合并的是**整段续局子博弈**，子博弈依赖**所有在场玩家**
  状态（后面轮到谁、各家能下多少），不止当前 actor。公共下注 key 是两玩家共享的公共局面（`node_id`
  本来也是公共的）。

### 3.2 可派生 → 故意不进 key 的量

上面字段的函数，进了就是冗余：

| 量 | 由什么算出 |
|---|---|
| 每家 `stack` | 起始码量 − `committed_total`（筹码守恒 I-001 `:22`）。起始码量是 config 常量 |
| `pot` | Σ committed_total（`:331`） |
| `max_committed_this_round`（当前注级） | max(committed_this_round)（`:750`） |
| `to_call`（要跟多少） | max_committed − 自己 committed_this_round |
| `cap` / all-in 额 | committed_this_round + stack（`:266`） |
| SPR | 派生 stack / pot |
| 下一个行动者 | status + committed_this_round + raise_option_open + 座序（`:722`） |

> **A2 相对 B3 的硬优势就在这里**：`committed_*` 是**精确**的，`legal_actions()`（`:252-306`）是这些
> 精确额的**纯函数**，所以 **A2 key 相同 ⟹ 合法动作集相同（按构造）**——不需要 B3 那个
> `legal_action_set_id` 补丁，也不可能撞 F17（`info.rs:73-78`）。B3 把 `committed` 抹成
> `facing_size_bucket`/`spr_bucket` 才会"同 key 不同 legal_actions"（原文档 §B3 实测 100 万次）。

### 3.3 故意**排除**的字段（= A2"对 range 有损"的具体来源）

| 排除的东西 | 为什么不进 key | 代价 |
|---|---|---|
| **`last_aggressor`**（`:66`） | 只喂 `compute_showdown_order`（`:997`），而本引擎摊牌按**牌力**分钱（`pot_winners` `:940`），揭牌顺序不改收益 → **不改动态** | 丢"谁是侵略者" = **底池被动跟大 vs 被加注打大** 这层 range 区分 |
| **精确动作序列 / 街内逐手次序 / 弃牌先后** | 殊途同归到同一 committed 向量后，路径不改续局动态 | 同上，丢路径携带的 range 信息 |
| **本街已加注次数**（原文档 §A2 informal 的 `本街已加注数`） | NLHE **没有 3-bet 上限规则**，加注次数本身不 gate 合法动作——真正 gate 的是 `last_full_raise_size` + `raise_option_open`（已在 3.1） | 无（除非叠 A1 raise-cap，见 §4） |

**关键认知**：A2-key 是**续局动态**的充分统计量，故意不带 `last_aggressor` 这种只携带 range、不改
动态的信息。所以 A2 把"底池怎么变大的"忘掉了——这就是它虽"对动态无损"却"对 range 有损"的出处。

## 4. 与原文档 §A2 / §B3 的对应

**精化原文档 §A2 informal `(各家已投筹码, 该谁动, 本街已加注数)`**：
- `各家已投筹码` → 拆成精确 `committed_total` + `committed_this_round`（两者都要）；
- `该谁动` → `actor` 相对 button；
- `本街已加注数` → 替换为 `last_full_raise_size` + `raise_option_open`（这俩才真正决定能不能加、
  最小加多少）。
- ⚠ **A2 叠 A1 raise-cap 时**，"本街加注次数（capped）"**要重新进 key**——这时 cap 到顶会砍掉
  sized raise，次数反过来 gate 合法动作。单用 A2 不需要。

**与 B3 字段表逐项对照**（A2 精确 / B3 分桶）：

| 维度 | A2 | B3 |
|---|---|---|
| 该谁动 | `actor` 相对 button（精确） | `actor_position` 相对 button（同） |
| 在场结构 | 每座位 status（Active/Folded/AllIn，更细） | `live_players` bitmask（Active∪AllIn 合一） |
| 面对尺寸 | 精确 committed_this_round → 精确 to_call | `facing_size_bucket`（5 桶） |
| 深度 | 精确 committed_total → 精确 SPR | `spr_bucket`（12 桶） |
| 侵略者 | **排除** | `last_aggressor` 2 槽（**保留**） |

⚠ **A2 与 B3 是不同的划分、不是嵌套**：A2 几何上更细（精确额）但**故意丢 aggressor**；B3 几何上
更粗（分桶）却**特意保留 aggressor 去补 range**。谁的 range-skew 更小要看局面，不能一概而论。A2
确定占优的只有一条：**对博弈动态无损 + legal_actions 天然一致**（这条严格成立）。

## 5. 具体例子：A2 合并了什么、丢了什么

HU，turn 起手，两条 preflop 线：

- **路径 P**：preflop SB 加注到 3、BB 跟 → flop BB check、SB 下 3、BB 跟 → turn 起手 committed_total=(6,6)。
- **路径 Q**：preflop SB **limp 到 1**、BB check → flop BB check、SB 下 5、BB 跟 → turn 起手 committed_total=(6,6)。

turn 起手两边**所有 A2 字段逐一相等**：actor=BB、committed_total=(6,6)、committed_this_round=(0,0)
（新街清零）、双方 Active、`last_full_raise_size`=BB（新街重置 `:701`）、`raise_option_open`=(true,true)。

→ **同一 A2 key，合并成一个 infoset**（省内存的"赢"）。
→ 但 P 里 SB 是 **preflop 加注** range，Q 里 SB 是 **limp** range，完全不同。A2 强迫 BB 在两种 SB
range 下共用一份策略（这是"丢"——且因 A2 不带 `last_aggressor`，连"SB 这条线是否侵略过"都分不出）。
正是原文档警告的"忘了底池怎么变大的"。

## 6. key 形态与位预算

与 B3（定宽 bitfield）不同，A2 公共部分是**精确向量**，自然形态 = 对
`(actor, [per live seat: committed_total, committed_this_round, status], last_full_raise_size,
raise_option_open)` 做**规范化哈希 / 去重**：

- HashMap 后端（C5）：直接拿规范化 key 当 map key，无需定宽。
- dense 后端：建表时**走树收集 distinct A2 key** → 分配 dense 下标（替代现在按 `node_id` 的 prefix-sum
  `src/training/nlhe_dense.rs:9-13`）。
- 抽象动作网格让 committed 只取有限值，distinct 向量数有界——**到底多少 = 待 `A2_TRANSPOSE` 探针实测**
  （镜像 `B3_SUMMARY`，key 换成本文字段，断言 0 次 legal_actions 违规——应按构造成立，触发即说明 key
  漏字段如 `raise_option_open`）。

## 7. 下一步候选

- 给 `tools/nlhe_betting_tree_sizing.rs` 加 `A2_TRANSPOSE` 探针：用本文字段拼 key，跑 `{1.0}` /
  `{1.0,2.0}` / `{0.5,1.0}` + HU self-check，把对照表补成三列 `node_id(路径) → A2(精确局面) →
  B3(分桶)`。中间列 = 新数据，直接读"无损能到哪"。
- 决策规则：A2 列若已进内存预算 → 用 A2 跳过 B3（更安全、无补丁、动态无损）；不够 → B3 分桶是唯一
  出路，而 node_id→A2 是免费的、A2→B3 是花 range 信息换的，风险已量化。
- imperfect recall 收敛无保证 → 仍须 exploitability/LBR 对 `{1.0}` perfect-recall baseline 实测裁定。

## 参考

见原文档 `docs/temp/betting_history_abstraction_options_2026_05_31.md` §参考（Waugh / Kroer-Sandholm /
Johanson / Pluribus / DeepStack）。本文新增佐证：transposition table 是**完美信息**博弈技术
（Wikipedia: Transposition table），不完美信息下因 range 路径依赖不免费——DeepStack 敢在 public tree
推理是因为它同时携带 range + 对手 CFV，tabular blueprint 无此通道。

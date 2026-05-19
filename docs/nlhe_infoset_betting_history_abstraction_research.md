# NLHE InfoSet 下注历史压缩调研

## 文档目标

本文记录 2026-05-19 对 `SimplifiedNlheInfoSet` 中下注历史维度的调研结论：当前
`InfoSetId v2` 用抽象 betting tree 的 `node_id` 表示完整公开下注路径，能避免跨街
history collision，但也让 reachable infoset 数随 betting tree 规模快速膨胀。本文调研
公开 poker solver / poker AI 文献中是否存在类似“按街压缩下注历史”的做法，并给出
后续实现建议。

本文只讨论调研与设计方向，不改动当前编码实现。

## 当前问题

当前简化 NLHE 的 `InfoSetId v2` 逻辑大致是：

`hand_bucket | node_id | street_tag`

其中 `node_id` 来自抽象 public betting tree，单射于从 root 到当前决策点的抽象动作序列。
这个设计修复了旧版仅按手牌 bucket / 街 / 粗 betting state 编码时的跨街 collision：
例如“SB preflop 加注后 BB 跟注进入 flop”和“SB limp、BB 加注、SB 跟注进入 flop”
虽然可能到达同一条街、同一手牌 bucket、同一当前玩家，但双方 range 与主动权结构不同，
不应该共用同一策略。

代价是 infoset 膨胀。`docs/status.md` 中已有规模记录：100BB 抽象 betting tree
节点约 `5,201,712`，200BB 约 `29,744,992`。这些节点大多来自下注历史路径，而
CFR regret / average strategy 表会围绕 infoset key 展开。若每条路径都保持唯一，
训练内存与采样覆盖都会变重。

核心问题是：是否可以把完整 `node_id` 压缩为“每街下注形态摘要”，例如：

- `CheckCheck`
- `BetCall`
- `BetRaiseCall`
- `BetRaiseRaiseCall`
- `CheckBetCall`
- `CheckBetRaiseCall`
- `AllInCalled`

并在 infoset 中只保留每一街的这类枚举，而不是完整抽象 betting tree 节点。

## 调研结论

可以做，但它不再是完整 public history 的 perfect-recall key，而是一个 lossy information
abstraction，很多情况下也可视为 imperfect-recall abstraction。公开系统里有相近思路：
主流做法不是保留真实下注额和完整真实路径，而是保留“抽象后的 action sequence”、
“每街 raise 次数 / 下注形态”、或在后续街使用更粗的抽象。

更稳妥的表述是：

> 不建议直接把 `node_id` 替换成极粗的全局 raise count；建议替换成“按街 public
> betting pattern + 当前街更细状态 + 必要的 sizing bucket / aggressor 信息”。

也就是说，压缩方向有公开依据，但实现时必须守住两个约束：

1. 同一个 infoset key 下，`legal_actions` 的数量和语义必须一致。
2. 被合并的历史应保留足够的 range 信息，至少区分主动权、是否 facing bet、是否有 raise
   option reopened、以及关键下注大小 bucket。

## 公开案例

### Pluribus：按抽象动作序列与信息 bucket 合并

Pluribus 论文说明它使用两类 abstraction：action abstraction 和 information abstraction。
action abstraction 只保留少量下注尺寸；information abstraction 把相似决策点合并。论文中
写到，Pluribus 在任意决策点只考虑少数下注大小，数量随局面变化，大约在 `1..=14`
之间；真实对手若下注到 abstraction 外，后续通过实时搜索或映射处理。

补充材料进一步说明：Pluribus 的 infoset 由“相同 action-abstraction sequence”和
“相同 information-abstraction bucket”合并；也就是说，它保留的是抽象下注序列，
不是无限注真实下注历史。补充材料还提到 blueprint 的 action abstraction 在第一轮最细，
第二轮更粗，第三、第四轮首个 raise 最多只有 `0.5 pot`、`1 pot`、`all-in`
三类，后续 raise 最多两类。

参考：

- [Superhuman AI for multiplayer poker](https://noambrown.github.io/papers/19-Science-Superhuman.pdf)
- [Supplementary Materials for Superhuman AI for multiplayer poker](https://noambrown.github.io/papers/19-Science-Superhuman_Supp.pdf)

对本项目的启发：

- `node_id` 保留完整抽象路径，比 Pluribus-style blueprint 的“抽象 action sequence +
  bucket”更细。
- 后续街可以更粗；当前街应相对更细。Pluribus 实战中也强调当前搜索街更细，未来街更粗。

### Libratus：blueprint 使用 action abstraction，off-tree 用 nested solving

Libratus 论文把 HUNL 的巨大状态空间分成三步处理：先求解较小 abstraction 得到
blueprint；实际到达后续局面时使用更细 subgame solving；对手选择 abstraction 外动作时，
通过 nested subgame solving 重新计算响应，而不是简单永久依赖最近邻映射。

论文中也说明 action abstraction 的直觉：下注 `100` 与 `101` 差别很小，因此可以把连续
下注空间离散化，使用较少的下注尺寸来降低复杂度。它没有直接说“每街 enum”就是最终
key，但确认了大规模 HUNL 系统不会把完整真实下注额路径逐一作为策略表 key。

参考：

- [Superhuman AI for heads-up no-limit poker: Libratus beats top professionals](https://noambrown.github.io/papers/17-Science-Superhuman.pdf)
- [Safe and Nested Subgame Solving for Imperfect-Information Games](https://noambrown.github.io/papers/17-NIPS-Safe.pdf)

对本项目的启发：

- blueprint 可以粗，实时 search / future re-solving 可补精度。
- 如果后续引入 off-tree 或 finer subgame，当前 infoset key 不必承载所有真实路径细节。

### Tartanian：discretized betting model 控制下注序列规模

Tartanian5 / 早期 CMU no-limit bot 文献明确指出，no-limit 与 limit 的主要差异是下注
动作空间巨大，因此必须使用 discretized betting model。Tartanian5 在摘要和 betting
abstraction 章节中都强调：为了让 betting sequences 数量可处理，只允许每个局面少数下注
大小。

参考：

- [Tartanian5: A Heads-Up No-Limit Texas Hold'em Poker-Playing Program](https://www.cs.cmu.edu/~sandholm/www/Tartanian_ACPC12_CR.pdf)
- [A heads-up no-limit Texas Hold'em poker player: Discretized betting models and automatically generated equilibrium-finding programs](https://www.cs.cmu.edu/~./sandholm/tartanian.AAMAS08.pdf)

对本项目的启发：

- 当前项目已经有 action abstraction，但 `node_id` 仍保留完整抽象 betting tree 路径。
- 下一步可在“动作已经离散”的基础上，再对“历史序列”做合并。

### CPRG / Limit Hold'em：按每街 betting sequence 拆分

Michael Johanson 的 thesis 中有一个非常接近“每街 enum”的例子：Limit Hold'em 中能进入
flop 的 preflop betting sequences 被列为有限几类，例如：

- `check-call`
- `check-bet-call`
- `check-bet-raise-call`
- `check-bet-raise-raise-call`
- `bet-call`
- `bet-raise-call`
- `bet-raise-raise-call`

这些序列被用来拆分 postflop game tree 和并行计算任务。虽然这是 limit poker，不是
no-limit blueprint key 设计，但它说明“用有限下注线形态标识跨街上下文”是 poker solver
工程中的常见结构。

参考：

- [Robust Strategies and Counter-Strategies: Building a Champion Level Computer Poker Player](https://citeseerx.ist.psu.edu/document?doi=cbdf486e1e4d6832b2b2131fd06632526a858783&repid=rep1&type=pdf)

对本项目的启发：

- 用户提出的 `check-check`、`我加注-对手跟注`、`我加注-对手再加注`，与这类 betting
  sequence 枚举非常接近。
- 对 heads-up 来说，每街序列种类有限，特别适合做 compact public history summary。

### Automated Action Abstraction：限制每轮 raise 次数与下注大小

Hawkin / Holte / Szafron 的 action abstraction 论文在 no-limit Leduc 上使用非常小的动作
集合作为 baseline：`fold`、`call`、`pot bet`、`all-in`，并限制每轮最多两次 raise。论文
展示了在大型 no-limit-like 游戏中，限制下注尺寸与每轮 raise 次数可以把状态空间从不可解
压到很小的抽象游戏。

参考：

- [Automated Action Abstraction of Imperfect Information Extensive-Form Games](https://webdocs.cs.ualberta.ca/~holte/Publications/AAAI11_betSizeSolving.pdf)

对本项目的启发：

- “每街 raise count capped / absorbed” 是可参考的压缩维度。
- 但仅用 raise count 不够；还需要区分 bet-call、check-raise、bet-raise-call 等形态。

### Imperfect-recall abstraction：理论上允许遗忘部分历史，但需评估损失

把完整下注历史压成粗 enum，意味着玩家可能忘掉自己或对手的某些历史细节，这属于
imperfect-recall abstraction 的范畴。Cermak / Lisy / Bosansky 的论文研究了自动构建
bounded-loss imperfect-recall abstraction，并指出这种方法可以显著降低信息集数量，但需要
检测哪些信息不能丢。

较新的 KrwEmd 研究也从反面提醒：过度遗忘历史会造成 excessive abstraction，影响策略质量；
更好的 hand abstraction 应该把历史信息以压缩特征纳入，而不是完全丢弃。

参考：

- [Automated Construction of Bounded-Loss Imperfect-Recall Abstractions in Extensive-Form Games](https://arxiv.org/abs/1803.05392)
- [KrwEmd: Revising the Imperfect-Recall Abstraction from Forgetting Everything](https://arxiv.org/abs/2511.12089)

对本项目的启发：

- `node_id -> per-street pattern` 是合理的 imperfect-recall trade-off。
- 不应一步压到“只看当前街 / 只看 hand bucket”；应保留少量历史特征，避免过度合并。

## 推荐抽象方案

建议新增一个独立的 public betting history abstraction，而不是直接复用现有
`BettingState`。

### 每街结束摘要

对已经结束的街，使用较粗枚举：

```text
StreetPattern =
  NoAction
  CheckedThrough
  BetCall
  BetRaiseCall
  BetRaiseRaiseCall
  BetRaiseRaisePlusCall
  BetFold
  RaiseFold
  AllInCalled
```

heads-up postflop 可额外区分：

```text
ProbeOrDonk
CheckRaiseCall
CheckRaiseFold
CheckBetRaiseCall
```

preflop 建议单独处理，因为 big blind 是强制下注，`limp`、`open raise`、`3bet`、`4bet`
和 postflop `bet/raise` 语义不完全一致。

### 当前街状态

当前街不能只用结束摘要，因为 actor 的合法动作与 regret vector 长度依赖当前局面。建议至少保留：

- actor 是否 facing bet。
- 当前街 voluntary raise count，超过阈值吸收为 `Raise3Plus`。
- 最后 aggressor 的相对身份：`Hero` / `Opponent` / `None`。
- 本街是否 check-through 仍可能发生。
- actor 是否 still-open，是否因 full raise 重新打开 raise option。
- 当前 call amount / SPR / effective stack bucket。
- 最后一注或上一轮 raise 的 size bucket，例如 `Small` / `HalfPot` / `Pot` / `OverPot` /
  `AllIn`。

### 编码草案

`InfoSetId` 高位中的下注历史维度可以从：

```text
node_id: 26 bit
```

替换为类似：

```text
preflop_pattern: 5 bit
flop_pattern:    5 bit
turn_pattern:    5 bit
river_pattern:   5 bit
current_state:   6-10 bit
```

具体 bit 宽可等实现前的统计实验决定。若需要兼容 6 人长线扩展，`StreetPattern` 不应写死
`Hero/Opponent`；内部可以用 heads-up relative role 优化，但 API 和数据结构应允许未来扩展
为 seat-relative aggressor / caller mask。

## 风险与验收

### 主要风险

1. **legal action collision**：两个不同 `node_id` 合并后，当前 actor 的合法动作数量不同。
   CFR 表同一个 key 下 action vector 长度不同会直接破坏训练。
2. **range collision**：下注线形态相同但 size 差异巨大，例如 `min-raise-call` 与
   `2x pot raise-call`，对后续 range 和 SPR 影响不同。
3. **主动权丢失**：只记 raise count 不记最后 aggressor，会混淆 c-bet、donk、check-raise
   等策略结构。
4. **imperfect recall 损失**：压缩太粗会让策略忘记自己前面街的选择，可能增加 exploitability。
5. **checkpoint 不兼容**：`InfoSetId` layout 变化会让现有 NLHE checkpoint 失效，需要 bump
   schema / fingerprint。

### 建议先做的实验

在改训练主路径之前，先做一个只读统计工具：

1. 遍历或采样抽象 betting tree，计算当前 `node_id` key 数量。
2. 用候选 `StreetPatternKey` 重新映射，计算压缩后 key 数量。
3. 对每个 compressed key 收集：
   - legal action arity 分布。
   - action tag 集合是否一致。
   - pot / SPR / call amount bucket 分布。
   - last aggressor / raise count / street 分布。
4. 输出 collision report：
   - `safe_merge_count`
   - `legal_action_collision_count`
   - `range_feature_collision_count`
   - top-N 最大合并桶样例。

验收门槛建议：

- `legal_action_collision_count == 0`。
- 若同 key 下 call amount / SPR bucket 分裂严重，必须把对应 bucket 加入 key。
- 先在 H3 简化 NLHE 上对比训练曲线、LBR proxy、baseline winrate，再决定是否替换默认
  `InfoSetId`。

## 推荐决策

短期不要直接删除 `node_id`。推荐按以下顺序推进：

1. 新增 `StreetPatternKey` 统计工具，只读评估压缩率和 collision。
2. 设计 `NlheInfoSetHistoryMode`，支持 `FullNodeId` 与 `StreetPattern` 两种模式。
3. 先让 `StreetPattern` 作为实验 profile 跑 H3，不影响默认 checkpoint。
4. 只有当 collision report 干净，并且 H3 指标不明显退化时，再考虑把默认 infoset key
   从 `node_id` 切到每街 pattern。

总体判断：用户提出的“每街几个枚举值”有公开实践背景，尤其接近 Pluribus 的抽象 action
sequence 和 CPRG 的 betting sequence 分类。但为了避免过度 imperfect recall，工程实现不应只
记录 `check-check / raise-call / re-raise` 这类最粗标签，而应把当前街 legal action 所需信息、
主动权和关键 size bucket 一起纳入 key。

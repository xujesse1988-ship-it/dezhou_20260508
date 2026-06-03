# 6-max NLHE 实时搜索（depth-limited subgame solving）实现方案

> 日期 2026-06-03 / 分支 `6max`。把 `docs/six_max_nlhe_target.md` S6（= `docs/temp/pluribus_path.md` 阶段 6）
> 从 **parked / 非目标** 推进为**可落地工程方案**。所有 `file:line` 已对真实代码核验（多 agent 独立精读 +
> 对抗验证）。本文是**设计探索**，不是立项承诺——是否 un-park S6 由用户拍板。
>
> 一句话定调：**走 Pluribus / Modicum 式 tabular depth-limited search + 多 biased continuation strategies**，
> 复用现有 `blueprint_advisor` off-tree 引擎 + 多人 generic CFR 递归核，**不引神经网络、不追求 exploitability
> 理论保证**，质量全程以实测 mbb/g + CI 为准。

---

## 0. TL;DR（先读这一页）

1. **这是正确的下一根杠杆，且本仓库异常适配**：实时搜索是 Pluribus 真正赢人的机制（论文称占总量 30–40% 且最易写错）。
   但本仓库已有约 **70% 底座**——off-tree 引擎（`blueprint_advisor.rs`）、多人 generic CFR 核（`recurse_es`）、
   可配 bet 菜单的建树器（`build_with_rules`）、dense blueprint、甚至"blueprint 当值函数"的活范例（`aivat_value.rs`）。
   真正要新写的只有四件：**range 估计、subgame Game 包装（中途根）、multi-valued biased 叶子、以及围绕 6 个易错点的正确性纪律**。

2. **方法已钉死（对抗验证 = TRUE）**：用 **Pluribus（Science 2019）/ Modicum（NeurIPS 2018）的 tabular depth-limited
   search + 续局策略**；**不**用 DeepStack 式 safe subgame solving（gadget/CFV 的安全性证明依赖 2 人零和，在 6-max
   结构性失效），**不**用 ReBeL（神经网络路线，明令非目标）。诚实结论：6-max 下我们**放弃全部理论保证**——但
   DeepStack/ReBeL 的保证在一般和里**本就失效**，所以"留在 tabular"实际上**没损失任何真有效的保证**；NN 唯一买到的
   是 O(1) 泛化叶子值（延迟），我们用 blueprint rollout 替代（~4× 成本，但都是廉价查表）。

3. **一个必须先正视的前提风险（本文最重要的独立判断）**：续局/搜索的全部经验有效性**前提是 blueprint 已近似均衡**
   （Modicum §4 / Pluribus 明写）。但本仓库 blueprint **自评仍欠训练**（`project_6max_s4_undertrained_reshape`：1B
   仅 56% 表覆盖仍线性爬、preflop 支配翻转 14% 噪声）。**在弱基底上建续局，搜索可能放大 blueprint 的系统偏差而非修正它。**
   → MVP 的第一要务就是**证伪这个风险**（单层搜索到底退不退化），而不是先堆 biased leaf。这也牵出一个排期权衡（§2）。

4. **落地形态低侵入**：搜索只替换决策循环里"查 blueprint 出分布"那**一行**（`blueprint_advisor.rs:421`），
   `outgoing_action`（真实下注翻译）与影子推进**完全不动**。新增一个 `SubgameNlheGame impl Game`（只重写 `root()`），
   就能让 `EsMccfrTrainer`/`VanillaCfrTrainer` **原样**跑 subgame，**不碰 `trainer.rs`**。

5. **验收范式改写**：6-max 下 LBR/exploitability 失去理论意义，**删掉 pluribus_path.md 6b 的"LBR 显著下降"闸门**，
   改为 `evaluate_cross_abstraction_h2h` 的**受控 A/B（search-on vs search-off）实测 mbb/g + CI** + OpenPoker live。

---

## 1. 方法选型与独立判断

### 1.1 候选与裁决

| 路线 | 采用 | 理由（对齐本仓库约束） |
|---|---|---|
| **Pluribus continual re-solving + 4 biased leaf**（Science 2019） | ✅ 主线 | 与本仓库**完全同一游戏**（6-max 100BB 无 rake）+ **同一技术栈**（external-sampling Linear MCCFR blueprint + tabular 实时重解，非 NN）的唯一公开超人系统。`blueprint_advisor.rs` 的 OUR/OPPONENT-turn 推进骨架就是论文 Algorithm 2 的现成壳，缺的只是 `Search(G_root)` 这一步。 |
| **Modicum multi-valued states / continuation strategies**（NeurIPS 2018） | ✅ 借机制 | 提供"叶子不给单值、给 N 个续局值向量、对手选最不利"这个**禁 NN 下唯一的 depth-limited 叶子估值机制**。Pluribus 的 4 biased leaf 就是它的推广（所有在场玩家都选，而非只对手）。实测：Modicum 4-core/16GB/20s 一手就打赢了 Baby Tartanian8 / Slumbot——**笔记本级算力**。 |
| **DeepStack / Safe & Nested Subgame Solving**（2017） | ❌ 不照搬 safe gadget | gadget（Resolve/Maxmargin/Reach）的安全性 = 2 人零和的 minimax 对偶 + CFV 零和守恒。6-max 一般和里 `u₁≠−u₂`、无单一"可剥削性"标量、CFV 的"终止收益"语义失效 → **整套保证结构性失效**（不是工程难度，是定理前提缺失）。价值网络更直接违反禁 NN。**只借工程构件**：range 沿历史累乘、就地子树构造、nested off-tree 扩展（均无保证）。 |
| **ReBeL（深度 RL + search）** | ❌ 非目标 | 它的招牌优势是 2p0s 的 Nash 保证——在我们的一般和游戏里**本就 void**；且强制 GPU 神经网。trade-off 结论：**我们损失的是"泛化的 O(1) 叶子值"（延迟），不是一个对我们有效的保证**。 |

### 1.2 多人一般和如何改变结论（不是复述论文）

这是本方案最重要的诚实标注：**Pluribus/Modicum 在 6-max 下所有"鲁棒/抗剥削"性质均无理论保证**——论文自己反复明说
（main p.2 "not guaranteed to converge to a Nash equilibrium outside two-player zero-sum"；p.3 "no theoretical
benefit to using average strategy in six-player"；Supp p.20 承认 unsafe search "lacks theoretical guarantees …
there are cases where it leads to highly exploitable strategies"）。具体哪些前提塌：

1. **Modicum Proposition 1 是 2p 定理**：依赖单一对手 P2 + minimax 可交换性。6-max 的"对手 BR"不是单一对象——
   "5 家联合最优响应 = 最坏情况联盟"过度保守且不对应任何真实对手分布。
   **设计推论：续局的"对手选最不利"不能建成"全桌串通超级对手"（过保守），而是"每个在场对手各自独立从 k 个 biased
   续局里选"**（更真实，但失去 minimax 安全证明，退化成"对 blueprint 池的经验鲁棒响应"）。Pluribus 选的就是后者。
2. **unsafe re-solve 的根分布"最优性"** 论文明加条件（Supp p.21）：只有"≤2 人还在手 或 所有剩余玩家用同一解"才理论最优；
   6-max 多人在场时是实战折中。
3. **CFR 不保证收敛 Nash**（CLAUDE.md / six_max_target 已明确）→ subgame solve 出的 σ 没有"安全策略下界"语义，
   只能当"对当前续局集合的经验改进"。

### 1.3 明确放弃 / 保留了什么

- ❌ 不宣称任何 exploitability / 不被剥削的**理论**保证；**6-max 验收不以 LBR/exploitability 为金标准**。
- ❌ 不引价值网络（NN 路线非目标）；不引全局 RNG；规则/抽象/off-tree 层不用浮点（CFR 内部 regret/σ 与 leaf EV 的
  `f64` 是 invariants 既有豁免）。
- ✅ 保留：byte-equal 可复现（走 `RngSource`）、tabular、无 `unsafe`。
- ✅ 唯一质量金标准：**实测对战 mbb/g + CI**（OpenPoker live / 跨抽象 h2h `evaluate_cross_abstraction_h2h` /
  Slumbot+AIVAT）。

---

## 2. 必须先正视的前提风险 + 排期权衡（独立战略判断）

**风险**：depth-limited search 的有效性建立在"blueprint 已近似均衡"上。本仓库的 `nolimp`/`preopen` blueprint 经
独立核验**仍欠训练**（1B 仅 56–65% 表覆盖、仍线性爬；preflop 域内支配翻转噪声非盲位虽已 reshape 压到 <1%，但全表
深层多人线远未训透）。续局策略 = blueprint 的 bias 变体；若 blueprint 在某些线本身就系统性偏，**搜索会忠实地把这个偏
当"真值续局"放大**，可能比 blueprint-only 更差。

**这牵出一个真实的排期选择**（建议提交用户决策，不要默认）：

- **路线甲（先把 blueprint 训透，再上搜索）**：把现有算力继续投在 `preopen` 训练（提覆盖率）+ 可能扩抽象（更多 bet 档），
  让搜索建在更稳的基底上。代价：搜索收益推迟；且 blueprint-only 单独**已知达不到 Pluribus 级**（文档明列），
  纯靠训练边际递减。
- **路线乙（先上搜索 MVP，用它反过来诊断 blueprint）**：先做单层搜索 MVP（§10），用"search vs blueprint-only 是否退化"
  这个信号**当 blueprint 质量的探针**——如果连单层搜索都退化，就是 blueprint 太弱的实锤，回去训练；如果不退化甚至小赢，
  说明基底够用、可继续堆 6b。代价：可能白做一轮 plumbing（但 plumbing 本身是 S6 必经之路，不浪费）。

**我的倾向：路线乙**。理由：(1) MVP plumbing 无论如何都要写；(2) 它能用最小成本**证伪/证实**"基底够不够"这个当前最大未知数；
(3) 与 feedback「正确性大于一切」「写测试前问能否让错算法 fail」一致——MVP 的不退化 smoke 正是这样一个判别器。

---

## 3. 现有底座盘点（≈70% 已就绪）

| 构件 | 复用度 | 关键 `file:line` | 说明 |
|---|---|---|---|
| off-tree 翻译引擎 | 改造 | `blueprint_advisor.rs:354-456`（决策循环）/`193-246`（outgoing）/`260-316`（incoming） | 权威 `GameState` + 每 blueprint 一份 `SimplifiedNlheState` 影子；**插桩点 = L421**。outgoing/incoming 翻译可零改动复用。 |
| 多人 generic CFR 核 | direct | `trainer.rs:644-726`（`recurse_es`）/`746-848`（并行）/`187-271`（`recurse_vanilla` 精确期望）/`352-367`（LCFR rescale） | `recurse_*` 第一个参数就是**任意 `G::State`、root-agnostic**；`payoff(traverser)` 不取负、`%n_players` 轮换 = 已是 Pluribus 式 general-sum-capable。 |
| 建树器（可配菜单/宽度） | 改造 | `nlhe_betting_tree.rs:219-238`（`build_with_rules`）/`240-358`（`walk`）/`373-385`（`path_to_root`） | `walk` 的 `entrants/raises_on_street/parent` 都是**入参**；`StreetActionAbstraction::per_street` 已支持每街独立 raise 集。缺中途根入口（见 §5a）。 |
| dense blueprint + 索引 | direct（查策略）/ 缺（查值） | `nlhe_dense.rs:155-176`（`locate` O(1)）/`181`（`row_for`）/`374`（`average_strategy_by_info`） | **只存策略/regret，不存值**——无任何 `value_by_info`。"从 dense 表直接查 EV"路径**不存在且数学上不可达**（见 §5c）。 |
| blueprint 当值函数（范例） | 改造（2→N 人） | `aivat_value.rs:38-67,139-242`（`build_value_tables`） | 跑一遍 blueprint self-play 缓存 `(node_id,bucket)→E[U]`——**正是 6a 的"blueprint-as-value-fn"活范例，零 NN**。现硬编码 2 人，需推广 N-player。 |
| off-tree 下注映射 | 改造（6c 升级） | `action.rs:387-493`（`map_off_tree`，D-201 stub） | 现为 nearest-ratio stub，确定性 no-panic，**非 Pluribus-grade**；6c 升级为 pseudo-harmonic randomized rounding。 |
| 受控评测骨架 | direct | `blueprint_advisor.rs:556-661`（`evaluate_cross_abstraction_h2h`） | mbb/g + CI95 + per-position + desync 计数，rayon 并行 bit-可复现——直接当 S6 验收 harness。 |

**缺口（必须新写）**：① per-player range/belief 通道（影子只持点状态，无 range）；② 中途 public state 为根的 subgame 构造
（`GameState` 全字段私有、唯一构造路径 deal-from-scratch，无 `from_raw_state`）；③ depth-limit 叶子 + multi-valued biased
续局；④ desync→blueprint 防御兜底（搜索中途撞结构性 gap 无法事后排除）。

---

## 4. 目标架构

### 4.1 唯一插桩点

```
现状（blueprint-only，blueprint_advisor.rs:421-422）:
  let dist = strategy_distribution(&info, &legal_abs, contestants[bp_idx].strategy);  // L421 查表
  let chosen = sample_discrete(&dist, sample_rng);                                     // L422

搜索版（替换 L421）:
  let dist = if should_search(&auth, actor) {
      subgame_search(&auth, actor, &shadows[bp_idx], &legal_abs, &leaf_eval, search_cfg)
          .unwrap_or_else(|_| strategy_distribution(&info, &legal_abs, contestants[bp_idx].strategy)) // 兜底
  } else {
      strategy_distribution(&info, &legal_abs, contestants[bp_idx].strategy)  // 首轮非搜索情形纯 blueprint
  };
  let chosen = sample_discrete(&dist, sample_rng);
```

`outgoing_action`（L425）与影子推进（L432-439）**完全不动**——这是"低侵入"的关键。

### 4.2 一次决策的数据流

```
play_cross_abstraction_hand 决策循环（blueprint_advisor.rs:375-456）
  权威 GameState auth ──(真实筹码/牌/board, L406-409)──┐
  影子 shadows[bp_idx] ──(current_node_id, legal_abs, L410-411)──┐
                                              ▼        ▼
  ┌──────────────────────── subgame_search() 新模块 ────────────────────────┐
  │ ① should_search 触发  —— 首轮: 对手 off-tree raise > 1BB 且 ≤4 人在场     │
  │                          二/三/四轮: 永远搜（Pluribus 规则）              │
  │      false → 回 blueprint dist（兜底）                                    │
  │      true ▼                                                              │
  │ ② 估 range：从 blueprint 沿 public history 累乘 reach                     │
  │      per-player **marginal** range（preflop 169 / postflop 200 桶），     │
  │      card-removal 后归一。绝不存联合分布。  [SubgameRange]                │
  │      ▼                                                                   │
  │ ③ 建 subgame 树：从【当前下注轮起点】为根（不是最近决策点！）             │
  │      当前街 finer bet 菜单 + lossless 当前牌；未来街截到 depth-limit      │
  │      build_subtree(root_node, entrants, raises_on_street, finer_abs)      │
  │      守 num_nodes ∈ [100,2000]，超限 abort 兜底。  [SubgameTree]          │
  │      ▼                                                                   │
  │ ④ leaf 值：depth-limit 处叶子 = 对手"选续局"的选择节点                    │
  │      6a: 单值 unbiased ; 6b: N=4 {unbiased, fold/call/raise-biased}       │
  │      值来自 N-player blueprint self-play EV 表（aivat_value 推广）        │
  │      ▼                                                                   │
  │ ⑤ CFR 求解：SubgameNlheGame impl Game → EsMccfr/Vanilla recurse           │
  │      root 按 range 加权采样发牌；叶子对手对 N 续局 minimize（CFR 真 action）│
  │      出牌用最后一次迭代策略；σ（信念传播用）用加权平均策略更新           │
  │      ▼                                                                   │
  │ ⑥ 返回 root infoset 的 K 维分布（对齐 actor 的 legal_abs）               │
  └──────────────────────────────────────────────────────────────────────────┘
      ▼ dist
  sample_discrete → chosen → outgoing_action(L425 不变) → auth.apply → 推进所有影子
```

---

## 5. 关键构件逐个落地

### 5.a subgame 树构造（中途根 + 当前街 finer 菜单 + lossless 当前牌）

**复用**：`build_with_rules`（可配 bet 菜单 / `drop_small_reraise` / `width_redirect` / `no_open_limp`）；
`StreetActionAbstraction::per_street([pre,flop,turn,river])`；`walk`（上下文已是入参）。

**新增**：`pub fn build_subtree(root_node_id, entrants, raises_on_street, finer_abs, rules)`——从中途 public state DFS
建小树。当前 `walk` 只被 `build_with_rules` 从固定 `root_id=0` 调，无中途入口。

**关键正确性修复**：`TreeNode`（`nlhe_betting_tree.rs:308-315`）只存
`street/player_acting/parent/action_from_parent/legal_actions/children`，**不存 `entrants`/`raises_on_street`**。从中途
`NodeId` 重启子树时这两个 A3×A4 上下文灭失 → `filter_actions` 重算错动作集（典型：`raises_on_street==0` 误判 0.5pot
为开池档）。**解法（推荐）**：`build_subtree` 由调用方显式传入当前 `(entrants, raises_on_street)`——它持有权威
`GameState`，能现算（不改 `TreeNode` 布局、零存储成本）。

**中途 `GameState` 怎么来**：`GameState` 全字段私有（`state.rs:34-75`），唯一构造路径 `with_rng_opts` 只能
deal-from-scratch（Fisher-Yates + post blinds，固定 `Street::Preflop`）。**没有 mid-state 构造入口，也别新写
`from_raw_state`**（侵入 rules 层、易破发牌不变量）。→ 中途 root 直接 `auth.clone()`（决策循环里 `auth` 已是中途真实状态）。

**风险**：subgame 爆炸——当前街 finer 菜单快速放大 action-sequence，必须守 **100–2000**（建树后 `num_nodes()`
cross-check + 上限 abort 兜底纯 blueprint）。`StreetActionAbstraction` 现是 4 档、无"街内深度分割"，6a 不需要（YAGNI）。

### 5.b range 估计（从 blueprint 沿历史累乘；per-player marginal）

**复用**：`path_to_root(node_id)` 回溯抽象动作序列（每节点带 `player_acting`）+ `locate`（O(1)）+
`average_strategy_by_info` → 标准 CFR reach 乘积，**所有原语都在**。

**新增**：`fn estimate_range(history, player, blueprint) -> SubgameRange`——固定该玩家一手私牌，沿 public path 在其每个
decision node 查**当前街 bucket** 上的 σ，乘所走动作概率 → reach；对 ≤1326 私牌求和归一。preflop 走 169 lossless 精确，
postflop 落 200 桶。

**两个正确性陷阱（对抗验证标红）**：

1. **索引语义是"当前街 bucket"非"固定 bucket"**：同一手私牌的 bucket **逐街随 board 变**。**绝不能在一个固定桶上累乘**——
   必须以私牌为载体逐街 re-bucket（用 `info_set_for_cards` 注入真实牌算当前街桶）。
2. **联合多人 range 规模爆炸**（实测组合数）：

   | | N=2 | N=3 | N=4 | N=6 |
   |---|---|---|---|---|
   | preflop 精确组合 (1326ᴺ) | 1.76e6 | 2.33e9 | — | 5.4e18 |
   | postflop 桶空间 (200ᴺ) | 4e4 | 8e6 | 1.6e9 | 6.4e13 |

   **只有 ≤3-way 桶空间联合（≤8M）落在"可控"内。** → **决策：用 per-player 独立 marginal range（各家一份长度
   169/200 的 reach 向量），不存跨玩家联合分布**。玩家间负相关（同张牌不能在两人手里）不显式建模，而是让 subgame CFR
   在同一 board chance 下对每个 infoset 做 vector-based CFV 时隐式承载 + 在采样 board/对手手时做 **card-removal 去冲突
   再归一**（这是 Pluribus/DeepStack 标准做法，也是唯一可扩到 6 人的做法）。A3×A4 把 postflop 限 ≤3-way 正好兜住，
   最坏 3×200=600 量级 entry。postflop range 只到**桶粒度**（有损），preflop 精确——在 S6 抽象边界显式标注。

### 5.c leaf / continuation 值（blueprint EV 表 + biased multi-valued 叶子）

**核心澄清（对抗验证 V2）**：**"不需价值网络"成立，但"从 dense 表直接查 EV"不成立**。dense 表只存策略/regret，
average strategy 是**分布**，得值必须再对子树期望或 rollout。**别承诺直接查表得 EV。**

**复用**：`aivat_value.rs:139-242` `build_value_tables`——跑 blueprint self-play 缓存 `(node_id,bucket)→E[U]`，
**这正是 6a 的 unbiased 叶子值**（= 假设对手在叶子之后照 blueprint 打），零 NN。`row_for` 索引复用。

**新增**：
- **N-player 推广**：现 `AivatValueTables` `vf_mean:[Vec<f64>;2]` / `pos=button?0:1` / `seat 0..2` 硬编码 2 人，
  推广为 `[Vec<f64>; n_players]` 或按相对按钮位置 offset 拆。
- **EV 计算用精确期望，不用 16-rollout 采样**：6a 用 `recurse_vanilla` 式精确期望（Chance `Σprob×child` / Player
  `Σσ×cfv`）截断到值表查询，避开 `lbr.rs` 那种采样方差，同时拿下"多次 solve 噪声内可复现"门槛。
- **biased 续局派生（6b，零额外训练）**：对 `average_strategy` 闭包外套一层，按 `AbstractActionTag` 识别 fold/call/
  aggressive 槽，对应概率 **×bias 系数后重归一**（纯整数前缀逻辑 + f64 仅最后归一）。每个 biased meta-strategy 各跑一遍
  self-play 得一份叶子 EV。
- **multi-valued leaf 布局**：叶子值表 `vf[pos][row*N + n]`，原样复刻 `NlheDenseIndexer::from_tree` 的 prefix-sum。

**风险**：
- **bias ×5（fold/call/raise）是 HU/Pluribus 经验值**，6-max 多街多人需自己消融重标（pluribus_path.md 6b 已要求每条单独开关消融）。
- **前提坑**（= §2）：续局建在欠训练 blueprint 上 → 搜索可能放大偏差。**这是本方案最大的不确定性，靠 6a 不退化 smoke 先证伪。**

### 5.d subgame CFR solver（不改 trainer.rs / 不改 Game trait 签名）

**裁决（V1 = PARTLY）**：递归核可 direct 复用，但中途根构造全无支持 → 新增**薄包装**，不从零写 explicit-tree solver、
也不硬改 `Trainer::step`/`Game::root` 签名（侵入生产热路径）。

**新增 `struct SubgameNlheGame impl Game`**：`State/Action/InfoSet` delegate `SimplifiedNlheState/AbstractAction/InfoSetId`，
**只重写 `root()`** 返回 §5a 的中途根（`auth.clone()` 注入 + 按 §5b range 采样各家 hole，而非 `with_rng_no_history`
的 uniform 全局先验），其余方法转调 `SimplifiedNlheGame`。这样 `EsMccfrTrainer<SubgameNlheGame>` **原样**跑
`recurse_es`，**完全不碰 `trainer.rs`**。临时 `RegretTable`/`StrategyAccumulator` 小表快丢。

**叶子改造**：depth-limit 处叶子 utility 不走真实 showdown，而变成"对手选 n 的选择节点"——把 N 个 biased 续局当对手在该
叶子 infoset 的 N 个动作，值 = `LeafValueTable[f64;N]`，CFR 内对手对 N 续局 minimize。这是对 `NodeKind`/`payoff` 的一处
扩展（叶子插一层选择节点），**不动核心 recurse**。

> 注：finer 菜单导致 `InfoSetId` 的 `betting_state`（仅 3 bit，raise_count capped "3+"）不够细——但 subgame solver 用
> **新建子树自己的 local NodeId** 索引 regret/σ（同 `NlheDenseIndexer::from_tree` 模式），infoset 同一性由 local node +
> bucket 决定，**全局 `InfoSetId` 的 betting 位上限不在 subgame 内绑定**。叶子查 blueprint 值时才回落全局 InfoSetId。

### 5.e off-tree / nested 处理（map_off_tree 复用 + 6c 升级）

**复用**：`advance_shadow_by_applied`（incoming off-tree：`map_off_tree` 选最近 ratio → 投影合法集 → desync 检测）；
`outgoing_action`（solver 选出 abstract action → 真实 bet size，不动）。

**新增（6c）**：
- `map_off_tree` 从 nearest-ratio stub 升级为 **pseudo-harmonic randomized rounding**（Ganzfried-Sandholm；走 `RngSource`
  可复现）。确定性阈值 `x*=(A+B+2AB)/(A+B+2)`（**不是**算术中点、**不是**几何均值——后两者更可剥削）；A=0（check 与
  pot-bet 间）退化时 pseudo-harmonic median=1/3 仍良性。**这是 6c 验收"卡边界对手剥削"过关的关键**（DeepStack §7：
  translation 比 nested 差 ~12×，纯 translation 很可能过不了）。
- **nested 扩展（可选增量）**：对手打抽象外 size 时，把该真实 `to` 作为新 `AbstractAction::Raise` 注入影子当前节点合法集、
  从本街根重解（Pluribus Algorithm 2 AddAction），零翻译误差。先用 map_off_tree 翻译，nested 留后续。

**风险**：
- **结构 gap（limp vs no-limp）**：`advance_shadow_by_applied` 对 open-limp 进 no-limp 影子返回 `Err`，**搜索中途无法事后排除**
  （自对弈里能 desync 排除，live 对战不能）。**防御兜底**：solver 推进对手回合若 `advance_shadow_by_applied` 失败 →
  abort 该分支用 blueprint action。
- blueprint 是 ≤3-way（A3×A4）训的：**4-way+ subgame 没有忠实的 blueprint continuation/leaf 值**——S6 多人 subgame 要么限
  ≤3-way、要么先扩 blueprint 抽象，**别在 4-way+ 硬解**。

---

## 6. Pluribus 复现的 6 个易错点（landmines，正确性关键）

来自 Science Supp 逐条核对——这些写错**不崩溃、只让 bot 静默变弱/可剥削**，是 30–40% 工作量里"最容易低估"的部分：

1. **从【当前下注轮起点】重解，不是【最近决策点】**（Algorithm 2 CHECKNEWROUND：只在 BettingRound 推进才换 root，同轮内
   多次行动复用同一 root 反复 Search）。从"最近决策点"起是论文明说更可剥削的旧做法。
2. **冻结范围**：只冻【自己】【本轮已选动作】【且只对真实手牌】的动作概率；对手概率不冻、自己其余可能手牌不冻。
   （否则二次搜索给出 A vs B 两套自洽互斥策略，使已落子 A 之后的策略变 nonsensical。）
3. **续局的"选"是 subgame 里的真 action 由 CFR 优化，不是固定 min/max**；且是 **infoset-level**——同一 infoset 内所有不可区分
   leaf 必须选同一续局（写成 per-node 会静默重引入可剥削性）。
4. **出牌用最后一次迭代策略，但 σ（信念传播用）用加权平均策略更新**——两者分开，混用会让下一决策点 root 分布算错。
5. **subgame 深度限规则按街分支**：首轮搜索→叶子在第二轮起点 chance；二轮且开局 >2 人→叶子在三轮 chance 或第二个 raise 后
   （取早）；其余→直接解到终局。不是一刀切"解一条街"。
6. **bias = 在先验概率上 ×系数再归一**，不是设成固定概率；fold-bias 乘 fold、call-bias 乘 call、raise-bias 乘**所有 raise**。
   （另：Linear-CFR 折扣/pruning 在 Pluribus 只在训练特定时段开、末轮和直达终局动作**永不 prune**——照搬"全程 prune"会损正确性。）

---

## 7. 分阶段计划 + 量化验收

**总原则改写**：删 pluribus_path.md 6b 的"LBR exploitability 显著下降"闸门（6-max 失去理论意义）。改为：

> **怎么验"搜索不退化/更强"**：`evaluate_cross_abstraction_h2h` 做**受控 A/B**——同 seed 池、同对手座次，一边 search-on
> 一边 search-off（blueprint-only），对比 per-seat 净 PnL 的 mbb/g + 配对 CI95；再上 OpenPoker live。统计金标准 =
> mbb/g 配对差 + CI（与现有 h2h 一致）。

### 阶段 6a：单层 subgame 重解（unbiased leaf；打通正确性 + 性能）
- **代码**：① `build_subtree`（传 `entrants/raises` 上下文）；② `SubgameNlheGame impl Game`（delegate + 重写 root）；
  ③ `estimate_range`（marginal）；④ `aivat_value` N-player 推广（unbiased 单值叶子，精确期望）；⑤ `subgame_search` 接进
  `blueprint_advisor.rs:421`（带 `should_search` 触发 + 节点数上限 abort 兜底 + desync→blueprint 兜底）。
- **测试**：N=2 小树 byte-equal 验证不破默认路径；建树 `num_nodes()∈[100,2000]`；fixed-seed 同 public state 多次 solve
  策略差异在噪声内。
- **门槛**：flop/turn/river 都能从当前 public state 启动 subgame 返回策略；当前街 finer/近 lossless、未来街受控桶；
  单次决策 **P95 < 30s**；subgame action seq ∈ 100–2000；**1M 手 h2h：search 不显著差于 blueprint-only（不退化）**。

### 阶段 6b：continual re-solving + biased leaf strategies
- **代码**：① 4 biased 续局派生（fold/call/raise ×bias 重归一）；② `LeafValueTable[f64;N=4]`（每 biased 各跑一遍 self-play）；
  ③ 叶子改造为"对手选续局"选择节点（CFR 内 minimize，infoset-level）；④ 每决策点重 solve（不缓存上一决策 subgame）；
  ⑤ bias 系数消融开关。
- **门槛（删 LBR）**：≥4 leaf strategies；叶子对每个 biased 维护独立 EV、solve 中对手选最不利；**每条 biased 单独消融：
  去掉后 h2h 对"卡边界/激进剥削对手"的实测 mbb/g 显著变差**（替代原 LBR 上升门槛）；**1M 手 h2h + OpenPoker live：search
  显著优于 blueprint-only（CI 不跨 0）**。

### 阶段 6c：off-tree action handling 验证
- **代码**：`map_off_tree` D-201 stub → pseudo-harmonic（randomized，`RngSource`），写入策略版本元数据；nested AddAction（可选）。
- **门槛**：fuzz 1M off-tree 金额映射稳定可复现、无非法/越界；on-tree vs off-tree 同 public state 策略差异分布；**故意卡边界
  对手 1M 手实测是否拿显著正收益**（可剥削性证据）；off-tree vs on-tree 路径覆盖率分别报告。

---

## 8. 算力 / 内存预算

- **Pluribus 实测**：实时搜索 28-core/128GB（**无 GPU**），单 subgame 1–33s、平均 20s/手；blueprint 训练是分离资源
  （64-core/8 天/~$144 spot）。Modicum：4-core/16GB/20s 一手就赢 BT8。
- **本仓库**：dense 两表 4.62 GiB（目标 13.48 GiB）≪ 128GB，**内存不是瓶颈**；blueprint 已有，省掉训练成本。
- **vultr（4-core/7.7GiB）**：跑得动**单层 flop subgame 正确性测试**（≤3-way、≤5 raise 档、几千迭代）；但稳定 P95<30s
  + 多人多 board 采样 + 1M 手评测 → 按 `feedback_high_perf_host_on_demand` 向用户申请 **≥8-core**（AWS c6a.8xlarge 量级）。
- **注意**：Pluribus 的低延迟是【多核 + 向量化 CFR + 续局压缩（每 abstract infoset 只存 1 个采样动作）】共同压出来的；
  **朴素 Rust HashMap 重解会慢一个量级**——subgame 用 arena/向量化 + 临时容器快丢，借鉴 `nlhe_dense.rs` 的连续 arena 布局
  （**用安全 index slice，不学 postflop-solver 的 raw `*mut`**，invariants 禁 unsafe）。

---

## 9. 与现有 invariants 的守门

- ✅ 无结构冲突：tabular（无 NN）、无 `unsafe`、无全局 RNG（pseudo-harmonic randomized rounding + 续局采样 + range 采样
  全走 `RngSource`，byte-equal 可复现）、规则/抽象/off-tree 层禁浮点（CFR regret/σ + leaf EV 的 `f64` 是既有豁免）。
- ⚠ `map/` 子模块 D-252 clippy deny float——PHM 升级用整数 milli ratio + `target_to` 比较（沿用 `map_off_tree:445-446`
  的整数算法骨架），不在 `map/` 引浮点。
- ⚠ `feedback_correctness_no_carveout`：搜索若放大 blueprint 偏差导致退化 ≥10×，**立即停下追根因，不 carve-out**。
- ⚠ S1 PokerKit 跨验证不受影响：搜索全在抽象层 + `auth.clone()`，**不碰** `state.rs` 的 `legal_actions`/side pot/showdown/payouts。

---

## 10. 最小可行第一步（MVP）

**第一刀切在 `blueprint_advisor.rs:421` + 最薄 subgame solver，目标 = 跑出"单层 subgame 解 vs blueprint-only 不退化"的 smoke
（= §2 的 blueprint 质量探针）。**

1. **`build_subtree`**（`nlhe_betting_tree.rs`）：从中途 `NodeId` + 调用方传入 `(entrants, raises_on_street)` 建小树；
   node 计数 cross-check 守 100–2000。
2. **`SubgameNlheGame impl Game`**：delegate `SimplifiedNlheGame`，`root()` 返回 `auth.clone()` 注入的中途状态
   （**MVP 先用 uniform 先验，range 估计留下一刀**——先证 plumbing 通）。
3. **unbiased 叶子值最简版**：subgame `recurse_vanilla` 解到 terminal（小残余树不截断），叶子走真实 showdown payoff
   （**MVP 先不接 N-player 值表**，避免一次改太多）。
4. **接插桩点**：`subgame_search` 替换 L421，**仅在 flop 第一个决策点触发**（缩小验证面），其余兜底纯 blueprint。
5. **smoke（vultr）**：N=2 小树 byte-equal 不破默认路径；`EsMccfrTrainer<SubgameNlheGame>` 从中途根跑通；fixed-seed 同
   public state 多次 solve 差异在噪声内。
6. **不退化 smoke**：先 vultr 小样本（1k–10k 手 h2h），再 AWS 1M——search-on（仅 flop 首决策）vs search-off 的 mbb/g 配对差
   CI 跨 0 = 不退化通过。

**MVP 不做**：biased leaf（6b）、N-player 值表、PHM 升级（6c）、continual re-solving、全街触发、跨玩家联合 range——
都是后续增量，与 CFR 核解耦。

**MVP 能证伪的最大风险**：blueprint 欠训练是否导致搜索放大偏差（§2/§5c）——**若 MVP 单层 search 就显著退化，先停下查
blueprint 质量（回路线甲），不堆 biased leaf。**

### 10.1 进度（2026-06-03，分支 `6max-rts-mvp` commit `cf9efdb` + 审核收尾 `ceae72a`，vultr 验证；**未并入 6max**）

**实现暴露的关键偏差（已据此修正方案）**：§10 step 2「`root()=auth.clone()` + uniform 先验」**行不通**——`GameState`
全字段私有、唯一构造路径 deal-from-scratch（Fisher-Yates 随机全板）、无 in-game chance（D-308），`auth.clone()` 是
**单一已知发牌**（完美信息 → CFR 退化）。**用户拍板的修法 = 加性 `GameState::resample_hidden(rng)`**（`rules/state.rs`，
pub(crate)）：克隆中途态、保留公共牌前缀 + 全部下注/筹码/街/行动权、**重发所有未弃牌座位底牌 + 未见 runout 后缀**、
重算 `showdown_ranks`；终局收益仍走**权威** `payouts()`（side pot/showdown 一行不改 → **S1 PokerKit 跨验证不受影响**）。
`EsMccfrTrainer::step` 每 step 调 `game.root`（`trainer.rs:534`）→ resample 即 **per-step external chance**，整套
MCCFR 不改即用（`trainer.rs` 零改）。root 重发**所有**人（含 hero）= 对全 range 求解（balanced）；hero 真实手只在
事后由 advisor 索引结果桶，非对单一 hero 手 best-response。

**已落地 + vultr 验证**：

| step | 内容 | 状态 |
|---|---|---|
| 1 | `PublicBettingTree::build_subtree(state, abs, rules, entrants, raises)`（`nlhe_betting_tree.rs`） | ✅ + 2 测试（from-root ≡ 全树 / from-interior ≡ 后代块）|
| 2′ | `GameState::resample_hidden`（`rules/state.rs`，替代原 step 2 的 auth.clone uniform） | ✅ + 3 测试（保留态/无重复牌/showdown 自洽、推进终局守恒、byte-equal 可复现）|
| 3′ | `SubgameNlheGame impl Game`：delegate + 重写 root；叶子 = 解到 subgame 终局走**真实 showdown payoff**（与 step 3 同效；求解器用 `EsMccfrTrainer` external-sampling 而非 `recurse_vanilla`——配 resample chance 更自然） | ✅ + 1 端到端 smoke（CFR 跑通 + 可复现）|
| 5 | smoke（vultr）：6 新测试全过 + 全 lib 68/68 无回归（cf9efdb 时 67 + 审核收尾 `ceae72a` 新增 A 测试 1，byte-equal 守门未破）；本地 build/fmt/clippy `--all-targets -D warnings` 绿 | ✅ |
| 4 | `subgame_search` 接 `blueprint_advisor.rs:421` + `should_search`（flop-only 触发）+ desync→blueprint 兜底 | ✅ 落地（commit `a8f1b96`，§10.2）|
| 6 | 不退化 h2h 探针（search-on vs blueprint-only，需真 ckpt 如 `run_6max_s4_nolimp`）| ◑ 工具 `tools/six_max_search_probe` + vultr smoke 已过；**大样本判决待定**（confound + AWS 预算，§10.2）|

→ **MVP subgame-solver 核心闭环已证**（construct → resample → CFR → 可复现），架构 blocker 解除。剩 step 4+6 =
把它接进 live 决策点 + 真正跑「不退化探针」（§2 的 blueprint 质量判别器）。注：MVP 仍 uniform range、同抽象、解到终局
（无 finer 菜单/depth-limit/biased leaf——那些是 §3 + 6b）。

**审核收尾（commit `ceae72a`，vultr 68/68 验证）**——对 cf9efdb 独立审核发现的两个「接 step 4 前隐患」已落地：

- **A（rules-on `build_subtree` cross-check，已落地）**：cf9efdb 的两个 cross-check 用 default rules（不读
  `entrants`/`raises_on_street`，传 0/0 恒安全），**测不到** rules-on 透传——而 step 4 接的就是 rules-on profile
  （`first_small_6max` 等），传错 `raises_on_street` 会让 `drop_small_reraise` 把 re-raise 的 0.5pot 误当开池档保留
  （§5a / `build_subtree` 文档标红的坑）。新增 `build_subtree_from_interior_rules_on_threads_raises_context`：仅开
  `drop_small_reraise`，路径 SB Call→BB Check→flop `Bet{0.5}`→K(`raises_on_street==1`)，**双向钉死**——正确 seed
  `raises=1` 子树逐节点 ≡ 全树 K 后代块；错误 seed `raises=0` root 漏删 `Raise{0.5}`、与 K 不同构（证 `raises_on_street`
  真被读取，不是被静默吞掉）。**遗留**：step 4 调用方仍需从权威局现算正确的 `(entrants, raises_on_street)`——`GameState`
  无现成 getter（只有 `street()`），需自数本街进攻数。
- **B（resample 强制 no-history fast path，已落地）**：`resample_hidden` 返回状态强制 `track_history = false`，对齐 base
  `SimplifiedNlheGame::root` 的 `with_rng_no_history`。否则从 `track_history=true` 的权威局 `auth.clone()` 来时，每步
  re-solve 都会在 `apply` 的 action 记录 / `finalize_terminal` 历史写入 / **逐 apply 增长的 `history.actions`（使
  per-node clone 退化成 O(depth²)）** 上白付开销（CFR / `payouts` 都不读 history；`hand_history()` 在 subgame 状态上
  无意义）。3 个 `resample` 测试在新路径下复跑全过（规则层不变量 + 终局守恒未破）。

### 10.2 进度（2026-06-03，分支 `6max-rts-mvp` commit `a8f1b96`，vultr 验证；**未并入 6max**）

**step 4 + step 6 已落地**——MVP 实时搜索接进 live 决策环 + 不退化探针工具。具体：

| 件 | 内容 | 状态 |
|---|---|---|
| step 4 搜索驱动 | `subgame.rs`：`SubgameSearchConfig` / `should_search`（flop **未起注**首决策点触发）/ `subtree_context`（postflop `entrants`=live bitmask、`raises_on_street`=0 现算，补 §10.1 A「遗留」的 getter 缺口）/ `subgame_search`（建 subgame→CFR→取 actor 真实手 root 策略，按 tag 对齐 `legal_abs`，任一失败回落 blueprint）/ `SubgameNlheGame::root_query` | ✅ |
| step 4 插桩 | `blueprint_advisor.rs:421` 改 search-or-blueprint 分支（`outgoing_action`/影子推进**不变**）；`Contestant.search: Option<SubgameSearchConfig>`（默认 `None` = byte-equal 旧行为）；`decision_ordinal` 透传搜索 RNG（可复现 + 跨手独立）；`SearchObserver`（attempts/successes 原子计数，report 暴露 fallback 率）| ✅ |
| step 4 支撑 | `SimplifiedNlheGame` 存 `rules` + `rules()`（subgame 重建子树须同规则透传）；`EsMccfrTrainer::game()` accessor | ✅ |
| step 6 探针 | `tools/six_max_search_probe`：同一 blueprint 拆 hero(search-on) vs field(search-off)，`evaluate_cross_abstraction_h2h` 出 mbb/g + CI95 + search 触发/fallback 计数（取代 6-max 失效的 LBR 闸门）| ✅ 工具 |
| 测试 | vultr lib **71/0/8**（+3：`should_search`/`subtree_context`、`subgame_search` 契约+可复现+tiny cap 回落、search-on 守恒+可复现；+1 ignored 诊断）；本地 build/fmt/clippy `--all-targets -D warnings` 绿 | ✅ |

**实测子树尺寸**（`_measure_flop_subtree_sizes` 诊断，机器无关）：6-max `first_small(3)` flop 子树（3-way，limped/深码 = 最大情形）= **4434 节点** < 默认 `max_subtree_nodes` 8000 → 探针**不会被 cap 误拒**（修正 §5a「100–2000」低估，但默认 8000 仍有余量）。HU 默认 `{0.5,1,2}` flop 子树 = 58160（仅诊断/契约测试用大 cap；HU 非生产目标）。

**smoke（vultr，真 1B nolimp ckpt `run_6max_s4_nolimp/...final_001000000000`，bucket 200/200/200，100 手/座 = 600 手）**：plumbing 健康——加载 1B ckpt 无 OOM（11Gi 机，峰值 cache ~3.5Gi）；**desync=0 / illegal=0**（同抽象自对弈，搜索路径不破 lockstep/记账）；**search 触发 66 决策点、真搜索 65（fallback 仅 1.5%）**——1500 迭代下 root 桶可靠命中，搜索确在跑（confound 是**欠迭代解的噪声**，非 fallback）；mbb/g = −667，CI95 = [−1857, +524]（600 手样本太小 → CI 巨宽、跨 0，点估为负但纯噪声，**不能据此判强弱**）。

→ **step 4 plumbing 端到端证实**（construct→resample→CFR→取分布→outgoing→真实对局，desync=0、search 真触发、可复现）。**未闭 = step 6 的「大样本判决」**：需 ~100k–1M 手才有有效 CI（按 `feedback_high_perf_host_on_demand` 上 AWS），且**信号被 §2 三 confound 削弱**（uniform range / 解到真实终局无 blueprint 叶子 → 测不到「搜索放大 blueprint 偏差」、只测「均匀-range 全解 vs blueprint」/ 欠迭代噪声）。**战略岔路（待用户拍板）**：(甲) 直接上 AWS 跑 confounded 大样本探针（验「均匀-range 全解 vs blueprint」+ plumbing 规模化）；(乙) 先做 §5b range 估计（blueprint 沿历史累乘 reach 加权 resample）把探针**去 confound**、使其真能答 §2，再上大样本。MVP plumbing 是两条路的共同前置，已不浪费。

### 10.3 §5b range 去 confound 已落地（2026-06-03，commit `ac3968b`，vultr 验证；用户选路线乙）

**做了什么**：subgame root 不再 uniform 重发底牌，而是按 blueprint **沿历史累乘 reach** 的
per-seat marginal range 加权采样（顺序 card-removal）。这把探针从「均匀-range 全解 vs blueprint」
升级成**真正的 §2 判别器**——subgame 现在解的是 blueprint 真 range 下的子博弈：blueprint range
若偏（欠训练），全解建在偏 range 上 → 可能更差（§2 实锤）；range 好 → 全解改进策略。

| 件 | 内容 | 状态 |
|---|---|---|
| 规则层装牌 | `GameState::resample_hidden_with_holes(holes, rng)`：装入给定底牌 + runout 从「52−board−holes」补 + 重算 showdown；终局仍走权威 `payouts()` | ✅ + 测试 |
| range 估计 | `estimate_range`：对每候选 hole 沿该 seat 决策累乘 blueprint σ 走该动作的概率，**逐街 re-bucket**（陷阱①）；空 σ 退均匀、撞 board 置 0、归一。+ `all_hole_combos`(1326) / `decisions_on_path` / `board_prefix_for_street` | ✅ + 测试 |
| range-weighted root | `SubgameNlheGame::new_with_ranges`；`root()` 顺序 card-removal 加权采样（`sample_discrete`，受限全零退均匀）→ `resample_hidden_with_holes` | ✅ + 测试 |
| 接线 | `SubgameSearchConfig.use_blueprint_range`（默认 true）；`subgame_search` 加 `node_id`+`strategy`；advisor 透传；探针 `--uniform-range` 作 A/B | ✅ |

**仍在的近似**（陷阱②的工程折中）：range = per-seat **marginal**（玩家间负相关只靠采样期
card-removal 近似，不建联合分布）+ postflop **桶粒度**（有损）、preflop 精确。MVP 解到**真实
showdown 终局**（6-max first_small flop 子树 ≈ 4434 节点、小）→ **无叶子近似**，故「无 blueprint
续局值」在这里**不是 confound**（反而比 Pluribus 截 depth-limit 查值更精确）。

**vultr 验证**：lib **74/0/8**；真 1B nolimp ckpt smoke（600 手）range-on vs `--uniform-range`
A/B 均 plumbing 健康——desync=0/illegal=0、search 触发 66/真搜索 65（fallback 1.5%）、无 OOM；
mbb/g range-on −520 [CI −1680,+639] vs uniform −667 [−1857,+524]，600 手 CI 巨宽两者均跨 0、
不可分辨（去 confound 改变的是**解读**，大样本才出有效信号）。

→ **探针已去 confound、可答 §2**。剩 = **大样本判决**（CI 在 600 手无意义）：vultr 中样本
（free，~10⁴ 手）出首个真信号，AWS 大样本（~10⁵–10⁶ 手）收紧 CI。提迭代数（`--search-iterations`）
压 per-bucket 噪声。

---

## 附：引用 + 关键 `file:line` 索引

**文献**
- Pluribus：Brown & Sandholm, Science 365(6456):885-890, 2019（主论文 Fig 3/4 + p.3-4 search 节；Supp "real-time search
  algorithm" / "Leaf node values" / Algorithm 2 Nested search）。
- Modicum（depth-limited / continuation strategies）：Brown, Sandholm, Amos, NeurIPS 2018, arXiv:1805.08195。
- Safe & Nested Subgame Solving：Brown & Sandholm, NeurIPS 2017 best paper, arXiv:1705.02955；DeepStack: Moravčík et al.,
  Science 2017。Action translation：Ganzfried & Sandholm（pseudo-harmonic）。ReBeL：Brown et al., NeurIPS 2020（NN 路线，
  仅作 trade-off 对照）。

**代码（绝对路径根 `/home/shaopeng/dezhou_20260508`）**
- 插桩点：`src/training/blueprint_advisor.rs:421`（决策循环 354-456；outgoing 193-246；incoming 260-316；h2h 556-661）
- 树构造：`src/training/nlhe_betting_tree.rs:219-358`（walk 上下文 entrants/raises 248-249；path_to_root 373-385；
  TreeNode 布局 308-315）
- CFR 核：`src/training/trainer.rs:187-271`（recurse_vanilla 精确期望）/`644-726,746-848`（recurse_es 多人 generic）/
  `352-367`（LCFR rescale）；`game.rs:42-122`（Game trait）；`regret.rs:40-197`（RegretTable/StrategyAccumulator）
- Game 包装：`src/training/nlhe.rs:405-425`（root 硬编码 deal-from-scratch）/`624-635`（chance_distribution panic）
- 值表：`src/training/aivat_value.rs:38-67,139-242`（2 人硬编码，待 N-player 推广）
- 索引/查询：`src/training/nlhe_dense.rs:155-176`（locate）/`181`（row_for）/`374`（average_strategy_by_info，无 value 查询）
- off-tree：`src/abstraction/action.rs:387-493`（map_off_tree D-201 stub，待 6c PHM）
- GameState 构造约束：`src/rules/state.rs:34-75`（全字段私有）/`83-244`（仅 deal-from-scratch，无中途构造入口）
- 抽象/桶：`src/abstraction/info.rs`（InfoSetId：bucket 24b/pos 4b/stack 4b/betting_state 3b/street 3b）/`map/mod.rs:22-102`
- 门槛文档：`docs/temp/pluribus_path.md:129-170`（6a/6b/6c）

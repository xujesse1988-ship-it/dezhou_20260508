# 场景化启发式策略设计（scenario forest）

> 一句话：把策略层从「隐式 if 分支」重组为**显式场景森林**——约 100–300 个有名字的上下文节点；
> 节点内用连续牌局信息（equity-vs-range / made / draws / 画像）+ 节点参数出响应；每个决策落
> **节点遥测**，按节点归因 EV，带样本纪律地跑「打 → 按节点分析 → 新增/拆分节点 → 回归门 → 再打」。
>
> 定位：前瞻设计草案，**代替 `docs/multi_field_strategy_design.md`（2026-07-02 废止，原文见
> git 历史）**。旧文档三大块的去向：
> - **§2 加权 range**：已落地（`src/heuristic/range.rs` / `equity.rs` / `range_online.rs`，
>   代码级走读见 `docs/heuristic9_strategy.md`「对手 range 建模详解」，ML 侧见
>   `docs/opponent_range_ml_design.md`）。
> - **§3 场型选路**（`Strategy` trait / `classify_field` / Balanced 策略族）：**废弃**——
>   降级为场景节点的画像门控参数，理由见本文 §5.3。
> - **§4 优化 backlog**：择要并入本文 §7，**保留原编号**（P0-1…P2-3，代码/测试注释仍在引用）。
>
> 配套必读：`docs/heuristic9_strategy.md`（现策略代码级走读——本文默认读者已读它）、
> `docs/heuristic9_design.md`（架构 + 迭代闭环纪律）、`docs/opponent_features_inventory.md`
> （对手特征采集清单——与本文 §5.2 共享节点坐标系）、`docs/invariants.md`（硬约束）。
>
> 工作语言：中文散文 + 英文 code identifier；结论尽量带 `file:line`。

---

## 0. 缘起：回归启发式的本义

启发式打法的本义是：**在打牌中不断归纳场景；轮到 hero 行动时，判断处在什么场景，用「场景 +
牌局信息」推出响应；对战后分析哪些场景没覆盖、哪些场景亏钱，新增或拆分场景，再对战。**
知识的组织单位是「场景」（人类职业玩家口中的 spot），升级的方式是场景库的生长与细化。

现状诊断——引擎其实已经是启发式（`docs/heuristic9_strategy.md`），但**场景是隐式的**：

- `preflop_decide`（`src/heuristic/policy.rs:260`）的「短栈 / 无人加注 / 面对加注」三分支，
  `postflop_decide`（`policy.rs:424`）的「无人下注 / 面对下注」两分支 × 优先级规则，
  **就是一棵浅场景树**——只是节点没有名字。
- 没有名字就没有遥测：`heuristic9_analyze.py` 只能按粗 spot（街×位置×动作面）聚合，
  回答不了「哪个场景在亏钱」「哪些决策走了哪条分支」「哪些局面落进了兜底逻辑」。
- 所以「打 → 分析 → 加场景」这条闭环**转不起来**——不是缺场景思想，是缺场景的
  显式化、遥测与归因。

本设计做三件事：**① 场景显式化**（有名字、有 id 的节点）；**② 节点级遥测与 EV 归因**；
**③ 带样本纪律的节点拆分迭代**。它不推倒重来：信息层（GameView / equity / ML range / 画像）
原样保留（§2），只重组 policy 层（决策流水线的第 [4] 步，`heuristic9_design.md` §2）。

---

## 1. 核心因式分解：场景 = 上下文；手牌不进场景 key

### 1.1 两层分工

```
决策 = 场景节点（「我在哪」，离散、可枚举、有名字）
     × 节点内响应（「我拿什么」，连续信号 + 节点参数）
```

**场景 key 的维度**（全部离散或粗桶，都能从现有 `GameView` 读出）：

| 维度 | 取值 | 现有来源 |
|---|---|---|
| 街 | preflop / flop / turn / river | `view.street` |
| 池型 | limped / 单加注（SRP）/ 3bet / 4bet+ | `preflop_num_raises`（已保留到翻后，`heuristic9_strategy.md`「角色判据」）|
| hero 角色 | 进攻方 / 跟注方 / 盲注防守 / 无主动权 | `preflop_aggressor` + `street_aggressors`（`view.rs:231`）|
| 面对动作 | 无人下注 / 面对 bet / 面对 raise / 面对 donk / 面对 barrel-N | `to_call`、`high_bet`；barrel-N 需 P0-1 派生量（§7）|
| 人数 | HU / 3 人 / 4+ | `num_active` |
| SPR 桶 | 低（<3）/ 中 / 深（>8） | `commit_spr` |
| 位置桶 | Early / Mid / Late / SB / BB；翻后 IP/OOP | `my_pos_bucket`（`view.rs:727`）|

**节点内响应的输入**（连续，全部已有）：equity-vs-range（ML 加权，`equity_dispatch`，
`policy.rs:134`）、made 档位（`features.rs:98`）、听牌（`features.rs:193`）、质地
（`features.rs:239`）、底池赔率、对手画像（`opponent.rs`）。节点参数 = 该节点的
阈值 / 频率 / 尺寸，进 config（沿用 partial-JSON 覆盖机制）。

### 1.2 为什么手牌身份不进场景 key

反例剖析——「翻前拿 AA、KK、QQ，对方 3bet」不是一个好场景：

- AA 面对 3bet 是**平凡问题**（怎么打都对，差别只在榨取效率）；真正难的是 AJs、KQs、99
  这些**边缘牌**——而边缘牌恰恰已经被连续信号刻画（`hand_strength`、equity vs villain
  的 3bet range）。
- 手牌身份一旦进 key，就是 **169 类 × 所有上下文的笛卡尔积**——这正是「预计要维护几千个
  场景」的来源，也是最难维护的形态：绝大多数格子永远没有样本，规则之间必然打架。

正确表达：场景节点 = `pre.3bp.facing_3bet.*`（按 IP/OOP、深度、是否 squeeze 分几个子节点），
节点内响应 = 4bet 价值阈值 / 平跟赔率线 / 弃牌，其中 villain 的 3bet range 宽窄由画像
（3bet%，`opponent_features_inventory.md` §A）驱动——AA 和 AJs 走**同一个节点**，
被 equity 自然分开。

**手牌身份的唯一例外：显式例外表**。少数 equity 会误导的地方——blocker 选诈唬牌
（持 A 阻断对手 AA/AK 时的 4bet-bluff）、低 SPR 小对 set-mine、Axs 的可玩性溢价——
做成**有名字的、挂在节点上的例外条目**，数量控制在十位数，逐条可追溯。

### 1.3 边界连续性与规模纪律

- **硬边界只放在天然离散的维度上**（池型 / 角色 / 面对动作本来就是离散事件）。连续量
  （SPR / 人数 / 强度）只做**粗桶**，桶内用连续参数插值——避免「SPR 3.99 与 4.01 打法
  突变」这种可被针对、也难调参的断崖。
- **显式节点数目标 100–300**，不是几千。「几千」会以**参数格子**的形式自然存在
  （节点 × 各自的参数向量 × 画像修正），但**规则按节点族写**，人（和 AI）维护的是几百个
  有名字的节点。
- 每轮迭代能**可靠评估**的节点只有几十个（样本决定，§4）——这是规模的真实上限，
  与愿望无关。

---

## 2. 牌局信息层：输入已基本齐备

场景化需要的牌局信息，对照现状（这一层**不属于本设计的改动范围**，列出是为了标注缺口）：

| 所需信息 | 现有实现 | 状态 |
|---|---|---|
| 公共牌 / 底池 / 自己手牌 / to_call | `GameView` 重放（v0.4 起 driver 发权威街标/盲座/栈） | ✅ |
| 自己筹码 / 位置 / SPR | `effective_stack_chips` / `my_pos_bucket`（`view.rs:727`）/ `commit_spr` | ✅ |
| 每个对手的位置 / 筹码 | `GameView` 逐座状态 | ✅ |
| 每个对手的预估 range | **ML 默认**：`build_opp_ranges_ml`（`policy.rs:119`）→ 169 类 `RangeVec`；兜底标量 `build_opp_thresholds`（`policy.rs:55`） | ✅ |
| 按 range 计算的 equity | `equity_dispatch`（`policy.rs:134`）→ `equity_weighted`（`equity.rs:284`） | ✅ |
| 对手画像 | `OpponentTable`（VPIP/PFR/AF，`opponent.rs`） | ⚠ 仅 3 个标量，扩充方向 = `opponent_features_inventory.md` |
| 行动线派生量（capped / delayed-cbet / facing-barrel-N） | 数据基座已落：`seat_actions`（`view.rs:228`）+ `street_aggressors`（`view.rs:231`） | ⚠ 派生量未做（P0-1 剩余，§7） |
| 主动权 | `i_have_initiative()` | ⚠ 过期主动权 bug 未修（P0-1 剩余） |

结论：**信息层已经够 Phase A/B 起步**——场景 key 的每个维度今天就能从 `GameView` 读出；
只有「面对 double barrel」这类线敏感场景要等 P0-1 派生量。这也是本设计与 range 优化的关系：
**优化 range 是为了给每个节点更准的牌局信息，两条线正交且互相成就。**

---

## 3. 场景森林

### 3.1 森林，不是单树

场景不必强求一棵树。按第一层维度（街 × 池型）天然切成**多棵并列的树**：翻前一棵
（按「面对什么」分层），翻后按池型 × 角色若干棵。树与树之间无共享状态，遍历顺序无关——
每个决策点**恰好命中一个叶节点**（key 的维度互斥且完备，兜底节点接住一切没匹配的），
这是遥测归因能成立的前提。

### 3.2 节点 key 与 id 规范

```
<street>.<pot_type>.<role>.<facing>[.<qualifier>]
```

例：`pre.srp.btn.unopened`、`pre.3bp.oop.facing_3bet`、`flop.srp.agg.unopened.multiway`、
`turn.3bp.caller.facing_barrel2`、`river.srp.oop.facing_overbet`。

**id 稳定性规则**（遥测跨版本可比的命根）：

1. 节点 id 一旦发布**永不改义、永不复用**；
2. 拆分产生子节点 = 父 id + 新 qualifier（如 `flop.srp.agg.unopened` 拆出
   `….unopened.paired`），父节点退化成「其余情况」兜底但 id 不变；
3. 每个 Decision 记录**完整节点路径**，不只叶 id——路径前缀聚合让「拆分前后」的数据可对齐。

### 3.3 三棵示例树（从直觉例子长出的正确形态）

**树 A：翻前面对 3bet（`pre.3bp.facing_3bet.*`）。** 一层拆 IP/OOP，一层拆 HU/被 squeeze，
低 SPR（<25bb 已有短栈分支，`policy.rs:286`）单列。节点内响应：4bet 价值阈值（现
`policy.rs:355-370` 的行为参数化）、平跟赔率线（现 `policy.rs:374-399`，含深栈投机牌
隐含赔率折让）、弃牌；villain 3bet range 由画像 3bet% 收放 → equity 自动变化；
fold-to-4bet 采到样本后（§5.2）解锁 4bet-bluff 开关（现引擎完全没有的动作，P1-4）。
「拿 AA」不出现在任何分支上——它在这个节点里 equity 爆表，自动落进 4bet 价值。

**树 B：翻后进攻方、无人下注（`flop|turn|river.<pot>.agg.unopened.*`）。** 这是现
`postflop_decide` 无人下注侧（`policy.rs:464-499`：价值 / 承诺 / 半诈 / c-bet / 放弃）的
显式化。拆分候选（按遥测热度决定做不做）：paired board、multiway、3bet 池（现引擎对
SRP 和 3bet 池的 c-bet 用同一套参数——几乎肯定值得拆）、delayed-cbet 线（需 P0-1）。

**树 C：翻后持听牌面对加注（`flop|turn.<pot>.*.facing_raise` 下的听牌响应）。** 这正是
直觉上「需要考虑更多因素」的场景——位置、人数、SPR 全是它的 key 维度。现引擎只有两行
规则（combo 听牌低频半诈加注 `policy.rs:535-541`；赔率+折让跟注 `policy.rs:543`）。
显式化后节点内响应 = 听牌类型（combo/FD/OESD/gutshot）× 直接赔率 × 隐含赔率（SPR、
人数修正）× fold-equity（对手画像的弃牌率）→ fold / call / semibluff-raise 三选一 +
尺寸。**注意**：它是树 B 同一森林里的另一批叶节点，不是独立体系——「几棵树」只是
key 第一层维度的投影。

### 3.4 节点响应的统一形态；共享地板不变

每个节点输出统一为「候选动作打分 → 动作 + 尺寸」，与现有代码同构。以下**全局地板对
所有节点保持不变**，不进任何节点逻辑：

- `legalize`（`policy.rs:198`）：输出严格落在 `valid` 集合内；
- `safe_fallback`（`policy.rs:241`）：异常兜底，能 check 则 check；
- 受控随机：频率门用手牌状态种子的确定性 RNG（`situation_seed`，`policy.rs:33`），
  守 `invariants.md` §2（无全局 RNG）；
- 节点参数进 `HeuristicConfig` 新子树（`#[serde(default)]`，旧 JSON 照常解析）。

---

## 4. 迭代闭环与样本纪律（本设计的命门）

### 4.1 闭环

```
打（live / sim）→ 每决策落节点 trace → analyze 按节点聚合 (n, EV, CI, 动作分布)
   → 产出两类候选：① 高频落兜底节点的局面（新增节点候选）
                  ② 高热度 × EV 缺口节点（拆分/调参候选）
   → 逐节点改动 → sim 回归门（配靶 baseline）→ 不退化才上线 → 再打
```

这就是「对战 → 找没考虑到的场景 → 找不合理的节点 → 优化 → 再对战」的工程化版本，
沿用 `heuristic9_design.md` §4 的既有纪律，只是归因单位从「整个引擎」细化到「节点」。

### 4.2 为什么「看到节点亏钱就调」会死：两个已发生的反例

本仓库已经用真金白银验证过两次「直觉漏洞 ≠ 真漏洞」（`heuristic9_design.md` §6）：

- **limp K-rag「漏洞」（v0.2）**：live 数据看是翻前被动漏洞（76 手被支配 offsuit 跛入），
  EV 归因后竟是**强 +EV**（vs station +7.35bb/手）；收紧后回归门实测**真退化 2–4σ**。
  教科书正确 ≠ 本场 +EV。
- **PairedTrap（v0.3）**：从最大输池（−7031）复盘出的「成对面深加注战」场景，8000 手 sim
  里**只出现 ~8 次**——单节点样本个位数；限深改动对真实激进场一致小亏 → 驳回。

推论：**per-node EV 是强噪声量**。1000 手 live 摊到几百个节点上，大多数节点单轮只有
个位数样本；扑克单手 EV 方差又极大。按「这个节点亏得多」直接调参 = **在噪声上过拟合**，
几千个节点各自往噪声方向拧一格 = 系统性灾难。

### 4.3 三条纪律

1. **节点永远带分母**：遥测按节点存 `(机会数 n, EV 均值, CI95, 动作分布)`；`n` 低于门槛的
   节点只标「待观察」，**禁止下结论**；小样本节点的读数用群体先验收缩——与
   `opponent_features_inventory.md` §0.5/§H「采原始计数 + 分母 + 收缩」完全同构。
2. **优化前先造靶**：要动某节点，先造一个能让该场景**高频出现**的 sim baseline
   （`Limper` / `PairedTrap` 先例——为让漏洞可门控测量而专门加对手模型），让改动
   在回归门里可测；live 攒样本太慢，sim 靶是唯一现实的显著性来源。
3. **拆分要显著性**：单次大输池复盘只能**立假设**；拆分/调参的收益必须多 seed 过 CI
   才 ship。回归门驳回的改动记进设计文档（如 v0.2/v0.3 先例），防止反复重提。

### 4.4 版本节奏

每轮迭代：按 `热度 × EV 缺口 × 可造靶性` 排序，取 **top-K（K ≈ 单位数到十位数）** 节点
处理。场景库的生长是**多轮累积**的结果，不是一轮铺几千个——初版只需把现有隐式树 1:1
显式化（Phase B），之后每轮长几个节点。

---

## 5. 与既有资产的关系

### 5.1 ML range / equity：主货币，不动

场景化只重组决策流水线第 [4] 步（Policy）。[1] GameView、[2] Features、[3] ML range
（`heuristic9_strategy.md`「决策流水线」）原样为节点供数。range 预测得越准，每个节点的
equity 信号越干净，节点参数才调得动——**先有好输入，再谈好场景**，两条线已在正确的
依赖顺序上。

### 5.2 对手画像：与场景 key 共享坐标系（最大协同）

`opponent_features_inventory.md` §0.5 的 per-opponent 节点计数表
`key = (街, 节点类型, 面对动作, 位置, 底池类型, …)` 与本文的场景 key **就是同一套坐标**。
统一后：

- **剥削 = 节点级查询**：在节点 `river.srp.agg.unopened`，查 villain 在对偶节点
  （面对 river 下注）的弃牌率，与 `s/(s+P)` 比大小 → 该节点的诈唬开关。这把剥削层从
  「5 类离散画像 ±cap」（`classify`，`opponent.rs:113`；`aggregate_exploit`，
  `opponent.rs:176`）升级成连续、按节点的读数（原 P0-3 的目标形态）。
- hero 侧遥测和 villain 侧统计**复用同一套节点判定代码**——一次实现，两处受益，
  且两边的分母/收缩纪律天然一致。
- ML range 后续接画像（`prof` 当前传 `None`，`range_online.rs:50`，明确的后续项）时，
  画像特征也按同一坐标组织。

### 5.3 旧「场型选路」（multi_field §3）的去向：废弃，降级为节点参数

废弃理由：

1. **检测盲点无解**（旧文档 §3.3 自己承认）：VPIP/PFR/AF 判不出「会平衡的强 reg」，
   整套 `classify_field` + 滞回 + Strategy trait 架构建立在一个当前测不出的信号上；
2. **收益被节点级剥削覆盖**：场型切换的本质是「按对手调打法」，而 §5.2 的节点级画像
   查询以更细的粒度做同一件事，不需要「整桌换一套策略」的重型机制；
3. 少一层架构 = 少一层维护与回归面。

**保留的洞察**：`aggressor_size_tighten`（`policy.rs:71-79`，「大注=价值」的弱场假设）
的反剥削风险是真的。去向：作为**节点参数按画像门控**（确证弱才收紧，未知/确证强不收紧
甚至反读 polar）——即原 P2-3，在 Phase C 按节点落，不需要多策略架构。

### 5.4 P0-1 / P0-3：重新定位为场景 key 的前置

「面对 double barrel」「delayed cbet」「villain capped」这些**线敏感场景没有行动线派生量
就定义不出来**。P0-1 数据基座已落（`seat_actions` / `street_aggressors`），派生量与
`i_have_initiative` 修复排进 Phase A/B（§7）。

---

## 6. 落地路线（三阶段，每步可验）

**Phase A — 纯遥测（零行为变化，byte-equal）。**
给 `decide` 加节点 trace：每个决策打上「场景路径 + 驱动信号（equity、made、关键阈值比较
结果）+ 所选动作」，经 advisor 落进 action log；`heuristic9_analyze.py` 加按节点聚合
`(n, EV, CI, 动作分布)`。trace 只写不读，**决策路径逐字节不变**（sim 回归门一票否决）。
产出：现有隐式树的**热度 / 盈亏 / 兜底率图**——它直接回答「先拆哪里」，让 Phase C 的
优先级不靠拍脑袋。顺带落 P0-1 派生量（纯增字段，同样 byte-equal）。

**Phase B — 显式分派重构（1:1 等价迁移）。**
把 `preflop_decide` / `postflop_decide` 的 if 结构拆成显式节点表（节点 enum/id + 分派 +
每节点参数收进 config 新子树）。**先不改任何行为**：默认参数下与旧代码 byte-equal
（一票否决门），trace 从「事后标注」变「分派即路径」。此阶段结束，场景森林作为代码结构
存在，节点数 ≈ 现有隐式分支数（约 30–60 个）。

**Phase C — 逐节点迭代（常态循环）。**
按 Phase A 的图跑 §4.1 闭环：每轮 top-K 节点 → 造靶 baseline → 拆分/调参 → 回归门。
预期先动的节点（按已知证据）：3bet 池 c-bet 与 SRP 分参、树 C 听牌防守细化、
check-raise 计划节点（P1-1）、翻前 3bet-bluff 频率（P1-4）。「几千个格子」从这一阶段起
自然生长，但每一步都有分母、有靶、有门。

---

## 7. 继承 backlog（自旧文档 §4，保留原编号）

| 原项 | 内容 | 新角色 / 去向 |
|---|---|---|
| **P0-1** | 行动线记忆。数据基座**已落**（`seat_actions`，`view.rs:228`；`street_aggressors`，`view.rs:231`；`tests/heuristic_p0_1_action_line.rs`）| **剩余部分升格为场景 key 前置**：派生量（`villain_capped` / `is_delayed_cbet` / `facing_barrel_n`）+ `i_have_initiative()` 过期主动权修复（行为变更、须 sim 门）→ Phase A/B |
| **P0-2** | 下注尺寸引擎（多档尺寸 / 跨街 geometry / 接线 `size_mid`）| 融入节点参数化：尺寸 = 节点参数的一部分，Phase C 按节点落，不再是独立引擎 |
| **P0-3** | per-street 对手统计（fold-to-cbet 等）+ 连续剥削 | 与 `opponent_features_inventory.md` 合流 → §5.2 节点计数表；剥削从「离散 ±cap」改「节点级连续查询」 |
| **P1-1** | check-raise 专家 | 新场景节点（OOP 无主动权 × 强牌/强听 → check 计划 raise），Phase C；须配「会 cbet 的 aggressor」靶 baseline（P1-5）|
| **P1-2** | equity MC 方差 / 缓存 | 信息层课题，与场景化正交，原样保留（live 提 `samples` 仍是快赢）|
| **P1-3** | 特征粒度（坚果度 / kicker / runout）| 节点内响应信号增强，按消费节点拉动（如 river 价值节点需要坚果度）|
| **P1-4** | 翻前树浅（无诈唬 3bet/4bet、无 squeeze 建模）| 翻前森林的节点扩充，Phase C；3bet-bluff 频率 = 节点参数 + 画像（fold-to-3bet）门控 |
| **P1-5** | 测试 / sim baseline 覆盖 | **升格为 §4.3 纪律的基础设施**：造靶 baseline 是每次节点优化的前置，随 Phase C 每轮增补 |
| **P2-1** | 边界 softmax 混合（`mixing.temperature` 死档）| 节点参数（低优先，弱场对手不针对）|
| **P2-2** | multiway / 有效栈 nuance | SPR/人数桶的细化课题（低优先）|
| **P2-3** | `aggressor_size_tighten` 可剥削性 | → §5.3：节点参数按画像门控 |

---

## 8. 开放问题与拍板点

| # | 问题 | 倾向 |
|---|---|---|
| ① | trace 落点：进 `Decision` 结构随 response 返回，还是 advisor 侧独立日志流？ | 进 `Decision`（新增 `Vec<&'static str>` 路径字段），advisor 原样落盘——单一事实源 |
| ② | 节点 EV 归因口径：整手 net 摊给该手命中的所有节点，还是按「决策点后续盈亏」切分？ | 先用整手 net + 命中节点集（简单、无模型假设），Phase C 视噪声再议 |
| ③ | `n` 门槛与收缩先验取值 | 与 `opponent_features_inventory.md` §H 共用一套（群体先验 + 伪计数），实测校准 |
| ④ | 场景覆盖率指标 | 「落进兜底节点的决策占比」进 analyze 常规输出，作为『还有多少场景没归纳』的量化答案 |
| ⑤ | Phase B 节点表的代码形态（enum 大 match vs 注册表） | enum + `match`：编译器保证完备性（每个局面恰好一个节点），拒绝字符串注册表的运行时缺口 |

---

## 附：关键代码锚点

| 主题 | 位置 |
|---|---|
| 决策入口 | `src/heuristic/policy.rs:170`（`decide`）|
| 现隐式场景树 | `policy.rs:260`（`preflop_decide`）/ `:424`（`postflop_decide`）|
| 共享地板 | `policy.rs:198`（`legalize`）/ `:241`（`safe_fallback`）/ `:33`（`situation_seed`）|
| 尺寸 | `policy.rs:589`（`pick_bet_size`）|
| ML range（信息层，不动）| `policy.rs:119`（`build_opp_ranges_ml`）/ `:134`（`equity_dispatch`）/ `src/heuristic/range_online.rs:49`（`MlRange::predict`）|
| 标量 range 兜底 | `policy.rs:55`（`build_opp_thresholds`）/ `:71-79`（`aggressor_size_tighten`，§5.3 门控对象）|
| equity | `src/heuristic/equity.rs:284`（`equity_weighted`）/ `:156`（`equity`）|
| 特征 | `src/heuristic/features.rs:98`（`made_class`）/ `:193`（`draws`）/ `:239`（`texture`）|
| 行动线数据基座（P0-1）| `src/heuristic/view.rs:228`（`seat_actions`）/ `:231`（`street_aggressors`）|
| 位置桶 | `view.rs:727`（`my_pos_bucket`）|
| 对手画像 | `src/heuristic/opponent.rs:113`（`classify`）/ `:176`（`aggregate_exploit`）|
| 迭代闭环先例 | `docs/heuristic9_design.md` §6（limp K-rag 驳回、PairedTrap 驳回、v0.3 station 尺寸放大 shipped）|
| sim baseline 先例 | `tools/heuristic9_sim.rs`（`Limper` / `PairedTrap`）|

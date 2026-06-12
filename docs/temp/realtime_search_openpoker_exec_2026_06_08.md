# 执行文档：6-max NLHE 真实对抗场景的实时搜索 bot

> 分支 `6max`。本文是 6-max 实时搜索的**目标与落地计划**。OpenPoker 是**验证场**，不是目标本身；
> 强弱只认真实对局。复用的底层机制（建树器 / off-tree 映射 / CFR 子博弈解）见文末「相关文档」。
>
> 结构：§0 定义（目标 / 洞察 / 边界）→ §1 架构 → §2 按三个维度拆问题 → §3 现状 / 缺口 → §4 落地（分步验收 + 并行数据轨道）
> → §5 把关 → §6 风险 → §7 算力 → §8 排期。

## 0. 目标 · 核心洞察 · 适用边界

### 0.1 目标

**在真实 6-max 无限注德州扑克的牌局分布下，尽量打出接近最优的策略。** 真实牌局有三个绕不开的维度：

1. **人数**：见 flop 可能 2 人，也可能 4–5 人。现有 blueprint 只在 **≤3-way** 上训练
   （`width_redirect` N=3），4 人及以上没有真正练过的策略。
2. **码深**：各家起始码深**不相等、且连续变化**，可能 14BB 短码，也可能 500BB 深码。blueprint 只在
   **100BB 对称**码深上训练——`openpoker_advisor.rs:24` 自己也写明「码深 ≠ 100BB：solver 树 / SPR 都按
   100BB 解」，也就是任何真实码深都被强行当成 100BB 来解。off-tree 只翻译下注**尺寸**，修不了树形 / SPR。
3. **限时**：每步决策可设时限 5/10/20s（深码 / 多人树能不能在 5s 内解出来，目前还是没验证的假设，见 §2.3）。

「最优」在多人非零和博弈里没有 Nash 均衡保证，实际含义 = **在真实牌局上解出的一个稳健、接近自我一致（不动点）的策略**。

### 0.2 核心洞察（架构支点）

**blueprint 是提前算好、固定不变的一张表，覆盖不了「码深 × 人数」这个连续变化的空间。** 你不可能为每个码深
（14…800BB）× 每种人数 × 每种不对称的栈组合都预存一张表。**唯一能覆盖这个连续空间的办法，是在每次决策时
按真实牌局（真码深、真人数、真 board）现场建子树、现场求解——也就是实时子博弈搜索。** blueprint 退到次要位置，
只当**先验（对手 range）+ 兜底**，不当最终答案——子树一律**解到终局**、用真实摊牌值，blueprint 不再充当叶子续局值
（深码下它根本给不出，§2.1）。这个结论是独立的工程判断，不依赖任何对战实测。

主干**复用现有的 CFR 子博弈求解器**（`subgame.rs`：`EsMccfrTrainer` + `build_subtree`），
**不引入新的求解核心**——包括把主线 blueprint 已在用的 LCFR 加权接进子树解（加速收敛、是限时的第一杠杆，
机制已在共享 `EsMccfrTrainer` 里、零新核，见 §2.3 / 缺口①）。要补的只有 §3 列的那些缺口。

### 0.3 实时搜索的适用边界（为什么它是主干）

真实牌局离 blueprint 的训练条件（100BB、≤3 人）越远，实时搜索越有用。分两种情况看：

- **blueprint 练得好的牌局（100BB、≤3 人）**：搜索能加的有限——**这不是主目标。** 而且这种牌局里也测不出搜索到底
  强多少：设计 §11.5d 试过「一边开搜索、另一边拿同一个 blueprint 当对手」的自对弈，结果是结构上就测不出绝对强度——
  blueprint 越强、当对手越难打，搜索的任何近似偏差就越亏，这跟「加了搜索是不是更强」根本是两回事。**想知道绝对
  强弱，只能拉外部对手来打，自对弈探针做不到。**
- **blueprint 没练过的牌局（深码 / 4–5 人）**：blueprint **根本给不出答案**——它把任意码深都当 100BB 来解、把 4+way
  硬收成 ≤3-way（§2.1 / §2.2）。这里的问题不是「搜索打不打得过 blueprint」，而是「除了现场解一遍，没有别的办法能在
  真实牌局里出一个像样的策略」。**主目标就在这里，主攻这里。**

**核心区（深码 / 多人）没有一把干净的离线 EV 尺子来量强弱，这是选择直面它要付的代价。** 唯一有干净尺子的是短码
单挑：树小到能离线 CFR 跑到收敛当真值，单挑又回到两人零和、exploitability 是个确定的数。但短码对主目标帮助很小——
出现频率低，而且「短码打得好」不会迁移到深码 / 多人，为它建标尺解决不了核心区。所以核心区只有两条判据，都不是
干净 EV 尺子：

- **(a) 结构性正确（离线、能硬证）**：blueprint 在 off-100BB / 4+way **解的根本是另一个游戏**——树形（SPR、all-in
  阈值）错了，range 先验也错了（info set 的 `stack_bucket` 被硬编码成 0，`nlhe.rs:123`，由它累乘出来的 per-seat range
  等于「假设 100BB 时的 range」）。实时搜索在真实牌局里建真树、解真游戏。「解真游戏比解错游戏好」这一点，靠引擎
  正确性就能硬证：解出来的树，SPR / all-in 阈值跟真实 `GameState` 对得上、守恒、跟 PokerKit 对得上、实时解 ≈ 同状态
  离线 CFR 收敛解，**不需要 exploitability。** 但它只能证「搜索确实有用」（blueprint 在真实码深下被 off-tree 硬 clamp、
  出的是变形动作），**证不出「到底有多有用」**——这个量级在核心区没有干净的离线尺子能量。
- **(b) 外部对手实战（OpenPoker，弱判据）**：量真实 EV 增益的唯一办法，但不能配对、功效低、还带系统偏差（§4 功效
  预算）。在账号能打到的手数里基本判不出「显著更强」，只能当「别打更差」的护栏 + 一个弱方向参考。

**所以 §0.1 说的「接近最优」，对核心区是努力方向，不是 v1 能硬验收的强声明。** v1 能硬交付的是「在真实牌局里解真
游戏、引擎正确、时限内解得动」（离线可证）；至于 EV 上到底赚多少，等外部对手的功效预算到位后，再一格一格转成可
验收。第一道能硬验收的关卡，是深码 / 多人的引擎正确性 + 时限可行性（§4 步 A）；EV 上的强弱，老实留给 live + 时间。

### 0.4 剥削 = 可选的加分项（后置）

同一个子博弈求解器，对手的 range / 策略默认来自 blueprint（稳健打法）；**等对某个对手攒够了可靠数据，就把那部分
输入换成实测到的对手倾向**，于是从「稳健」平滑切到「剥削」。一套引擎，两个数据源。先把稳健这版做好，
剥削是后面的加分项，整体**后置**（上线注意事项见 §6）。

> 注意区分：这里说的「剥削」专指**对手建模**（换对手 range 的数据源），和设计 §11.4 的 biased 叶子续局机制是两回事——
> 后者在本方案主路径上**根本不出现**（子树一律解到终局、没有叶子续局值这一环，§2.1）。放弃 depth-limit / 叶子值的缘由见 §6 #2。

## 1. 架构（实时搜索为主干，剥削为可选加分项）

```
每决策：
  1. 读真实状态：真码深（各家不等）/ 真在场人数 / 真 board / 真下注历史
  2. gating：先决定走 blueprint 还是搜索（should_search，subgame.rs:702）
       · preflop → 直接用 blueprint 策略，不搜索（现有 SearchTrigger 全是 postflop 触发）
       · postflop 且结构接近 blueprint 练过的 infoset（≈100BB / ≤3-way / 下注线 on-tree）→ 仍用 blueprint 策略
       · 否则（off-100BB / 4+way / off-tree——blueprint 解的是错游戏）→ 实时搜索，走 3–6 步（§0.1 主目标区）
  3. 在真实状态上建子树（build_subtree，真 SPR / 真人数）；建到终局、不 depth-limit，下注菜单按码深缩放（深码单一 {1pot}、短码可多档）
  4. 取对手 range：默认来自 blueprint；有可靠对手数据时换成实测（剥削加分项，后置）。无叶子续局值——子树解到终局
  5. 在预算内求解子树（墙钟 anytime，到时限返回当前平均策略，§2.3）；真解不出来（建不了树 / 连一轮有用迭代都跑不完）
     就**降级取安全动作**（能 check 就 check、否则 fold；2026-06-09 改，原「直接 fold」，§2.3）——不回落 blueprint（off-distribution 下它解的是错游戏）
  6. 返回动作（off-tree 尺寸经 map_off_tree 翻译）
持续：
  记全桌动作 + 摊牌（HH 日志）→ ①统计真实分布覆盖热力图 ②攒对手数据（加分项用）
```

**gating = 把 §0.3 的可靠边界落到决策环里，不是新机制。** blueprint 只在它练过的条件（≈100BB、≤3-way、on-tree）下可靠，
所以这些局面 + 全部 preflop 直接用 blueprint 策略，搜索只接管 blueprint 解错游戏的区域（off-100BB / 4+way / off-tree，§0.1 主目标区）
——**搜索是对 blueprint 的增量、不是替换**：能用 blueprint 的地方继续用，省算力也避开搜索的近似偏差（§0.3：blueprint 越强、
搜索近似越亏）。注意 blueprint 在 gating 里只剩**一个角色**：preflop + 近 blueprint 局面的**主答案**；搜索区里它**不再当降级兜底**
（搜索解不出来就 check-when-free、不回落 blueprint——off-distribution 下它解的是错游戏，§2.3）。preflop 不搜**已经是代码现状**（`should_search`（`subgame.rs:702`）对 preflop 两个
trigger 都返回 false、测试断言「preflop 不搜」`:1269`，注释也写明「preflop 走 blueprint」`:697`）；要补的是 postflop 那条
「结构接近 blueprint infoset」的判据——现在只由窄触发面 `FlopFirstUnraised` 粗略代理（只搜 flop 未起注首点这类覆盖好的局面），
把「≈100BB / on-tree」量准要复用 §4.2 那套「可靠 vs Desync vs 乱映射」分类，还没建。

主干不写新的求解核心：root 在真实状态重发隐藏牌、对全 range 求解、事后再索引 hero 的真实桶
（`subgame.rs:416` root / `:331` query_at）。决策环里真正的 search-or-blueprint 分支在
`blueprint_advisor.rs:498-539`（函数 `play_cross_abstraction_hand:410`，靠 `Contestant.search=None`
保持旧行为 byte-equal）。

**把搜索接进去是整条管线重建，不是改一行。** 生产入口 `openpoker_advisor.rs` 是无状态的单决策重放模型，
和 `play_cross_abstraction_hand`（整手自对弈 harness）结构对不上；要接进生产得做三件事：
①协议 / 解析层捕获各家的真实栈（原 `Request` 结构没有 per-seat stack 字段 → 真码深从入口就丢了）；
②`decide()` 里新建 search 分派；③重写 outgoing（原来是「100BB 解 → ÷scale → clamp 进真实区间」，
不是按真码深算尺寸）。在这之前，生产 bot 在非 100BB 局面**悄悄解的是错的游戏**（短码 all-in 被当成小注、也没有
fallback 标记），这正是 §0.1 维度 2 要修的毛病。**这三件已落地（2026-06-09，commit `7413da2`，缺口②，见 §3.2）**：
`Request` 加 optional `stacks[6]`、`decide()` 真栈 search 分派（`build_real_auth` + `GameState::inject_external_cards`）、
outgoing 按真栈算尺寸；非 100BB 不再悄悄解错游戏（搜索区解不出来 check-when-free、不回落 blueprint）。守 `search=None` byte-equal。
**2026-06-10 续：影子失同步区也接入搜索**——off-stack all-in 线 / 真实 4+way / limp 池的触发点不再止步于兜底，走脱影子
`subgame_search_unanchored`（node_id / 触发 / 子树全来自真栈，range 退 uniform；§3.2 缺口② v1 边界① 已收口）。

## 2. 三个真实维度 + 各自打法

### 2.1 码深（搜索最能发挥的地方）

引擎本身**已经能处理任意 / 不相等的码深**——代码核验过：betting tree 只按 `AbstractActionTag` 分叉、不存金额
（`nlhe_betting_tree.rs:31-42`），bet 尺寸在运行时按真实 pot / stack 现算（`action.rs:368-411`），all-in 点 = 真实
per-seat `cap = committed + stack`（`state.rs:438/476`），`build_subtree` 从真实的中途状态展开、subgame 全程用
`root_state.config().clone()`（`subgame.rs:1020/1036/1047`），`TableConfig.starting_stacks` 是 per-seat `Vec`、可以不相等。
**「现场求解天生能处理不对称栈」在机制层成立，预计算表做不到。** 但这只说明「引擎能做」，生产 advisor 还没把真实栈喂进去
（见 §1 / §3.2 缺口②）。

- **深码（150–800BB）= 主目标核心、最难、直接攻**：**直接解到终局**——摊牌值由真实 `GameState::payouts()` 精确给出
  （零叶子近似）；控树靠**下注尺寸抽象**，深码把下注菜单收到**单一 {1pot}**（外加 fold / call / all-in），把到终局的树压到
  可解，时限内**尽力解、不保证收敛**（anytime + LCFR，§2.3）。核心工程 = 「**时限内把 {1pot} 窄树解到终局**」（缺口①/③），
  码深维度最难，**v1 直接做、不绕开。**
- **不对称栈 = 码深维度里最常见的形态、也是关键前提**：现场求解天生能处理（建真树就行），预计算表做不到。但必须先验证
  引擎在不对称栈下守恒 + SPR / all-in 阈值和真实栈一致（§5），才能把「现场求解天生处理不对称栈」当成前提用。
- **短码（14–30BB）= 同一套打法、树小所以菜单可以更宽**：和深码一样直接解到终局，唯一区别是树小——短码很快就 all-in、
  到终局的层数少，所以**能负担更丰富的下注尺寸菜单**（深码被迫收到单一 {1pot}，短码可以多档），是码深维度里*最省事*的一档；
  引擎在深码上做对了，短码自动落在能力范围内，不必单独列。

### 2.2 人数 >3（最难的结构问题，正面解决）

- **打法 = 实时解 N-way 子树**：多人解到终局时，摊牌值由真实 `GameState::payouts()` 的 N-way side-pot showdown 精确给出。
- **深码 × 多人是两个难点叠加、最难**，要单独出设计；但真实牌局里这种情况最常见，正是要主攻的地方，不回避。

### 2.3 限时（必须先打好的地基）

- 现状：`SubgameSearchConfig`（`subgame.rs:650`）有 8 个字段（含 `depth_limit` / `biased_leaf` 等机制开关——
  **本方案深码 / 多人都不用这两个**：放弃 depth-limit、改解到终局，控树靠下注尺寸抽象，§2.1），
  但**没有 `time_budget`**、也**没有 CFR 变体开关**——`subgame.rs:1066` 直接 `EsMccfrTrainer::new`、跑的是
  vanilla ES-MCCFR（不加权）；求解循环 `:1068` 是固定的 `for _ in 0..iterations`、不会按时间中断——还不是随时可停的求解器。
  单决策耗时（wall）**从来没单独测过**（现有数据全是整条手臂的）。

- **收敛加速 = LCFR（先接进子树解，是限时的第一杠杆）**：限时的本质矛盾是「迭代数 ↔ 解得多准」，LCFR 直接改善这个兑换比。
  LCFR = 按迭代序号线性加权（越靠后的迭代权重越大，Brown & Sandholm 2018），**相同迭代数 / 相同 wall 下离收敛更近**——
  正是 5s 预算最缺的。机制**已经在共享的 `EsMccfrTrainer` 里**（`with_lcfr_period` → `maybe_lcfr_rescale`，
  `trainer.rs:332/352`，主线 blueprint 训练用的就是它），子树解只是没接；接进来 = `subgame_search` 构 trainer 那行
  （`subgame.rs:1066`）补一个 `.with_lcfr_period(...)`，**不是新写求解核**。**唯一要重标的是 period 粒度**：blueprint 的
  period（千万–亿级 update）放进子树解的几千迭代里**一次都不会触发**——`trainer.rs:332` 的 doc 注明 period 要让
  `总更新 / period` 落在 20–100，否则线性权重不充分；所以子树解得按 `cfg.iterations` 现算一个小 period（例如 `iterations/50`），
  否则等于没开。这直接让墙钟 anytime 同时限解得更准（每迭代更接近收敛），正是 5s 预算最缺的。

- **限时打法 = 墙钟 anytime（解到时限就停、返回当前平均策略）。** 「解到时间用完就返回当前策略」会让迭代数取决于
  机器速度 / 负载，同一个 `(state,seed)` 会产出不同策略，**从原理上就做不到 byte-equal**——所以限时求解器**不靠 byte-equal**，
  改靠 seeded RNG（用局面派生种子）+ replay / AIVAT 一致性来保证可复现（须说明 G1–G3 怎么过）。仍要先离线产出
  「(节点数, 迭代数) → 单决策 wall」的回归曲线（= §4 步 A 的 wall 量化），用来判 5/10/20s 在目标树上大概能跑到多少迭代、
  够不够用（控树只靠下注菜单宽度、解到终局、不 depth-limit / 不 limit_street，§2.1）。

  byte-equal 仍是发现算法 bug 的便宜检查手段（`invariants.md §2`），但只在**不限时**的路径上保留
  （`search=None` 回归 / HH 日志隔离 / 对称树引擎正确性 fixture，见 §5）；限时求解本身做不到、也不强求。

- **降级 = check-when-free（2026-06-09 修订，原「直接 fold」）。** 墙钟 anytime 到时限返回当前平均策略，这本身就是
  可用结果、不算降级；只有**真解不出来**（建不了树 / 连一轮有用迭代都跑不完 / 会 panic）这种罕见情况才降级。**降级动作 =
  能 check 就 check、否则 fold**（不回落 blueprint——off-distribution 下它解的是错游戏）。原定「直接 fold」在**可 check 的局面**
  （搜索触发面 `FlopFirstUnraised` 全是 flop 首点 = 必可 check）会白丢一个免费 check（严格劣于 check），故改 check-优先；
  check / fold 都**绝不会打出「错游戏」动作**、也都不回落 blueprint，省掉「建 off-tree 兜底启发式」这个子项（缺口①）。
  生产实现 = `openpoker_advisor::search_giveup`（`source=search_giveup:*`，与 blueprint `fallback:*` 分桶）。

- 算力参照：Pluribus 实时搜索用 28-core/128GB、**平均 ~20s/手**——单决策 wall 是（树大小 × 迭代数 × **核数**）的函数。
  **本方案不锚定某台机器**：开发 / 测试机和真实部署机不是一回事，部署机可能强得多。所以先在手头能跑的机器上测出 wall 曲线，
  再按目标部署机的核数外推；机器越强，同样时限内能解的树越大 / 迭代越多。**5s 内深码 / 多人能不能解出来，得在目标部署机上实测**
  ——若部署机也吃紧，再把目标范围收到 10–20s。**深码 / 多人树能不能在时限内解出来，是最根本的难点。**

## 3. 现有底座 / 缺口

### 3.1 已落地可复用

| 构件 | `file:line` | 状态 |
|---|---|---|
| 真实状态建子树 | `nlhe_betting_tree.rs:271 build_subtree` / `:309 depth_limited` | ✅ 可接任意中途 `GameState` 作 root |
| CFR 子博弈求解 | `subgame.rs:650 SubgameSearchConfig` / `:1066 EsMccfrTrainer` | ✅ 解到终局；超 cap / 未访问安全回落（`depth_limit` 字段仍在，但深码 / 多人不用它——解到终局、控树靠下注尺寸抽象，§2.1）。**缺口① 已接（2026-06-09，commit `ce25ee6`）**：`SubgameSearchConfig.lcfr`（默认 false = vanilla，既有 probe/advisor/§11.5 基线 byte-equal）；`lcfr=true` → `.with_lcfr_period((iterations/50).max(1))`，零新核（机制在 `trainer.rs:332`）。**`time_budget` 墙钟 anytime 已接（commit `c9dd154`）**：`SubgameSearchConfig.time_budget: Option<Duration>`，跑到 iterations 上限或 wall 达预算就停；默认 `None` = 既有固定迭代、逐 infoset byte-equal 不变；`iterations==0` 退化 → `Err`=fold。**仍未做**：随求解循环周期性查 deadline 的更细粒度截断（现每迭代查一次，µs 级树足够）+ 按部署机核数外推 |
| off-tree 尺寸映射 | `action.rs:476 map_off_tree`（pseudo-harmonic randomized rounding） | ✅ 任意下注尺寸；纯函数可复现 |
| blueprint 加载 / 兜底 / fallback 统计 | `nlhe_dense_trainer.rs` / `openpoker_advisor.rs:119 safe_fallback` | ✅ 冷启动 / 失败退路 |
| 多人 equity | `tools/multiway_equity_probe.rs:197 multiway_equity_mc` | 🟡 离线私有函数；主路径解到终局用真实 `payouts()`、**生产不需要它**（§2.2；降级是 check-when-free，也不用它） |

### 3.2 缺口（须新写 / 改造）

1. **限时求解器**：用墙钟 anytime（解到时限就停、返回当前平均策略，§2.3）——不要求 byte-equal（限时求解原理上做不到），
   靠 seeded-RNG + replay/AIVAT 保持可复现。仍要离线产出「(节点数, 迭代数) → 单决策 wall」回归曲线，判 5/10/20s 在目标树上
   够不够迭代。off-tree 解不出来就**降级取安全动作**（check-when-free，不建启发式、不回落 blueprint，§2.3）。**wall 曲线要直接
   画在深码 / 多人的目标树上**（不是最省事的小树）。
   **外加：把 LCFR 接进子树解**（`subgame.rs:1066` 那行 `EsMccfrTrainer::new` 后补 `.with_lcfr_period(...)`，period 按
   `cfg.iterations` 现算小值 ≈ `iterations/50`；机制已在 `trainer.rs:332`、零新核）——同 wall 收敛更快是限时的第一杠杆；
   wall / 收敛曲线要 **vanilla 与 LCFR 各量一条**，「5s 能否解到有用迭代数」按开了 LCFR 的那条判。
   - **进度（2026-06-09，commit `ce25ee6`→`c9dd154`，vultr 全绿）**：✅ **LCFR 已接**（`SubgameSearchConfig.lcfr`，零新核）；
     ✅ **`time_budget` 墙钟 anytime 本体已接**（`Option<Duration>`，跑到 iterations 上限或 wall 达预算就停；默认 `None`
     逐 infoset byte-equal；`iterations==0` → `Err`=fold）；✅ **wall 曲线已画到真目标树**（`_measure_subgame_wall_and_convergence`
     扩到 HU 500BB {1pot} 解到终局 + 4/5way limped {1pot}，用 `deep_single_pot()` 菜单）+ ✅ **ε/δ_conv 机制**（新
     `_measure_convergence_calibration`：river/turn 子树 L1 + root-EV 差 MC）。**仍未做**：① ε/δ_conv **真阈值**须在**真桶表**上跑
     （stub 桶全归桶 0 = 退化 ε，换 `BucketTable::open` + river/turn 真 board 即可）；② 深码×多人**叠加**大树 + 按**部署机**核数外推。
   - **within-round solve 缓存已落地（2026-06-10，commit `c0bce25`，vultr 全绿 497/0）**：RoundStart + round-stable seed
     的「同街多决策共享字节相同 solve = 一个均衡内自洽」（§6 #2）只在固定迭代下成立——advisor 逐决策无状态重解，
     (a) 同街第二决策重建重解一遍字节相同的子博弈 = 纯浪费 wall；(b) 开 `time_budget` 后 anytime 迭代数随机器负载变 →
     同街两次重解可停在不同迭代数 = 读**不同均衡**（§6 #2 想避免的 mid-round 不一致部分回来，正是当年 AllPostflop
     朴素放宽实测退化的机制根源）。修法 = advisor 常驻进程持 `SubgameSolveCache`（容量 1）：key = solve **全部**实际
     构造输入在 solve 边界现算 blake3（桶表身份 / cfg 全字段 / hand_seed+街 ordinal / root 几何全可见面 /
     entrants+raises / §5b reach 向量逐位 / 子树菜单+规则——漏一项 = 读错均衡是唯一认真风险，故不从请求层推导）；
     命中 → 复用 trainer 只重导航（mid-round 导航用 solve 时存下的同一份 sub_abs）。效果：「每轮恰好一个 solve」恢复
     （time_budget 下同街读同一均衡）+ mid-round 决策 wall ≈ 0（免建树+求解，build 正是深码×多人真瓶颈 A②）+ 首决策
     可放心把 time_budget 用满。`subgame_search*` 拆薄壳 + `*_cached` 变体（cache=None = 原行为，既有调用点逐 infoset
     byte-equal 不动；锚定 / 脱锚两路都吃缓存，kind 进 key 不串条目；depth_limit 路径不缓存——key 须带 blueprint 树
     叶子映射身份，非生产路径）。固定迭代下命中输出 byte-equal 从头重解（测试钉死：hit/miss 计数硬证不重解 + 换
     hand_seed / iterations / root 几何必 miss + advisor 端到端 mid-round 命中 `search_within_round_cache_hits_and_byte_equal`）。
   - **live_traversers 已落地（2026-06-10，commit `7546c0b`，vultr 全绿 500/0，限时杠杆②、与 LCFR 正交）**：
     `EsMccfrTrainer::step` 的 alternating traverser 按 `config.n_seats`（=6）全座轮转，而子树里**弃牌 / all-in 座
     零决策节点**（规则引擎只让 `Active` 座当 `current_player`、σ/regret 都只在 `actor == traverser` 节点累积）→
     轮到它们的迭代**纯零学习**、成本照付（root resample + 路径遍历）。浪费比 = `(n_seats−n_active)/n_seats`：
     fold 剩 3 人 = 50%、剩 2 人（live 最常见形态）= **67% → 修复后同 wall 有效迭代 ×2-3**（h2h `SearchObserver`
     的浪费遥测量的就是它，现已感知该旗：开修复浪费记 0）。实现 = `EsMccfrTrainer::with_traverser_rotation`
     （默认 `None` = 既有全座轮转逐位不变、不存 checkpoint；轮转 = 全集时与默认**逐位等价**，测试钉死「只换
     选择来源、不引入行为差」）+ `SubgameSearchConfig.live_traversers`（默认 `false` = 全部基线 byte-equal；
     `solve_subgame` 从子树根现算 Active 座轮转表；**旗进 solve 缓存 key**——两种轮转是不同 rng 流的均衡，
     串读 = 读错均衡）+ advisor `--search-live-traversers`。开旗后 rng 消费序列改变 → 与 false 基线不 byte-equal
     （固定迭代 + seed 自身仍确定性可复现）。测试：Kuhn 轮转 `[1]` → 座 0 σ 无条目 + regret 恒零（零学习机制
     本体）/ 全集 ≡ 默认逐位 / off-stack 6 座 2 Active 端到端 + 翻旗必 cache miss。与 within-round solve 缓存
     叠加：缓存省 mid-round 重解，本旗省首决策 solve 内部的零学习步（time_budget 等效 ×2-3）。
2. **生产 advisor 接搜索**：`openpoker_advisor.rs` 现在完全不调 `subgame_search`、写死了
   `default_6max_100bb`（`:191`）、`Request`（`:84-96`）也没有 per-seat stack 字段。要捕获真实栈 + 在 `decide()` 里新建
   search 分派 + 重写 outgoing（见 §1）——是整条管线重建，不是接一行。
   - **已落地（2026-06-09，commit `7413da2`，vultr 全绿）**：`decide()` 从纯 blueprint 单决策重放扩成
     **blueprint / search 双路**：①`Request` 加 optional `stacks[6]`（hand-start 真栈，OpenPoker 单位）；②`--search`
     开 + 命中触发面（`should_search`，postflop）→ 新增 `build_real_auth` 在**真码深** `TableConfig`（各座
     `stack×scale`）上重放本手 → 新增 `GameState::inject_external_cards` 注入真实 board + hero 底牌（`query_at` 索引
     hero 真桶必需）→ `subgame_search` 解到终局（`time_budget` / LCFR 经 CLI）；③outgoing 按**真栈 auth** 算尺寸
     （非「100BB 解 ÷scale」）。**解不出来 = check-when-free**（能 check 就 check、否则 fold；建不了真栈树 / 子博弈
     `Err`），**不回落 blueprint**（§2.3；`source=search_giveup:*` 与 blueprint `fallback:*` 分桶，对齐 §4.1 fallback 护栏）。④python driver 跟
     `committed_total` + 各座 remaining（`your_turn.players` / `player_action.stack`）→ 回推 hand-start 真栈送进 `stacks`。
   - **守 `search=None` byte-equal（硬不变量，测试 `search_off_byte_equal_blueprint`）**：未开 `--search` / preflop /
     未触发的 postflop 决策一律走原 100BB blueprint 路径、逐字节等价旧行为；search 只在触发点改输出。
   - **v1 边界（①已收口）**：①~~取 `node_id` / `legal_abs` 仍靠 100BB 影子重放~~——**已收口（2026-06-10，
     commit `5c43dd8`/`be4a389`，vultr 全套件 486 passed / 0 failed，脱影子搜索）**。根因核验：off-stack all-in 线的失同步是**结构级**的——blueprint
     全局树按 100BB 对称栈建，短码 shove 在树里是全栈 all-in，「raise-over / call 完还活着」的后续节点**根本不
     存在**，影子导航再鲁棒也修不了。修法 = 搜索路径不再要 blueprint 锚：`subgame_search_unanchored`（子树根 /
     entrants 从真栈轮起点现算 + within-round 导航用当前街真实动作序在子树上重放（tag 以真栈几何经 `map_off_tree`
     现算、每步 actor 校验）+ **range 先验退 uniform**（§5b 留作 A/B 的那条路；off-100BB 下 blueprint range 本就是
     「假设 100BB 的 range」§0.3，诚实退化）+ 返回子树自身合法集分布（deep_menu 同契约））。advisor 侧：lockstep
     失同步且 `--search` 开且 postflop → `decide_search_unanchored`（真栈 auth 判触发 → 脱锚搜索 → outgoing 按真栈 +
     子树抽象；`source=search:unanchored`）；影子可用时仍走原锚定路径（blueprint range 先验更好）；preflop / 未开
     搜索维持旧兜底（`search=None` byte-equal 不破）。**连带把 A4 width_redirect 在脱锚子树里关掉**（它是 blueprint
     训练期的 preflop 收口装置，>N-way 真实局会触发 `build_subtree` 的 ≤N 断言 panic；关掉后解**真游戏宽度**，树宽由
     `max_subtree_nodes` cap 兜底）→ **真实 4+way 见 flop 的触发点现在可搜**。**重要副作用：limp 池进搜索**——
     open-limp 在 nolimp 影子上 preflop 即 structural_gap（旧路径整手只能兜底），但 limp 在真栈重放里完全合法 →
     limp 多人池（真实分布最常见形态）的 flop 触发点从 check-when-free 变成真搜索（测试
     `limped_pot_flop_searches_unanchored` 钉死）。**残留边界**：脱锚搜索 range = uniform（对手建模 / 部分前缀 reach
     是后续细化）；blueprint 区（preflop + 未触发 postflop）的影子失同步仍走旧兜底（修不了、也不该用搜索接管）。
     ②子树菜单：默认沿用 blueprint 菜单，**`--search-deep-menu` → 单一 {1pot}**（深码窄菜单 = **缺口③ 已落地**，
     2026-06-09 commit `0fb41da`，见下缺口③；脱锚路径同样支持 deep_menu）。
   - **range 先验平滑已落地 + 搜索激进度根因细化（2026-06-12，commit `7fac852`→`d18c2da` + 收尾，vultr 受影响测试全绿）**：
     searchon50 实跑撞出锚定搜索极端激进——97o 河牌空气（`6h 8c 2c 2h 3c`）面对 check 给 check 0.0002 / allin 0.7265
     （20s 预算更极端 0.91 = 收敛非噪声；turn 同手 allin 0.58；实战被三条 call）。诊断（`origin/debug/subgame-dump` 分支
     `POKER_SUBGAME_DEBUG` dump + `POKER_SUBGAME_UNIFORM` A/B）：对手 river reach 塌缩（有效 50 组合、78 一类占 36%、
     近乎无同花 = 封顶），解内对 jam 弃 72.6% > 65.7% 盈亏平衡 → 空气 jam 印钞。
     **修法 = `SubgameSearchConfig.range_uniform_mix`**：**对手**座位 reach 与非撞 board 组合 uniform 按 `r'=(1−λ)r+λu`
     混合；**hero 保持原 reach**——对称混合实测方向反掉（hero range 灌入强牌 → 对手被迫更尊重下注 → 弃牌率虚增，
     λ=0.5 比 λ=0 还激进），理论同向：只有对手 range 的不确定性有「向 uniform 回拉」的正当性，hero 自己的 range 是
     「实际怎么走到这条线」的自一致输入。默认 0 = 全部既有基线 byte-equal；生产 advisor 默认 0.25
     （`--search-range-uniform-mix`，driver 透传；显式 0 = 关）。λ 进 solve 缓存 key（cfg 字段 + 混合后 reach 向量双重覆盖）。
     **判决 sweep（固定 150k 迭代 ×5 seeds，直接读「对手面对 jam 的解内弃牌率」）把根因细化成三层**：
     ①对手 prior 影响比初判小——λ∈{0,0.25,0.5,1} 弃牌率 0.73→0.76 平，对手全 uniform（同花/三条全在）仍弃 ~0.76；
     ②该点位 jam 偏好的主驱动 = **hero 自身 reach range 在同花成牌河的坚果占比**（polar 超池 jam 结构性近均衡；
     早先 sym-uniform A/B 的「回 check 0.31」来自 hero range 被 uniform 化 = 解错游戏，不是对手 range 的功劳）；
     ③**per-bucket 均衡选择噪声巨大**（同配置换 seed，空气桶 jam 0.44↔0.91——近无差异下注档之间 CFR 平均策略落点
     半任意）。**λ 平滑的诚实定位 = 对手 range 永不缺关键组合的尾部保险（0.25 实测有效组合 50→86.5），不承诺改写
     激进度**；激进度后续杠杆（未做）= 对手真实动作在解内频率过低时降信 / 输出向 blueprint 分布回拉 /
     per-bucket 噪声购更多迭代或 purification。**连带发现**：range 加权采样吞吐 ~7× 掉速（同 5s 预算 145k vs 993k
     updates，`sample_holes_from_ranges` 每 root 采样 O(1326×座位)）= 后续可收的限时杠杆。
3. **深码窄菜单解到终局**：深码 = 把下注菜单收到**单一 {1pot}**（短码可放宽）、解到终局用真实 `payouts()`，在 `time_budget`
   内尽力解、不保证收敛（anytime + LCFR，缺口①）。核心工程 = 「**时限内把 {1pot} 窄树解到终局**」（不重建叶子值，缘由见 §6 #2）。
   - **已落地（2026-06-09，commit `0fb41da`，vultr 全绿）**：`{1pot}` 菜单（`deep_single_pot`）**接进 `subgame_search` + 生产
     advisor**。`SubgameSearchConfig.deep_menu`（默认 `false` = 既有行为 byte-equal）：`true` → 子树用 `deep_single_pot()` 单档
     菜单建+解，**与 blueprint 菜单解耦**（桶表按 cards/board 归桶、与菜单无关，§2.1）。关键工程坑 = **菜单不匹配**：`{1pot}` ⊊
     blueprint `{0.5,1,2}`，旧 `subgame_search` 把子树策略**对齐 `legal_abs`（blueprint 影子合法集）必失败**（多出的 0.5pot 找不到
     对应 → `Err` → 降级）；故 deep_menu 路径改为**返回子树自身合法集上的分布**（`{1pot}` 动作携带按 `auth` 真实 pot 算的 `to`），
     advisor 端 outgoing 也用 `{1pot}` 抽象算尺寸（自洽）。`openpoker_advisor` 加 `--search-deep-menu` flag（拒绝静默 guard 同
     `--search-lcfr`）。deep_menu 与 depth_limit **互斥**（早 `Err`，深码无叶子值 §6 #2）。测试：`subgame_search_deep_menu_single_pot`
     （只含 1.0pot 档 + 不因菜单不匹配 `Err` + 可复现）/ `deep_menu_and_depth_limit_mutually_exclusive` /
     `search_deep_menu_legal_and_reproducible`（深码不对称栈 600BB vs 200BB 端到端 `source=search`）；守 `search=None` / 非 deep
     路径逐 infoset byte-equal。
   - **v2 两细化已落地（2026-06-10，commit `2c29df4`→`f1e0455`，vultr 全绿）**：
     ②**SPR + 人数自适应菜单宽度**（`deep_menu_for`，纯函数——`subgame_search*` 选子树菜单与 advisor outgoing 必须各自重算出
     **同一**菜单，否则 0.5pot 档在 {1pot} 抽象下塌成 all-in = 错动作）：浅 SPR（第二大 Active 剩余栈 ≤ 4×pot）**且 ≤3 Active**
     → `{0.5,1}` 两档（`deep_wide_half_pot`，first-bet-small 口径）；否则维持 {1pot}。**人数闸是 vultr 首跑实测逼出来的**：
     6-way 25BB 恰边界宽档子树 = **558,360 节点 vs {1pot} 27,108（20.6×）**——多人加宽是乘性爆炸、单建树 ~1.5s 吃掉 5s 预算
     大半；live 浅码池最常见形态本就是 fold 剩 2–3 家的小池，4+way 浅码维持 {1pot}。3-way 13BB 边界树大小测试钉死 + 200k 绝对护栏。
     ③**deep_menu 配 `AllPostflop` 的 within-round 导航**：deep 菜单 ≠ blueprint 菜单 → blueprint tags 在子树上必失配，mid-round
     改用**当前街真实动作序**在子树上重放导航（`navigate_subtree_by_real_actions`，与脱锚路径同口径；`subgame_search` 加
     `within_round_real` 参数，openpoker_advisor 把 `build_real_auth` 已产出的 `within` 喂进去、blueprint_advisor h2h 维护
     `round_within`）；未提供动作序 → `Err` 安全降级。测试 `deep_menu_allpostflop_midround_navigates_by_real_actions`
     （0.5pot mid-round：提供动作序 Ok / 不提供 Err / byte-equal 可复现）。
     **仍未做**：①`{1pot}`（及浅码 `{0.5,1}`）对**深码 / 多人策略质量**够不够 = B/live 问题（与可解性无关，§4.1 A② 续）。
4. **多人 >3 的树**：见 §2.2，**实时解 N-way 子树**——解到终局、用真实 N-way side-pot `payouts()`。
5. **真实分布覆盖度量**：见 §4.2（HH 日志 + 覆盖热力图，都还没建）。
6. **多人 AIVAT 降方差**：`aivat_nlhe.rs` 现在是 HU 单对手（两人写死），要推广到 N 座。单边 P_a={chance, 我方}
   就无偏（不需要对手策略，用 blueprint 当值函数也合法）。这是 §4 live
   功效预算里唯一不需要配对的降方差手段（OpenPoker 不能配对，配对带来的方差缩减用不上）。缺口⑥**纯用于评测降方差**；设计 §5.c 里那个把 `aivat_value`
   当「搜索叶子值函数」的用法，随放弃 depth-limit / 改解到终局已**不在本方案内**（主路径无叶子值函数，§2.1）。
   - **估计器已落地（2026-06-10，commit `acf6388`→`e90e0c5`，vultr 全绿 493/0）**：新 `src/training/aivat_multiway.rs`
     ——**live 可见信息口径**（对手底牌大多不可见，只有摊牌 `shown_cards`，与 HU 全知日志本质不同）。修正项 v1 =
     **AIVAT = U − (c_deal_us + c_runout)**：c_deal_us 精确无偏（发牌不条件在对局信息上；VF-1 `(rel_pos,169类)` 经 trait
     注入、缺省 0）；**c_runout = 主力**（all-in 锁定后纯发牌段，剩余牌堆精确枚举、逐补全经新
     `GameState::with_external_cards_and_runout` + 权威 `payouts()`——N-way side pot / all-in-for-less / odd-chip 与真实
     结算同口径，多人下不能用 HU 的 `m·(2eq−1)` 闭式）。**c_deal_opp 任何情况不纳入**（只在摊牌手纳入 = 纳入条件在
     对局结果上 = selection bias）；c_board / c_act v1 不做（HU 生产实测自对弈 VF 的这两项**净加噪声**，
     `docs/aivat_eval.md` §10；c_act 数学上精确无偏、等 HH 日志带 σ 后可作 v2）。**已知近似唯一一处（模块 doc 明示）**：
     弃牌座未知底牌留在 runout 牌堆（card-bunching 残差）。
   - **闸门（vultr 实测）**：①3-max 不等栈 side-pot flop 锁定 903 补全，估计器 E_runout 与 stacked-deck 全程重放 oracle
     逐补全一致 + 每补全 Σ payouts==0（这测试同时撞出并钉死了一个真 bug：c_runout 的 prefix 误用重放态随机 board =
     有偏修正，commit `e90e0c5` 修）；②6-max 底牌相关合成策略自对弈、估计器只喂 live 可见信息：**N=6000 配对
     |mean(d)|=35.8 ≤ 84.7=1.96·SE ✓ 无偏**（runout 手 497、未知弃牌座 1434 个——card-bunching 近似被重度压测仍过闸），
     **SE 缩减 1.33×（方差 1.77×）**（合成自对弈、postflop all-in 频率偏高，live 实测会更低）。
   - **预期修正（对上面"SD 缩到 1/2–1/3"的下修）**：那是论文全知设定；HU 生产实测推荐集只有 1.10×、本合成闸门 1.33×
     → **AIVAT 白拿一截 CI 收窄但救不动 live 功效**（§4.1 结论不变、反而更稳）。
   - ~~接真实 HH 日志 + VF-1 小表~~——**已落地（2026-06-10，commit `e9b5738`，vultr 模块/集成/真 1B selftest 全绿）**：
     ①driver `--hh-log`（默认开、append）每手落全桌 HH JSONL → ②`src/training/openpoker_hh.rs::hh_to_multiway_input`
     （×scale + 动作转换重放 + U=(final−start)×scale，全不一致 loud Err）→ ③新 bin `openpoker_hh_aivat` 出 raw/AIVAT
     mean±SE（mbb/g）；④VF-1 = `Vf1DealTable`（169×N JSON artifact）+ 新 bin `vf1_deal_table_build`（blueprint 自对弈，
     holes 由同 hand_seed `GameState::new` 复现）——**正式表已产**：preopen 10B 自对弈 1M 手 → vultr
     `artifacts/vf1_deal_table_preopen10b.json`（0 desync、1014/1014 格全覆盖、最薄格 2888 样本、wall 20s；数值合理性
     抽查过：AA@BTN +13.2BB、72o 非盲位恰 0 / SB 恰 −0.5BB、零和加权总均值精确 0、BTN 位置均值最优 / BB 最差）。
     详见 §4.2 进度。
   - **live 真数据采集已开（2026-06-11，api_key 到手）**：smoke 10 手（preopen 10B，blueprint-only）全绿——
     并用真数据校准掉解析侧三个约定假设（commit `f65fcc5`/`b4a7b9f`）：①服务端发显式 `null`（非摊牌
     `shown_cards:null`）；②**短桌手**（6 座只发 4 家，`final_stacks` 键集 = 发牌座位，盲注跳过空座 → driver 满桌
     seeding 的 stacks_start 短桌必错）→ 解析器重映射到 0..n_dealt 紧凑 ring + `actions_ext.contribution_delta`
     重建投入；③`winners.amount` = **净赢**（对账两手实测）→ 回推公式改 net 约定。smoke 9 手 HH 全链路
     0 失败（short_handed=1 正确重映射、VF-1 修正 9/9 生效）。**500 手长跑采集进行中**（vultr nohup，
     `openpoker_hh_live.jsonl` 累积；读数 = `openpoker_hh_aivat --hh-log ... --vf1 ...`）。
   - **首轮 500 手长跑已完成（2026-06-11，blueprint-only 基线臂，preopen 10B，账号 jesse_xu）**：
     HH 共 625 手（runs 1-3）、500/500 捕获 0 丢失、watchdog 0 触发。**真值总账（服务端 final−start 口径，
     不依赖重放）= +348BB，raw +558 ± 640 mbb/g，CI95 [−697, +1812] 跨 0**——方向为正、判不动显著性，
     与 §4.1 功效预算预期一致。决策面：8.6% 兜底（limp structural_gap ~6% + 短桌 seat_mismatch ~2.4%）。
     **发现①（管道修复项，高优先）**：5 手 all-in 大锅（合计 +533BB）U 重放校验失败被 AIVAT 报表剔除 →
     报表均值被拉负（−289）；根因 = 对手 hand-start 栈回推误差（committed_total 的 all_in 缺口）→
     all-in-for-less 退注几何错位。修法 = 解析侧改用 `actions_ext.stack_before/after` 重建对手栈
     （原始数据已在 HH 里，5 手可追溯修复；修完 AIVAT 才能吃 all-in 手 = c_runout 主场）。
     **发现②（策略证据，喂 B/C 搜索臂立项)**：BB 防守系统性过紧——重放探针（200×）证实
     AJo@BB 对 UTG 3.2x+2 跟 = **纯弃**；KTs 顶对好踢脚对 min-check-raise = **纯弃**（seed 1/2/3/42
     同 infoset 一致；min-raise 被 off-tree 映成 ≥0.5pot 档 = 粒度税实锤，seed=99 哈希舍入映到另一节点
     变纯 allin——跨 session 映射不保证一致，设计内）。位置切片 BB −837 mbb/g 最差；见 flop 人数
     2-way +1211 / 3-way −218 / 4-way −534（多人更差，与 ≤3-way 训练边界一致，样本小只看方向）。
     真实尺寸重解正是搜索臂的主场 → 下一步 = 同条件 search-on 500 手对比臂。
   - ~~短桌幻影座映射~~——**已落地（2026-06-11，commit `a60ffda`→`0fe6852`，vultr 测试 + 真 10B
     selftest 全绿 + 25 手 live smoke 实证）**：k 人局映成 6-max 树「UTG 侧前 6−k 位先 fold」的
     真实节点。关键设计：①**不能在空座原位插 fold**——树上 SB/BB 固定 button+1/+2 且必须发盲，
     必须重映环序（真实 BTN/SB/BB → 树座 0/1/2、其余真实玩家按环序占 CO 侧靠后位 = 标准短桌
     位置等价）；②**占座推断只认 table_state.seats[].in_hand**（live 625 手实测：
     `your_turn.players` 有 20 手含「在座未发牌」等局虚座、不可作依据；已弃牌者 1019/1019 留在
     players[]）∪ 已行动座 ∪ {我, button}，本手收到过 table_state 才送 `dealt_seats`（唯一可判门，
     判不清维持旧兜底）。advisor `decide` 两态 lockstep 与 `build_real_auth`（搜索路径）同款接入；
     短桌真栈回推按 dealt ring 重算盲注座（满桌 btn+1/+2 seeding 短桌必错）、非发牌座 placeholder；
     HH 落 `dealt_est` 供事后对 `final_stacks` 键集验证推断质量。测试钉法 = 短桌请求与满桌等价
     请求（前 6−k 位真 fold）**info_set 相等**（两种空座布局 + flop 多街贯通 + HU）。
     **③k=2 = preflop 可映 / postflop 映不进（`0fe6852`→两轮 smoke 40 手 HU 实测校准）**：
     数据钉死 **OpenPoker HU 的盲注按环规则贴（button 发 BB、非 button 发 SB——非标准 HU），
     但行动序是角色序（preflop SB 先、postflop BB 先 = 标准 HU 的角色顺序）**；注意
     `player_action.street` 是**动作后**的街标签（收街动作挂下一街名），裸看会误判行动序。
     advisor：preflop 按 SB→树座 1 / BB(button)→树座 2 / 幻影 [3,4,5,0] 先 fold 映到真实节点
     （smoke2 实测 19 决策 blueprint）；postflop 树是环序（SB 先）表达不了跨街反转 → 显式
     `fallback:short_hu_postflop`（顺序错位本会被重放 seat 校验拦下、smoke2 实测 0 漏网，门只是
     把原因标清楚）。HU 桌是 live 最大出血点（smoke1 87.5% 兜底、−2200 mbb/g 全是被盲注磨）→
     preflop 现走 blueprint。**同根因连修 `openpoker_hh` 解析器**：原「n_dealt==2：button=SB」
     假设令 smoke1 24/25 HU 手转换全挂 → 统一环规则盲注 + **引擎 button 设为 OpenPoker 的 SB
     座做 role-for-role 对齐**（引擎 n=2 标准 HU 的角色序与 OpenPoker 一致）→ 连 postflop 都对
     （smoke2 含打满 5 街 HU 手 15/15 全转换+估计；两轮 smoke 40/40 hands_ok）。占座推断两轮
     40/40 精确（dealt_est == final_stacks 键集）、table_state 每手必达。
   - **仍未做**：c_act v2（需 σ 日志）+ U-fail 5 手的对手栈重建修复（见发现①）+ HU postflop
     若要真打需 n=2 真栈子博弈搜索路径（引擎能表达、6-max 树不能；条件项，HU 桌占比说话）。

## 4. 落地（分步验收）

### 4.1 分步推进顺序 + 量化验收

推进顺序 = **A 前置 → {B 深码 ‖ C 多人} → D 剥削**；贯穿全程的
并行数据采集见 §4.2，不在这个顺序里。短码由同一个引擎当小树特例覆盖，不单列（§2.1）。

| 步 | 做什么 | 放行判据 |
|---|---|---|
| **A**（前置 / 离线） | **两件都要从零做**：① **引擎在各种码深下都正确**：守恒（`payouts()` Σ==0）/ byte-equal / 和 PokerKit 自洽 / 实时解 ≈ 同状态离线 CFR 收敛解，**样例必须含深码中途根 + per-seat 不对称栈 + 多人 side-pot 中途根**（不是只测对称 100BB）；② 「(节点数, 迭代数) → 单决策 wall」回归曲线 + **在真实深码 / 多人目标树上**判定 5/10/20s 时限可行性（缺口①前置；曲线 **vanilla 与 LCFR 各一条**——LCFR 同迭代更接近收敛、直接决定 5s 能否解到有用迭代数） | ① 守恒 + byte-equal + 收敛距离达阈（见下「判据定义」）；② wall 曲线产出 + **深码 / 多人目标树在 5s 预算下能解到有用的迭代数**（按开了 LCFR 的那条曲线判；否则先把时限收到 10–20s 或换更强机器，再开 B/C）。**注意：核心区没有干净的离线 EV 标尺（§0.3），A 验的是“解的是真游戏、而且解得动”，不验“赚多少”——后者留给 live** |
| **B**（深码实时搜索） | **解到终局（不 depth-limit、不要叶子续局值）**+ 把下注菜单收到**单一 {1pot}** 控树（缺口③；§2.1）；接生产 advisor（缺口②，管线重建、**把各家真实栈喂进去**）；限时求解器（缺口①，墙钟 anytime，§2.3，**时限内尽力、不保证收敛**） | **离线（硬性放行）**：终局摊牌值 = 真实 `payouts()`（零叶子近似）+ 守恒 + byte-equal（用于 `search=None` 回归 / 引擎正确性 fixture；限时求解本身改用 seeded-RNG + replay/AIVAT 一致，§2.3）；**advisor 真的喂入 per-seat starting_stacks（不再写死 100BB）**；**含不对称栈样例（如 hero 200BB vs 对手 60BB）的深码守恒 + SPR/all-in 阈值和真实栈一致**；no-panic / 归一；单决策 wall ≤ budget（{1pot} 窄树能在 time_budget 内解到有用迭代数）；**限时解不出来时降级 = check-when-free（能 check 就 check、否则 fold）、不回落 100BB blueprint**。**live（观察项，不作放行）**：见下「live 功效预算」——降为“别打更差”的护栏 |
| **C**（多人 >3） | **实时解 N-way 子树**（§2.2）：解到终局用真实 N-way side-pot `payouts()`、**不要 N-way 叶子值**（**无「建叶子值」这一步**——随放弃 depth-limit 消除）；接生产 + live | 4 人及以上见 flop 有可靠、可解的子树（离线核：守恒 + N-way side-pot payouts 正确）+ live 不退化（同 B，功效不足 → 过 ≠ 兑现多人核心，只兑现“解真游戏 + 不退化”） |
| **D**（后置可选） | 剥削加分项：按置信度门控替换对手 range 的数据源 | **前置**：对手 name 稳定可追踪（§4.2 已验）；数据足够的对手上增量为正（同样受 live 功效限制，复用下面的护栏）；对池中最稳健的对手分项不亏（防被反剥削） |

**步 A 进度（2026-06-09，commit `ce25ee6`→`31cf7ab`，vultr 全绿）**：

- **缺口① LCFR + `time_budget` 本体均已接**（A② 第一杠杆 + 限时打法，见 §3.1/§3.2）：`time_budget` 墙钟 anytime
  默认 `None` 逐 infoset byte-equal（测试 `time_budget_anytime_stops_and_is_valid` 硬证不绑定档 == None 路径）。
- **缺口③ {1pot} 深码菜单已接**（`nlhe_betting_tree::deep_single_pot`，测试 `deep_single_pot_menu_is_single_full_pot` 锁单档契约）。
  **2026-06-09 续：菜单已从「树层函数存在」推进到「接进 `subgame_search` + 生产 advisor」**（commit `0fb41da`，`SubgameSearchConfig.deep_menu`
  + `--search-deep-menu`，菜单不匹配坑已解——deep 路径返回子树自身分布、不对齐 blueprint `legal_abs`；见 §3.2 缺口③）。
- **A① 引擎在各种码深下都正确——离线半已交付**（`subgame.rs` / `state.rs` 新测试）：
  - 守恒（`payouts()` Σ==0）+ 重采样保牌分布 + 不变量检查 在**深码（HU 200BB）/ 不对称（hero 200BB vs 60BB）/
    多人 side-pot 中途根（3 座短码 BB all-in）**经 `build_subtree` 那条路全过；byte-equal 可复现。
  - **SPR / all-in 阈值 = 真实 per-seat 栈**：不对称局短码 all-in 额 = 其 `committed_this_round + stack` 且严格 <
    深码栈（证引擎按真码深算、非「都当 100BB」；§0.3 现场求解处理不对称栈的前提现已硬验）。
  - ⚠ **过程中修了一个真实规则 bug**：`all_in_amount` 在不对称 / 短码 all-in-for-less 线误报非法 all-in（commit `f6c1a26`），
    对称树 byte-equal 确证不破 S1/S2/S3。这是「引擎天生处理不对称栈」从代码核验升级到实测时才暴露的——引擎现在才真的对不对称栈正确。
  - **实时解 ≈ 离线 CFR 收敛（A① 第三判据）= 机制已验**：实时解 vs 离线参考的 per-infoset 平均策略 L1 随迭代
    **单调下降**（早期 stub 桶：HU 0.34→0.13、不对称 0.42→0.18 @300→10000 iter；小可枚举 multiway 0.026→0.0006）。
  - **ε/δ_conv 真阈值已在真 schema-v4 桶表上标定（commit `453c1ba`→`ef071f3`，vultr 全绿；`_measure_convergence_calibration`
    换真桶 + 两菜单）**：**river（单街）干净收敛 → ε≈0.05（mean per-infoset L1，实测 floor 0.029–0.045）/ δ_conv≈1 chip
    （root EV 差，floor 0.01–0.35 chip ≈ 0 vs pot）**，~100–300k 迭代可达、1M 参考已饱和收敛（infoset 数饱和、EV 差→0），
    default 与 {1pot} 两菜单一致。**这就是步 A① 收敛判据要的非退化真阈，A① 离线半收敛判据闭合。**
  - ⚠ **多街到终局树收敛比单街慢得多（真发现，连带细化 A② 结论）**：真桶下 **default-menu turn infoset 爆炸到 29.5 万+**
    （river 跑出 ×46 × 真 river 桶 ~500）、1M 参考欠收敛、不是干净锚；**{1pot} 把 turn 压到 120 节点 / ~5742 infoset、river 压到
    20 节点 / 40 infoset**（生产菜单让多街可解）——但 **{1pot} turn @300k 迭代 mean_l1≈0.15**（仍 ~10× river floor、还在降，要数百万
    迭代才到 0.05）。**即 A② 的「330k 迭代塞进 5s」对多街到终局树是「塞得进」非「已收敛」——5s 给的是 L1≈0.15 的偏收敛解。**
    这偏收敛够不够打 = 步 B 质量问题（非 A① 门槛）；A② 的「时限内解窄树不是瓶颈」细化为「**wall 不是瓶颈，多街收敛深度才是 B 要盯的**」。
- **A② wall（中等树，迭代吞吐）已画到真目标树**（vanilla / LCFR 各一条，单线程 4-core vultr，commit `c9dd154`）：**结论 =
  {1pot} 解到终局让深码 / 中等多人树的迭代吞吐 5s 单线程极宽裕**（深码×多人**角落**另有建树瓶颈，见下条）——
  - HU **500BB** {1pot} 解到终局 = **752 节点**、~13–28 µs/iter；4way **100BB** {1pot} = **45,440 节点**、~12–19 µs/iter；
    5way **60BB** {1pot} = **82,270 节点**、~12–16 µs/iter（对照：HU 200BB default`{0.5,1,2}` 58,160 节点 ~120–200 µs/iter）。
  - **~15 µs/iter → 5s 单线程 ≈ 330k 迭代**，远超收敛所需（[A1-conv] L1 在 10k 迭代已大幅降）。{1pot} 是把树压小的关键——
    深 SPR 下 pot-size 单档把分叉收窄，**迭代吞吐对深码 / 中等多人不是瓶颈**（§6 #2/#4 的核心担忧在这两档上消解）。
  - LCFR wall ≈ vanilla（杠杆是每迭代收敛、非每迭代 wall）。
- **A② 深码×多人叠加大树 wall——已测（commit `31cf7ab`，4-core vultr，`_measure_deep_multiway_wall`），发现真瓶颈 = 单线程建树
  时间**（N-way 3..6 limped flop × 100..500BB、{1pot} 解到终局全梯度）。**修正上一条**：它只测了预建小树的迭代吞吐，**没算建树
  （`build_subtree` DFS）时间**——三个量分开看才对：
  - **内存 / 迭代吞吐都不是瓶颈**：全角落可建，最大格 6-way **500BB = 7.73M 节点（~0.9GB）** < 15M 防护阈（{1pot} 让节点随码深
    **次线性**涨、200BB 后趋平，OOM 担忧消解）；µs/iter 全程 **~11–33**（随树**深**非总节点数，连 7.7M 节点也 ~23）。
  - **建树时间才是 5s 的真约束**（随节点数线性）：3-way ≤40ms / 4-way ≤0.64s / 5-way 0.35→**4.4s** / 6-way 1.4→**20.4s**。扣建树的
    有效求解预算 `(5000−build_ms)/µs_per_iter` → **5s 可行（≥100k 求解迭代）= 3/4-way 全码深 + 5-way ≤400BB + 6-way ≤150BB**；
    **边际 = 5-way 500BB（建树 4.4s → 仅 ~20k 迭代）**；**5s 不可行（建树就 >5s）= 6-way ≥200BB**（8.3–20.4s，6-way 500BB 的
    20.4s **连 20s 预算都超**）。
  - **结论**：深码×多人瓶颈是**单线程建树、非内存非迭代吞吐**——杠杆在 **build 侧**（建树并行化 / 增量 apply-undo 省 GameState
    clone / 更快单核），不是 solve 侧；坐实 §2.2「深码×多人单独设计」+ §6#4 决策点。注：build 单线程 → **§7「按部署机核数外推」
    对 build 不成立**（核数只助已并行的 solve，build 只随单核速度缩放）。
  ⚠ **仍未做**：① {1pot} 单档对**深码 / 多人策略质量**够不够 = B 阶段 / live 问题（与可解性无关）；② build 侧优化（并行 /
  增量 apply）——若要把 6-way 深码拉进 5s，这是必走的一步。

**放行判据定义（必须能判，不留含糊）**：

- **收敛（步 A①）= 距离达阈（已标定，commit `ef071f3`）**：实时解和离线 CFR（≥ M 迭代）的 per-infoset 平均策略 L1 距离均值 < **ε≈0.05**，且 root EV 差 < **δ_conv≈1 chip**（真 schema-v4 桶表 river 子树标定：floor L1 0.03 / EV 差 ≈0；~100–300k 迭代可达）。这是 **CFR 对 CFR 的一致性、不是 best-response**，抓的是实时路径特有的偏差（建树 / resample / 限时截断 / 索引 hero 真桶错位）。**干净锚 = 单街（river）子博弈**（小、1M 参考即饱和收敛）；**多街到终局子树（turn 及更深）收敛慢得多**（真桶下 {1pot} turn @300k 迭代仍 L1≈0.15、default-menu turn infoset 爆炸 1M 参考都欠收敛）——能枚举但定 floor 要数百万迭代，故 **ε 锚在 river**、多街偏收敛是步 B 质量问题不是 A① 门槛。
- **深码 / 多人非劣性 margin δ（步 B / C1）**：放行 = 在功效范围内能排除「劣化超过 δ」（CI 下界 > −δ），不是「CI 跨 0」——后者会把「真的不劣」和「样本不够、测不出」一起判成 PASS（等于接受零假设）。δ 要连同「账号能打到的手数内能分辨的最小 δ」一起给；如果能分辨的 δ 远大于有意义的阈值，就老实标注「这一格 live 在当前手数下判不动」、**放行主要靠离线的结构性正确（解真游戏 + 引擎正确），不靠 live**。

**限时可行性是整张表的前提**（是主目标的核心难点，不只是 §6 的风险）：`subgame.rs` 求解器现在固定迭代、没有 `time_budget`，单决策
wall 从没单独测过；Pluribus 28-core 平均 20s/手——同一棵树在不同机器上 wall 差很多，5s 可行性是部署机核数的函数、不锚定某台机器。**时限问题从一开始就是核心难点**：
步 A② 先在真实深码 / 多人树上回答「5s 档能不能解出来」，B/C 各带一个时限可行性门；任何一格 5s 解不出来，就是「收窄时限范围 /
换更强机器」的决策点，不能默默当作做到了。

**验证（按真实桶读数）**：总 mbb/100 + 按码深桶 + 按见 flop 人数桶 + **栈对称 / 不对称切片**；对比 blueprint-only baseline。
注意双层（× 街）分桶会把本就稀薄的 live 样本再稀释 ~5–25×、每桶功效更差——分桶读数只看方向，强弱判定看总量（加上 AIVAT 降方差后）。

**live 功效预算（必须先算，先统一单位）**：评测口径统一用 **mbb/g**（每手，和 §11.5d 一致）。从 §11.5d 反算
每手 PnL 的 **SD ≈ 1.7 万 mbb**（48k 手、A 臂 CI95 半宽 ≈150 → SE ≈ 76.5 mbb/g → SD = SE·√n）。要判一个真效应
E（mbb/g），需要 n ≈ (1.96·SD/E)²（CI≠0；power 0.8 再约 ×2）：

| 真效应 E | n（CI≠0） | 现实性 |
|---|---:|---|
| 0.30 mbb/g（= +30 mbb/100） | ~1.2×10¹⁰ | 不可能 |
| 30 mbb/g | ~1.2×10⁶ | 账号只够数百–数千手 → 判不动 |
| 100 mbb/g | ~1.1×10⁵ | 仍远超账号可达手数 |

**单位容易混**：+30 mbb/100 = 0.30 mbb/g，和 SE 80–160 mbb/g 不是一个量纲、不能直接代公式。而且「SE 80–160」是 marginal
SE，真正的配对 SE ≈ 54–67（丢掉配对只升 ~20–30%）。结论很硬：**任何合理的 effect 下，live 都需要远超账号能打到的手数 → 强弱
判定不能靠 live：核心区靠「结构性正确（解真游戏、引擎正确，离线能硬证）」当主锚，live 只作“别打更差”的护栏 + 弱方向。
这是核心区唯一诚实的判定组合。**

**两类 √n 压不掉的限制**：
- **系统偏差 ≠ 方差**：OpenPoker 不能配对（同一手不能跑两臂）+ lobby 混合桌 + bot 池漂移 + 单账号分时段 → search-on
  臂和 blueprint-only 臂面对的是不同时段 / 不同对手构成的 field，两者之差里混进了「对手池强度差」这个**不随 n→∞ 消失**的系统偏差。
  所以 live 给的是「带未知符号偏差的方向读数」、不是能判的增量。缓解办法（交错短轮换 / 记每段对手 name 做协变量校正 / Pro 号
  并行同桌）都要标注残余偏差。
- **降方差唯一杠杆 = 多人 AIVAT（缺口⑥，估计器已落地，预期已下修）**：不需要配对（live 唯一可用的降方差手段）、
  也救不了上面的系统偏差。**原「SD 缩到 1/2–1/3 → 手数降到 1/4–1/9」是论文全知设定（双方底牌全可见 + 真值函数），
  必须下修**：HU 生产实测（`docs/aivat_eval.md` §10，全知 Slumbot 日志）推荐修正集 deals+runout 也只有 **1.10× SE**；
  live 多人比 HU 还少 c_deal_opp（对手底牌不可见，摊牌选择性纳入 = selection bias → 永不纳入）。v1 实测见 §3.2 #6
  进度——**结论：AIVAT 收窄 CI 但救不动 live 功效，「强弱判定不能靠 live、核心区靠结构性正确当主锚」的判定组合不变
  （反而更稳）**。裸 mbb/g 先拿方向 → 上 AIVAT 白拿一截 CI 收窄，但别指望它翻显著性。

**fallback 护栏（要先标定、分两类）**：fallback 高的情况 = blueprint 解错游戏的情况 = 短码 / 深码 / 4–5 人 = 主目标所在；
fallback 低的情况是接近 100BB / ≤3-way（搜索作用有限、不是主目标）。所以**不能用「fallback 高就不可解读」一刀切**
（会把最该测的情况判废）。区分：① **baseline 臂**的 off-dist fallback 是**真信号**（正是要测搜索能不能救，应该优先、而不是排除）；
② **search 臂**的「连搜索都解不出来才 fold」才是质量问题。阈值用步 A 的离线锚去量「fallback 决策亏多少 mbb」来标定，不凭感觉拍 40%。

**B/C 的 live 半段 = “别打更差”的护栏，不是“显著更强”才放行**（接上文：live 在账号手数内判不动“显著更强”）：放行只认离线半段；
live 作观察项，护栏 = 「search 臂 fallback < 标定阈值，且 mbb/100 的 CI 下界不显著为负」（能判的“不退化”）。「live 上显著强过
blueprint-only」在当前账号手数下达不到、不作放行条件。

**引擎正确性（解真游戏，离线能硬证）+ live 功效预算 + 多人 AIVAT 是 B/C 立项的硬前提；核心区（深码 / 多人）没有干净的离线
EV 标尺，只有 live 这一个弱 EV 判据 + 结构性正确性论证。**

诚实标注：码深会漂（实测同桌 14–800BB）+ bot 池会漂 + 单账号分时段——live 方差大、迭代慢、不能配对、还带系统偏差。

### 4.2 并行轨道：数据管道

**定位**：挂在现有 blueprint-only live bot 上、**贯穿全程的后台采集**，**不是 §4.1 推进顺序（前置 A / 深码 B /
多人 C …）的前置**——A/B/C 不必等它做完就能开。它给的是频率权重 + 对手数据，但「搜索在哪里是必须的」这个优先级 §0.3 已经
从结构上先验给出（blueprint 只在 100BB/≤3-way 可靠，其余全是搜索必须的区域），所以它不算顺序里的一步、随时可以起。

**做什么**：把 OpenPoker 客户端日志从「只记我方决策点」升级成「**全桌手牌历史（HH）**」。分两件、工作量不同：

- **日志升级（解析并落盘的轻活）——已落地（2026-06-10，commit `e9b5738`）**：driver `--hh-log`（默认开、append 跨重连
  累积）每手 `hand_result` 落一行全桌 HH JSONL：整手 `actions` + `actions_ext`（player_action 原始字段子集
  `contribution_delta`/`stack_before` 等——live 校准字段，比 driver 自跟 committed 更稳，留作 all_in 无 amount 缺口的
  修复材料）+ board + 对手 name（`your_turn.players`）+ 回推 hand-start 真栈 + winners/final_stacks/shown_cards **原样**。
  消息路由抽成 `Session` 类——run_real 与 selftest canned 序列**共用同一条真实路径**，byte-equal 隔离测试才测得到真代码。
  **invariant 测试已配（selftest 场景 5）**：同一 canned 整手序列（到摊牌）挂 / 不挂 `--hh-log` 各跑一遍，advisor
  请求/响应流 + 发出的 action 包**逐字节一致** + HH 行字段齐；请求流 byte-equal ⟹ 同 seed 无状态真 advisor 输出也
  byte-equal。Rust 侧解析（`openpoker_hh.rs`，canned 记录与 python 同一手互为 oracle）+ `openpoker_hh_aivat` 报表
  见 §3.2 #6 进度；canned→HH→报表端到端 vultr 实测对账（+40 op 净赢 = +2000 mbb/g）。
- **覆盖热力图（不是轻活）**：码深桶（14–30 / 30–60 / 60–150 / 150–400 / 400–800BB）× 见 flop 人数
  （2/3/4/5/6）× 街，每格统计 blueprint 有没有可靠策略 / 在 fallback / 在乱映射——要判「可靠 vs Desync vs 乱映射」
  得复用 shadow/off-tree 的分类逻辑，不是单纯 parse。

**两个目的：**
1. **统计真实分布覆盖热力图**：给出**频率先验**。注意频率 ≠ EV 影响——一个罕见但高 EV 的情况（比如深码 SPR 转折点 /
   4-way squeeze 阈值）该先做、但频率低。**（2026-06-10 用户拍板修正：B/C 直接推进、不等这条排序）**——原定「『EV 损失
   × 频率』联合排序决定先攻深码（B）还是多人（C）」不再作为 B/C 的前置：热力图降级为**并行参考**（之后用于校准触发面 /
   读数分桶），B/C 按工程就绪度直接推进，不被数据管道卡住。
2. **攒对手数据**（剥削加分项后置用）+ 验证对手 name 是否稳定可追踪（稳 → 逐个对手建模；不稳 → 按 population 建模）。

**放行判据**：字段齐 / 摊牌名字捕到；advisor 挂 / 不挂 HH 日志 byte-equal；每格 blueprint 缺席率 + fallback 率出图。

## 5. 正确性 smoke / invariants 把关

- **HH 日志**：selftest 不破坏 advisor 路径（挂 / 不挂 byte-equal）；真挂上时字段齐、摊牌 / 名字捕到。
- **限时求解器（墙钟 anytime）**：**做不到 byte-equal、也不要求**（迭代数随机器速度 / 负载变，§2.3），改用
  seeded `RngSource`（局面派生种子）+ replay / AIVAT 一致性来保证可复现（须说明 G1–G3 怎么过）；仍要 no-panic /
  策略归一 / 输出动作合法（不破规则层）。接 LCFR 不破这些：`maybe_lcfr_rescale`（`trainer.rs:352`）同比缩放 regret +
  strategy_sum、是确定性的，所以在**固定迭代的离线路径**（`search=None` 回归 / 步 A 诊断 / 主线 blueprint 训练）上接 LCFR
  后**仍 byte-equal**、归一也不变。
- **接生产回归**：`Contestant.search=None` ⇒ 输出 byte-equal 当前 blueprint（守住已验证的 advisor 薄壳成果，能做到就必须守）；
  search-or-blueprint 分支不破坏影子推进的 lockstep（配测试，不只是声明）。
- **引擎在各种码深下都正确**：补「**深码中途根 + per-seat 不对称栈（如 hero 200BB vs 对手 60BB）+ 多人 side-pot
  中途根**」的 `build_subtree` + subgame solve → `payouts()` per-seat PnL Σ==0，且和 PokerKit / 解到终局口径自洽的
  cross-check（现有守恒测试只覆盖对称 base 态 `state.rs:1370`；不对称栈 + 多人 side-pot 中途根 + 深码中途根
  解到终局都没单独验过；`tests/side_pots.rs` 已覆盖不等栈守恒，但走的是直接 apply、不是 `build_subtree`，可以当 oracle 复用）。不对称栈
  是码深维度里最常见的形态、又是「现场求解天生处理、预计算表做不到」（§2.1）的关键前提，**必须验过才能当前提用**。小树守恒是
  这套 fixture 的*便宜子集*、顺带就覆盖了，但 fixture 的目标是**深码 / 多人正确**。
- **求解核均衡正确性**：另外锚到现有的 Kuhn/Leduc exploitability 真值 + 小子树和 PokerKit 口径自洽——核心区判强弱不依赖
  NLHE best-response（不用新写）。LCFR 接进子树解后同样过这套 Kuhn/Leduc 锚，并验它**收敛方向与 vanilla 一致、达同精度更快**
  （LCFR 在主线 blueprint 已是成熟变体，但子树解是新接线，得在小博弈上确认加速真出现、period 粒度选对了）。
- **正确性优先**（CLAUDE.md）：搜索接进实战前，off-stack 树形要和真实 `GameState` 的 SPR / all-in 阈值一致（含不对称栈）。

## 6. 已知风险（诚实）

1. **搜索 off-distribution 的价值 = 有理由相信、但还没证**：必须在 off-stack / 多人上对**外部对手**证它（步 B/C 的 live 半）。
   它的离线前提 = **引擎正确（解真游戏）+ 结构性论证（blueprint 在 off-100BB / 4+way 解错游戏，能硬证）**，也就是**步 A**；
   live 这条锚依赖还没算的功效预算。在 A 和功效预算落地前，B/C 的 **live 半**是 0% 可执行、不是“待跑”；但 A 的**离线半**
   （引擎正确 + wall/时限）现在就能开。
2. **深码 / 多人「时限内把窄树解到终局」= 真正难做的部分**：100BB blueprint 的值不能迁移，深码下也**没有可靠的离线
   叶子值真值可重建**——所以本方案**不重建叶子值、也不 depth-limit，改直接解到终局**（缺口③④），终局用真实 `payouts()`，
   控树靠把下注菜单收到单一 {1pot}（短码可放宽）。难点因此从「重建叶子值」转成「**big 窄树在 time_budget 内解到有用迭代数**」
   （anytime + LCFR；5s 能否解动是部署机核数的函数，§4 步 A② / §6 #4）。§11.5d 的 depth-limit 读数（unbiased 只是 wash、
   biased 更偏离）正是放弃 depth-limit 的旁证——结论不是「叶子值要重建」，而是「这条路本身不该走」。
3. **多人 >3 没有免费的好处**：实时解 N-way 吃时限 / 难度——窄树（{1pot}）在多人下仍大，5s 内解到有用迭代数是核数的函数（§2.3 / §4 步 A②）。
4. **限时（深码×多人已部分实测，commit `31cf7ab`）**：深码×多人树 5s 可解性已测——**真约束是单线程**建树**时间，非迭代吞吐、
   非内存**（6-way 500BB 建树 20.4s ≫ 5s；5s 可行前沿 = 3/4-way 全码深 + 5-way ≤400BB + 6-way ≤150BB，见 §4.1 A② 续）。这**修正**了
   「wall 是核数的函数」：建树**单线程** → wall 的建树部分**不随核数缩放**、只随单核速度（求解部分若并行才吃核数）；6-way 深码要进 5s
   靠 **build 侧优化**（并行 / 增量 apply），换核数无用。仍要在目标部署机上实测、不拿测试机数当部署结论。限时求解用墙钟 anytime
   （解到时限就停），它和 byte-equal 互斥——限时求解做不到 byte-equal 也不强求（§2.3），靠 seeded-RNG + replay/AIVAT 可复现。
   **5s 解不出来 → 收时限范围 / 换更强单核 / build 优化**是 §4.1 步 A② / B / C 的明确决策点，不能默默当作做到了。
5. **验证闭环慢且脆**：放弃自对弈真值后，核心区离线只剩**结构性正确**（守恒 / byte-equal / PokerKit / 实时解≈离线CFR /
   解真游戏的 SPR-all-in 一致），EV 量级只剩 OpenPoker live（不能配对、功效低）。别指望有便宜的离线判别器——「把高频对手聚成
   粗粒度 HUD bot 做离线 A/B」既没实现、真做也会引新 confound（HUD ≠ 真 bot / 在 100BB 对称模拟器里测 = 测错情况）。
6. **剥削加分项上线时**：best-response 针对错的模型会被反剥削；6-max 没有两人零和那种安全网。置信度加权要用带可剥削度上界的
   形式化（按 per-infoset 观测数加权 / 在求解层用 p 参数），**不要**对两个最终策略做线性插值（凸组合不保 EV）。
7. **算力预算 / 排期未定**：缺口①②③④⑤⑥都是非平凡的新建项，还没有人天 / wall / $ 估算。实时搜索的大样本评测（1M 手、要更多核）
   得起**按需高性能机器**，而起这类机器本身就是硬前提：按 `feedback_high_perf_host_on_demand`，开工前要给对应步列 wall + $、
   向用户报预算后才起机。**这是 §4 任何要起按需高性能机器的步（B/C/D 评测、大样本 live）的立项硬前提、不只是风险**；手头小机器
   能跑的离线锚（步 A）不受这个约束。

## 7. 算力（不锚定具体机器，按需求分档）

- **开发 / 离线小活（小机器够用）**：深码 ≤3-way 子博弈的正确性测试、wall 曲线初测、离线小样本。手头的测试机就能跑。
- **大活 / 评测（要更多核 + 更大内存）**：稳定 P95、多人多 board 采样、1M 手评测。按需申请更强机器，
  规格按 `feedback_high_perf_host_on_demand` 向用户报预算后起。
- **实时部署机是独立变量**：时限可行性（5s）按部署机的核数算、不是测试机的——**别拿测试机的 wall 当部署结论**，要在目标部署机上实测
  （§2.3 / §6 #4）；部署机可能比开发 / 测试机强得多。
- **代码同步 / 测试流程**：代码改动 push → 远端 fetch/reset；cargo test 走远端机（本机只 build / fmt / clippy，结果不可信）。

## 8. 排期（待补）

- **优先级**：引擎在各种码深下都正确（步 A①，离线小机器可跑）→ 限时求解器（含把 LCFR 接进子树解）+ wall 曲线 + **深码 / 多人目标树**
  5s 时限可行性判定（缺口①，步 A②）→ 生产接线（缺口②，含喂真栈）→ 深码窄树解到终局（缺口③ = {1pot} 菜单 + 解到终局 +
  时限求解，步 B；**非「叶子值重建」**）/ 多人（缺口④ N-way 树，步 C；解到终局用真实 N-way `payouts()`、**无 N-way 叶子值**）
  → live（含多人 AIVAT 缺口⑥降方差）。
- **起按需高性能机器前，要先给该步列 wall + $、向用户报预算再起机**（§6 #7 硬前提）。
- 各步 wall + $ 估算 = 立项前要补齐的（§6 #7）。

**步 A 离线半基本闭（2026-06-09，commit `ce25ee6`→`31cf7ab`，vultr 全绿，见 §4.1「步 A 进度」）**：
A①引擎在深码 / 不对称 / 多人 side-pot 上经 `build_subtree` 守恒 + SPR/all-in 阈值 = 真 per-seat 栈 + byte-equal 实测验过
（过程中修了 LA-007 一个真实规则 bug，对称树 byte-equal 确证不破 S1/S2/S3）；实时解≈离线CFR 收敛**机制**已验（L1 随迭代单调降）。
**A②/A③（本轮新落地）**：缺口① LCFR + `time_budget` 墙钟 anytime 本体已接（默认 None byte-equal）；缺口③ `deep_single_pot`
{1pot} 菜单已接；**A② wall 已画到真目标树并下硬结论**——{1pot} 解到终局把 HU 500BB（752 节点）/ 4way 100BB（45k）/ 5way 60BB
（82k）压到 ~12–28 µs/iter，5s 单线程 ≈ 330k 迭代，**迭代吞吐对深码 / 中等多人不是瓶颈**；**深码×多人叠加大树也已测
（commit `31cf7ab`）——真瓶颈是单线程**建树**时间（非内存非迭代吞吐）：6-way 500BB 建树 20.4s ≫ 5s，5s 可行前沿 = 3/4-way 全
码深 + 5-way ≤400BB + 6-way ≤150BB，杠杆在 build 侧（见 §4.1 A② 续）**。**ε/δ_conv 真阈值已标定**
（`_measure_convergence_calibration` 换真 schema-v4 桶表 + default/{1pot} 两菜单，commit `453c1ba`→`ef071f3`）：**river 子树 →
ε≈0.05 / δ_conv≈1 chip**，A① 收敛判据闭合；连带细化 A②——**多街到终局树收敛比单街慢，5s/330k 迭代是「塞得进」非「已收敛」
（{1pot} turn @300k 仍 L1≈0.15）**，多街收敛深度留作步 B 质量项。

**下一步**：① **ε/δ_conv 真阈值——已完成（commit `453c1ba`→`ef071f3`，vultr 全绿）**：真 schema-v4 桶表 river 子树标定出
**ε≈0.05 / δ_conv≈1 chip**（~100–300k 迭代可达），A① 收敛判据闭合（见 §4.1「步 A 进度」）；连带发现 **多街到终局树收敛比单街慢
（{1pot} 把 turn 压到 120 节点 /~5742 infoset 但 @300k 仍 L1≈0.15）→ A② 的「330k 塞进 5s」是「塞得进」非「已收敛」**，多街收敛
深度留作步 B 质量项。② **深码×多人叠加大树 wall——已测（commit `31cf7ab`，`_measure_deep_multiway_wall`，vultr 全绿）**：全角落
可建（最大 6-way 500BB = 7.73M 节点 < 1GB、**内存不是瓶颈**）、迭代吞吐全程 ~11–33 µs/iter（**也不是瓶颈**）；**真瓶颈 = 单线程
建树时间**——5s 可行前沿 = 3/4-way 全码深 + 5-way ≤400BB + 6-way ≤150BB；**6-way ≥200BB 建树就 >5s**（20.4s@500BB 连 20s 都
超），杠杆在 **build 侧**（建树并行 / 增量 apply），**非 solve；§7「按核数外推」对 build 不成立**（build 单线程，只随单核速度缩放）。
残留 = build 侧优化（若要把 6-way 深码拉进 5s 必走）。③ **缺口② 生产 advisor 重建——已落地（2026-06-09，commit `7413da2`，
vultr 全绿）**：`openpoker_advisor` Request 加 optional `stacks[6]` + `decide()` 分派 `subgame_search`（`build_real_auth` 真栈
重放 + `GameState::inject_external_cards` 注入真牌）+ outgoing 按真码深 + 解不出来 check-when-free 不回落 + python driver 回推 hand-start
真栈送 `stacks`；守 `search=None` / preflop / 未触发 byte-equal（测试钉死，§3.2 缺口②）。**v1 边界（当时）**：node_id 仍靠 100BB 影子
（off-stack all-in 线失同步 → 降级 check-when-free；深码无 all-in 的 on-tree-preflop 线可搜）——**边界① 已由下面 ⑤ 收口（2026-06-10）**；
子树用 blueprint 菜单（深码 {1pot} = 缺口③）。
④ **转 B/C（下一步真正的推进）**：**2026-06-10 用户拍板：B/C 直接推进、不等「EV 损失 × 频率」排序**（原定由 §4.2
热力图 + 功效预算决定先攻哪格；现热力图降为并行参考，不卡 B/C）。可解性边界仍是：B 深码 ≤3-way 全可解；C 多人
≤5-way≤400BB / 6-way≤150BB 可解，仅 **6-way 深码角落建树 >5s、待 build 优化**。缺口② 落地后**生产 bot 已能在真码深
局面解真游戏**；**缺口③ 也已落地（2026-06-09，commit `0fb41da`，vultr 全绿）——`--search-deep-menu` 把子树菜单收到单一
{1pot} 接进 advisor，深码 {1pot} 解到终局端到端跑通**（菜单解耦 + 不匹配坑已解，见 §3.2 缺口③）。
⑤ **off-stack all-in 线 node_id 脱影子——已落地（2026-06-10，commit `5c43dd8`/`be4a389`，vultr 全绿 486/0，v1 边界① 收口）**：
`subgame_search_unanchored` + advisor `decide_search_unanchored`（机制 / 根因 / 残留边界见 §3.2 缺口② v1 边界①）。
覆盖增量 = off-stack all-in 线 + **真实 4+way 触发点**（脱锚子树关 width_redirect、解真游戏宽度）+ **limp 多人池触发点**
（真实分布最常见形态，原 S5 结构 gap 在触发区收口）；range 先验退 uniform 是已知代价。
⑥ **多人 AIVAT（缺口⑥）+ deep_menu 两细化——已落地（2026-06-10，commit `2c29df4`→`e90e0c5`，vultr 全绿 493/0）**：
(a) `aivat_multiway` 估计器（live 可见信息口径，AIVAT = U − (c_deal_us + c_runout)，N-way side pot 走权威 `payouts()`；
6000 手自对弈配对无偏闸门过、SE 缩减 1.33×）——**live 功效预算的诚实更新：AIVAT 收窄 CI 但救不动 live 功效**
（原「SD 缩到 1/2–1/3」系论文全知设定，已下修，§3.2 #6 / §4.1）；(b) deep_menu SPR + 人数自适应菜单
（`deep_menu_for`：浅 ≤4×pot 且 ≤3 Active → {0.5,1}，实测 6-way 边界宽档 558k 节点 = 20.6× 爆炸 → 人数闸）+
`AllPostflop` mid-round 真实动作导航（`within_round_real`，§3.2 缺口③ v2）。
⑦ **within-round solve 缓存——已落地（2026-06-10，commit `c0bce25`，vultr 全绿 497/0）**：advisor 常驻进程按
solve 全部输入做 key（`SubgameSolveCache`，solve 边界现算、不从请求层推导），同手同街第二决策命中 → 复用 solve
只重导航——恢复 §6 #2「每轮恰好一个 solve」一致性（`time_budget` anytime 下逐决策重解会停在不同迭代数 = 读不同
均衡）、mid-round 决策 wall ≈ 0、首决策可放心用满 time_budget（机制 / key 覆盖面 / byte-equal 守护见 §3.2 缺口①
进度末条）。
⑧ **live_traversers——已落地（2026-06-10，commit `7546c0b`，vultr 全绿 500/0，限时杠杆②）**：subgame solve 的
traverser 只轮**子树根仍 Active** 的座（弃牌 / all-in 座零决策节点 = 零学习迭代；fold 剩 2-3 人的最常见局面浪费
50-67% → 同 wall 有效迭代 ×2-3，与 LCFR 正交、与 ⑦ 叠加）。默认 false 全部基线 byte-equal；旗进 solve 缓存 key；
advisor `--search-live-traversers`（机制 / 测试见 §3.2 缺口① 进度末条）。
⑨ **live 半段数据管道（HH 日志升级 + HH→AIVAT 解析报表 + VF-1 小表）——已落地（2026-06-10，commit `e9b5738`，
vultr 模块/集成/真 1B selftest 全绿 + 端到端对账）**：driver `--hh-log` 全桌 HH JSONL（actions_ext / names / 回推真栈 /
hand_result 原样；`Session` 抽路由，selftest 场景 5 钉死挂/不挂 advisor 请求流 byte-equal）→
`openpoker_hh::hh_to_multiway_input`（×scale + 动作转换重放 + loud Err）→ 新 bin `openpoker_hh_aivat`（raw/AIVAT
mean±SE mbb/g）；VF-1 = `Vf1DealTable` + 新 bin `vf1_deal_table_build`，**正式表 = preopen 10B 自对弈 1M 手**
（vultr `artifacts/vf1_deal_table_preopen10b.json`，0 desync、1014/1014 全覆盖、数值合理性抽查过，§3.2 #6 / §4.2）。
接下来 = **live 实跑采集已开（2026-06-11 api_key 到手：smoke 10 手全绿 + 解析约定 live 校准 `f65fcc5`/`b4a7b9f`
（短桌重映射 / net 结算 / null 容忍）+ 500 手长跑进行中，见 §3.2 #6）** + 覆盖热力图（§4.2 非轻活半段，分类逻辑复用 shadow/off-tree）
+ 脱锚 range 细化（部分前缀 reach / 对手数据，后置；**设计探索已记 `unanchored_range_design_2026_06_10.md`**——
三档方案 + AllIn-tag 坑 + 实现要点）+ 6-way 深码 build 侧优化（条件项）。**

---

**相关文档**：实时搜索的底层机制与历史实验（建树 / range / 叶子值 / off-tree 映射 / 已跑 A/B / §11.5d 负判决）见
`docs/temp/realtime_search_design_2026_06_03.md`；OpenPoker 客户端与协议见
`docs/temp/openpoker_client_design_2026_06_02.md`。

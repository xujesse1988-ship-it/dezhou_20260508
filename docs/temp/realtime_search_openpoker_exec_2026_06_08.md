# 执行文档：6-max NLHE 真实对抗场景的实时搜索 bot

> 2026-06-08 / 分支 `6max`。本文是 6-max 实时搜索的**当前目标与落地计划**。OpenPoker 是**验证场**，
> 不是目标本身；强弱只认真实对局。复用的底层机制（建树器 / off-tree 映射 / CFR 子博弈解）见文末「相关文档」。
>
> 结构：§0 定义（目标/洞察/决策）→ §1 架构 → §2 三轴拆问题 → §3 现状/缺口 → §4 落地（分步验收 + 并行数据轨道）
> → §5 守门 → §6 风险 → §7 算力。

## 0. 目标 · 核心洞察 · 决策记录

### 0.1 目标（北极星）

**在真实 6-max 无限注德州对抗分布下，尽量打出接近最优的牌。** 真实分布的三条硬轴：

1. **人数**：见 flop 可能 2 人，也可能 4–5 人。现有 blueprint 只在 **≤3-way** 上训练
   （`width_redirect` N=3），4+way 无忠实策略。
2. **码深**：各家起始码深**不等且连续**，可能 14BB 短码、也可能 500BB 深码。blueprint 只在
   **100BB 对称**码深上训练——`openpoker_advisor.rs:24` 自承「码深 ≠ 100BB：solver 树/SPR 都按
   100BB 解」，即任意真实码深都被强行当 100BB 解。off-tree 只翻译下注**尺寸**，修不了树形 / SPR。
3. **限时**：每步决策可配时限 5/10/20s（深码/多人树能否在 5s 内解动是未证假设，见 §2.3）。

「最优」在多人一般和里无 Nash 保证，实践含义 = **在真实状态上解出的一个稳健、接近不动点的策略**。

### 0.2 核心洞察（架构支点）

**blueprint 是预计算的固定切片，覆盖不了「码深 × 人数」这个连续空间。** 你不可能为每个码深
（14…800BB）× 每种人数 × 每种非对称栈组合都预存一张表。**唯一能覆盖这个连续空间的，是在决策时
按真实状态（真码深、真人数、真 board）现场建子树、现场求解 = 实时子博弈搜索。** blueprint 退居
二线，当**先验 + 叶子续局值 + 兜底**，不当最终答案。这个论证是独立的工程结论，不依赖任何对战实测。

主干**复用现有 CFR 子博弈求解器**（`subgame.rs`：`EsMccfrTrainer` + `build_subtree`），
**不引入新求解核**。要补的只有 §3 缺口列表。

### 0.3 实时搜索的适用边界（为什么它是主干）

实时搜索的价值，**正比于真实状态偏离 blueprint 训练分布的程度**：

- **blueprint 已练透的格子（100BB、≤3 人）**：搜索锦上添花有限——这没关系，**这不是北极星**。
  已有实测（设计 §11.5d）表明，在这个格子里、用「开搜索 vs 同一 blueprint 当对手」的自对弈方式
  **结构上测不出绝对强度**（blueprint 越强、当对手就越硬，搜索的任何近似偏移就亏越多，与"加搜索是否
  更强"无关）。所以**绝对强弱只能靠外部对手判**，不靠自对弈探针——这是关于*测量方法*的结论。
- **blueprint 缺席的格子（短码 / 深码 / 4–5 人）**：blueprint **根本没有答案**，搜索不是"打不打得过
  blueprint"，而是"除了现解，没别的东西能出像样策略"。**北极星正活在这里。**

**短码（尤 HU）是唯一能离线证伪的格子，但它的离线锚要先建。** 短码树小 → 能解到终局（不依赖叶子值
近似），更关键的是树小到可以**离线 CFR 跑到收敛当真值**。于是短码上 OpenPoker **之前**就能离线验两件事：

- **① 引擎正确性**：实时解 ≈ 该状态离线 CFR 收敛解。这只需用同一 CFR 核多迭代当参照、比策略/EV 距离，
  抓的是实时路径独有的偏差（建树 / resample / 限时截断 / 索引 hero 真桶错位），**现有能力即可做**。求解核
  本身的正确性另锚到既有 Kuhn/Leduc exploitability 真值 + 小子树对 PokerKit 口径自洽（§5）。
- **② 有肉可吃**：量 100BB blueprint 在短码状态的可被剥削度。HU 恢复两人零和，exploitability 是干净标量，
  但**当前代码没有 NLHE 的 best-response**（`best_response.rs` 只 impl Kuhn/Leduc），这是一件须新写的离线
  工具（§3.2 缺口⑥），不是现成能力。它度量的是「把 100BB 表当成该 HU 短码子博弈策略时被榨多少」，
  **不**等于在 6-max 原生分布里恢复了不可剥削=最优。

这个离线锚跟被否的"自对弈探针"本质不同——前者比对真实小树的**真值**，后者比对 blueprint 当 field。
但它**随 3-way → 多人 → 深码迅速变弱**：4+way 无 blueprint 忠实续局/叶子值、深码叶子值 off-distribution，
这两块回到只能 OpenPoker live（且 live 不可配对、功效低，见 §4）。**这正是"短码/HU 先做"的理由，**
也意味着北极星的多人/深码核心区在相当长时间内只有 live 这一个弱判据，须诚实标注、配功效预算。

**因此 §0.1 的「接近最优」是分阶段可判的，须如此理解：** 它对全北极星是**目标方向**；但「接近」要可判断须有标尺，
而标尺只在短码（尤 HU）干净（exploitability，须先建 best-response）。故 **v1 把目标做实在短码边缘**——
这是当前唯一能量化兑现的承诺、可证伪的第一仗；**多人 / 深码核心暂无标尺，是方向而非 v1 可验收的强声明**，
待离线锚工具与外部对手功效预算到位后才逐格转成可验收目标。

### 0.4 剥削 = 可选外挂（后置）

同一个子博弈求解器，对手 range/策略默认来自 blueprint（稳健）；**当对某对手攒够可靠数据时，把那部分
输入替换成实测对手倾向**，就从「稳健」平滑切到「剥削」。一套引擎、两个数据源。稳健的蛋糕先烤好，
剥削是糖霜，整体**后置**（上线注意事项见 §6）。

> 注意区分：这里的「剥削」专指**对手建模**（换对手 range 数据源）。它与设计 §11.4 的 biased 叶子续局机制
> 是两回事——后者在设计 §11.5d 的 search-vs-self 配对探针下，C−B 跨两个 blueprint 符号一致为负
> （−64 / −92 mbb/g，但两条 CI 都跨 0、不单独显著），而该探针只能测「偏离 blueprint」、测不了对外部对手的
> 绝对强弱。故**本方案默认不启用 biased 叶子是保守选择**（在偏离最小化下合理）、非「实锤绝对有害」；如重启
> 须在外部对手上单独消融，深码还须先按真实码深重标叶子值（§2.1 / §4 步C）。

### 0.5 决策记录（2026-06-08 用户拍板）

1. 目标 = **真实分布下接近最优**（多人 >3 / 码深 14–800BB 不等 / 限时 5–20s）。
2. 主干 = **真实状态上的实时子博弈搜索**（复用 CFR 子博弈解），blueprint 当先验 + 兜底 + 叶子值。
3. 验证锚在 **off-stack / 多人 × 外部对手（OpenPoker）**，短码格子另有**离线真值锚**（须先建 NLHE best-response）。
4. 剥削 = 可选外挂（同引擎换对手数据源），置信度门控，**后置**；biased 叶子机制默认弃用。

## 1. 架构（实时搜索为主干，剥削为可选外挂）

```
每决策：
  1. 读真实状态：真码深（各家不等）/ 真在场人数 / 真 board / 真下注历史
  2. 在真实状态上建子树（build_subtree，真 SPR / 真人数）
  3. 取对手 range/续局值：默认来自 blueprint；有可靠对手数据时替换为实测（剥削外挂，后置）
  4. 在预算内求解子树（按预算静态选树粒度，见 §2.3）；解不动则降级——但降级须留在真实状态上
     （收窄菜单 / 加深 depth-limit 后的真实树粗解 / 稳健启发式），blueprint 仅在其训练格子内才作兜底
  5. 返回动作（off-tree 尺寸经 map_off_tree 翻译）
持续：
  记全桌动作 + 摊牌（HH 日志）→ ①量真实分布覆盖热力图 ②攒对手数据（外挂用）
```

主干不写新求解核：root 在真实状态重发隐藏牌、对全 range 求解、事后索引 hero 真实桶
（`subgame.rs:416` root / `:331` query_at）。决策环里真正的 search-or-blueprint 分支在
`blueprint_advisor.rs:498-539`（函数 `play_cross_abstraction_hand:410`，靠 `Contestant.search=None`
守 byte-equal 旧行为）。

**「接搜索」是管线重建，不是改一行。** 生产入口 `openpoker_advisor.rs` 是无状态单决策重放模型，
与 `play_cross_abstraction_hand`（整手自对弈 harness）结构不同步；要接进生产须：
①协议/解析层捕获各家真实栈（现 `Request` 结构 `:84-96` 无 per-seat stack 字段 → 真码深从入口就丢了）；
②`decide()` 里新建 search 分派；③重写 outgoing（现是「100BB 解 → ÷scale → clamp 进真实区间」，
非真码深尺寸）。在此之前，生产 bot 在非 100BB 局**静默解错游戏**（短码 all-in 被当小注、无 fallback 标记），
这正是 §0.1 轴 2 要修的病。

## 2. 三条真实轴 + 各自打法

### 2.1 码深（搜索的主场）

引擎层**已天生处理任意/非对称码深**——这是短码先做的承重前提，代码核验成立：betting tree 只按
`AbstractActionTag` 分叉、不存金额（`nlhe_betting_tree.rs:31-42`），bet 尺寸运行时按真实 pot/stack 现算
（`action.rs:368-411`），all-in 点 = 真实 per-seat `cap = committed + stack`（`state.rs:438/476`），
`build_subtree` 从真实中途态展开、subgame 全程用 `root_state.config().clone()`（`subgame.rs:1020/1036/1047`），
`TableConfig.starting_stacks` 是 per-seat `Vec` 可不等。**「现解天生处理非对称栈」在机制层成立、预计算表做不到。**
但这只是「引擎 CAN」，生产 advisor 尚未喂入真实栈（见 §1 / §3.2 缺口②）。

- **短码（14–30BB）= 最干净、最该先做**：树小 → 能**解到终局**（无须叶子值近似、无须 depth-limit）。
  而 100BB blueprint 在 25BB 上**本来就是错的**：SPR、all-in 阈值是树几何错；**range 先验同样错**——
  blueprint info set 把 `stack_bucket` 硬编码 0（`nlhe.rs:123`）、equity 桶码深无关，沿其累乘得的 per-seat
  range 是「100BB 假设下的 range」。短码解到终局只绕开**树几何错 + 叶子值近似**这两层；**桶层 stack-agnostic
  的策略分辨率限制仍在**，short-stack MVP 的 range 要么用非-blueprint 短码 range、要么承认这层近似并在
  步 A 的离线 exploitability 里量它有多大。这是确定性最高、可证伪的第一个胜仗。
- **深码（150–800BB）= 真难处**：树大 → 必须 depth-limit + 叶子续局值，而 100BB 训出的叶子值在
  500BB 上 off-distribution（脏）。设计 §11.5d 在 100BB 抽象 + all-postflop 下测得：depth-limit unbiased
  vs 解到终局是 **wash**（既不明显 help 也不明显 hurt）、biased 续局值比 unbiased 更偏离 blueprint
  （C−B 符号一致为负、CI 跨 0）——注意这是 search-vs-self 探针的*相对偏离*读数，不是对外部对手的绝对强弱。
  即便如此，depth-limit=wash 本身就说明「深码靠 depth-limit + 叶子值拿净增益」证据偏弱：深码必须把叶子值
  **按真实码深重建/重标**（不能复用 100BB 表）、别照搬经验系数，且其收益须到外部对手上才算数。放 §4 后段。
- **非对称栈**：现解天生处理（建真树即可），预计算表做不到。

### 2.2 人数 >3（最硬的结构题）

- `width_redirect`（`nlhe_betting_tree.rs:101`、断言 `:379`）= 把第 N+1 个进场者收成 squeeze/fold 的
  **多路收口机制**（N=3 甜点），让 ≤3-way 子树可枚举。它**不是**「放开就能多路」的开关——放开
  （`WIDTH_REDIRECT_OFF`）会让 blueprint 落到它根本没训过、无忠实续局/叶子值的区域，不能直接硬解。
- 两条路（须立项选）：
  - **(甲) 扩抽象/训练到 4-way**：N=4 已实测 = **1.445B infoset / 48 GiB**（≈6.3× N=3 的 8.04GiB@200，
    需 ≥56GiB 机；同 1B 预算仅 ~9% 覆盖 → 大概率 backfire）；≥5-way 仍未覆盖。
  - **(乙) 实时解 N-way 子树**：**短码多人解到终局时摊牌值由真实 `GameState::payouts()` 的 N-way side-pot
    showdown 精确给出，不需 equity 估计**（`multiway_equity_mc` 仅在 `tools/multiway_equity_probe.rs:197` 的
    私有离线 fn，生产不可调用）。`multiway_equity_mc` 只在**深码 depth-limit 叶子**才相关，且语义更粗
    （叶子后假设全员摊牌、无后续下注），须新接线 + 评估方差/速度/side-pot 语义——是缺口不是现成件。
    深码多人还需 **N-way 叶子续局值**（blueprint 给不了），真硬骨头。
- **短码多人好啃限定在 ≤3-way**：树小可解到终局对短码成立，但 (i) 当前桶是 HU 单对手 equity 桶、S3 只验证过
  ≤3-way 可复用，4+way 桶重排未量；(ii) N-way range 采样 + card removal 成本随座位线性升。4+way 须先补
  S3 多人桶验证（OCHS-multiway / hist 重标边界）。
- **深码多人最难**，须单独出设计。

### 2.3 限时（必做地基）

- 现状：`SubgameSearchConfig`（`subgame.rs:650`）有 8 个字段（含 `depth_limit` / `biased_leaf` 等机制开关），
  但**无 `time_budget`**；求解循环 `:1068` 是固定 `for _ in 0..iterations`、无墙钟中断——不是 anytime 求解器。
  单决策 wall **从未隔离测过**（现有数据全是整臂）。

- **墙钟中断与 byte-equal 不变量直接冲突，故采静态预算选粒度。** `invariants.md §2` 把 byte-equal 复现列为
  发现算法 bug 的最低门槛，§5 也要求搜索可复现；而「解到墙钟用完返回当前策略」让迭代数取决于机器速度/负载，
  同一 `(state,seed)` 产出不同策略，**按构造不可能 byte-equal**（这与 off-tree map 当初被强制做成纯函数、
  用局面派生种子的理由正面打架）。因此**预算→粒度做成建树前的静态决策、保固定迭代数**：先离线产出
  「(节点数, 迭代数) → 单决策 wall」回归曲线（= §4 步 A 的 wall 量化），据此把 5/10/20s 预算
  **反解成 limit_street + 菜单档数 + 迭代上限**。真正的运行时墙钟中断只在「树规模在线高度可变、静态标定不可得」时
  作为带可复现性豁免的退路（须显式说明 G1–G3/replay/AIVAT 一致性怎么过）。**目前「按预算选树粒度」
  尚无 budget→粒度模型，是待立项研究项，那条回归曲线是它的前提。**

- **降级必须留在真实状态上，但真实状态的兜底阶梯目前几乎是空的。** blueprint 只在它训练过的格子
  （≤3-way、近 100BB）才是有效兜底；在它缺席的格子回落 blueprint = 回落到一个**解错了游戏**的策略。
  方案承诺的降级阶梯「真实树粗解 → 多人 equity / push-fold 启发式 → blueprint 最后无奈项」中：收窄菜单会撞
  abstraction↔blueprint 桶/叶子值耦合（`subgame.rs:126`）、加深 depth-limit 落回 §11.5d 的 off-distribution 叶子值、
  **中间的稳健启发式层（多人 equity / push-fold）在 `src/` 内根本不存在**。须把「off-tree 格子的真实状态兜底」
  当缺口①的必要子项一起做，否则限时解不动时只剩回落 blueprint。

- 算力参照：Pluribus 实时搜索 28-core/128GB **平均 ~20s/手**（设计 §8）；vultr 4-core 设计文档只敢承诺
  **P95 < 30s**，已高于 5–20s 目标轴下界。**5s 内深码/多人能否解动须先实测**——很可能须把目标轴收到 10–20s
  或上多核机。

## 3. 现有底座 / 缺口

### 3.1 已落地可复用

| 构件 | `file:line` | 状态 |
|---|---|---|
| 真实状态建子树 | `nlhe_betting_tree.rs:271 build_subtree` / `:309 depth_limited` | ✅ 可接任意中途 `GameState` 作 root |
| CFR 子博弈求解 | `subgame.rs:650 SubgameSearchConfig` / `:1066 EsMccfrTrainer` | ✅ 解到终局或 depth-limit；超 cap / 未访问优雅回落 |
| off-tree 尺寸映射 | `action.rs:476 map_off_tree`（pseudo-harmonic randomized rounding） | ✅ 任意下注尺寸；纯函数可复现 |
| blueprint 加载 / 兜底 / fallback 统计 | `nlhe_dense_trainer.rs` / `openpoker_advisor.rs:119 safe_fallback` | ✅ 冷启动 / 失败退路 |
| 多人 equity | `tools/multiway_equity_probe.rs:197 multiway_equity_mc` | 🟡 离线私有 fn，未接生产、未做 N-way 叶子值表（见缺口④） |

### 3.2 缺口（须新写 / 改造）

1. **限时求解器**：先离线产出「(节点数, 迭代数) → 单决策 wall」回归曲线，据此实现**按预算静态选树粒度**
   （limit_street + 菜单档数 + 迭代上限，保固定迭代 / byte-equal）；off-tree 格子的兜底用稳健启发式而非 blueprint
   （该启发式层现不存在，须一并建）。墙钟中断仅作豁免退路。
2. **生产 advisor 接搜索**：`openpoker_advisor.rs` 现完全不调 `subgame_search`、硬编码
   `default_6max_100bb`（`:191`）、`Request`（`:84-96`）无 per-seat stack 字段。须捕获真实栈 + `decide()` 新建
   search 分派 + 重写 outgoing（见 §1）——是管线重建，不是接一行。
3. **off-stack 叶子续局值**：深码下 100BB 叶子值 off-distribution，须按真实码深重建。短码解到终局可绕开（先做短码）。
4. **多人 >3 树 + N-way 叶子值**：见 §2.2，立项选甲/乙；4+way 先补 S3 多人桶验证。
5. **真实分布覆盖度量**：见 §4.2（HH 日志 + 覆盖热力图，均待建）。
6. **NLHE best-response（exploitability 离线锚）**：`best_response.rs` 现仅 impl Kuhn/Leduc、`exploitability()`
   硬编码两人零和；NLHE 侧只有 `lbr.rs` 的 LBR proxy（`nlhe_h3_report.rs` 自承 "not formal exploitability"）。
   步 A 的「量 blueprint 在短码可被剥削度」依赖一个须新写的 `impl BestResponse<SimplifiedNlheGame>`（短码 HU
   树小、`SimplifiedNlheGame` 已 impl Game 故可写，但须先核验全树 full-tree PI 的 infoset 规模是否可承受）。
7. **多人 AIVAT 降方差**：`aivat_nlhe.rs` 现是 HU 单对手（两人硬编码），须推广到 N 座。单边 P_a={chance, 我方}
   即无偏（不需对手策略、用 blueprint 当值函数也合法），预期 2–3× SD 缩减 → 所需手数降 4–9×。这是 §4 live
   功效预算唯一不需配对的降方差杠杆（OpenPoker 不可配对，配对探针的方差缩减用不了）；不补它，§4 的 live 功效
   结论是在「无降方差」假设下算的、过度悲观。注意与设计 §5.c 的 `aivat_value` N-player 推广（搜索叶子值函数用途）
   区分——复用同基建、目的不同。

## 4. 落地（分步验收）

### 4.1 分步推进顺序 + 量化验收

推进顺序 = **A 前置 → B 短码 MVP → C 深码 → D 多人 → E 剥削**；贯穿全程的并行数据采集见 §4.2，不在此顺序里。

| 步 | 做什么 | 放行判据 |
|---|---|---|
| **A**（前置 / vultr） | **真正的下一步、四件都不存在**：① `best_response` sizing → 可行则写 `impl BestResponse<SimplifiedNlheGame>`（缺口⑥）；② 短码引擎正确性闭环（解到终局守恒 / PokerKit 自洽 / byte-equal + 实时解 ≈ CFR 收敛真值）；③ 用 BR 量 100BB blueprint 在短码可剥削度；④「(节点数, 迭代数) → 单决策 wall」回归曲线 + 时限可行性判定 | ① BR 可行（二值，见下「判据定义」）；② 守恒 + byte-equal + 收敛距离达阈（见下）；③ **短码可剥削度 ≥ 阈值 T（见下，否则 B 无肉、不开）**；④ wall 曲线产出 + **5s 预算下短码 ≤3-way 能解到收敛迭代数**（否则先收时限轴到 10–20s 或申请多核，再开 B） |
| **B**（短码 MVP） | 短码 ≤3-way 实时搜索：限时求解器（缺口①，据 A 的 wall 曲线静态选粒度，**含 off-tree 真实状态兜底启发式层**）+ 接生产 advisor（缺口②，管线重建、**喂入各家真实栈**） | **离线（硬放行）**：exploitability < A 测得的 blueprint 短码可剥削度（真把肉吃下来）；no-panic / 归一；单决策 wall ≤ budget；**advisor 实喂 per-seat starting_stacks（不再硬编码 100BB）**；**含非对称起始栈样例（如 hero 20BB vs 对手 80BB）的短码守恒 + SPR/all-in 阈值对真栈一致**；**限时解不动时降级动作来自真实状态启发式、不回落 100BB blueprint**。**live（观察项，非放行）**：见下「live 功效预算」——降为不退化护栏 |
| **C** | 深码叶子续局值（off-stack leaf value），按真实码深重建；biased 默认弃用（保守选择，非「实锤有害」，§2.1）；**该格须在目标时限内可解到有用迭代数**（否则降级为「收时限 / 上多核」决策点，不宣称兑现） | **离线（可兑现）**：叶子值按真实码深重建 + 守恒 + byte-equal。**live**：非劣性 `mbb/100 CI 下界 > −δ`（δ 与基线见下），且 **live 半段功效大概率不足判决 → C 过 ≠ 兑现北极星深码核心，只兑现「叶子值正确 + 不退化」** |
| **D** | 多人 >3，拆三步：**D0** S3 4+way 桶可复用性离线验证（vultr 可跑）→ **D1** 据 D0 立项选甲（扩抽象 4-way，48GiB）/ 乙（实时 N-way 解，短码用真实 payouts、深码须 N-way 叶子值）→ **D2** 建 N-way 叶子值 + live | **D0**：4+way 桶 signal/floor 达阈 或 重标边界方案定（独立放行，先于选型）；**D1**：甲/乙取舍拍板；**D2**：4+way 见 flop 桶有忠实树（离线核）+ live 非劣（同 C，功效不足 → 过 ≠ 兑现多人核心） |
| **E**（后置可选） | 剥削外挂：置信度门控替换对手 range 数据源 | **前置**：对手 name 稳定可追踪（§4.2 已验）；数据足的对手上增量为正（同受 live 功效限制，复用下方护栏）；vs 池中最鲁棒对手分项不亏（防反剥削） |

**放行判据定义（须可判，不留含糊）**：

- **BR 可行（步 A①）= 二值**：全树 full-tree PI 的 infoset 规模 ≤ N 且 wall ≤ W、峰值 RSS 在 vultr 11.67GiB 内跑完。
- **收敛（步 A②）= 距离达阈**：实时解与离线 CFR（≥ M 迭代）的 per-infoset 平均策略 L1 距离均值 < ε 且 root EV 差 < δ_conv（ε / δ_conv 在步 A 标定）。
- **短码可剥削度阈值 T（步 A③）**：exploitability 是确定标量（`best_response.rs` 全树回溯、无抽样噪声），对任何非 Nash 策略恒 > 0——故「> 0」不是判据。要的是「大到值得搜索」：T = 100BB blueprint 在短码可剥削度 ≥ X mbb/手（或 pot 的 Y%），X 对照 blueprint 自身在 100BB 的可剥削度量级定。删原文「显著」二字（标量无统计意义）。
- **深码非劣性 margin δ（步 C / D2）**：放行 = 功效内能排除「劣化超过 δ」（CI 下界 > −δ），不是「CI 跨 0」——后者把「真不劣」与「样本不足测不出」同判 PASS（接受零假设）。δ 须连同「账号可达手数内能分辨的最小 δ」一起给；若可达 δ 远超有意义阈值，诚实标注「该格 live 当前手数不可判」、放行主锚移到离线。

**限时可行性是全表前提**（北极星硬轴，不只 §6 风险）：`subgame.rs` 求解器现固定迭代、无 `time_budget`，单决策
wall 从未隔离测过；Pluribus 28-core 平均 20s/手、vultr 连 P95<30s 都要上 ≥8-core。故步 A④ 先回答「5s 档能否解动」、
C/D 各带时限可行性门；任一格 5s 解不动是「收窄时限轴 / 上多核」的决策点，不是静默宣称兑现。

**验证（按真实桶读）**：总 mbb/100 + 按码深桶 + 见 flop 人数桶 + **栈对称/非对称切片**；vs blueprint-only baseline。
注意双层（×街）分桶把本就稀薄的 live 样本再稀释 ~5–25×、每桶功效更差——分桶读数仅作方向，强弱判据看总量（+ AIVAT 降方差后）。

**live 功效预算（必须先算，且先统一单位）**：评测口径统一 **mbb/g**（per-hand，与 §11.5d 一致）。从 §11.5d 反算
per-hand PnL 的 **SD ≈ 1.7 万 mbb**（48k 手、A 臂 CI95 半宽 ≈150 → SE ≈ 76.5 mbb/g → SD = SE·√n）。判一个真效应
E（mbb/g）须 n ≈ (1.96·SD/E)²（CI≠0；power 0.8 再约 ×2）：

| 真效应 E | n（CI≠0） | 现实性 |
|---|---:|---|
| 0.30 mbb/g（= 旧文「+30 mbb/100」字面） | ~1.2×10¹⁰ | 不可能 |
| 30 mbb/g | ~1.2×10⁶ | 账号只够数百–数千手 → 判不动 |
| 100 mbb/g | ~1.1×10⁵ | 仍远超账号可达手数 |

旧文「+30 mbb/100 → 10⁵–10⁶ 手」单位混了：+30 mbb/100 = 0.30 mbb/g，与 SE 80–160 mbb/g 不同量纲、不能直接代公式；
且「SE 80–160」是 marginal SE，真正的配对 SE ≈ 54–67、丢配对只升 ~20–30%、非「远高于」。结论方向不变反而更硬：
**任何合理 effect 下 live 都需远超账号可达手数 → 首要资源压到短码离线锚（缺口⑥）、不靠 live 定强弱。**

**两类 √n 压不掉的限制**：
- **系统偏差 ≠ 方差**：OpenPoker 不可配对（同手不能跑两臂）+ lobby 混合桌 + bot 池漂移 + 单号分时段 → search-on
  臂与 blueprint-only 臂面对不同时段 / 不同对手构成的 field，差里混入「对手池强度差」这个**不随 n→∞ 消失**的系统偏差。
  故 live 给的是「带未知符号偏差的方向读数」、非可判增量。缓解（交错短轮换 / 记每段对手 name 做协变量校正 / Pro 号
  并行同桌）须标残余偏差。
- **降方差唯一杠杆 = 多人 AIVAT（缺口⑦）**：不需配对、预期 2–3× SD → 手数降 4–9×，把上表 n 拉回部分可行区间（但救不了
  上面的系统偏差）。裸 mbb/g 拿方向 → CI 太宽即上 AIVAT。

**fallback 护栏（须先标定、分两类）**：fallback 高的格子 = blueprint 解错游戏的格子 = 短码 / 深码 / 4–5 人 = 北极星正活处；
fallback 低的格子是近-100BB / ≤3-way（搜索锦上添花有限、非北极星）。故**不能用「fallback 高即不可解读」一刀切**
（会把最该测的格子判废）。区分：① **baseline 臂** 的 off-dist fallback 是**真信号**（正是要测搜索能不能救，应优先而非排除）；
② **search 臂** 的「连搜索都解不动退回」才是质量问题。阈值用步 A 的离线锚量「fallback 决策的 mbb 损失」标定，不拍 40%。

**B/D live 半段 = 不退化护栏，非「显著优于」放行**（承上：live 在账号手数内判不动「显著优于」）：放行只认离线半段；
live 作观察项，护栏 = 「search 臂 fallback < 标定阈 且 mbb/100 CI 下界不显著为负」（可判的不退化）。「live 显著优于
blueprint-only」在当前账号手数内不可达、不作放行条件。

**离线真值锚（短码）+ live 功效预算 + 多人 AIVAT 是 B/D 立项的硬前提；OpenPoker 多人/深码格子目前只有 live 这一个弱判据。**

诚实标注：码深漂移（实测同桌 14–800BB）+ bot 池漂 + 单号分时段——live 方差大、迭代慢、不可配对且带系统偏差。

### 4.2 并行轨道：数据管道

**定位**：挂在现有 blueprint-only live bot 上**贯穿全程的后台采集**，**不是 §4.1 推进顺序（前置 A / 短码 B
…）的前置**——A/B 不必等它做完就能开，它给频率权重 + 对手数据，但「搜索在哪必须」的优先级 §0.3 已结构性先验给出（blueprint 只在 100BB/≤3-way
忠实、其余全是搜索必须区），故不当顺序里的一步、可随时起。

**做什么**：把 OpenPoker 客户端日志从「只记我方决策点」升级成「**全桌手牌历史（HH）**」。分两件、工作量不同：

- **日志升级（parse-and-persist 轻活）**：driver 现在**收到但丢弃**了 name / winners / shown_cards
  （`openpoker_play.py` `_handle_hand_result` 只用 final_stacks 做 leave/rejoin），落进新增独立 `--hh-log` 即可。
  **隔离 advisor 路径**：`build_request` 不消费 name / 摊牌（现 `:128-140` 字段如此），保持 advisor 输出 byte-identical
  ——这是**待守门的目标性质**（真加日志时若为持久化重排消息时序，仍可能间接动到 `HandState.actions`），
  须配 invariant 测试（挂/不挂 HH 日志 advisor 输出 byte-equal）。
- **覆盖热力图（非轻活）**：码深桶（14–30 / 30–60 / 60–150 / 150–400 / 400–800BB）× 见 flop 人数
  （2/3/4/5/6）× 街，每格统计 blueprint 有无忠实策略 / 在 fallback / 在乱映射——判「忠实 vs Desync vs 乱映射」
  须复用 shadow/off-tree 分类逻辑，不是 parse。

**双目的：**
1. **量真实分布覆盖热力图**：给出**频率先验**。注意频率 ≠ EV 影响——一个罕见但高 EV 的格子（如短码 all-in 阈值）
   该先做却频率低。优先级真正判据 = blueprint 在该格的 EV 损失（短码可离线拿真值）× 频率；热力图与 §0.3 的
   确定性/可证伪先验**联合**排序，不单独「钉死优先级」。这条联合排序也给出 **B 闭环后攻 C 还是 D 的分叉触发**：
   由「EV 损失 × 频率」最高且其离线锚 / 功效预算就绪的格子决定，而非 §4.1 表里 C-before-D 的固定枚举。
2. **攒对手数据**（剥削外挂后置用）+ 验证对手 name 是否稳定可追踪（稳→逐对手；不稳→population）。

**放行判据**：字段齐 / 摊牌名字捕到；advisor 挂/不挂 HH 日志 byte-equal；每格 blueprint 缺席率 + fallback 率出图。

## 5. 正确性 smoke / invariants 守门

- **HH 日志**：selftest 不破 advisor 路径（挂/不挂 byte-equal）；真挂场字段齐、摊牌 / 名字捕到。
- **限时求解器**：静态选粒度路径保 byte-equal（固定迭代）；no-panic / 策略归一 / 解输出动作合法（不破规则层）；
  走 `RngSource` 可复现。若引墙钟中断退路，须显式标为可复现性豁免并说明 G1–G3 / replay / AIVAT 一致性如何处理。
- **接生产回归**：`Contestant.search=None` ⇒ 输出 byte-equal 当前 blueprint（守已验证的 advisor 薄壳资产）；
  slumbot HU 复用同核不受波及；search-or-blueprint 分支不破影子推进 lockstep（配测试，非仅声明）。
- **短码引擎正确性**：补「14–25BB、3-way、含 all-in 中途根、**且含一个 per-seat 不等起始栈样例（如 hero 20BB
  vs 对手 80BB）**」的 `build_subtree` + subgame solve → `payouts()` per-seat PnL Σ==0 且对 PokerKit / 解到终局
  口径自洽的 cross-check（现有守恒测试只覆盖对称 base 态 `state.rs:1370`，短码多人 side-pot 中途根 + 非对称栈未
  单独验；`tests/side_pots.rs` 已覆盖不等栈守恒但走直接 apply、非 `build_subtree`，可作 oracle 复用）。非对称栈
  是码深轴最高频形态、又是「现解天生处理、预计算表做不到」（§2.1）的承重前提，**必须验过才能当前提**。再下
  「短码引擎正确」结论。
- **正确性优先**（CLAUDE.md）：搜索接进实战前，off-stack 树形与真实 `GameState` 的 SPR / all-in 阈值一致（含非对称栈）。

## 6. 已知风险（诚实）

1. **搜索 off-distribution 的价值 = 有理由相信、未证**：必须在 off-stack 上对**外部对手**证它（步 B 的 live 半）。
   其离线前提（短码有肉 + 引擎正确）= **步 A**，依赖未建的 NLHE best-response（缺口⑥）；live 锚依赖未算的功效预算
   ——**在 A 与功效预算落地前，B 是 0% 可执行而非"待跑"**。
2. **深码 / 多人叶子值 = 真硬骨头**：100BB blueprint 值不转移；短码解到终局是绕开它的原因（故先做短码）。
   biased 叶子在 §11.5d 探针下更偏离 blueprint（相对读数、非绝对有害），默认弃用是保守选择；且 depth-limit
   unbiased 本身只是 wash → 深码靠它拿净增益证据偏弱，须把叶子值按真实码深重建/重标，收益到外部对手上才算数。
3. **多人 >3 无免费午餐**：甲（扩抽象）吃内存（N=4=48GiB 已测）、乙（N-way 叶子值）吃难度。须立项明确取舍。
4. **限时**：深 / 多人树在 5s 内解不解得动未知（设计 §8：vultr 4-core 连 P95<30s 都要上 ≥8-core，5s 档更没底）；
   须实测单决策 wall，且墙钟 anytime 与 byte-equal 冲突（§2.3），故走静态选粒度。**5s 解不动 → 收时限轴 / 上多核**
   是 §4.1 步 A④ / C / D 的显式决策点，不是静默宣称兑现。
5. **验证闭环慢且脆**：放弃自对弈真值后，短码靠离线真值锚（须建缺口⑥），其余只剩 OpenPoker live
   （不可配对、低功效）。「把高频对手聚成几个固定粗 HUD bot 做离线 A/B 当便宜判别器」的设想当前**代码不存在**，
   且实现出来会引入新 confound（HUD≠真 bot / 在 100BB 对称模拟器里测=错格子 / 自观测过拟合）——是未实现提案，
   不是现成的便宜判别器。
6. **剥削外挂上线时**：best-response 对错模型可被反剥削；6-max 无两人零和安全网。置信度加权须用带可剥削度上界的
   形式化（per-infoset 观测数加权 / 求解层 p 参数），**不要**对两个最终策略做线性插值（凸组合不保 EV）。
7. **资源争用 / 排期未定**：缺口①②③④⑥⑦均是非平凡新建项，无人天/wall/$ 估算。实时搜索的 AWS 评测（1M 手 / ≥8-core）
   与主线 preopen blueprint 续训（卡在 ~2.1B AWS 暂停、LCFR 不可 resume）抢同一台按需 AWS 机。**这是 §4 任何上 AWS
   的步（C/D/E 评测、大样本 live）的立项硬前提、非仅风险**：开工前须按 `feedback_high_perf_host_on_demand` 为对应步
   列 wall + $，并明确与 preopen 续训的先后/互斥、向用户报预算后再起机。vultr 可跑的离线锚（步 A、D0）不受此约束。

## 7. 算力 / 排期（待补）

- **vultr（4-core/11.67GiB）**：跑得动短码 ≤3-way 子博弈正确性测试 + 离线小样本；稳定 P95、多人多 board 采样、
  1M 手评测须上 ≥8-core（AWS c6a.8xlarge 量级）。
- **优先级**：短码离线锚（缺口⑥，vultr 可跑）→ 静态限时求解器 + wall 曲线 + 5s 时限可行性判定（缺口①）→ 生产接线
  （缺口②，含喂真栈）→ live 短码确认（含多人 AIVAT 缺口⑦降方差）。深码（③）/ 多人（先 D0 桶验证，vultr 可跑；再
  ④ N-way 叶子值）排在短码可证伪闭环之后，且上 AWS 前须与 preopen 续训抢机的取舍先拍板（§6 #7 硬前提）。
- 各步 wall + $ 估算 = 立项前补齐项（§6 #7）。

**状态：目标已定（本文）。下一步 = 步 A 四前置（见 §4.1 表 A①–④：① NLHE best-response 离线真值锚，缺口⑥；
② 短码引擎正确性闭环，含非对称栈守恒；③ 用 BR 量短码可剥削度 ≥ 阈值 T；④ wall 回归曲线 + 5s 时限可行性判定，
缺口①），全在 vultr；live 功效预算（统一 mbb/g + 多人 AIVAT 缺口⑦）随 live 半段在步 B 前算。A 成立后开短码实时
搜索 MVP（步 B = 接生产喂真栈 + live 不退化确认）。数据管道（§4.2）= 可并行后台采集，不卡住 A/B、随时可起。**
代码改动 push → vultr fetch/reset；测试一律走 vultr。

---

**相关文档**：实时搜索的底层机制与历史实验（建树 / range / 叶子值 / off-tree 映射 / 已跑 A/B / §11.5d 负判决）见
`docs/temp/realtime_search_design_2026_06_03.md`；OpenPoker 客户端与协议见
`docs/temp/openpoker_client_design_2026_06_02.md`。

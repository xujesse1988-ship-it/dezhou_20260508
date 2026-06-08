# 执行文档：6-max NLHE 真实对抗场景的实时搜索 bot

> 2026-06-08 / 分支 `6max`。本文是 6-max 实时搜索的**当前目标与落地计划**。OpenPoker 是**验证场**，
> 不是目标本身；强弱只认真实对局。复用的底层机制（建树器 / off-tree 映射 / CFR 子博弈解）见文末「相关文档」。

## 0. 目标 · 核心洞察 · 决策记录

### 0.1 目标（北极星）

**在真实 6-max 无限注德州对抗分布下，尽量打出接近最优的牌。** 真实分布的三条硬轴：

1. **人数**：见 flop 可能 2 人，也可能 4–5 人。现有 blueprint 只在 **≤3-way** 上训练
   （`width_redirect` N=3），4+way 无忠实策略。
2. **码深**：各家起始码深**不等且连续**，可能 14BB 短码、也可能 500BB 深码。blueprint 只在
   **100BB 对称**码深上训练——`openpoker_advisor.rs:24` 自承「码深 ≠ 100BB：solver 树/SPR 都按
   100BB 解」，即任意真实码深都被强行当 100BB 解。off-tree 只翻译下注**尺寸**，修不了树形 / SPR。
3. **限时**：每步决策可配时限 5/10/20s，须 **anytime**（预算内随时给出当前最优）。

「最优」在多人一般和里无 Nash 保证，实践含义 = **在真实状态上解出的一个稳健、接近不动点的策略**。

### 0.2 核心洞察（架构支点）

**blueprint 是预计算的固定切片，覆盖不了「码深 × 人数」这个连续空间。** 你不可能为每个码深
（14…800BB）× 每种人数 × 每种非对称栈组合都预存一张表。**唯一能覆盖这个连续空间的，是在决策时
按真实状态（真码深、真人数、真 board）现场建子树、现场求解 = 实时子博弈搜索。** blueprint 退居
二线，当**先验 + 叶子续局值 + 兜底**，不当最终答案。

主干**复用现有 CFR 子博弈求解器**（`subgame.rs`：`EsMccfrTrainer` + `build_subtree`），
**不引入新求解核**。要补的只有 §3 缺口列表。

### 0.3 实时搜索的适用边界（为什么它是主干）

实时搜索的价值，**正比于真实状态偏离 blueprint 训练分布的程度**：

- **blueprint 已练透的格子（100BB、≤3 人）**：搜索锦上添花有限——这没关系，**这不是北极星**。
  已有实测表明，在这个格子里、用「开搜索 vs 同一 blueprint 当对手」的自对弈方式**测不出绝对强度**
  （blueprint 越强、当对手就越硬，搜索的任何近似偏移就亏越多，与"加搜索是否更强"无关）。
- **blueprint 缺席的格子（短码 / 深码 / 4–5 人）**：blueprint **根本没有答案**，搜索不是"打不打得过
  blueprint"，而是"除了现解，没别的东西能出像样策略"。**北极星正活在这里。**

**两点直接推论，贯穿全文：**
1. **绝对强弱只能靠外部对手判**（OpenPoker / 固定参考），不靠自对弈探针。验证锚到
   **off-stack / 多人 × 外部对手**。
2. **短码先做**：短码树小、能解到终局，天然不依赖叶子值近似——这是确定性最高、可证伪的第一个胜仗。

### 0.4 剥削 = 可选外挂（后置）

同一个子博弈求解器，对手 range/策略默认来自 blueprint（稳健）；**当对某对手攒够可靠数据时，把那部分
输入替换成实测对手倾向**，就从「稳健」平滑切到「剥削」。一套引擎、两个数据源。稳健的蛋糕先烤好，
剥削是糖霜，整体**后置**（上线注意事项见 §7）。

### 0.5 决策记录（2026-06-08 用户拍板）

1. 目标 = **真实分布下接近最优**（多人 >3 / 码深 14–800BB 不等 / 限时 5–20s）。
2. 主干 = **真实状态上的实时子博弈搜索**（复用 CFR 子博弈解），blueprint 当先验 + 兜底 + 叶子值。
3. 验证锚在 **off-stack / 多人 × 外部对手（OpenPoker）**——blueprint 缺席的格子。
4. 剥削 = 可选外挂（同引擎换对手数据源），置信度门控，**后置**。

## 1. 架构（实时搜索为主干，剥削为可选外挂）

```
每决策：
  1. 读真实状态：真码深（各家不等）/ 真在场人数 / 真 board / 真下注历史
  2. 在真实状态上建子树（build_subtree，真 SPR / 真人数）
  3. 取对手 range/续局值：默认来自 blueprint；有可靠对手数据时替换为实测（剥削外挂，后置）
  4. 在 time_budget 内求解子树（anytime CFR），解不动则优雅回落 blueprint
  5. 返回动作（off-tree 尺寸经 map_off_tree 翻译）
持续：
  记全桌动作 + 摊牌（HH 日志）→ ①量真实分布覆盖热力图 ②攒对手数据（外挂用）
```

主干不写新求解核：root 在真实状态重发隐藏牌、对全 range 求解、事后索引 hero 真实桶
（`subgame.rs:603`）。插桩点 = `blueprint_advisor.rs:421`（该处已引入 `should_search` /
`subgame_search`，生产 `openpoker_advisor.rs` 尚未启用，决策路径要从「纯 blueprint 重放」改成
「search-or-blueprint」）。

## 2. 三条真实轴 + 各自打法

### 2.1 码深（搜索的主场）

- **短码（14–30BB）= 最干净、最该先做**：树小 → 能**解到终局**（无须叶子值近似、无须 depth-limit）。
  而 100BB blueprint 在 25BB 上**本来就是错的**（SPR、all-in 阈值全错）。这是确定性最高、可证伪的
  第一个胜仗。
- **深码（150–800BB）= 真难处**：树大 → 必须 depth-limit + 叶子续局值，而 100BB 训出的叶子值在
  500BB 上 off-distribution（脏）。已有实测：biased 叶子续局值（"对手在叶子选最不利续局"那套）**净有害**，
  深码必须把叶子值消融重标，别照搬经验系数。放 §5 后段。
- **非对称栈**：现解天生处理（建真树即可），预计算表做不到。

### 2.2 人数 >3（最硬的结构题）

- `width_redirect`（`nlhe_betting_tree.rs:101`、断言 `:380`）= 把第 N+1 个进场者收成 squeeze/fold 的
  **多路收口机制**（N=3 甜点），让 ≤3-way 子树可枚举。它**不是**「放开就能多路」的开关——放开
  （`WIDTH_REDIRECT_OFF`）会让 blueprint 落到它根本没训过、无忠实续局/叶子值的区域，不能直接硬解。
- 两条路（须立项选）：
  - **(甲) 扩抽象/训练到 4-way**：内存量级 ~8GiB@200 桶（N=3 真值实测）；≥5-way 仍未覆盖。
  - **(乙) 实时解 N-way 子树**：摊牌值用多人 equity（`multiway_equity_probe.rs:197 multiway_equity_mc`
    已有 MC 估计）。深码多人需 **N-way 叶子续局值**（blueprint 给不了），真硬骨头。
- 短码多人相对好啃（树小可解到终局）；**深码多人最难**，须单独出设计。

### 2.3 限时（必做地基）

- 现状：`SubgameSearchConfig`（`subgame.rs:650`）只有固定 `iterations`（默认 1000）+ `max_subtree_nodes`
  （默认 8000），**没有 time_budget 字段**——不是按墙钟的 anytime 求解器。
- 要补：建树 → 解到墙钟预算用完 → 返回当前 `average_strategy` → 桶未被访问 / 超 cap 则优雅 `Err`
  回落 blueprint（`subgame.rs:1056` / `:1090` 已有回落口）。短树预算内解透；深/多人树用预算尽量迭代。
- 单决策 wall **从未隔离测过**（现有数据全是整臂），须实测（见 §5 Go-NoGo）。

## 3. 现有底座 / 缺口

### 3.1 已落地可复用

| 构件 | `file:line` | 状态 |
|---|---|---|
| 真实状态建子树 | `nlhe_betting_tree.rs:271 build_subtree` / `:309 depth_limited` | ✅ 可接任意中途 `GameState` 作 root |
| CFR 子博弈求解 | `subgame.rs:650 SubgameSearchConfig` / `:1066 EsMccfrTrainer` | ✅ 解到终局或 depth-limit；超 cap / 未访问优雅回落 |
| off-tree 尺寸映射 | `action.rs map_off_tree`（pseudo-harmonic randomized rounding） | ✅ 任意下注尺寸；纯函数可复现 |
| 多人 equity | `multiway_equity_probe.rs:197 multiway_equity_mc` | ✅ MC 估计（现仅离线调研用） |
| blueprint 加载 / 兜底 / fallback 统计 | `nlhe_dense_trainer.rs` / `openpoker_advisor.rs:119 safe_fallback` | ✅ 冷启动 / 失败退路 |

### 3.2 缺口（须新写 / 改造）

1. **anytime 限时求解器**：给 `SubgameSearchConfig` 加 `time_budget`，按墙钟中断返回当前最优。
2. **生产 advisor 接搜索**：`openpoker_advisor.rs` 现完全不调 `subgame_search`、硬编码
   `default_6max_100bb`（`:191`）；要在插桩点 `blueprint_advisor.rs:421` 改成 search-or-blueprint。
3. **off-stack 叶子续局值**：深码下 100BB 叶子值 off-distribution。短码解到终局可绕开（先做短码）。
4. **多人 >3 树 + N-way 叶子值**：见 §2.2，立项选甲/乙。
5. **真实分布覆盖度量**：见 §4。

## 4. Phase 0 — 数据管道（先行）

**做什么**：把 OpenPoker 客户端日志从「只记我方决策点」升级成「**全桌手牌历史（HH）**」。

- 落 `tools/openpoker_play.py`。**隔离 advisor 路径**：不改 `HandState` / advisor 请求 →
  advisor byte-identical（`build_request` 不消费 name / 摊牌）；新增独立 `--hh-log`。
- 字段（协议已确认提供）：`your_turn.players:[{seat,name,stack}]`、各座
  `player_action{seat,action,amount,street}`、`hand_result{winners,shown_cards{seat:[..]},final_stacks,pot}`。
  driver 现在**收到但丢弃**了 name / winners / shown_cards——是 parse-and-persist 轻活。

**双目的：**
1. **量真实分布覆盖热力图**：码深桶（14–30 / 30–60 / 60–150 / 150–400 / 400–800BB）×
   见 flop 人数（2/3/4/5/6）× 街，每格统计 blueprint 有无忠实策略 / 在 fallback / 在乱映射。
   **这张图直接钉死「搜索在哪是必须、哪是可选」的优先级。**
2. **攒对手数据**（剥削外挂后置用）+ 验证对手 name 是否稳定可追踪（稳→逐对手；不稳→population）。

## 5. 落地顺序 + 量化验收（Go-NoGo）

| 步 | 做什么 | Go 判据 |
|---|---|---|
| **A** | Phase 0 HH 日志 + 挂场数百–数千手，出**覆盖热力图** + 真实 fallback 率 | 字段齐、摊牌 / 名字捕到；得出每格 blueprint 缺席率 |
| **B** | **短码 ≤3-way 实时搜索**：解到终局 + anytime 限时求解器，接生产 advisor | 单决策 wall ≤ time_budget；no-panic / 归一；OpenPoker 短码桌总 mbb/100 显著 > blueprint-only |
| **C** | 深码叶子续局值（off-stack leaf value），biased 叶子须消融 | 深码桶 mbb/100 不劣于 blueprint-only |
| **D** | 多人 >3：立项选甲（扩抽象 4-way）/ 乙（实时 N-way 解 + 多人 equity 叶子值） | 4+way 见 flop 桶有忠实树；mbb/100 显著 > blueprint-only |
| **E**（后置可选） | 剥削外挂：置信度门控替换对手 range 数据源 | 数据足的对手上增量为正；vs 池中最鲁棒对手分项不亏（防反剥削） |

**验证（按真实桶读）**：总 mbb/100 + 按码深桶 + 按见 flop 人数分桶；vs blueprint-only baseline。
诚实标注：码深漂移（实测同桌 14–800BB）+ bot 池漂 + 单号分时段——live 方差大、迭代慢且不可配对，
须配套离线判别器（§7）。

## 6. 正确性 smoke / invariants 守门

- **HH 日志**：selftest 不破 advisor 路径（byte-identical）；真挂场字段齐、摊牌 / 名字捕到。
- **实时搜索**（Phase B 起）：no-panic / 策略归一 / ≤ time_budget；搜索输出动作合法（不破规则层）；
  可复现（走 `RngSource`，byte-equal）；search-or-blueprint 分支不破影子推进 lockstep。
- **正确性优先**（CLAUDE.md）：搜索接进实战前，subgame 解须在小子树上对 PokerKit / 解到终局口径自洽；
  off-stack 树形与真实 `GameState` 的 SPR / all-in 阈值一致。

## 7. 已知风险（诚实）

1. **搜索 off-distribution 的价值 = 有理由相信、未证**：必须在 off-stack 上对**外部对手**证它。
   Go-NoGo B 即此证伪点。
2. **深码 / 多人叶子值 = 真硬骨头**：100BB blueprint 值不转移；短码解到终局是绕开它的原因（故先做短码）。
   biased 叶子已实锤净有害，深码 depth-limit 须把叶子值消融重标。
3. **多人 >3 无免费午餐**：甲（扩抽象）吃内存、乙（N-way 叶子值）吃难度。须立项明确取舍，不能久拖。
4. **限时**：深 / 多人树在 5s 内解不解得动未知，须实测单决策 wall。
5. **验证闭环慢且脆**：放弃自对弈真值后唯一判别器 = OpenPoker live。须把高频对手聚成几个固定粗 HUD bot
   （call-station / nit / maniac / balanced）做**离线配对 A/B** 当 cheap gate，挂场只做最终确认。
6. **剥削外挂上线时**：best-response 对错模型可被反剥削；6-max 无两人零和安全网。置信度加权须用带
   可剥削度上界的形式化（per-infoset 观测数加权 / 求解层 p 参数），**不要**对两个最终策略做线性插值
   （凸组合不保 EV）。

**状态：目标已定（本文）。下一步 = Phase 0（HH 日志 + 覆盖热力图）→ 短码实时搜索 MVP（Go-NoGo B）。**
代码改动 push → vultr fetch/reset；测试一律走 vultr。

---

**相关文档**：实时搜索的底层机制与历史实验（建树 / range / 叶子值 / off-tree 映射 / 已跑 A/B）见
`docs/temp/realtime_search_design_2026_06_03.md`；OpenPoker 客户端与协议见
`docs/temp/openpoker_client_design_2026_06_02.md`。

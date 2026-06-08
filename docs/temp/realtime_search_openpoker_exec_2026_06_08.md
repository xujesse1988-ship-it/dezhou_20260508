# 执行文档：6-max NLHE 真实对抗场景的实时搜索 bot

> 2026-06-08 / 分支 `6max`。**本版取代早先的「剥削为中心」版**——那一版把"剥削弱 bot"
> 写成了主角，**把手段当成了目标**。真正的北极星见 §0。机制/基建复用
> `docs/temp/realtime_search_design_2026_06_03.md`（下称**设计文档**）；强弱只认真实对局
> （设计文档 §11.5d 已证自对弈探针测不了绝对强度）。OpenPoker 是**验证场**，不是目标本身。

## 0. 目标 · 核心洞察 · 决策记录

### 0.1 目标（北极星）

**在真实 6-max 无限注德州对抗分布下，尽量打出接近最优的牌。** 真实分布的三条硬轴：

1. **人数**：见 flop 可能 2 人，也可能 4–5 人。现有 blueprint 只在 **≤3-way**（A3×A4 +
   `width_redirect` N=3）上训练，4+way 无忠实策略。
2. **码深**：各家起始码深**不等且连续**，可能 14BB 短码、也可能 500BB 深码。blueprint 只在
   **100BB 对称**码深上训练。`openpoker_advisor.rs:24` 自承「码深 ≠ 100BB：solver 树/SPR 都按
   100BB 解」——即任意真实码深都被强行当 100BB 解，这是结构性偏差，off-tree 只翻译下注**尺寸**、
   修不了树形/SPR。
3. **限时**：每步决策可配时限 5/10/20s，须 **anytime**（预算内随时给出当前最优）。

「最优」在多人一般和里无 Nash 保证（设计文档 §1.2），实践含义 = **在真实状态上解出的一个稳健、
接近不动点的策略**；剥削是在此之上、有数据支撑时的**刻意偏离**。

### 0.2 核心洞察（架构支点）

**blueprint 是预计算的固定切片，覆盖不了「码深 × 人数」这个连续空间。** 你不可能为每个码深
（14…800BB）× 每种人数 × 每种非对称栈组合都预存一张表。**唯一能覆盖这个连续空间的，是在决策时
按真实状态（真码深、真人数、真 board）现场建子树、现场求解 = 实时子博弈搜索。** blueprint 退居
二线，当**先验 + 叶子续局值**，不当最终答案。

这恰好**复用现有 CFR 子博弈求解器**（`subgame.rs`，`EsMccfrTrainer` + `build_subtree`），
**不需要**早先剥削版要的那个全新 expectimax 引擎。目标摆正后，工程量反而更小、更聚焦。

### 0.3 §11.5d 负判决为何不否定这条路（重要）

设计文档 §11.5d 实测「搜索机制不 beat 终局 blueprint、biased 叶子净有害、自对弈探针测不了绝对
强度」。但那是在 **100BB、≤3-way**——即 blueprint 本就练透、搜索最帮不上忙的**唯一那个格子**里、
且用**自对弈探针**（search-on vs 同一 blueprint 当 field，强 blueprint = 强 field）测的。

**北极星活在相反的格子**：短码/深码、4–5 人——**那里 blueprint 根本没有答案**。在这些格子，
搜索不是「打不打得过 blueprint」，而是「**除了现解，没有别的东西能给出像样策略**」。§11.5d 没测
这些格子。所以它否定的是 (a) biased 叶子那个具体技巧、(b) 自对弈探针这个测量工具，**不是搜索在
off-distribution 的价值**。本文把验证锚到 **off-stack/多人 × 外部对手**，正是 §11.5d 的 fallback。

### 0.4 剥削的正确位置 = 可选外挂（不是主角）

同一个子博弈求解器，对手 range/策略默认来自 blueprint（稳健、GTO-ish）；**当对某对手攒够可靠
数据时，把那部分输入替换成实测对手倾向**，就从「稳健」平滑切到「剥削」。一套引擎、两个数据源。
稳健的蛋糕先烤好，剥削是糖霜。剥削的已知风险（错模型反被剥削、6-max 无零和安全网、置信度加权
须用 DBR/RNR 形式化而非手搓）见 §7，**整体后置**。

### 0.5 决策记录（2026-06-08 用户拍板，修正剥削版）

1. 目标 = **真实分布下接近最优**（多人 >3 / 码深 14–800BB 不等 / 限时 5–20s），不是「剥削弱 bot」。
2. 主干 = **真实状态上的实时子博弈搜索**（复用 CFR 子博弈解），blueprint 当先验 + 兜底 + 叶子值。
3. 验证锚在 **off-stack/多人 × 外部对手（OpenPoker）**——§11.5d 没覆盖、blueprint 缺席的格子。
4. 剥削 = 可选外挂（同引擎换对手数据源），置信度门控，**后置**。

## 1. 架构（实时搜索为主干，剥削为可选外挂）

```
每决策：
  1. 读真实状态：真码深（各家不等）/ 真在场人数 / 真 board / 真下注历史
  2. 在真实状态上建子树（PublicBettingTree::build_subtree，真 SPR / 真人数）
  3. 取对手 range/续局值：默认来自 blueprint；有可靠对手数据时替换为实测（剥削外挂，后置）
  4. 在 time_budget 内求解子树（anytime CFR），解不动则优雅回落 blueprint
  5. 返回动作（off-tree 尺寸经 map_off_tree 翻译）
持续：
  记全桌动作 + 摊牌（HH 日志）→ ①量真实分布覆盖热力图 ②攒对手数据（外挂用）
```

主干**不引入新求解核**：复用 `subgame.rs` 的 `EsMccfrTrainer` 子博弈解（root 在真实状态重发隐藏
牌、对全 range 求解、事后索引 hero 真实桶，`subgame.rs:603`）。需要新增/改造的只有 §3 缺口列表。

## 2. 三条真实轴 + 各自打法

### 2.1 码深（搜索的主场）

- **短码（14–30BB）= 最干净、最该先做**：树小 → 能**解到终局**（无须叶子值近似、无须 depth-limit、
  天然躲开 §11.5d 实锤的 biased-leaf 坑）。而 100BB blueprint 在 25BB 上**本来就是错的**（SPR、
  all-in 阈值全错）。这是确定性最高、§11.5d 没覆盖、可证伪的**第一个胜仗**。
- **深码（150–800BB）= 真难处**：树大 → 必须 depth-limit + 叶子续局值，而 100BB 训出的叶子值在
  500BB 上 off-distribution（脏）。§11.5d 的 biased-leaf 净有害警告在这里**适用**，须谨慎。放 §5 后段。
- **非对称栈**：现解天生处理（建真树即可），预计算表做不到。

### 2.2 人数 >3（最硬的结构题）

- 现有 `width_redirect`（`nlhe_betting_tree.rs:101/380`）= 把第 N+1 个进场者收成 squeeze/fold 的
  **多路收口机制**（N=3 甜点），让 ≤3-way 子树可枚举。它**不是**「放开就能多路」的开关——放开
  （`WIDTH_REDIRECT_OFF`）会让 blueprint 落到它根本没训过的区域（设计文档 §5.e 明令别在 4-way+ 硬解）。
- 两条路（须立项选）：
  - **(甲) 扩抽象/训练到 4-way**：内存已知量级 ~8GiB@200（记忆 `project_6max_betting_abstraction_phase0`
    N=3 真值 8.04GiB）；≥5-way 仍未覆盖。
  - **(乙) 实时解 N-way 子树**：摊牌值用多人 equity（`multiway_equity_probe.rs:197 multiway_equity_mc`
    已有 MC 估计）。但深码多人需 **N-way 叶子续局值**（blueprint 给不了），这是真硬骨头。
- 短码多人相对好啃（树小可解到终局）；**深码多人最难**，须单独出设计。

### 2.3 限时（必做地基）

- 现状：`SubgameSearchConfig`（`subgame.rs:650`）只有固定 `iterations`（默认 1000）+ `max_subtree_nodes`
  （默认 8000），**没有 time_budget 字段**——不是按墙钟的 anytime 求解器。
- 要补：建树 → 解到墙钟预算用完 → 返回当前 `average_strategy` → 桶未被访问/超 cap 则优雅 `Err`
  回落 blueprint（`subgame.rs:1056/1090` 已有回落口）。短树预算内解透；深/多人树用预算尽量迭代。
- 单决策 wall **从未隔离测过**（现有数据全是整臂），须实测（见 §5 Go-NoGo）。

## 3. 现有底座 / 缺口（诚实盘点 + 纠正剥削版事实错误）

### 3.1 已落地可复用

| 构件 | `file:line` | 状态 |
|---|---|---|
| 真实状态建子树 | `nlhe_betting_tree.rs:271 build_subtree` / `:309 depth_limited` | ✅ 可接任意中途 `GameState` 作 root |
| CFR 子博弈求解 | `subgame.rs:650 SubgameSearchConfig` / `:1066 EsMccfrTrainer` | ✅ 解到终局或 depth-limit；超 cap/未访问优雅回落 |
| off-tree 尺寸映射 | `action.rs map_off_tree`（pseudo-harmonic randomized rounding） | ✅ 任意下注尺寸；纯函数可复现（设计文档 §12） |
| 多人 equity | `multiway_equity_probe.rs:197 multiway_equity_mc` | ✅ MC 估计（现仅离线 S3 调研用） |
| blueprint 加载/兜底/fallback 统计 | `nlhe_dense_trainer.rs` / `openpoker_advisor.rs:119 safe_fallback` | ✅ 冷启动/失败退路 |

### 3.2 缺口（须新写/改造）

1. **anytime 限时求解器**：给 `SubgameSearchConfig` 加 `time_budget`，按墙钟中断返回当前最优。
2. **生产 advisor 接搜索**：`openpoker_advisor.rs` 现**完全不调** `subgame_search`，硬编码
   `default_6max_100bb`（`:191`）。要把决策路径从「纯 blueprint 重放」改成「search-or-blueprint」
   （设计文档 §4.2 的插桩点 `blueprint_advisor.rs:421` 已规划）。
3. **off-stack 叶子续局值**：深码下 100BB 叶子值 off-distribution。短码解到终局可绕开（先做短码）。
4. **多人 >3 树 + N-way 叶子值**：见 §2.2，立项选甲/乙。
5. **真实分布覆盖度量**：见 §4。

### 3.3 纠正剥削版的事实错误（避免按错误认知排期）

- ~~「实测 ~58% 决策 fallback」~~ = **凭空数字**，全仓库无出处。真实测量：1.5%/1.8%（self-play probe，
  设计文档 :439/:473）、13.5%（解到终局撞 8000 cap @100BB ≤3-way，:739）、OpenPoker live 4 手 0%。
  → 用 §4 的真实热力图替代。
- ~~「改 solve 即可」~~：主干**复用 CFR 子博弈解**（非「改几行」也非「新写 expectimax」）；剥削版要的
  fixed-opponent expectimax 引擎是**后置可选外挂**，现成 `best_response.rs` 只 impl Kuhn/Leduc 且依赖
  零和不变量（6-max 不成立），不能直接复用。
- ~~「≥4-way 放开 width_redirect 否则 panic」~~：`nlhe_betting_tree.rs:379-385` 是 `debug_assert!`
  （release 编译掉、不 panic）；`width_redirect` 是收口机制不是开关（见 §2.2）。
- ~~「去 8000 cap + OOM 后备 200k」~~：8000 在 `subgame.rs:684`（超限优雅 `Err` 回落、非 OOM 崩）；
  200k 是 `six_max_search_probe.rs` 的叶子采样手数（与 OOM 无关）。

## 4. Phase 0 — 数据管道（仍先行，目的扩大）

**做什么**：把 OpenPoker 客户端日志从「只记我方决策点」升级成「**全桌手牌历史（HH）**」。

- 落 `tools/openpoker_play.py`。**隔离 advisor 路径**：不改 `HandState`/advisor 请求 →
  advisor byte-identical（`build_request` 不消费 name/摊牌）；新增独立 `--hh-log`。
- 字段（协议已确认提供，`openpoker_client_design_2026_06_02.md:38/41`）：`your_turn.players:[{seat,name,stack}]`、
  各座 `player_action{seat,action,amount,street}`、`hand_result{winners,shown_cards{seat:[..]},final_stacks,pot}`。
  driver 现在**收到但丢弃**了 name/winners/shown_cards——是 parse-and-persist 轻活，非协议阻断。

**为什么先做它（双目的）**：
1. **量真实分布覆盖热力图**（替代假的 58%）：码深桶（14–30/30–60/60–150/150–400/400–800BB）×
   见 flop 人数（2/3/4/5/6）× 街，每格统计 blueprint 有无忠实策略 / 在 fallback / 在乱映射。
   **这张图直接钉死「搜索在哪是必须、哪是可选」的优先级。**
2. **攒对手数据**（剥削外挂后置用）+ 立即验证对手 name 是否稳定可追踪（稳→逐对手；不稳→population）。

## 5. 落地顺序 + 量化验收（Go-NoGo）

| 步 | 做什么 | Go 判据 |
|---|---|---|
| **A** | Phase 0 HH 日志 + 挂场数百–数千手，出**覆盖热力图** + 真实 fallback 率 | 字段齐、摊牌/名字捕到；得出每格 blueprint 缺席率（替代 58%） |
| **B** | **短码 ≤3-way 实时搜索**：解到终局 + anytime 限时求解器，接生产 advisor | 单决策 wall ≤ time_budget；no-panic / 归一；OpenPoker 短码桌总 mbb/100 显著 > blueprint-only |
| **C** | 深码叶子续局值（off-stack leaf value） | 深码桶 mbb/100 不劣于 blueprint-only（biased-leaf 须消融，§11.5d 警告） |
| **D** | 多人 >3：立项选甲（扩抽象 4-way）/乙（实时 N-way 解 + 多人 equity 叶子值） | 4+way 见 flop 桶有忠实树；mbb/100 显著 > blueprint-only |
| **E**（后置可选） | 剥削外挂：置信度门控替换对手 range 数据源（DBR/RNR 形式化） | 数据足的对手上增量为正；vs 池中最鲁棒对手分项不亏（防反剥削） |

**验证（按真实桶读）**：总 mbb/100 + 按码深桶 + 按见 flop 人数分桶；vs blueprint-only baseline。
诚实标注：码深漂移（实测同桌 14–800BB）+ bot 池漂 + 单号分时段——live 方差大（设计文档 §11.5d
用 48k 手/臂才得 ±150 CI），迭代慢且不可配对，须配套离线判别器（§7）。

## 6. 正确性 smoke / invariants 守门

- **HH 日志**：selftest 不破 advisor 路径（byte-identical）；真挂场字段齐、摊牌/名字捕到。
- **实时搜索**（Phase B 起）：no-panic / 策略归一 / ≤ time_budget；搜索输出动作合法（不破 §1
  规则层）；可复现（走 `RngSource`，设计文档 §9）；search-or-blueprint 分支不破影子推进 lockstep。
- **正确性优先**（CLAUDE.md）：搜索接进实战前，subgame 解须在小子树上对 PokerKit/解到终局口径
  自洽；off-stack 树形与真实 `GameState` 的 SPR/all-in 阈值一致。

## 7. 已知风险（诚实）

1. **搜索 off-distribution 的价值 = 有理由相信、未证**：必须在 off-stack 上对**外部对手**证它
   （§11.5d 测的是错的格子）。Go-NoGo B 即此证伪点。
2. **深码/多人叶子值 = 真硬骨头**：100BB blueprint 值不转移；短码解到终局是绕开它的原因（故先做短码）。
   §11.5d 已实锤 biased 叶子净有害——深码 depth-limit 须把叶子值消融重标，别照搬 ×5 经验系数。
3. **多人 >3 无免费午餐**：甲（扩抽象）吃内存、乙（N-way 叶子值）吃难度。须立项明确取舍，不能久拖
   （用户已明示这是目标的一部分）。
4. **限时**：深/多人树在 5s 内解不解得动未知，须实测单决策 wall（现有数据全是整臂）。
5. **验证闭环慢且脆**：放弃 self-play 真值后唯一判别器 = OpenPoker live；须把高频对手聚成几个固定
   粗 HUD bot（call-station/nit/maniac/balanced）做**离线配对 A/B** 当 cheap gate，挂场只做最终确认。
6. **剥削外挂（§0.4，后置）**：best-response 对错模型可被反剥削；6-max 无两人零和安全网
   （Ganzfried-Sandholm 安全剥削保证不转移）；置信度加权须用文献形式化（DBR per-infoset 观测数加权
   / RNR 求解层 p 参数，带可剥削度上界），**不要**对两个最终策略做线性插值（凸组合不保 EV）。

**状态：目标已修正（本版）。下一步 = Phase 0（HH 日志 + 覆盖热力图）→ 短码实时搜索 MVP（Go-NoGo B）。**
代码改动 push → vultr fetch/reset（`feedback_vultr_sync_via_git`）；测试一律 vultr（`feedback_tests_on_vultr`）。

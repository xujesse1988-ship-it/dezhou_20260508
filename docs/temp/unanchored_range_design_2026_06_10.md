# 脱锚搜索的 range 先验：从 uniform 升级的设计探索（2026-06-10）

> 状态：**档一已实现 + 生产默认已改 ON（2026-06-14，commit `c5a0363`→`ded2756`，vultr subgame
> 50/0 + advisor 29/0 全绿）。** A/B 结论（§5）：自对弈触发率 ~0（测不动），但真实 live 触发手上
> 档一把「uniform 先验导致 stack-off 漏洞」改成正确弃牌、单点 EV 摆动数十 BB（§5.1，3 seeds 一致）
> → **用户据此拍板把 `--search-unanchored-prefix-reach` 生产默认改 ON**（`off` 显式关 = A/B 对照臂 /
> 回退；`DEFAULT_SEARCH_UNANCHORED_PREFIX_REACH=true`）。**仍 pending = live 多手 EV 确认**（§5.1 是
> n=1 决策级证据；正确性早单测硬证、守护 search=None byte-equal，故默认开可放心、随时可 off）。
> 档二′ 的一般形态（每个失同步动作各解一次粗解）仍未做，但**最便宜的「跨街复用」特例已实现**
> （2026-06-18，commit `89e7a59`，默认关 `--search-unanchored-cross-street off`，A/B 用，vultr
> subgame cross_street 4/0 含真桶确定性证据 + advisor 37/0 全绿）：上一街本身 unanchored、子树已解
> （within-round 缓存当前条目）时，复用那棵子树的 σ 对上一街实际动作线做贝叶斯条件化 → 下一街后验
> root range，§1 末「档二′-跨街复用」末「✅ 落地」+ §4。背景见
> `realtime_search_openpoker_exec_2026_06_08.md` §3.2 缺口②续（脱锚搜索落地时把 range 诚实退化为
> uniform，「脱锚 range 细化」列为后置项）。本文把 2026-06-10 讨论出的可行路线 / 坑 / 实现要点钉
> 下来，避免捡起来时重推导。
>
> **档一落地要点（实现与本文设计的差异）**：lockstep 闭包失同步时透传的是**已同步影子节点**
> （`NodeId`，`LockstepErr.synced_node`）而非决策三元组列表——`decide_search_unanchored` /
> `prewarm` 用 `synced_prefix_decisions(game, synced_node)`（导出的 `decisions_on_path` 包装）现取
> 三元组，再包成 `PrefixReach{strategy, decisions}` 喂 `subgame_search_unanchored_cached`
> （新增 `Option<PrefixReach>` 参数）。`estimate_range` 加 `skip_all_in`（档一 v1 跳 AllIn-tag）。
> 默认关：CLI `--search-unanchored-prefix-reach`（`SearchRuntime.unanchored_prefix_reach`，默认
> false）→ 强弱走 h2h A/B、不默认上生产。**前缀里无当前街之前的决策（limp 池首动作即失同步 →
> synced_node=root → prior 空）→ 退 uniform（`ranges=None`，与既有 byte-equal）**，不走
> `new_with_ranges` 的均匀向量（那是不同采样路径）。预热（脱影子）也接前缀：`subgame_search_
> unanchored_prewarm` 加 `hero` + `prefix_reach`（hero 经 actor_override 让 range 平滑「不混」座
> 与决策一致 → 同 key 命中，否则预热静默失效）。算出的 reach 进 solve 缓存 key（开/关自动 miss）。

## 0. 现状与问题

脱锚搜索（`subgame_search_unanchored`，`src/training/subgame.rs:1721`）覆盖三类失同步场景：
off-stack all-in 线、真实 4+way、limp 池。这些正是主目标分布（深码/多人）最常见的形态，但当前
range 先验一律 uniform（`SubgameNlheGame::new` 的 root uniform resample）——理由是 blueprint reach
要沿全局树路径累乘，而该路径在失同步线上结构性不存在（100BB 树缺节点）。

**核心代码观察（本探索的支点）**：`estimate_range`（`subgame.rs:882`）的输入只是一组
`(node_id, tag, seat)` 三元组，逐个独立查 σ 再累乘——**它不需要一条连通的树路径**。锚定路径用
`decisions_on_path` 回溯只是「找到」这些三元组的方式。所以问题归结为：失同步之后，还能为哪些
历史决策配出**可辩护的** `(node_id, σ)`。

## 1. 三档方案（按可辩护程度）

### 档一：同步前缀 reach —— 干净，先做这个

失同步发生在某一个具体动作上；**之前的每一步影子都走通了，有精确的 blueprint 节点，不是近似**。
改法：lockstep 闭包（`tools/openpoker_advisor.rs:271`）失同步时不再丢弃整条路径，带回已同步前缀的
决策三元组列表 → 喂 `estimate_range`；失同步点之后的决策按无信息处理（因子 1）。

统计性质：这是**更粗的条件化，不是错误的条件化**——「给定已同步前缀的 range」是合法先验，
只是没用上后面的信息，不注入错信息（对比档二）。

按场景的覆盖增益：

| 场景 | 失同步点 | 前缀能恢复什么 |
|---|---|---|
| off-stack all-in 线（如 `offstack_allin_req` 测试场景：UTG 短码 shove → SB raise-over 时断） | raise-over 动作 | shove + 各家 fold 之前的全部 preflop 决策 = 大部分 range 信息 |
| 真实 4+way | 第 4 个进池者的 call（width_redirect 收口处） | 前 3 家的决策 |
| limp 池 | 第一个动作（open-limp 无节点） | 前缀为空 → 与现状 uniform 等价，无增益（见档三） |

**前缀内的坑（必须处理）：AllIn-tag 决策要跳过或设地板。** 前缀里 tag 为 `AllIn` 的决策，σ 语义
仍是「100BB 全栈 shove」。真数：1B nolimp blueprint 的 RFI 表里 AA 在 node 0 的 `σ[AllIn] = 0.001`
——blueprint 在 100BB 几乎从不开池 shove，而真实 30BB shove 的 range 宽得多。把这个 σ 乘进去会把
shover 的 range 错误收成「100BB shove range」（几乎只剩超强牌的微小混合），**比 uniform 更糟**。
普通 ratio 档 / 被动动作的几何失真是温和的（尺寸已按比例投影），AllIn 是 100BB 假设（`stack_bucket=0`，
`nlhe.rs:123`，exec 文档 §0.3）撒谎最狠的地方。v1 = 直接跳过 AllIn-tag 决策（因子 1）。

### 档二：失同步点之后的代理节点映射 —— 不建议默认开

给断点之后的决策找特征相近的 blueprint 节点当代理（按街 / 位置 / 本街 raise 数 / pot-odds 桶匹配）
查 σ。技术上可做，但本质是**拿可能错的信息换没有信息**：代理节点的局面结构（limp 池 vs raised 池、
SPR）与真实局面不同，σ 答的是另一个问题，且 §0.3 的批评双重生效（节点错 + 码深错）。uniform 至少
是诚实的零信息，错先验会把搜索往坑里带。若试：独立 flag + off-stack 场景 h2h A/B（uniform vs 代理）
拿到证据再说。

### 档二′：失同步动作用「真栈子树解出的 σ」做贝叶斯条件化 —— 文献标准做法，替代档二（2026-06-10 网上调研补充）

档二的根本缺陷是 σ 来自错局面（节点错 + 码深错）。文献里成熟求解器的统一做法是**根本不查
blueprint，用搜索自己解出的策略更新 belief**：

- DeepStack（continual re-solving）：自己的 range 用「上一次 re-solve 解出的策略」做贝叶斯更新；
  对手侧只跟踪反事实值上界，**从不用对手的真实动作更新对手信息**，因此 off-tree 动作不需要任何
  translation（arXiv 1701.01724，补充材料的三条更新规则）。
- ReBeL / Student of Games：public belief state 沿历史逐动作更新，σ 来自每一步搜索解出的策略
  （arXiv 2007.13544 / 2112.03178）。
- Pluribus：对手 belief 同样按「自己会怎么打」（blueprint 段查表 + 搜索段用搜索解）做贝叶斯收窄
  （Science aay2400）。

映射到本仓库：失同步动作发生时，**在该动作的决策点以真栈建一棵脱锚子树（管道已有：
`subgame_search_unanchored` 的 build 侧），解到粗精度，从解出的策略提取 σ(实际动作 | hand)，
乘进 range**，再到当前决策点正常建子树求解。与档二同样是「给断点之后的决策配 σ」，但 §0.3 的
批评双重失效：子树是真实规则 + 真实码深建的，AllIn 边在 30BB 真栈下就是真 30BB shove——
σ(shove|hand) 答的是正确的问题，档一里「跳过 AllIn tag」的补丁在这条路线下根本不需要。

成本与 v1 收缩：

- 每个失同步动作多一次子树求解，wall 是真约束（6-way 深码单线程建树已 >5s）。v1 只在**失同步点
  那一个动作**上做（前缀仍走档一），后续失同步动作仍因子 1。
- range 先验不需要解到 ε——几百 iter 的粗解已远好于 uniform（DeepStack 的 auxiliary game 同样
  是粗解）；菜单用粗档（deep_menu 同款）。
- solve 缓存照用（ranges 已进 `solve_cache_key`）。
- 零概率坑的一般形式：若解出的 σ(实际动作|hand) 对所有 hand ≈0（动作映射后整列塌零），贝叶斯
  更新崩 → 设 ε 地板，全塌则回退 uniform。

旁证（可借鉴但不作主路线）：

- safe subgame solving（maxmargin / reach，arXiv 1705.02955）用对手 CFV 约束替代「信任 range」，
  对错 range 鲁棒——但理论只在两人零和成立，且需要逐节点跟踪对手 CFV 的基建（不存在），与北极星
  （多人）不匹配，不走。
- DecisionHoldem（arXiv 2201.11580）对 off-tree 节点显式解**多个不同对手 range** 取鲁棒解——
  「uniform 与 prefix-reach 各解一次取混合」是廉价的鲁棒性兜底，成本 ×2，留作 A/B 失败后的备选。
- KLSS / opponent-limited search（arXiv 2106.06068，ICML'23 liu23k）解决的是 common-knowledge
  closure 爆炸——本仓库子树根在局部闭包，本来就是 1-KLSS 形态，无新增益。

### 档二′-跨街复用：上一街已解的真栈子树 σ → 下一街 range —— 档二′ 最便宜的形态（2026-06-18 讨论）

档二′ 上面框成「每个失同步动作多解一次粗解」。但**顺序对局**里有个几乎免费的特例：**上一街
（如 flop）本身就 unanchored、已为它建并解过真栈子树**时，不必为「条件化下一街（turn）的 range」
再解——直接复用那棵子树的 σ。

问题（对着代码）：turn 搜索的前缀 reach 取同步前缀里 `street < turn` 的决策（`subgame.rs:2314-2325`）。
`synced_node` 在 flop 失同步后**冻结**在断点前 → 进 turn 的 `prior` 只剩 **preflop + flop 断点之前**；
**断点及其后的 flop 下注战（往往是最强价值信号，如 re-raise）按因子 1 丢掉**，range 退回
「preflop 3bet range」粗粒度。

修法 = belief 沿街前递（DeepStack continual re-solving 递归，§1 已引）：flop 子树
（`ResolveRoot::RoundStart`，根在 flop 街起点 → **含整条 flop 下注所有节点的 σ**）解出的 σ，对 flop
实际动作线做贝叶斯条件化 → 进 turn 的后验 root range。§0.3 双重批评（节点错 + 码深错）在此失效
（子树真规则真码深），连档一「跳过 AllIn-tag」补丁都不需要。

**动机 = §5.1 的直接续集**：§5.1 在 off-tree flop 决策（88@A73r）上把 uniform 先验的 stack-off
漏洞改成正确弃牌。但若 hero call 了 re-raise 走到 turn，档一把 range 退回「preflop 3bettor」、
**恰好丢掉 BTN 那个 flop re-raise** → §5.1 刚堵的洞下一街又开。同一漏洞的下一街，非新边角。

**落地前要钉死的正确性点（概念对、管道大半现成，但非「换参数」）**：

- **(a) σ 索引不通用 = 主要工程量**：`estimate_range`（`subgame.rs:1046`）在 **blueprint 树**上走决策
  （`game.tree()` + `info_set_for_cards`）；flop 子树 σ 按**子树自己的 node id / 桶**索引，对不上
  blueprint 节点 → **不能直接当 `PrefixReach.strategy`**，需「沿 flop 子树节点走实际 flop 动作线逐点
  读 σ」的变体。导航子问题已解（`within_round_real` 把 real 动作映进菜单），但实际尺寸 off-菜单时
  读 σ 要走同款 off-tree 尺寸映射（粒度税跑不掉）。
- **(b) 纯函数 / replay-AIVAT 可复现 = 真正 gating（也是当初推迟档二′的原因）**：档一前缀 reach 是
  **请求的纯函数**（静态查表）→ seeded 可复现、replay/AIVAT 一致（§2，gate②）。本法让 turn range
  依赖一次 **CFR 解**（flop 子树）。**可以**保持纯（解在给定 seed/iters/range/菜单 下确定，均可从手
  历史派生），前提是**确定性重派生 σ（重解或按 key 命中缓存），绝不 stash 依赖墙钟迭代数的浮点
  后验**。代价：live 便宜（`solve_cache_key` session 内命中时）；但**离线 replay/AIVAT
  （`openpoker_hh_aivat`）要真重解 flop 子树、不再查表 → eval 管道变重**。
- **(c) 误差复合 + 零概率地板**：flop 子树 σ 是「对着它自己那个（可能 uniform 的）root range 的
  均衡」，跨街条件化误差累积 → 带档二′ 同款规则：σ 整列塌零设 ε 地板，全塌退 uniform。
- **(d) 自我 vs 对手 belief 不对称**：DeepStack 只用自己 σ 更新自己 range；ReBeL/Pluribus 两边更新。
  本法两边都用子树 σ → 与档一现有「同质 blueprint / 对手按均衡打」假设（`estimate_range` 同质假设）
  一致，只是把同一模型依赖往深推一街——明确这是「假设对手按真栈均衡打」的后验，非真人 range。
- **(e) 街间 card removal**：flop 后验要按 turn 新牌重新去撞牌组合再用。

**门槛不在概念，在 (b) replay/AIVAT 可复现 + (a) σ 索引改写**——定它是「接缓存」还是「动 eval 管道
的中等工程」。**下一步先钉死**：`solve_cache_key`（`subgame.rs:1120`）缓存是否在 session 内留存、
turn 时能否命中 flop 的解 → 「复用缓存」（几乎免费）vs「重解」（退回档二′ wall 成本）。取证同档一：
自对弈造不出触发手（§5），干净标尺 = 构造 2-way 深码 HU 的 off-tree-flop→turn 线（零和有 EV 真值，
如 §5.1），或 live 按「flop 失同步且续打到 turn」分桶。

**✅ 落地（2026-06-18，commit `89e7a59`，默认关 `--search-unanchored-cross-street off`）**：

- **「先钉死」结论 = 复用缓存可行（几乎免费）**：`solve_cache` 是 advisor 进程常驻**单实例**
  （容量 1，`openpoker_advisor.rs` main loop），turn 解的 **miss 路径**上（turn solve 尚未 `store`
  替换前）缓存仍持上一街解——决策路径与街起点预热同此窗口（街间无 hero 决策 → 无其他 solve 替换），
  故跨街逻辑放在 `subgame_search_unanchored_cached_inner` 的 miss 路径即同时覆盖两路。默认固定 1000
  iter → 缓存解 byte-deterministic，复用 == 确定性重派生（gate② 成立）；`time_budget` 下复用墙钟
  截断解 = session 内自洽但不离线可复现（同 §(b)/全 budgeted 搜索的既有 caveat，未额外加 guard）。
- **(a) σ 索引** = 新增 `SubgameNlheGame::info_set_for_cards`（子树索引版，与 `nlhe.rs` 同路径、树换
  本子树 → 与子树 solve 期 `Game::info_set` byte-equal）+ `subtree_decisions_on_real_line`（沿上一街
  子树实际动作线收集 `(node,tag,actor)`）→ `cross_street_posterior_range` 逐点读 σ。off-菜单尺寸经
  `map_off_tree` 同款映射（粒度税）。
- **身份验证**（拒绝复用错条目，否则退档一/uniform，不破现状）：`SolvedSubgame` 加 `kind`/`hand_seed`
  + `SubgameSolveCache::current()` peek；验「`kind==UNANCHORED`（anchored 时档一已由同步影子覆盖上一
  街，无需跨街）+ 同 `hand_seed` + 子树 board 是本街 board 严格前缀且长度恰少 1（紧前一街）」。
- **(c)/(e)** 按设计：整列 σ<ε 跳过（ε 地板）；某座全塌→全零（下游退均匀）；撞本街新牌 hole→0。
- **(b) replay/AIVAT**：live 复用缓存便宜（已实现）；离线 `openpoker_hh_aivat` 真重解上一街子树尚未接
  （eval 管道变重，留待真要按「flop 失同步且续打到 turn」分桶比 EV 时再做）。
- **测试**（subgame 4 + advisor 2，vultr 全绿）：`cross_street_posterior_structure`（结构/归一/
  card-removal/可复现 + 真桶 range 非 uniform）、`identity_rejects`（跨手/非紧前街→`None`）、
  `no_cache_falls_back_byte_equal`（不可达退档一）、`cross_street_changes_solve_key`（**真桶 3-miss
  确定性证「跨街复用解出的 range ≠ 档一前缀 range」= σ 真进了 range**；stub 全归桶 0 退化成
  2-miss-1-hit）、advisor `parse` + flop→turn 端到端 plumbing/可复现。
- **仍 pending = 强弱（live A/B）**：同档一节奏（默认关、`on` 开实验臂），自对弈测不动（§5 触发率
  ~0），干净标尺 = 构造 off-tree-flop→turn 线或 live 分桶；§动机（档一在 turn 丢掉 flop 断点后加注战）
  是机制论证、未 live 量化。

### 档三：limp 池 = 结构性死路，只能等对手数据

limper 的 range 在 blueprint 里**不存在**——nolimp 树剪掉了所有 open-limp 边，dense 表里没有任何
一行回答「什么牌会 limp」，任何映射都是无中生有。诚实答案 = §4.2 数据管道（HH 日志）+ 剥削加分项
（步 D）的 population 先验（limper 偏被动 / range 封顶），数据源是实测不是 blueprint。

**档二′对此的软化（2026-06-10）**：「结构性死路」只对 blueprint 成立。脱锚子树按真实规则建，
limp 就是 preflop 的 call 边，子树里存在——在 limp 决策点解一棵真栈子树即得 σ(limp|hand)，一个
blueprint-free 的自洽先验（「若对手按真栈均衡打，什么牌会 limp」）。它仍不是 population limper
range（真人 limp 偏离均衡是常态），长期答案不变 = 对手数据；但作为过渡先验，它不是无中生有，
比 uniform 可辩护。注意均衡 limp 频率可能很低（≈nolimp 的训练结论）→ 零概率地板规则在这里
最容易触发，触发即回退 uniform，不比现状差。

## 2. 实现要点（管道几乎现成）

- `SubgameNlheGame::new_with_ranges`（`subgame.rs:168`）已接受任意 per-seat range 向量，锚定/脱锚
  共用同一求解核；脱锚现在只是传 `None` 走 uniform。给 `subgame_search_unanchored_cached` 加
  `Option<ranges>` 参数即可。
- **solve 缓存 key 已逐位哈希 ranges**（`solve_cache_key`，`subgame.rs:1120`；None 哈希 `[0]` 标记）
  ——前缀 reach 接进去自动进 key，不会读错均衡（缓存正确性不需要额外动作）。
- 前缀决策是请求的纯函数 → seeded 可复现 / replay / AIVAT 一致性不破。
- RoundStart 的街切分照旧适用：只累乘当前街**之前**的决策（当前街 betting 在子博弈内由 CFR 解，
  `subgame.rs:1285`）。
- advisor 侧：lockstep 闭包返回 `Err(reason)` 时附带已同步前缀的 `Vec<(NodeId, tag, seat)>`，
  经 `decide_search_unanchored` 透传。

## 3. 守护与验收

- 默认关：不带前缀 reach 时脱锚路径与现行为 byte-equal（既有测试不动）。
- 新测试钉死：①off-stack 场景下前缀 reach 产出的 range 非 uniform 且 AllIn-tag 决策被跳过；
  ②limp 池场景前缀为空 → 与 uniform 路径 byte-equal；③ranges 进 key（开/关前缀 reach 必 cache miss）。
- 强度验收走 h2h A/B（uniform vs 前缀 reach，off-stack 触发场景集），不凭直觉上生产。

## 4. 结论（2026-06-10 网上调研后修订）

分两步走，第二步以第一步为输入：

1. **档一（前缀 reach + 跳过 AllIn tag）——✅ 已做（2026-06-14）**：便宜、干净，且前缀 ranges 正是
   档二′在失同步点建子树时的 root range 输入（档一是档二′的前置，不是互斥方案）。落地见本文头部状态
   栏；下一步 = h2h A/B（uniform vs 前缀 reach，off-stack 触发场景集）拿证据再决定生产默认。
2. **档二′（失同步动作的真栈子树 σ 条件化）作为第二步**——文献标准做法（DeepStack/ReBeL/Pluribus
   的 belief 更新全部用搜索解而非 blueprint 查表），同时覆盖 off-stack all-in、真 4+way、limp 池
   三类场景，并使档一的 AllIn-tag 补丁失效（被更正确的机制取代）。代价 = 每次失同步多一次粗解，
   wall 预算要实测。**（2026-06-18）顺序对局里那个几乎免费的跨街特例——「档二′-跨街复用」已实现**
   （commit `89e7a59`，默认关、A/B，subgame 4/0 含真桶确定性证据 + advisor 37/0）：上一街本身
   unanchored、子树已解时，街间 belief 更新直接复用那棵子树的 σ，不必再解（落地见 §1 末「档二′-跨街
   复用」末「✅ 落地」）。「先钉死」已证复用缓存可行（容量-1 单实例、turn miss 路径仍持上一街解）。
   一般形态（每个失同步动作各解一次粗解）+ 离线 replay/AIVAT 真重解仍未做。强弱待 live A/B。

档二（代理节点映射）被档二′取代，不再考虑；limp 池长期答案仍是对手数据（步 D），档二′提供过渡
先验。两步各自走 h2h A/B（uniform vs 档一 vs 档一+档二′）拿证据，不凭直觉上生产。

## 5. h2h A/B 实跑结论（2026-06-14）

**结论：自对弈无法给 EV 判决——脱锚路径在 blueprint 自对弈里触发率 ~0；default 维持 OFF。**

**为什么既有 h2h harness 用不了**：`evaluate_cross_abstraction_h2h` 用**常驻影子**、失同步即
`HandError::Desync` 排除整手——**结构上到不了脱锚路径**、也表达不了前缀 reach。故新建自对弈探针
`tools/six_max_unanchored_prefix_ab.rs`（commit `f6c18da`→`7326ae6`）：单影子追 auth（game/影子
对称 100BB、auth 用 `--stacks` 不等码深），失同步后所有座走脱锚搜索，hero=前缀 reach vs
field=uniform，配对差 CI + 触发遥测 + 决策改变率（前缀臂 hero 搜索点额外算 uniform 分布记 TV /
argmax flip）。复用与生产 advisor **同一** `advance_shadow_by_applied` 失同步机制 → 触发判定同口径。

**实测（vultr，真 nolimp 1B / preopen 10B blueprint，5 种栈型 / ~1500 手）**：

| 栈型（BB） | reshape | 手数 | 到 flop | postflop 决策 | **失同步手** |
|---|---|---:|---:|---:|---:|
| 100 对称 | preopen | 120 | — | — | **0** |
| 100,80,60,40,25,15 | nolimp | 600 | — | — | **0** |
| 5×100 + 20 | nolimp | 300 | 124 (41%) | 604 | **0** |
| 3×100 + 3×30 | nolimp | 600 | 214 (36%) | 1127 | **0** |

flop **被到达**（35–41% 手有 postflop 决策），但**全在 on-tree**（同步）——0 个 off-tree flop。
根因：脱锚 postflop 路径需要**有 flop 决策的 off-tree 局面**，而 ① off-stack all-in 线多在 preflop
就解决（全下摊牌）或**单挑 vs all-in（无 flop 下注，引擎直接发到摊牌）→ 无 flop 决策**；② 真正需要
脱锚 postflop 决策的形态 = **多人短码边池**（短码 all-in + ≥2 个深码跟到 flop）或**真 4+way**，二者
在偏紧的 blueprint 自对弈里都罕见（getting 2 callers of a shove / 4 人入池）。即：blueprint 自对弈
几乎不产生承载前缀 reach 信号的牌局。

**与单测一致**：前缀 reach 只在**入池者有非-AllIn 同步决策**时才改 range（`unanchored_prefix_reach
_skips_all_in_tag` 真桶测试钉死：Raise 前缀 → range 非 uniform；canonical UTG-shove off-stack 线
shover 的 AllIn 被跳 → 前缀 ≈ uniform）。所以这是个**窄边缘先验**，EV 影响面本就小。

**判定组合（核心区无干净离线 EV 标尺，§0.3）**：
- 自对弈 EV 路线**证伪**（无触发，CI 无从谈起）。
- 构造场景 + 价值基线（强行造 off-tree flop 量 per-scenario EV）需额外工程，且核心区**没有干净的
  离线 EV 真值**（多人非零和），只能量决策改变（已由单测覆盖：会改、但仅窄线）。
- live OpenPoker 数据按「脱锚触发手」过滤 → 需海量样本（脱锚手本就稀），功效预算下判不动。

**自对弈维持 OFF 是因为它测不动；但下面 §5.1 的真实 live 触发手把判定显著推向「该开」。**

### 5.1 真实 live 触发手实测（2026-06-14）—— 决策级证据强烈支持档一

自对弈触发不了，但**真实 live 对局触发得到**（真人 / bot 会做深码 3bet 池里的 flop 加注战，
100BB 树表达不了 → 脱锚）。拿一笔真 live 手（preopen 10B blueprint，advisor 同款 deep_menu /
LCFR / λ=0.25，fixed 1M iter / 4 线程，重放 OFF vs ON 配对，3 seeds）：

**局面**：hero 88（~93BB effective），UTG open 2.25BB → BTN 3bet → hero call；flop **A♥3♦7♦**
hero check → BTN bet → hero raise → **BTN re-raise**（此处 100BB 影子失同步 → 脱锚）。失同步前的
**同步前缀全是非-AllIn**（hero open+call-3bet、BTN 3bet）→ 档一估出**真 range**而非 uniform。

| 决策 | uniform（现状） | 档一前缀 reach | seed |
|---|---|---|---|
| flop 面对 re-raise（88@A73r） | fold **0.0** / call 0.51 / **allin 0.49** | **fold 0.95** / call 0.0 / allin 0.05 | 0 |
| 同上 | fold 0.003 / call 0.46 / allin 0.54 | **fold 0.63** / call 0.37 | 7 |
| 同上 | fold 0.0 / call 0.40 / allin 0.60 | **fold 1.0** | 42 |
| turn（88@A73r9h，跟注后） | check 0.11–0.28 / bet 0.25–0.52 / **allin 0.36–0.47** | **check 0.95–0.98** | 0/7/42 |

**方向三 seed 一致、幅度巨大**：uniform **几乎从不弃牌**、拿 88 在 A 高面对 3bettor 的 flop 加注战
**stack off（call/jam ~全栈）**；档一**多数弃牌**（flop fold 0.63–1.0）、turn 也**收手 check ~0.97**。
（精确弃牌频率有 per-bucket 噪声 = seed 7 的 0.63 vs 0/42 的 0.95–1.0，但 fold≫uniform 是定性铁的。）

**这是清晰的正确方向**：BTN 3bet 翻前 + flop bet + re-raise = 强窄价值 range（AA/AK/AQ/套/同花听），
88 被压死 → 弃牌正确；uniform 把对手当「什么都有（多诈唬）」→ 错误地 call/jam 送掉 ~64BB。**正是
λ-smoothing 治的「uniform/噪声先验 → 解内过度 call/jam」病**，但 **λ=0.25 单独治不动**（uniform 臂
带 λ 仍从不弃牌）——因为脱锚区先验本就是 uniform（最坏情况），λ 只给地板、档一直接换成真 3bet range。

**改写判定**：脱锚路径在自对弈罕见、但**在 live 真实触发的深码 / 3bet 池 off-tree 点上，档一能挡住
uniform 先验导致的 stack-off 漏洞，单点 EV 摆动可达数十 BB**。这不再是「可忽略的窄边缘先验」。
**决定（2026-06-14，用户拍板）**：据此机制（uniform off-tree 先验 = stack-off 漏洞，档一是原则性
修法、λ 单独治不动）+ §5.1 决策级证据，**把 `--search-unanchored-prefix-reach` 生产默认改 ON**
（commit `ded2756`；flag 改取值 `on|off`，`off` = 对照臂 / 回退；正确性早单测硬证、守护 search=None
byte-equal，故默认开可放心、随时可 off）。**仍 pending（诚实）**：n=1（用户选的手）+ 精确弃牌频率有
per-bucket 噪声 + 此手是「干净 3bet range 前缀」最优情形 → **live 多手 EV 确认仍要做**（开 `off` 对照
臂跑 live、按脱锚触发手分桶比 EV；§0.3 live 是核心区唯一 EV 判据）。即「据强机制 + 强单点证据先开、
live 多手回填确认」，不是「单手就完事」。

### 5.2 自对弈测不动的判定组合（核心区无干净离线 EV 标尺，§0.3）
- 自对弈 EV 路线**测不动**（无触发，CI 无从谈起）——但 §5.1 证明这是「自对弈生成不出触发手」、
  **不是「档一无效」**。
- 构造场景 + 价值基线需额外工程，多人非零和**没有干净离线 EV 真值**（HU 深码 off-tree 如 §5.1 这手
  原则上有，因为 2-way 零和）；决策改变已由 §5.1 + 单测覆盖（会改、且方向对）。
- live OpenPoker 按「脱锚触发手」分桶 → 脱锚手稀、需较多样本，但 §5.1 的单点幅度大 → 比想象的可测。

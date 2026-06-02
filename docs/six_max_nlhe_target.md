# 6-max NLHE blueprint 求解器目标（立项）

## 文档目标 + 与既有文档关系

2026-05-30：heads-up NLHE 主线告一段落（1B dense blueprint 对 Slumbot 近 break-even、符合预期，
见 `status_v2.md`），主线转向 **6-max（6 人）No-Limit Texas Hold'em**。本文档是新主线的入口文档，
作用对应 heads-up 阶段的 `heads_up_nlhe_solver_target.md`。

- `heads_up_nlhe_solver_target.md`：heads-up 阶段（H1–H5），**已收尾**，仅剩 Slumbot 对战数据采集。
- `temp/pluribus_path.md`：6-max 长线 8 阶段全景参考（含实时搜索 / 服务化）。本文档**不是**重写它，
  而是把**当前承诺的范围**（路线 A：blueprint-only，跳过实时搜索）切出来、对齐真实代码状态、给可验收门槛。
- 本文档 = 当前主线验收目标；`pluribus_path.md` 的阶段 6（实时搜索）及之后**暂为非目标**。

## 决策锁定（2026-05-30，用户拍板）

- **D-6M-001 路线 A：blueprint-only 先打通**。先把 6-max 离线自对弈 blueprint 端到端跑通（参数化 game →
  多人抽象 → 复用现成 N-generic trainer → 实测对战评测），**不做实时 depth-limited search**。搜索（路线 B /
  `pluribus_path.md` 阶段 6）等 blueprint-only 闭环稳定后再立项。理由：复用约 70% 现有基建；blueprint 是一切
  地基；先把"多人抽象怎么做"这个最大未知数验证掉。
- **D-6M-002 码深 100BB**。6-max 标准码深 = Pluribus 同档，树比 HU 的 200BB 小、更贴近真实 6-max 场。
  （HU 的 200BB profile 不迁移。）
- **D-6M-003 评测范式切换**：6-max 是多人一般和博弈，LBR / exploitability 不再是真实质量度量（见下节），
  质量以**实测对战胜率**为主，LBR 仅作诊断。

## 6-max 的根本不同（决定整套范式）

heads-up 是二人零和 → CFR 可证收敛 Nash → exploitability/LBR 是真度量、不可剥削 == 最优。
**6-max 是多人一般和**，这一点推翻三件事：

1. **CFR 自对弈不再保证收敛 Nash**（多人 Nash 计算 PPAD-hard；且打 Nash 不保证赢——均衡选择 + 隐性合谋）。
   → 接受"无理论保证、靠实测"的 Pluribus 路线。收敛性靠**监控**（average-regret 应 sublinear、entropy、
   动作概率震荡），不靠定理。
2. **LBR/exploitability 失去理论意义**。仍可当"策略烂不烂"的诊断（能被 LBR 暴打就是差），但"LBR 低 = 好"
   不成立。heads-up 最依赖的质量闸门到这里换成**实测对战**。
3. **没有"训到 floor 就停"**。质量相对于对手场，无同种 floor。
4. **没有强 6-max 公开参考 bot**（不像 Slumbot 之于 HUNL）。这是真实评测缺口，S5 要专门解决。

## 现有代码就绪度（亲验 file:line，2026-05-30）

**✅ 已 N-generic，直接复用：**
- 规则引擎：座位/盲注/行动顺序/**多人 side pot**/多人 showdown/per-seat payoff vector。
  `src/rules/state.rs:846–938`（逐 contribution level 算 side pot，返回 per-seat 收益向量）；
  `src/rules/config.rs` 已有 `default_6max_100bb()`。
- CFR trainer（ES-MCCFR / LCFR，alternating traverser）：`recurse_es` 取终局值 `G::payoff(&state, traverser)`
  （`trainer.rs:653`），**不取负、不假设 `1 - player`**，traverser 按 `% n_players` 轮换（`trainer.rs:493`）。
  这正是 Pluribus 用的多人 external-sampling MCCFR 形式。
- InfoSetId 打包：position 已留 4 bit（支持 0..15 座），**不用改 bit 分配**（`src/abstraction/map/mod.rs`）。
- dense 表后端 / checkpoint 流式写：格式不绑人数（按 betting tree 结构走），复用。

**⚠️ 中等改动：**
- ✅ **已参数化（P4，commit `08b3edc`）**：`SimplifiedNlheGame` 去 `n_seats=2` 硬编码 —— 新增
  `new_with_abstraction(bucket, config, abs, rules)`，`new` 委托 HU 默认（byte-equal）；`n_players()` →
  `config.n_seats`；运行期 `legal_actions` 从树派生（F17-free by construction）。可端到端构造 6-max A3×A4 game
  （详见「S2 续」）。**桶仍占位**：6-max 暂用 HU 单对手 equity 桶，有意义训练待 S3。
- 动作抽象 `StreetActionAbstraction`：6-max preflop 动作空间远比 HU 丰富（open/3bet/4bet/squeeze/cold-call ×
  多位置），需扩到**按位置**的 size 集。
- Game trait 零和约束（D-332 `payoff(0)+payoff(1)=0`）→ 推广为"全玩家和 = 0"（筹码守恒，本就成立）。

**❌ 重活（也正是 Pluribus 论文的难点）：**
- 抽象层 equity / OCHS **假设 1 个对手**（`src/abstraction/equity.rs:39,79`）。原判"多人 equity 特征要重做、
  桶要重新聚类，是全项目最大未知数"——**S3 实验 A1+A2（2026-06-01）已推翻**：A3×A4（≤3-way）下单对手桶可复用、
  不需重算多人 equity（river ≈完美、flop/turn hist 桶重排埋在 k-means 噪声底下）。详见 S3 决策记录。
- 评测：LBR 硬编码 `probe_idx % 2`（`src/training/lbr.rs:5`）失去意义；AIVAT 单对手模型（`aivat*.rs`）要推广
  到多对手；baseline 全是 HU 模式 → 要新 baseline + 解决"没有强 6-max 参考对手"。

**一句话**：引擎和求解器内核当初真按 6-max 建好了，可直接吃；真正工作量集中在**抽象（多人 equity）**和
**评测（无 LBR、无强参考对手）**两端。

## 阶段与量化门槛（路线 A）

### S1：6-max 规则正确性钉死（正确性优先，先于任何训练）

**实测纠正前文「从没在 6-max 下验过」的假设（2026-05-30 探索 + vultr 实跑）**：规则层是**按 6-max 先建、
先验**的——项目最初目标就是 6-max（heads-up 是 2026-05-17 才降级的，见 `heads_up_nlhe_solver_target.md`）。
所有错误高发的规则都已实现并带决策记录、且有测试：

- 最小加注 / 不完整加注不重开下注（all-in for less 合法但不给已行动者重开）= `D-033/D-035`
  （`state.rs:500,508`，`last_full_raise_size` + `raise_option_open`）。
- 多人 side pot（3+ 同时 all-in / all-in-for-less / 未跟注返还）= `compute_payouts` / `contribution_levels` /
  `single_contributor_tranches`（`state.rs:826–940`）。
- 奇数筹码归属（button 左手第一个赢家）= `D-039-rev1`（`state.rs:909–923,984–995`）。
- 摊牌顺序（末轮最后加注者先亮，否则 button 左手第一个）= `D-037-rev1`（`state.rs:997–1011`，每街重置）。
- N 座盲注 / 行动顺序（preflop UTG=button+3 先动、BB 最后有 option；postflop SB 先动）= `state.rs:1013–1048`，
  heads-up 走 button=SB 特例（`D-022b-rev1`）。
- **dead button 不适用**：自对弈 solver 全程固定 N 座、无 sit-in/sit-out（`D-032`，`config.rs`），按钮机械左移，
  无空座 → 此前门槛里的 "dead button" 是误列，删。

测试现状（2026-05-30 vultr HEAD `fae5fdf` 实跑全绿）:`scenarios`(10) / `scenarios_extended`(27) /
`side_pots`(8,含 2/3/4-way + 奇数筹码 + 未跟注返还) / `heads_up_rules`(3) 全过;`cross_validation` 默认
`default_6max_100bb()`,**100 手对 PokerKit 0.4.14 `matches:100 diverged:0 skipped:0`**(独立参考实现逐手比对
payouts + showdown_order)。`.venv-pokerkit` 已在 vultr 装好。

**S1 唯一未闭项 = 重跑 100k 手 PokerKit 跨验证**（`cross_validation_pokerkit_100k_random_hands`，`#[ignore]`,
`scripts/run-cross-validation-100k.sh`）。理由不是走形式:`src/rules/` 在 D1 那次 100k 之后又被两个 **perf**
commit 动过 showdown/payout 热路径(`c8fff0a` showdown rank 预算 + pot_winners 缓存、`4e7b3a2` D-378 fast
path),按"正确性大于一切"这类改动后最该重跑最强 gate。成本:vultr 4 核 N=4 ≈ 2.75h(python 子进程 0.4s/手)。

### S2：6-max 树规模量化 + game 参数化

**工具已就绪**（commit `892d683` / `59edd80`）：`tools/nlhe_betting_tree_sizing` 从 HU-only 扩到任意
`TableConfig`——`walk` 本就玩家数无关，换 `default_6max_100bb()` 即枚举 6-max 树。6-max raise 集走 argv、
postflop 桶数走 env `XV_POSTFLOP`（preflop 固定 169 lossless），迭代抽象配置不用重编译。加 `NODE_CAP=100M`：
到上限即停下探并标记 capped，把"是否大到无法全宽枚举"当结论返回。HU self-check 复现 240,096 节点 /
119.7M infoset（exact，验证 refactor 不改计数）。

**实测结果（vultr，2026-05-30 `{1.0}`/`{0.5,1.0}` + 05-31 `{1.0,2.0}`，preflop 169 / postflop 200）**：

| raise 集 | 决策节点 | infosets | dense 两表(variable) | 可行性 |
|---|---:|---:|---:|---|
| `{1.0}` 1 档 | 4,685,850 | 933.9M | **29.14 GiB** | ✅ 64 GB 机富余（= HU c6a.8xlarge 同档） |
| `{1.0, 2.0}` 2 大档 | 10,034,988 | 2.00B | **62.03 GiB** | ⚠ 64 GiB 越界（RSS ~64–66）→ 需 ≥96 GiB 机 |
| `{0.5, 1.0}` 2 档 | **>100M**(capped) | **≥20B**(下界) | **≥645 GiB**(下界) | ❌ 爆，单机无解 |

- `{1.0}` max depth 38（HU 15）/ avg action_count 2.09（91% 节点只剩 call/fold 二选一，因 1pot 加注几轮即 all-in）。
  **内存不是瓶颈**：29.14 GiB 两表 + 小桶表 + working set ≈ 31–33 GiB RSS，落进 64 GB 富余。
- `{1.0,2.0}`（加一个**大**档，无小注）= **有限、枚举完整**（未撞 NODE_CAP）：决策节点 10,034,988、infoset
  1,999,687,552（≈2.00B）、两表 62.03 GiB。树只 **×2.14**、max depth 仍 38（大注顶到 all-in 更快，涨宽度不涨深度）。
  avg action_count **反降** 2.094→2.082：2.0pot 新开子树 **93.85% 是 fold/call 两动作节点**（一注吃掉大块 stack →
  下个决策连 1pot re-raise 都凑不齐 → 立即 all-in），贫节点比整树长得还快，把均值稀释。**内存：62.03 GiB 两表 +
  开销 ≈ 64–66 GiB RSS，越过 64 GiB c6a.8xlarge，需 ≥96 GiB 机**。
- `{0.5,1.0}` 那行是 **lower bound**（枚举到 1 亿节点被 NODE_CAP 截断，真实更大）：加一个**小档**（0.5pot）
  把树从 469 万炸到 >1 亿（>21× 且未到顶）。
- raise 比例语义（`action.rs:288`）= 标准 pot-sized：`candidate_to = max_committed + ceil(ratio × pot_after_call)`。
  翻前 UTG open 用 1pot = 100 + 1.0×250 = 350 = **3.5BB**（min-raise 2BB **不在** `{1.0}` 抽象里）。

**硬约束（S2 关键结论）**：6-max **不能裸加 bet size，尤其不能加小注**。

- 病因不是"档数"而是**小注**——**控制实验证实**（同是多加 1 档：加**大**档 `{1.0,2.0}` 树 ×2.14、有限可枚举；
  加**小**档 `{0.5,1.0}` 树 ≥21× 且撞 cap）：小注 → 底池几何增长慢 → all-in 前能塞很多轮 re-raise → 6 人各自再
  加注 → re-raise 序列**组合爆炸**。大注（≥1pot）几轮顶到 all-in，深度被卡住（`{1.0}`/`{1.0,2.0}` max depth 都 38）。
- **节点数与桶数无关**——是下注树本身的结构爆炸。postflop 砍到 50 桶甚至 1 桶都救不了 >1 亿（真实可能几十亿）
  节点的树，策略表都存不下。
- 能用的杠杆：① **按街/按位置只在个别街加档**（工具 per-street 已支持，现 CLI 是全街同集）；② **raise cap**
  限制每街 re-raise 轮数（Pluribus 真正做法，但当前下注树**无深度上限**，需引擎改动）；③ 小注尽量别要 / 只放一条街。

**剩余项**：① **已验证**（2026-05-31 vultr `b3033f6`）：跑 `{1.0,2.0}` 隔离病因——"小注=组合爆炸"坐实，
`{1.0,2.0}` 有限可解（树 ×2.14）但 62.03 GiB 需 ≥96 GiB 机；② 起步抽象不再卡在 **`{1.0}` vs `{1.0,2.0}`**
二选一（小注出局）——**专线探明小注可经 B3 或 A3×A4 进 64 GiB**，已收窄到 A3×A4（N=3 甜点 = preflop `{1.0}` +
postflop first-small `{0.5,1}`），详见下「S2 续」；③ ✅ **已做**（P4，commit `08b3edc`）：`SimplifiedNlheGame`
去 `n_seats=2` 硬编码、可端到端构造 6-max A3×A4 game（详见下「S2 续」）；④ 据最终抽象的
infoset 数定**训练机 + 算力预算**（`{1.0}` 的 64 GiB 已够内存、瓶颈在训练时长；`{1.0,2.0}` 内存也要升级，
均对齐 `feedback_high_perf_host_on_demand` 报预算）。

### S2 续：下注历史抽象选路 + A3×A4 接进生产（2026-05-31 / 06-01）

S2 把"裸加小注（全街全宽枚举）→ 爆"钉死后，开了条专线探"下注历史除显式 betting tree 外还能怎么抽象"。
完整方案谱系 + 实测见 `temp/betting_history_abstraction_options_2026_05_31.md`；A3×A4 落地契约见
`temp/a3xa4_wiring_design_2026_06_01.md`。摘要如下。

**Phase 0（vultr `eeba801`，全 `{0.5,1}` 精确枚举 287.86M 节点）= 进 64 GiB 共两条路，都不是"裸全宽"**：

| 方案（`{0.5,1}`） | 两表 | 进 64? | 性质 |
|---|---|---|---|
| 全树（裸全宽，perfect recall） | 1820 GiB@200 / 4519@500 | ✗（连 512 GB 不够） | S2 的 ❌ 坐实 |
| **B3** 紧凑 betting-state 摘要 key | 7.61 GiB@500（307,951 distinct key，~8× 余量） | ✅ | 不改游戏 / **改 recall**（imperfect，收敛无保证 + F17 风险 + 重写 InfoSetId 语义） |
| **A3×A4** first-small + width redirect N=3 | 8.04 GiB@200 / 19.97@500（redirect 真值） | ✅ | **改游戏**（postflop ≤3-way + 0.5 只开池不 re-raise） / 不改 recall（perfect，无收敛风险） |

- 两条都"有损"但损法不同：A3×A4 = **改游戏保全 recall**，B3 = **改 recall 保全游戏**。"要小机只能 B3"被
  A3×A4 推翻。**用户已选 A3×A4**（perfect recall 无收敛风险、代码改动最小 = legal_actions 加 width + menu 过滤）。
- A3×A4 叠加是 super-multiplicative（first-small × width = 649× > 单杠杆之积），顺手把 first-small 单用 7.02B
  infoset 的训练-wall 砍到 230.5M（N=3）/ 15.6M（N=2）——wall 不再是瓶颈。
- redirect = closing-action 优先（第 (N+1) 进场者禁被动进场，fold-or-squeeze 把见 flop 人数收到 ≤N）；实测证
  **drop 是松上界非下界**（真值小 2.26×，preflop 剪枝盖过 postflop 加回），原"N=3@500 余量仅 1.4× 敏感"消解
  （真值 3.2× 余量）。

**A3×A4 已接进生产 betting tree（commits `7c2a0ed` betting-tree 层 + `08b3edc` P4）**：

- **规则放抽象层、不碰规则引擎**（`a3xa4_wiring_design` §0）：改 `nlhe_betting_tree.rs`（建树过滤动作）+
  `nlhe.rs`（运行期 `legal_actions` 从树派生），**绝不碰** `state.rs` 的 `GameState::legal_actions` / side pot /
  showdown / `payouts` → **S1 的 PokerKit 跨验证不受影响**。
- betting-tree 层：`PublicBettingTree::build_with_rules(config, abs, BettingAbstractionRules{drop_small_reraise,
  width_redirect})` + `walk` 线程化 `entrants` bitmask + `first_small_6max(N)` profile 构造器（菜单 +
  drop_small_reraise + width_redirect 一处产出，杜绝菜单/标志错配）+ 不变量 `debug_assert`（postflop `live_count ≤ N`）。
- **cross-check 钉死**（builder vs sizing 工具两条独立代码路径对得上 = 接对了）：`num_nodes()` == probe 真值，
  **N=3 = 1,154,822**（infoset@200 230.5M / depth 25）、**N=2 = 78,852**（infoset 15.6M / depth 17）；默认 HU 路径守
  240,096 / 719,764 byte-equal（rules 默认值不改旧行为）。
- P4（`08b3edc`）：`SimplifiedNlheGame` 去 HU 硬编码 —— `new_with_abstraction(bucket, config, abs, rules)`，`new`
  委托 HU 默认（byte-equal）；运行期 `legal_actions` 从树派生（F17-free by construction，D-318：树是唯一真相源，
  filter 后必是 node tag 子集且同序）；`n_players()` → `config.n_seats`；`info_set` 在 `n_seats>2` 走 uncached 分支
  （HU 分支逐字不动，multiway cache 落 post-S3）。smoke：6-max A3×A4 game 构造 + 走到 terminal + 6 座 payoff 守恒 +
  树 == 78,852；HU 全绿。

**桶 caveat（已在 S3 解决）**：6-max 先用 **HU 单对手 equity 桶**（`equity.rs` 假设 1 对手）。原判这是"训练前
真正的 gate、全项目最大未知数"——**2026-06-01 实验 A1+A2 实测推翻**：针对 A3×A4（postflop ≤3-way），HU 单对手桶
**可直接复用、不需重算多人 equity**，gate 降级为"低风险、复用现有桶基建"。详见下 S3 决策记录。

### S3：多人信息抽象 —— 已验证：HU 单对手桶可复用（不需重算多人 equity）

> 前置已就绪（见「S2 续」）：betting-tree 层 + `SimplifiedNlheGame` 参数化（P4）让 6-max A3×A4 game 可端到端
> 构造、CFR 机制跑通。

**S3 决策记录（2026-06-01，实验 A1+A2 实测，逆转「必须重做多路桶」的立项假设）**：针对当前 A3×A4 游戏（postflop
≤3-way = hero + 2 对手），**HU 单对手 equity 桶可直接复用进 6-max，不需重算多人 equity**。原立项把"实现多人 equity
特征 + 按 6 位置重做聚类"列为训练前最大 blocker；两个验证实验把它降级为"低风险、复用现有桶基建"。

**理论先验（为什么单对手桶*可能*在多人失真）**：单对手 equity（`equity.rs:39/79` 写死 1 对手）系统性高估多人胜率
（HS_N≈HS_1^N，AA vs1≈85% → vs5≈49%）；牌型相对价值多人下重排（nut-potential/draws 升、showdown-value/边缘
top-pair 降）。但 Pluribus（Science 2019 + 补充材料）原样复用了 HUNL 单对手抽象技术（k-means on 等价的 OCHS +
EMD-hist 特征、200 桶/街、preflop lossless 169），未做任何多人特征——它靠实时搜索对冲质量损失，而我们走
blueprint-only（D-6M-001）无此对冲，故不能直接继承其"够用"结论，必须自己量。

**实验 A1 —— 标量 equity 排序保持度**（`tools/multiway_equity_probe.rs`，commit b8dfcf6；vultr，每街 4000
canonical 均匀采样、20k MC；自带采样器经 `equity_river_exact` 校验 max|Δ|≤0.008）。e1=vs 1 对手、eN=vs N 对手
同时摊牌 pot-share，Spearman(e1,eN)：

| street | 噪声底(n_opp=1) | 3-way(n_opp=2) | 6-way(n_opp=5) |
|---|---|---|---|
| flop | .9997 | .984 | .904 |
| turn | .9998 | .986 | .889 |
| river | .9999 | **.9995** | .995 |

→ **river ≈完美**（无 future card，多人只单调 dilution、不重排）；flop/turn 标量有真实重排（等 e1 下 draws/
nut-potential 升 ~0.2 percentile），3-way 适度、6-way 严重；dilution 0.50→0.36→0.18 单调、不自致重排。

**实验 A2 —— potential-aware hist 桶的重排**（`tools/multiway_hist_ari.rs`，commit 1c58182；vultr，flop 40k /
turn 100k、K=500）回答 A1 悬念："vs-1 hist 桶是否已吸收 draw/made 重排？"两处方法学关键，否则结论被假象污染：
- 多人 equity 用**真值 disjoint** N=2 精确闭式 `(b(b-1) − Σ_c d_c(d_c-1)) / (990·989 − 45·44·43)`，**不用 q^N**
  ——q^N 是逐 board 单调变换、只携带与 q 相同序信息，比 ARI 测到的纯属分箱假象；MC 自检 max|q2_exact-q2_mc|≤0.0012。
- **等量分箱**（octile 边界）去掉固定 [0,1] 等宽在多人压缩范围 [0,~0.4] 下的分辨率假象。

signal = ARI(vs-1 桶, vs-2 桶)；floor = ARI(vs-1 桶, vs-1 桶' 不同 k-means seed) = 初值噪声底：

| street | signal | floor | signal/floor | 对照(固定[0,1]bin,含假象) |
|---|---|---|---|---|
| flop | 0.552 | 0.576 | **0.958** | 0.205 |
| turn | 0.776 | 0.781 | **0.994** | 0.196 |

→ **vs-1 hist 桶与真值 vs-2 hist 桶的差异埋在 k-means 初值噪声底之下 = 可忽略**。A1 看到的 draw/made 标量重排，
hist 形状（右偏/双峰）本就编码 → 桶不动。固定 [0,1] 分箱对照（0.20）证明"换 equity 保留 [0,1] bin"的天真读法会
误判为大重排，其中 90%+ 是分箱假象。

**合并结论（Q1/Q3）+ 落地**：
- **river（OCHS_16）**：A1 证 ≈完美迁移 → 直接复用现有桶（123M canonical 最大表，省最多）。
- **flop/turn（hist）**：A2 证 vs-1 桶 ≈ 真值多人桶 → 复用；若要"多人理想"特征只需**重标 bin 边界**（等量分箱），
  零新 equity 计算。
- **唯一未直接测**：flop/turn 的 OCHS_8 分量（A2 只隔离 hist 以避 OCHS-multiway 口径歧义）。river OCHS_16 ≈完美
  （A1）+ flop/turn hist ≈完美（A2）两头夹 → 风险低，但属推断非实测。要彻底闭环可补 **实验 A3**（flop/turn OCHS
  vs-1↔真值多人 ARI，需先定多人 OCHS 口径 = vs cluster-k 对手 + 另一在座对手）。
- N=2 是 A3×A4 postflop cap 的正确对手数；preflop 仍 lossless 169（位置由 InfoSetId position bit 区分，不进桶）。

**仍需做（验证*复用*桶的有效性，沿用现成基建）**：
- bucket 质量闸门（`tools/bucket_quality_dump.rs` + `bucket_quality_report.py`）：桶内 spread、空桶=0、同手→同桶
  端到端、映射确定性（同状态 100 万次 bucket id 一致）——对**复用的**桶同样要过。
- 原"多人 equity 用精确枚举/MC/直方图"的成本权衡**已消解**：因复用单对手桶，不再是训练前 gate。
- 收尾选择：**(2) 直接复用 + 进 S4 训练**（推荐，evidence 已强）；或先补 (1) 实验 A3 OCHS 闭环再进。

### S4：6-max blueprint 训练（复用 N-generic trainer + dense 后端）

**已落地 + vultr 验证（commit `5b793e9`，2026-06-01）——训练基建打通，真训练可启动**：

- **训练入口**：`train_cfr` 加 `--profile {hu, six-max}` + `--postflop-cap {2, 3, 4}`（N=4 后加，commit `59bee41`；§续③ backfire——同 1B 预算 ~9% 覆盖、需 ≥56 GiB 机）。six-max 走
  `new_with_abstraction(default_6max_100bb(), first_small_6max(N))`，HU 路径 byte-equal 不变。dense
  trainer 从 `game.tree()` 自动定 indexer 尺寸 → 6-max 直接吃，**dense 算法层零改动**。
- **多人收敛监控**（`src/training/monitor.rs::ConvergenceMonitor`）：采样 preflop 根决策节点 × 169 手型类
  （每手必访、O(169) 与表规模无关），每 report 间隔出 ① mean entropy ② 平均正 regret
  `mean_I(Σ max(0,R)/update_count)`（应 sublinear→0）③ average-strategy L1 漂移（动作概率震荡）④ 覆盖率。
  `StrategySnapshot` trait 抽象，HashMap + dense 两后端各实现一份；只读不破 byte-equal。drive 主循环在
  report 点输出 `[monitor] ...` 行。
- **桶**：`is_supported_bucket_config` 加 `200/200/200`（6-max 生产桶，Pluribus 同档；S3 实测 HU 单对手桶
  可复用进 A3×A4 ≤3-way → 直接用 `artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin`）。
- **vultr 实测**（N=2 dense-lockfree + 真 200 表，24k update，`tests/six_max_a3a4_trainer.rs` + binary smoke）：
  监控信号教科书式收敛——平均正 regret 3.16→1.15、L1 漂移 0.45→0.25、覆盖 162→169/169；LCFR period
  rescale + checkpoint 保存/恢复曲线连续、策略查询逐位一致；dense 单线程 / deterministic 并行 / lockfree
  三条写路径都跑通。峰值 RSS 1.23 GiB（N=2）。

**N 座评测 harness + gate runner（已做，commit `dbb7ab5` + `6bb091b`）**：

- `nlhe_eval.rs::evaluate_blueprint_vs_baseline_multiway` + `NlheMultiwayEvalReport`：blueprint 轮遍全部
  `n_players` 座 vs 其余 N-1 座全打同一 baseline（复用现成 N-generic `rollout_blueprint_vs_baseline`），
  输出总 mbb/g + SE + CI95 + 按相对按钮位置（BTN/SB/BB/UTG/HJ/CO）拆收益。HU 2 座路径不动。
- `tools/six_max_eval`：加载 dense blueprint ckpt → 对 random/call-station/overly-tight 各跑 N 座评测 →
  判 gate（每 baseline CI95 下界 > 0 = BEAT，全过 = PASS）。

**AWS N=3 真训练 + S4 gate（2026-06-01，c6a.8xlarge 32vCPU/61GiB）**：

- run：profile six-max / postflop-cap 3 / dense+lockfree / 32 线程 / LCFR period 10M / 目标 1B update
  （到 1B 即停）/ 200 桶表。RSS **8.84 GiB**（dense 两表 8.04 + 桶 0.55，61 GiB 充裕——vultr 11 GiB 装不下，
  故上 AWS）。吞吐 ~135–164k/s，1B ≈ 1.7h ≈ $2。
- 监控强收敛：avg_pos_regret 50M=0.0018 → 200M=0.0006（sublinear→0）、覆盖 169/169、漂移收缩，无发散。
- **run 完成**：1B update / wall **8766s（2.43h）** / throughput 114k/s 均值 / 全程无崩溃无泄漏、RSS 平在
  8.84 GiB。监控全程强收敛：avg_pos_regret 50M=0.0018 → 200M=0.0006 → **1B=0.0002**、drift_l1→0.013、覆盖 169/169。
- **S4 gate PASS**（six_max_eval，每 baseline 102 万手；200M 早读 + 1B 最终）：

  | vs baseline | 200M | 1B（最终） | CI95 (1B) |
  |---|---|---|---|
  | random | +3882 | **+3382** | [+3252, +3511] |
  | call-station | +5161 | **+3738** | [+3691, +3785] |
  | overly-tight | +301 | **+312** | [+303, +320] |

  三者 CI95 下界全 > 0 → **门槛达成**。
- **反直觉但正确**：vs random/call-station 胜率 200M→1B **下降**（+5161→+3738）。非退步——CFR 收敛的是自对弈
  均衡（趋平衡/GTO-like），不是最大剥削某弱对手；越收敛越不极致剥削特定漏洞。故「更多训练 ≠ 对固定弱对手
  胜率更高」。这正印证门槛「**必要非充分**」：只证 blueprint 没退化、打得像样，**不证强**。真强度判据 = S5
  强对手评测（监控侧 regret 0.0002 才是收敛正解）。
- 24h 连续无崩溃/泄漏：本 run 2.43h 验稳定 + RSS 平；更长稳定留后续；checkpoint 续训已验。

**1B blueprint preflop 合理性诊断**（`nlhe_dense_preflop_169_dump --profile six-max`，沿全员 fold-chain 导出各位置
RFI，全表存 `artifacts/run_6max_s4_n3/preflop_rfi_1B.md`）：

| 位置 | limp | raise | VPIP |
|---|---|---|---|
| UTG | 16.6% | 5.9% | 22.5% |
| HJ | 18.6% | 8.2% | 26.8% |
| CO | 19.1% | 13.3% | 32.4% |
| BTN | 21.1% | 25.2% | 46.3% |
| SB | 43.2% | 22.9% | 66.0% |

- **宏观结构正确**：RFI 与 VPIP 都随位置单调放宽（UTG 最紧 → BTN 最宽）、SB 最松、72o 各位置开池 0.00、
  溢价牌大体在前——blueprint **确实学到位置概念、没退化**。
- **两个问题**（⚠ **2026-06-01 修正**：原括注「非训练 bug、regret 已收敛 0.0002」**判错**——见下「S4 续」，
  实为**欠训练**[1B 仅 56% 表覆盖、仍在爬]与抽象**两因叠加**；监控的 regret/覆盖只采 169 个 preflop 根、看不到全表）：
  ① **过度 limp**——非盲位每位置 limp 16–21%，真 GTO 几乎不 open-limp（⚠ **仅非盲位**；SB 43% **不是** 过度——
  blind-vs-blind 真 GTO limp≈49%，是核心 GTO，见⑥/⑦ 2026-06-02 修正）；「RFI 只有 GTO 一半」的真相是非盲位该 raise
  的牌跑去 limp 了。② **溢价牌未稳定满额进攻**（AA raise 0.54–0.93、KK UTG 仅 0.26），个别非单调点。
- **根因之一 = A3×A4 抽象**（另一半 = 欠训练，见 S4 续）：只有 1 个加注档（1.0pot≈3.5BB，无 iso-raise）+ capped/廉价 postflop（≤3-way、0.5 只开池、
  width redirect）→ limp 进多人池在抽象里不被惩罚、EV 被人为抬高，连 AA/KK 的 raise-vs-limp 在此抽象里都接近
  → 混合策略。要更强 blueprint 的杠杆在**抽象层**：去 limp 选项 / 加小开池档（2.5x）/ 放宽 postflop（属 S5 / 抽象迭代）。
- 印证 gate 的「必要非充分」：宏观对、微观受抽象限制，远非强策略。**下「S4 续」据此把「去 limp / 加小开池」
  从建议落地为代码，并用 run log 自身证伪「1B 已收敛」。**

### S4 续（2026-06-01）：独立核验推翻「1B 收敛」+ preflop reshape 落地

> 触发：用户「文档结论不一定对，独立思考」。复核 S4 两个 load-bearing 结论——「1B 已收敛」「更多训练没意义」
> ——**均被 run log 自己推翻**。下面是证据、被证伪的备选方向、与已落地的修法。

**① 「1B 收敛」证伪（证据 = `s4_run_monitor.log` 自身）**

监控每 report 都打 `visited_infosets`，但上文只引了 `sample_active=169/169`（那是 169 个 preflop 根类的样本覆盖，
50M 就满、无信息）。真正该看的**全表覆盖**：

| update | visited_infosets | 占 230.5M |
|---|---:|---:|
| 50M | 43.4M | 18.8% |
| 200M | 81.1M | 35.2% |
| 500M | 108.0M | 46.8% |
| **1B** | **128.2M** | **55.6%** |

到 1B **44% 的 infoset 一次没访问、曲线仍线性爬**（末 50M 更新 +1.5M 新 infoset）。这不是收敛，是训到一半。
`ConvergenceMonitor` 只采 169 个 preflop 根（各 ~75 万访问），其「稳定」≠ 全表收敛；`avg_pos_regret→0.0002` 也机械
（分母 = 全局 update_count，对任何 visit ≪ T 的节点都趋 0，1B/10B/100B 皆然，**不构成停止判据**）。

**② preflop 噪声量化 = 欠训练实锤**（`/tmp/preflop_noise.py`，域内 kicker 支配单调性）

支配族内（如 suited aces AKs≻AQs≻…≻A2s，preflop equity 严格单调）收敛策略 raise 频率应单调。实测**严格支配对翻转
14.1%**（>0.05 阈；A7o raise 0.96 vs A8o 0.01；K5s 0.93 vs K6s/K7s≈0；AQs 0.59 vs AKs 0.12），随范围变宽
UTG→SB 升 9.6%→18.4%。机制：preflop 根访问够（~75 万），但它在对**欠训练的 postflop 续局**做最优反应 → 叶子噪声
灌进根 →「稳定地错」。这解释了上节的非单调点：不是抽象的混合均衡，是噪声。

**③ 备选方向（含用户提的 500 桶 / N=4）实测会 backfire** —— 都 ×infoset → 同预算覆盖率更差：

| 方向 | 实测 | 结论 |
|---|---|---|
| N=4（width redirect）| sizing **1.445B infoset / 48 GiB**（6.3× N=3） | 内存够但同 1B 预算 ≈ 9% 覆盖 |
| 500 桶 | postflop infoset ×2.5 | 同理，除非训练量同步放大 |
| 200 桶坏了? | `bucket_quality_dump`：**0 空桶、intra-std flop .027/turn .033/river .065**（≈健康 500 桶档） | **桶健康，排除** |

→ 弱点不在桶、不在 postflop 宽度，在**覆盖率 + 太平抽象**（抽象把 EV 压平 → 噪声随便填 → limp basin）。

**④ 修法 = preflop reshape（落地 commit `fdc66db`）** —— 上节「去 limp / 加小开池」的建议落地为代码
（`src/training/nlhe_betting_tree.rs`）：

- `BettingAbstractionRules.no_open_limp`：preflop 未加注时删非 SB 位 open-limp `Call`（强制 raise-or-fold，
  SB complete 保留）。
- `first_small_preopen_6max`：preflop 菜单 `{0.5,1.0}`（0.5=2.25BB 开池档；`drop_small_reraise` 改 **raises-aware**
  → postflop 与历史 byte-equal、preflop 0.5 开池得以保留、3bet+ 仍 1.0pot）。

sizing（节点确定、机器无关、本机可信）：

| profile | infoset | 两表 | vs baseline | 治 |
|---|---:|---:|---|---|
| baseline（现 1B 用的）| 230.5M | 8.04 GiB | — | — |
| **nolimp**（+no_open_limp）| **55.2M** | 1.91 GiB | 0.24×（缩 4.2×）| ① limp |
| **preopen**（+2.25BB 开池）| **157.9M** | 5.46 GiB | 0.68× | ①+② 开池档太大 |
| **preopen-small**（preflop 单开池档·便宜 2.25BB，`be3b0e7`）| **103.0M** | 3.57 GiB | 0.45× | ①+②（preopen 与 nolimp 之间：单档但范围更宽） |

两者都比 baseline **小** → 同 1B 预算覆盖率从 56% → 接近饱和 → 噪声塌 + 目标更接近 GTO。**这是「更小且更好」，
正打 binding constraint（覆盖率），而非用户初想的「改大/加桶」。** byte-equal 守住：240096(HU)/78852(N=2)/
1154822(N=3)/719764 cross-check 全绿 + 结构守门 `reshape_root_drops_open_limp` + `reshape_preopen_small_single_open_size`（两旗默认关，新字段不动既有 4 个节点数）。`--reshape
{none|nolimp|preopen|preopen-small}` 接进 `train_cfr` / `six_max_eval` / `nlhe_dense_preflop_169_dump`。

**⑤ nolimp 1B 实测结果（2026-06-01→02 完成，`artifacts/run_6max_s4_nolimp/`，wall 7.5h / 37k·s）** —— 诊断链证实、自带因果：

与 baseline 同 1B / 同 200 桶、只换 betting 抽象的干净对照。preflop 支配翻转率（域内 kicker 支配单调性，`/tmp/preflop_noise.py`）：

| 位置 | baseline 1B | nolimp 200M | nolimp 1B |
|---|---|---|---|
| UTG | 9.6% | 3.4% | 2.3% |
| HJ | 11.0% | 2.3% | 0.3% |
| CO | 14.5% | 2.0% | 0.3% |
| BTN | 17.1% | 3.7% | 0.5% |
| SB（对照，未删 limp）| 18.4% | 19.1% | 18.5% |
| **非 SB 合计** | **13.0%** | 2.9% | **0.85%** |
| 总 | 14.1% | 6.1% | 4.4% |

- **删 limp 的 4 个非盲位：13.0% → 0.85%（15× 干净）。** AA/KK/QQ/AKs 全 raise 1.00、limp 结构性 0；baseline
  「KK limp 74%、QQ 84%」消失，RFI 干净单调近 GTO。**且在 200M（≈baseline-1B 的 visits/infoset 密度）就塌
  → 是抽象不是算力。**
- **SB（故意保留 limp 作对照）：18.4%→18.5% 纹丝不动、仍 limp AA(agg 0.43)。** 满 1B 也没救 → SB 翻转率高**不是欠训练**，
  是 limp + 3.5BB 单档造的平 EV 混合区（⚠ 后经 GTO Wizard 真值证实**这是 GTO 特征、非病**——SB limp/AA-limp 本就 GTO，
  见⑥/⑦ 2026-06-02 修正）；残留 4.4% 总噪声里 **120/142 条来自 SB**。**对照组证因果**：动了的塌、没动的不动。
- 覆盖率 56%→**64.5%**（树小 4.2× 的收益；非盲位 RFI 相关子树已训透，与全表 64.5% 不矛盾——剩余未访问是深层多人线）。
- **诚实限制**：nolimp 干净**但偏紧**（BTN raise 37%、UTG 14%，窄于 GTO）——只有 3.5BB 大档，EV 划算开池范围本就窄。
  这正是 preopen 要补的。

**⑥ preopen 部分结果（2026-06-02，AWS 5B run，2.1B 处暂停，checkpoint 存 vultr `artifacts/run_6max_s4_preopen/`）—— 只到 1B/2B，证据有限、不下定论**

5B 目标 / 每 1B 一个 checkpoint（专为看「训练是否充分」）；跑到 ~2.1B 用户暂停（LCFR 不可 resume，余 ~2.9B 未跑）。
preopen = nolimp + preflop `{0.5,1}`（0.5 = 2.25BB 开池档，非 SB 仍禁 limp、SB 保留 limp）。各位置支配翻转率：

| 位置 | nolimp 1B | preopen 1B | preopen 2B |
|---|---|---|---|
| UTG | 2.3% | 0.2% | 0.2% |
| HJ | 0.3% | 0.5% | 0.2% |
| CO | 0.3% | 0.2% | 0.2% |
| BTN | 0.5% | 0.6% | 0.5% |
| SB | 18.5% | 17.7% | 17.1% |
| 总 | 4.4% | 3.8% | 3.6% |

（覆盖率：preopen 1B = 56.6%、2B = 65.9%。）

- **非盲位（证据足，可下结论）**：preopen 1B 即干净（0.2–0.6%）**且范围放宽到接近 GTO**——BTN raise 37%(nolimp)→
  **44–45%**(preopen)、UTG 14%→18%。证实「加 2.25BB 开池档」修好了 nolimp 偏紧的毛病（诊断问题②）。1B→2B 无变化 =
  非盲位 1B 训练已充分。
- **SB（外部 GTO 真值已可下结论，2026-06-02 修正）**：preopen 便宜档让 SB limp 43%→30%、AA raise 43%→68%、
  仍 limp AA ~32%、翻转率 ~17%（1B→2B 17.7%→17.1% 近乎平）。**原判「SB 仍 over-limp / AA 还在 limp = 残留病」判错**
  ——拿 GTO Wizard `Cash 100bb / 6max cEV`（chip EV 无 rake，与本 solver **同码深、同无 rake**）folded-to-SB 节点
  交叉验证真值：
  - 真 GTO 的 SB **就是 limp 为主**：Call(limp) **49.3%** / Raise 3.5 17.7% / Fold 32.9% / Allin 0%。
  - 连 **AA 都 raise 52.9% / limp 47.1%**（limp-reraise 设陷 + 保护 limp range）。
  - 推论①：本 solver 的 SB limp 30–43%、AA limp ~32% **不是过度 limp，反而比真 GTO（limp 49% / AA limp 47%）还少**
    —— SB **没病**，宽 limp 混合（含 AA limp）是 blind-vs-blind 的核心 GTO。
  - 推论②：SB 翻转率卡 ~17%（非盲位 →0.5%）是**平 EV 重混合区的排序噪声特征**——大量牌 limp/raise≈50/50 → 域内
    raise 频率全挤在 50% 附近 → kicker 支配排序天然被噪声主导、即便收敛也翻。**不是欠训练，跑满 5B 也不会明显降。**
  - caveat：GTO Wizard 抽象更细 + 带 cold-call 2.5x，49.3/47.1 非本 solver 精确目标；**定性结论稳**——SB limp 占半、
    AA limp 占半是 GTO。
- **GTO 参照（按位置分，纠正旧表述）**：
  - **非盲位（UTG/HJ/CO/BTN）**：rake-free 下也几乎不 open-limp → `no_open_limp` + premium raise ~100% 正确、匹配
    GTO（⑤已验）。
  - **SB（blind-vs-blind）**：100bb cEV 下 limp 才是最高频（GTO Wizard 实测 SB limp 49.3% / AA limp 47.1%）。判 SB
    好坏**不能用「limp 低 / AA raise 100%」**——那是非盲位的尺子、对 SB 正好反了；SB 的 limp 频率本就高，要看的是 limp
    range 结构是否合理（弱牌 limp-fold、强牌混 limp-reraise），而非 limp 频率趋零。

**⑦ 待办**

- ~~**SB 悬而未决**~~ **SB 已结**（见⑥外部 GTO 真值）：SB limp / AA-limp 是 GTO、非缺陷 →
  (a)「跑满 5B 看 SB 翻转率」**不必做**（混合区，5B 也不降）;(b)「删 SB limp」变体**不做**（会主动偏离 GTO 的 limp 49% 目标）。
- 真正未决的只剩**实测对战是否更强**——属 S5（仍缺强参考对手）。reshape 只保证目标更干净 + 训得更透，不直接 = 更强。

### S5：6-max 评测重构

> **2026-06-02 定方向**：S4 已把三个 blueprint 训出来（baseline 1B、nolimp 1B、preopen 1B/2B），但**没一个被验证「更强」**
> ——S4 gate（vs random/call-station/overly-tight）是必要非充分且非单调，reshape「只更干净不直接更强」（S4续⑦）。
> 故 S5 的第一要务 = **证伪/证实「reshape 真的产出更强 blueprint」**。两条腿（相对 + 绝对），**共用一个 6-max off-tree advisor 引擎**（见下「关键工程发现」）。

**① 相对强度 = blueprint 互评（受控 A-vs-B）**

- 一张权威 `GameState`（规则引擎，N 座）跑自对弈，每座由一个 blueprint advisor 驱动；hero 坐 A、其余坐 B，
  循环赛 {baseline, nolimp, preopen} → 按位置拆 mbb/g + CI95，固定 seed 可复现。chip-EV 桌内零和 → 「A vs 全 B」净额就是 A 对 B 的边际。
- **复用现成**：`tools/nlhe_checkpoint_vs_checkpoint`（HU、**同树** A/B lockstep）+ `evaluate_blueprint_vs_baseline_multiway`
  （N-generic、按位置拆）。**但 baseline/nolimp/preopen 是不同 betting tree**（nolimp 删 open-limp、preopen 加 2.25BB 档）
  → 同树工具用不了，必须走下面的 off-tree advisor（这是真正的工作量，不是「小扩展」）。

**② 绝对强度 = OpenPoker 实测场（外部真实对手）**

- `openpoker.ai`（2026-06-02 发现）**正好补 line-36「无强 6-max 公开参考 bot」的缺口**，且格式与 `default_6max_100bb()` 高度对齐：
  6-max / 默认买入 2000@10-20 = **100BB** / 虚拟筹码 chip-balance 计分 = **无 rake**（已确认）/ WebSocket API / 支持 Rust / 免费。
- API：`wss://openpoker.ai/ws`，`Authorization: Bearer <key>`；server→bot `hand_start`/`hole_cards`/`your_turn`/`player_action`/
  `community_cards`/`hand_result`，bot→server `join_lobby{buy_in}`/`action{hand_id,action,amount,turn_token,client_action_id}`
  （raise `amount` = **总 to 额**，介于 valid_actions 的 min/max）。牌字符串 `Ah`/`Ts`（与 Slumbot 同）。详见
  `docs/temp/openpoker_client_design_2026_06_02.md`。
- 对手 = **其他开发者 bot 池 + 排行榜**（非固定强基准）→ 给「活的竞争场排名」，不是 Slumbot 式绝对基准；但对 6-max 这其实更接近真实质量。
- **caveat**：(1) rebuy + 买入 50–250BB + 赢家越打越深 → **有效码深会偏离 100BB**，靠 off-tree 映射兜、深码精度降（买入锁 100BB）；
  (2) 是 lobby 混合桌、非受控配对 → 测「我方最强 blueprint 对真实场强不强」，**变体之争仍归①互评**；(3) 免费号 1 bot →
  三变体只能分时段轮换或上 Pro；(4) 120s 行动超时 / 20 msg/s / 10 conn/min/IP；turn_token 一次性防重放。

**关键工程发现（①②共用一个引擎）**

- 跨抽象对弈（①的 A≠B 树、②的对手任意下注）= 同一个问题：**一张权威 `GameState` + 每个 blueprint 各持「抽象影子」`SimplifiedNlheState`，
  每个 applied 动作经 off-tree 翻译推进各自影子**。
- **off-tree 引擎已存在**：`ActionAbstraction::map_off_tree`（`src/abstraction/action.rs`，generic）；`tools/slumbot_advisor.rs`
  是**可工作的 HU 模板**（real+abs 两态 lockstep replay、`map_off_tree`→`project_tag_onto`→双态 apply、incoming/outgoing 翻译）。
- 真正要做 = **把 slumbot_advisor 的 off-tree 核（`replay`/`resolve_actions`/`project_tag_onto`/`outgoing_*`）从 HU binary
  抽进可复用 `src/` 模块 + 去 `default_hu_200bb` 硬编码、参数化 N 座/座位/TableConfig** → ①自对弈互评工具 与 ②OpenPoker WS 客户端
  都是它的薄壳。属**正确性关键**代码（off-tree 翻译）→ **必须 vultr 跑 HU 回归 + 新 6-max 单测**后才可信。

**③ 通用评测项（沿用）**

- **多对手 AIVAT** 降方差（6-max 方差远大于 HU、无 LBR 捷径 → 比 HU 更刚需；现有 `aivat*.rs` 单对手要推广）。先出裸 mbb/g 拿方向，CI 太宽再上 AIVAT。
- LBR 仅作诊断（注明多人理论局限），不当质量闸门。
- 固定 seed 可复现；每候选出评测报告 + 策略版本哈希。

**④ 已定的「不做」**

- **不跑 preopen 5B**（S4续⑦：非盲位 1B 已够、SB 是 GTO 混合区 5B 也不降）。preopen-2B（覆盖 65.9%）就是进 S5 的「干净」blueprint。

### S5 续（2026-06-02）：① off-tree 引擎落地 + 跨抽象 h2h + 两个正确性/方法学发现

**① 共用 off-tree 引擎落地 + vultr 验证**（commits `1514ca2` 引擎 / `a93670e` 测试 / `33bcb60` 并行）：

把 `tools/slumbot_advisor.rs` 的 off-tree 核抽进 `src/training/blueprint_advisor.rs`、去 HU 硬编码、参数化 N 座/
`TableConfig`（§6 共用底座）：
- 抽出 `parse_card`/`find_tag`/`project_tag_onto` + `outgoing_action`（concrete `Action` 版，真实 pot 算尺寸）；
  slumbot `outgoing_incr` 改薄壳调 `outgoing_action` 再字符串化 → **byte-equal**（vultr 跑 slumbot T2..T5 8 测全绿）。
- 新增 `advance_shadow_by_applied`（incoming：用 applied 真实动作以**影子自身几何** map_off_tree 推进单影子）+
  `play_cross_abstraction_hand`（一张权威 `GameState` + 每 distinct blueprint 一份影子；行动者影子按字面所选动作、
  其余按 incoming 推进）+ `evaluate_cross_abstraction_h2h`（rayon 并行独立手，~4× on vultr，bit-可复现）。
- 工具 `tools/six_max_blueprint_h2h`：加载 ≥2 不同 betting tree 的 dense blueprint，跑有序对 (hero,field) 的
  mbb/g + CI95 + 按位置拆 + desync 计数。
- vultr 验证：`blueprint_advisor` 5 单测 + slumbot 8 回归全绿；并行版与串行 **bit-identical**（同 seed 同结果）。

**② OpenPoker 客户端是此引擎薄壳**，待用户注册账号解锁（§7 步 3-4，需用户邮箱 + api_key）。

**发现 A（正确性边界，钉进引擎）：off-tree 只忠实「下注尺寸」差异，不解决「结构性」动作集差异。**
当两抽象动作集结构不同（典型：`baseline` 含 open-limp、`nolimp`/`preopen` `no_open_limp`），limp 进的池在
no-limp 影子里**没有对应节点** —— 把被动 `Call`/`Check` 静默映射到 `AllIn` 是 passive→aggressive 的 kind 变，
会污染回合顺序 / 价值。引擎对此**显式报 `HandError::Desync`**（不静默塌 AllIn），评测层计数 + 排除该手。
实测（真 avg-strategy，N=3）：
- **`nolimp` × `preopen`（都 no-limp、仅 preflop 开池 2.25 vs 3.5BB 尺寸差异）：desync 0–0.8%** = 纯尺寸、干净、可信。
- 牵涉 `baseline`（含 limp）的对：limp 高发 → 大量 desync = 结构性 gap，**结果不可信**。
→ **S5① 的受控互评只能在 `nolimp`×`preopen` 之间做干净对比**；`baseline` 的相对强度无法经 off-tree 干净测，
须靠 ②外部实测、或重训一个 limp-representable 的 baseline（含 open-limp 节点供影子表达）。

**控制实验（引擎无偏）**：`nolimp` vs `nolimp` 自对弈（同 ckpt，36k 手）= **−60.7 ± 58.5 mbb/g，CI 跨 0 ≈ 0，
0 desync** → 孤身 hero 轮坐全 6 座 vs 同策略 field 无系统偏置（per-position：blinds 负、BTN/HJ 正，符合位置 EV）。
（`preopen`-self 未测：vultr 11 GiB 装不下 2×preopen 5.46 GiB ≈ 11.4 GiB；需 strategy-only 加载或更大机。）

**发现 B（方法学 confound）：lone-hero-vs-field 跨抽象 h2h 把「内在强度」与「抽象粒度误感知税」混在一起，
不能干净排名 reshape 变体的*内在*强度。** 机制（粒度不对称系统性偏向细抽象 hero）：
- Run A（`nolimp` hero vs `preopen` field）：nolimp 把 preopen 的 2.25BB 小开池误感知成 3.5BB（更大）→ 过度
  respect → 劣势；且 nolimp 自己只开 3.5BB、被 preopen field 正确感知 → 无「小开池骗 fold」红利。
- Run B（`preopen` hero vs `nolimp` field）：preopen 正确感知 nolimp 的 3.5BB 开池；且自己的 2.25BB 小开池被
  nolimp field 误感知成 3.5BB → field 过度 fold → preopen 得红利。
两效应都偏向 preopen-hero、不利 nolimp-hero —— **与「preopen 内在更强」无关，是粒度不对称的产物**。
→ h2h 数字是**「部署场景」**（孤身 bot vs 一桌 field = ② OpenPoker 的模型）相关的**方向读数**，
**不是干净的内在强度排名**。干净的内在强度测需要 (a) 一个能表达两者的公共细抽象（未训）、或 (b) ②外部实测
（两者各自 vs 同一真实 field；免费号 1 bot 只能分时段轮换）。

**实测 h2h（`nolimp`×`preopen`，各 100k 手/座 = 600k 手/有序对，seed 0x53354831，vultr 4 核并行，2026-06-02）**：

| 有序对 | hero mbb/g | SE | CI95 | desync | 判定 |
|---|---:|---:|---|---:|---|
| `nolimp` hero vs `preopen` field | **−4.3** | 13.6 | [−30.9, +22.4] | 1.09% | 未分出（跨 0）|
| `preopen` hero vs `nolimp` field | **+15.6** | 14.1 | [−12.1, +43.3] | 0.06% | 未分出（跨 0）|

- **结论：600k 手/座（SE~14）下两 reshape 变体相对强度无显著差异（双向 CI 均跨 0）。** preopen-hero 点估 +15.6
  略正、nolimp-hero −4.3 ≈0，方向略偏 preopen —— 但 (a) 均不显著、(b) 正好是「发现 B 税」系统性偏向 preopen-hero
  的方向 → **不能归因于内在强度**。即「加便宜 2.25BB 开池档（preopen）是否产出更强 blueprint」= **受控 h2h
  测不出显著差异，两变体强度相当**（差异上界 ~43 mbb/g，相较 blueprint 打 call-station 的 +3738 是 ~1%、可忽略）。
- desync（1.09% / 0.06%）实测证实 nolimp×preopen 无结构 gap、对干净（1.09% 是 nolimp 把 preopen 小开池误感知后
  在 all-in 边界的偶发尺寸 desync，被引擎检出排除，远 < 2% 可信阈）。

**下一步**：①引擎 + 工具 = S5 共用底座已就位。① 干净对比受「无结构 gap」+「税 confound」双重限制 → 已得「两变体
强度相当」的方向结论；要分辨内在强度（若差异真 < ~15 mbb/g）须靠 ②外部实测（税 confound 在 h2h 里无法消除）。
**待用户决策**：(a) OpenPoker 账号注册解锁 ②（cleaner arbiter，§7 步 3-4）；(b) reshape 两变体强度既相当，
部署选哪个可由 ②或其它准则（如 preopen 范围更接近 GTO，S4续⑥）定，不必再纠结 ①。

### S6（非目标，parked）：实时 depth-limited search

路线 B / `pluribus_path.md` 阶段 6。Pluribus 真正赢人的机制，但占总量 30–40%、极易写错。**blueprint-only
闭环（S1–S5）稳定后再单独立项**，不在当前承诺范围。

## 明确非目标

- 当前不做多人 continual re-solving / biased leaf strategies / 完整 Pluribus 复现。
- 当前不追求 Pluribus 级超人类 6-max 质量（blueprint-only 单独达不到，这是已知的）。
- 不以线上牌局自动化为目标。
- 不换神经网络路线（ReBeL / 深度 RL）——与当前纯 Rust tabular-CFR 基建脱节，如未来重估再另议。

## 算力 / 主机假设

- 6-max blueprint 训练预期 ≥ Pluribus 量级（论文 64 核 / 8 天 / ~10¹¹ updates）。具体机型/时长**待 S2 sizing
  定数后**按 `feedback_high_perf_host_on_demand` 报预算给用户起机；HU 的 c6a.8xlarge 大概率不够，需更大/更久。
- dense 表更刚需（6-max infoset 可能十亿级）；bucket kmeans 拟合成本参考 HU 实测（c6a.8xlarge 量级），
  6-max 多人特征 + 6 位置会更贵，S3 先量。

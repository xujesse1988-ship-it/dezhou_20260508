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

- 用 ES-MCCFR / LCFR + dense 后端，6 traverser 轮流更新（trainer 已支持，无需改算法）。
- **多人收敛监控**（多人 CFR 可能震荡/regret 线性增长，必须能告警定位）：实时输出 average-regret 增长曲线
  （应 sublinear）、策略 entropy、动作概率震荡幅度。
- 单次训练连续 24h 无崩溃 / 无内存泄漏；checkpoint 保存恢复后训练曲线连续、策略查询一致。
- first-usable blueprint 门槛：update 数按 S2 量出的 infoset 数定（HU "100M 饱和"经验**不照搬**——预期
  10¹⁰–10¹¹ 量级，对标 Pluribus）。
- 门槛：blueprint-only 在 `1,000,000` 手评测中稳定击败 random / call-station / tight-aggressive 三类基线
  （必要非充分）。

### S5：6-max 评测重构

- head-to-head 实测对战（多人 baseline）：输出 mbb/g、SE、置信区间、**按位置拆分收益**。
- **多对手 AIVAT** 降方差（6-max 方差远大于 HU，且无 LBR 捷径 → AIVAT 比 HU 更不可或缺；现有 `aivat*.rs`
  单对手要推广）。评测报告同时出原始与方差缩减后的 mbb/g。
- LBR 仅作诊断指标（注明多人下的理论局限），不当质量闸门。
- 解决"无强参考对手"：可拿 HU 训的 blueprint 摆 6 座当 baseline，或自对弈不同 6-max blueprint 互评。
- 固定 seed 评测可复现；每个候选策略出评测报告 + 策略版本哈希。

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

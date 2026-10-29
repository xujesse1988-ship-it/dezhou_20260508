# 翻前行动 → 对手范围：现状问题与脱锚时的替代方案（调研，2026-10-29）

代码口径：`fa72600`（分支 `feat/real-hand-forced-deal`）。只读调研，**没改代码**。
标注约定：**[事实]** = 代码逐行确认；**[推断]** = 有代码 / 数据支撑但没直接验证；**[计算]** = 按代码公式手算的示意数。

## 0. 结论

1. **现在的画像修正有两处方向性错误，建议先修或先关**（§2.1、§2.2）：BB 免费过牌 / BB 防守被套成「VPIP 中段」，
   3bet / 4bet 线被套成「PFR 宽度」。两处都让对手范围明显偏离实际，而且 poker_manager 实盘配置
   （`--exploit on` + `--search-force-unanchored on`）每个翻后决策都会走到。
2. **结构性问题是「混合」而不是「条件化」**（§2.3）：画像分布 `w` 只看全局 VPIP / PFR 和 Chen 序，不看这条线上的具体动作，
   再以 α=0.5 跟 blueprint 范围做凸混合，等于把一半质量换成和行动线无关的分布，越是信息量大的线（3bet、4bet）稀释越重。
3. **推荐方案：blueprint 当先验排序 + 按对手在该局面的实测频率做指数倾斜 + 沿行动线连乘**（§4）。
   这是 Ganzfried & Sandholm 的 DBBR 思路在「单个决策点」上的最小 KL 解，数学上就是把每个决策的
   `σ(a|h)` 换成 `σ(a|h)·e^{θ_a}` 再归一，`θ` 由实测频率解出。它同时解决：画像跟行动线脱节、位置无关、
   Chen 序太粗、150 手硬门、`ranges_opt=None` 时画像失效、断点之后的动作被整段丢掉。
4. **代码可以显著变简单**（§4.5）：对手侧把「前缀 reach + 跳 AllIn + limp 当弃牌前缀 + limp_raisers 删步 + 均匀混合 +
   ExploitShape / width_band / Chen 倾斜 / preflop_entry_kinds」收成一条「每个对手决策 → 参考策略 → 倾斜 → 乘」的流水线；
   hero 侧把只覆盖 limp 的 `hero_limp_coef` 换成通用的「按 hero 当时实际用的决策函数重放」，顺带修掉 PM-17 等其它偏离。
5. **先做评估工具再改算法**（§5）：用 hh 日志里的摊牌牌（只用日志，不用 `/api/hands`）算各方法对亮牌的交叉熵，
   现方法 / 关画像 / 新方法同口径比较。没有这个尺子，§4 的收益无法确认。

## 1. 现状链路（脱锚搜索，生产路径）

| 步骤 | 位置 | 做什么 |
|---|---|---|
| ① lockstep 重放 | `openpoker_advisor.rs:1441` `lockstep_replay` | 真实局与 100BB 影子并行推进，第一处结构性失败处停，记 `synced_node`；`--search-limp-as-fold` 时平跟在影子里记弃牌 |
| ② 前缀决策 | `:1598` `unanchored_prefix_decisions` | `synced_node` 之前的决策三元组，删掉 hero 被映射的平跟步、hero 由 limp 规则决定的步、「平跟后又加注」的对手的映射弃牌步 |
| ③ hero limp 系数 | `:1714` `hero_limp_coef` | 对 hero 每个由 limp 规则决定的翻前步，逐 1326 组合重跑 `limp_heuristic` 得 0/1 系数 |
| ④ 估范围 | `subgame.rs:1463` `estimate_range` | 每座乘自己的 blueprint `σ(a|h)`，**跳过 AllIn tag**；空前缀且无 hero 系数 → `ranges_opt=None`（均匀） |
| ⑤ 均匀平滑 | `subgame.rs:1552` | 对手 `0.75·r + 0.25·u` |
| ⑥ 画像修正 | `subgame.rs:1665` `apply_exploit_width_prior` | 对已收敛对手：`0.5·r + 0.5·w`，`w` = Chen 序上一段带权 1、其余 0.05；段由全局 VPIP / PFR + 本手入池方式（`preflop_entry_kinds`）选 TopK / CallBand / RaiseBand |
| ⑦ 跨街 | `subgame.rs:1890` | 上一街有解时直接用后验，④–⑥ 不再算 |

跟范围直接相关的代码约 1300 行（advisor 约 650、subgame 约 350、opponent_profile 约 300），
光是保证预热和决策两路算出同一份范围、命中同一个缓存 key，就有多处「两路必须同源」的注释和测试。[事实]

复杂度主要来自**每发现一种 blueprint 表达不了的情况就加一个特例开关**：AllIn 跳过、limp 当弃牌、
平跟者再加注时删掉弃牌步、hero 的 limp 步换系数……每个特例都对，但互相叠加后，一个座位的范围到底由哪几步、
哪种语义决定，要读完 ②③④⑥ 才知道。

## 2. 发现的问题（按影响排序）

### 2.1 BB 免费过牌、BB 防守被套成「VPIP 中段」 —— bug

- `preflop_entry_kinds`（`openpoker_advisor.rs:3754`）把 `"call" | "check"` 都归为 `Passive`，注释明说含 BB option check。[事实]
- `Passive` → `CallBand` = Chen 序 `[ceil(pfr·n), ceil(vpip·n))` 这一段权 1、其余 0.05。[事实]
- 但 Profiler 的 VPIP **不计过牌**（`opponent_profile.rs` `observe_hand`：`Fold | Check => {}`），也不分位置。[事实]

BB 免费过牌的真实范围 ≈ 除了会加注的牌之外的所有牌；BB 面对开池的防守范围也远宽于该玩家的平均 VPIP。
用 VPIP 中段去描述它们，方向是系统性地把 BB 想得太强。

示意（翻牌后 1176 个有效组合，VPIP 25%、PFR 10%，BB 在前缀里没有决策 → 先验均匀）：[计算]

| | CallBand 中段（Chen 前 10%–25%） | 其余（Chen 25% 之后） |
|---|---:|---:|
| 实际（过牌范围 ≈ 去掉 Chen 前 12% 加注牌后均匀） | ≈ 15% | ≈ 85% |
| 现代码（α=0.5） | ≈ 46% | ≈ 47% |

触发条件：`--exploit on` 且 BB 的 PFR 也收敛。poker_manager 的 `unanchored_exploit_live_20261023` 就是这个配置，
加上 `--search-force-unanchored on`，平跟池、BB 防守池的每个翻后搜索都会用到。[事实]

**最小修法**：盲位的 `check` 不进 `Passive`，并且 BB / SB 没有主动入池（只过牌或只补齐 / 防守跟注）时这一座不做画像修正
（`TopK` 用的也是全局 VPIP，对 BB 同样不对）。长远看 §4 的方案按位置 × 局面统计，这个问题自然消失。

### 2.2 3bet / 4bet 线被套成「PFR 宽度」 —— bug

`Aggressive` → `RaiseBand` = Chen 序前 `ceil(pfr·n)` 个，不管这次是开池、3bet 还是 4bet。[事实]
PFR 是「翻前加注过的手占比」，基本等于开池率；4bet 范围通常只有它的十分之一左右。

示意（4bet 者，blueprint 4bet 范围 ≈ 30 组合 QQ+/AK，PFR 18%）：[计算]

- 经过 ⑤⑥ 后，这 30 个组合只占 ≈ 44% 的质量；Chen 前 18% 里的其余牌（99–JJ、AQ、AJs、KQ…）占 ≈ 37%，剩下是均匀尾巴。
- 也就是说，求解器认为对手 4bet 后一半以上时间不是 QQ+/AK。对手如果真是「4bet = 坚果」型，这个修正的方向和剥削意图正好相反。

另：`preflop_entry_kinds` 把 `all_in` 一律记 `Aggressive`，短码**跟注全下**也会被套 RaiseBand；
Profiler 同样把 `all_in` 记成加注，短码玩家的 PFR 被抬高。`action_kind`（`:1653`）已经有按合法集区分的写法，这两处没用上。[事实]

### 2.3 画像是「混合」不是「条件化」

`r' = (1−α)·r + α·w` 里 `w` 不依赖这手牌的行动线（只依赖全局 VPIP / PFR 和一个三选一形状）。[事实] 后果：

- **信息量越大的线稀释越重**：开池跟注线 `r` 本来就宽，混进 `w` 变化不大；4bet 线 `r` 很窄，混进 `w` 后被拉宽一大截（§2.2）。
- **双峰**：limp 当弃牌时，平跟者的 `r` = 「该位置开池范围之外的牌」（以垃圾牌为主），`w` = Chen 中段；两者混合得到「一半垃圾 + 一半中段」，
  两者都不像真实平跟范围。[推断]
- **跟 25% 均匀平滑叠加**：先混 25% 均匀再混 50% `w`，blueprint 部分只剩 37.5% 权重。

正确的做法是**把画像当成对每个决策的似然修正**（乘法），让每一步的动作都起作用。§4 就是这个。

### 2.4 最需要画像的地方反而不用

- `apply_exploit_width_prior` 遇到 `ranges_opt=None` 直接返回（`let mut ranges = ranges_opt?`）。[事实] 前缀为空时（满桌 UTG 首动作 limp 且没开 limp 当弃牌），
  blueprint 一点信息都没有，画像是唯一信号，却被跳过。
- 断点那一步和之后的翻前动作全部按「无信息」处理（因子 1）。[事实] 例如短码 open shove → 有人 3bet 隔离 → 原 open 者跟注：
  shove 被跳过（`skip_all_in`）、3bet 者在断点后，进翻牌时两个人都几乎是均匀范围。

### 2.5 hero 范围只修了 limp 一类偏离

hero 范围 = blueprint 沿 hero 走过的线算的 reach（hero 不混均匀）。hero 的翻前动作不来自 blueprint 时，真实手牌在自己范围里的权重可能接近 0。
`hero_limp_coef` 修了 limp 规则这一类；以下几类仍按 blueprint σ 算：[事实]

| 偏离来源 | 位置 | 例子 |
|---|---|---|
| PM-17 4bet 剥削 | `fourbet_adjust` | blueprint 弃牌的牌被改成 4bet，影子照常映射到 raise1pot，hero 范围 = blueprint 4bet 范围 |
| 翻前全下启发式 | `preflop_allin_response` | 按 `allin_range_width` + 胜率跟注 |
| 底池赔率地板 | `fallback_with_floor` / `limp_heuristic:pot_odds_call` | blueprint 弃牌的牌被改成跟注 |
| 抽样修剪 | `prune_for_sampling` | 低于 5% 的弃牌被去掉（影响小） |

强制发牌（`real_hand_forced_deal_design_2026_10_25.md`）解决的是访问量，解决不了「解里的对手以为 hero 范围更强」。
`bp2_115653` 分析 §3.1 已经指出过同一类问题。[事实]

### 2.6 其它不一致

- 对手全下（AllIn tag）被跳过 → 全下者范围 ≈ 均匀；而 hero 自己面对全下时用 `allin_range_width`（按码深的全下宽度表）算胜率。
  同一个人的全下范围，hero 决策和翻后搜索用了两套假设。[事实]
- 画像只有全局 VPIP / PFR，而 `opponent_analysis/spots.py` 已经在算分位置的 `rfi`（raise / limp / fold）、`vs_open`（fold / call / 3bet）、
  `vs_3bet`、`steal_defense`。[事实] Rust 侧没用上这些更细的统计。

## 3. 可选方案调研

| 方案 | 出处 | 思路 | 对我们 |
|---|---|---|---|
| A. 自身策略追踪 + 离树动作映射 | Pluribus（Brown & Sandholm, Science 2019） | 所有人的范围都按 bot 自己的策略算；翻前离树尺寸用伪调和映射到最近尺寸，偏差大（且人少）才在翻前做实时搜索 | 这就是我们的锚定路径。解决不了「对手根本不按均衡打」和「blueprint 没有这个动作（limp）」 |
| B. 嵌套子博弈求解 | Libratus（Brown & Sandholm, NeurIPS 2017 *Safe and Nested Subgame Solving*） | 离树动作加进树，从上一个在树节点的范围重解，新动作的范围由解给出 | 理论上最干净；翻前多人重解要叶子值，代价大；而且给出的仍是「均衡对手会拿什么牌这样打」，对偏离大的人群不准 |
| C. 均衡先验 + 频率约束的最小偏离（DBBR） | Ganzfried & Sandholm, AAMAS 2011 *Game Theory-Based Opponent Modeling in Large Imperfect-Information Games* | 以均衡策略为先验，在「各公共局面的动作频率 = 观测值」约束下找离均衡最近的对手策略 | **推荐**。KL 距离下的解是闭式的指数倾斜，计算量可以忽略；保留 blueprint 对手牌的排序，只调整宽度 |
| D. 贝叶斯后验（对手策略的分布） | Southey et al., UAI 2005 *Bayes' Bluff* | 对对手策略类型做后验，再对手牌做后验 | 需要策略空间参数化 + 大量摊牌，太重 |
| E. 手写人群范围表 | HUD / 人工 | 按位置 × 局面配范围 | `limp_heuristic` 就是这种。可以作 C 的「参考策略」来源（blueprint 没有节点时），但不该作为最终范围 |

## 4. 推荐方案：参考策略 × 频率倾斜 × 连乘

### 4.1 公式

对对手 `s` 的每个翻前决策 `t`（局面 `c_t`，实际动作类 `a_t ∈ {fold, call/limp, raise}`）：

```text
σ_ref(a | h)            参考策略（来源见 4.2），先加小抖动：σ_ε = (1−ε)·σ_ref + ε/|A|，ε≈0.02
σ_opp(a | h) ∝ σ_ε(a | h) · exp(θ_a)                     ← 最小 KL 投影的闭式解
θ 满足  Σ_h prior_t(h) · σ_opp(a | h) = f_c(a)  对每个 a   ← 2–3 个未知数，迭代比例拟合几轮收敛
range_s(h) ∝ Π_t σ_opp(a_t | h)
```

- `prior_t` = 该座在第 t 步之前的范围（同一遍连乘里顺手得到）。
- `f_c(a)` = 对手在局面类 `c` 下动作 `a` 的频率，带收缩：`f_c = (n_c·f̂_c + k·f_bp,c) / (n_c + k)`，
  `f_bp,c` 是 blueprint 在同一先验下的聚合频率，`k≈30`。**样本少时自动退回 blueprint，不需要 150 手硬门和漂移门**
  （漂移可以用近 N 手统计代替全历史，跟 `opponent_analysis` 的「近期口径」一致）。
- 动作类内部（例如 raise 的不同尺寸）保持 σ_ref 的比例，不单独倾斜。
- `ε` 的抖动替代现在的全局 25% 均匀混合：它按决策起作用，「对手偶尔拿 AA 平跟」这类可能性在每一步都保留，
  而不是最后整体灌一层均匀。

直观理解：**blueprint 决定「哪些牌更可能这样打」（排序），对手的实测频率决定「这样打的牌有多少」（宽度）**。
对手 4bet 率很低 → θ_raise 很负 → 只剩 σ_ref 最偏 4bet 的那几手；平跟率很高 → 平跟范围向下延伸到更弱的牌。

### 4.2 参考策略从哪来

| 情况 | σ_ref |
|---|---|
| 影子同步的决策（锚定前缀） | 该节点 blueprint σ（现状） |
| 断点之后的决策 | 「替身节点」：在 blueprint 里找同位置、同局面类型的节点（例如「面对 HJ 开池的 BTN」代替「面对 HJ 隔离加注、前有一个平跟的 BTN」），查它的 σ。不再整段丢弃 |
| 平跟（blueprint 没有 limp） | 在开池节点上合成三类：raise = σ_bp(raise)，limp 一列用「可玩性」分（例如 blueprint 在同位置面对开池的跟注概率，或 BTN 开池概率）当种子，fold = 其余；再由 rfi 的 raise / limp / fold 实测频率倾斜。得到「会加注的牌之下、能玩的牌」这条带，比「开池范围之外的所有牌」和 Chen 中段都更像真实平跟范围 |
| 短码全下（AllIn tag，现在跳过） | `allin_range_width` 已有的全下宽度表（按 `combos_by_strength` 排序）当 σ_ref，再按对手的全下频率倾斜。hero 决策和翻后搜索从此用同一个全下范围 |

### 4.3 统计口径

局面类直接沿用 `opponent_analysis/spots.py` 的定义，Rust Profiler 照着实现并加对拍测试：

- `rfi` × 位置（raise / limp / fold）
- `vs_open` × 位置组 × 是否有前跟者（fold / call / 3bet）
- `vs_3bet`（fold / call / 4bet）
- `vs_limp`（iso / overlimp / fold；spots.py 还没有，要补）
- BB / SB 的 option（check / raise）

每格样本少，收缩靠 `k` 兜底；格子没有数据就等于 blueprint，不会出错。离线库按现在的约定只做先验
（收缩目标可以从 blueprint 换成「该玩家离线频率」，再往 blueprint 收）。

### 4.4 hero 侧：一个通用重放代替 limp 特例

hero 的翻前决策函数是确定的（给定请求和底牌，输出一个分布）。对每个 hero 过去的翻前步、每个起手牌类（169 类即可，
翻前没有公共牌，只有 limp 规则的 Chen 序和全下胜率有极小的去牌效应），重跑一次 `decide_with` 的翻前部分，
取「实际动作类」的概率当系数。blueprint 路径给 σ（含 PM-17 改过的分布），limp 规则给 0/1，全下启发式给 0/1。

这样 §2.5 的几类偏离一次修掉，`hero_limp_route` / `HeroLimpCoef.nodes` / 前缀里删 hero 步的逻辑都不需要了
（hero 的所有翻前步一律由重放系数表达，前缀里不再放 hero 的决策）。翻前决策很便宜（查表或启发式），
169 × 步数（通常 ≤3）次调用，毫秒级。[推断]

注意：要复用 `decide_with` 本身而不是再写一份分流逻辑，否则又回到「两处必须同步改」的局面
（现在 `hero_limp_route` 就是这样，靠测试 `hero_limp_route_matches_decide` 守着）。

### 4.5 能删掉什么

| 现在 | 之后 |
|---|---|
| `skip_all_in` | 全下用 4.2 的全下 σ_ref |
| `--search-limp-as-fold` 在范围估计里的语义（映射弃牌步当平跟者先验）、`limp_raisers` | 平跟用 4.2 的合成 σ_ref；影子映射只用于定位节点，不再借「弃牌概率」表达平跟范围 |
| `HeroLimpCoef` / `hero_limp_route` / `unanchored_prefix_decisions` 里的 hero 删步 | 4.4 通用重放 |
| `mix_range_with_uniform`（对手） | 每步 ε 抖动 |
| `ExploitShape` / `width_band` / `tilt_seat_to_width` / `chen_score` 排序 / `preflop_entry_kinds` / `pick_shape` | 倾斜直接在每个决策上做 |
| Profiler 三道收敛门 | 收缩 |

粗估对手 + hero 范围相关代码能从约 1300 行降到 500 行以内，「预热与决策必须同源」的点也从三处（前缀、hero 系数、画像快照）
收成一处（`range_inputs(req) -> ranges`）。[推断]

不动的：跨街后验（§1 ⑦）照旧；翻后的动作仍由子树搜索策略条件化。本方案只替换「翻前行动 → 翻牌根范围」这一段。
（翻后也可以用同样的倾斜处理对手的翻后频率，但那是下一步，先不碰。）

### 4.6 代价与风险

- **排序仍来自 blueprint**。对手的牌序跟均衡差很多时（例如拿同花小连张 3bet、拿 AK 平跟），倾斜只改宽度、改不了顺序。
  这类偏差只有摊牌能看出来，§5 的评估会暴露它；必要时允许按摊牌统计对排序再做一次修正（例如某类牌的额外系数），但不建议第一版就做。
- **频率口径错位**：局面分类写错会直接错到范围上。所以统计必须和 `spots.py` 对拍，局面分类函数只写一份。
- **ε 太大**会重新稀释信号；太小则对手偶尔的反常牌（AA 平跟）在解里完全不存在。0.02 是起点，用 §5 的交叉熵调。
- **改动面大**：建议按 §6 分步，每步都能单独 A/B。

## 5. 怎么评估（先做这个）

只用 hh 日志（不用 `/api/hands`），对每个「对手亮牌」的手：

1. 用各方法算该对手进翻牌时的范围 `r`（只用翻前行动）。
2. 记 `−log r(亮出的那手牌)`，以及该牌在 `r` 里的分位。
3. 按方法汇总：平均交叉熵、分位分布是否均匀（校准）。对照组：均匀、blueprint 前缀（不混）、现生产（⑤⑥）、新方法。

**偏差**：亮牌的手偏向走到摊牌的牌（偏强），翻前全下亮牌的偏差最小，可以单独看。几种方法面对的是同一批手，
相对比较仍然有意义，但「绝对校准」要打折扣。[推断]

实现上可以放在 `opponent_analysis`（Python，已有重放和摊牌解析）或写一个 Rust example 读 hh 日志调 advisor 的范围函数；
后者能直接测到生产代码，推荐后者。

## 6. 建议的实施顺序

1. **小修（2026-10-29 已做，未提交）**：§2.1 BB / SB 过牌不进 `Passive`；§2.2 3bet 及以上的线不套 `RaiseBand`（退回不修正）；`all_in` 跟注不记加注。
   如果 `--exploit on` 还在任何实盘上开着，这三处应该先上。
2. **评估工具**（§5），先量出现方法相对「关画像」到底是好是坏。
3. **hero 通用重放**（§4.4）：独立于对手侧，收益明确（修 PM-17 等偏离），同时删掉 limp 特例。
4. **对手侧倾斜**（§4.1–4.3）：先只在锚定前缀上替换画像修正（参考策略 = blueprint 节点），A/B；
   再加替身节点、平跟合成、全下 σ_ref，逐个删掉 §4.5 的旧特例。

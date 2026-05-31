# 下注历史抽象方案：调研意见 + 对抗验证（2026-05-31）

对 `betting_history_abstraction_options_2026_05_31.md` 的评审。方法：多角度联网调研
（动作 translation / depth-limited search / Pluribus 内核 / imperfect-recall 理论 /
public-state DAG / 商用 solver / 神经方法 / bet-size EV 成本 / 多路 width / 2023-26 新进展 /
range 充分统计量）+ 代码回查 + 对每条决策性结论做对抗式 refute。

**读法**：对抗验证里几乎所有结论都判 **PARTIAL**——核心成立、措辞夸大。本文用的是**修正后**
版本。原文档质量很高（"病根是小注不是档数"控制实验、B3 distinct-key ~10⁵ 实测、A2 对 F17 的
分析都扎实）；下面只讲它**漏掉或说偏**的部分。

---

## 1. 三个改变结论的盲点

### 盲点 1：爆炸是 width（多路）问题，文档杠杆几乎都在打 depth / encoding

A1 = raise 深度；A2/B3/B4 = 单节点 key 编码；A3 = 首注限制。**没有一个限制"同时在场人数"**——
而病根恰恰是"6 人各自再加注"的宽度组合。调研里唯一**生产验证过**的 width 杠杆：GTO Wizard AI
的 **"最多 N 人见后续街"**（用"closing-action / 投入最多者优先"的确定性存活规则决定谁留下），
把 6-max preflop 树砍 ~20×。与 A1/B3 正交、可叠加。文档完全没有。

### 盲点 2：645 / 224 GiB 是 enumerated（全预分配）上界，从没量过 reached set

Pluribus 枚举 664,845,654 条动作序列，**实际只 reach 413,507,309 条（62%）**，靠 lazy 分配
（= C5 HashMap 后端）落地。C5 已经在跑，但 S2 报的是 enumerated dense 字节数。**reached set 这个数
从没量过**——它可能直接把"必须 512 GB"变成"64 GiB 够"。最便宜、最高信息量的实验。

### 盲点 3：论证小注价值的 EV 很小，且 6-max 下根本没被验证

"塌成单一最优 size 只损 ~0.3% pot / ~2bb/100"（GTO Wizard）——但**验证戳破了**：那是**自对弈、
且 size 按局面最优选**（OOP ~50%、IP 75-100%，**不是** 1pot）的条件下。固定 `{1}`/`{1,2}` 树面对
真实对手（在你两档之间下注）不享受这个数（translation 可剥削区，见下）。**反过来**：小注价值集中在
**河牌**（终局、无再加注爆炸），恰是树爆炸最轻的街——所以 **"只在 turn/river 开池允许 0.5"** 能用
零头树成本拿走大部分 EV，是 A3 没有的对偶杠杆。

**合起来 = 别急着为 static A3 租 512 GB 机器。** 先跑 §5 Phase 0 的便宜 probe，很可能推翻
"必须保小注 / 必须 512GB"前提。

---

## 2. 逐方案对比（验证后修正）

| 方案 | 性质 | 验证后修正 | 定位 |
|---|---|---|---|
| **A1 raise cap** | 无损，depth | 大档有效（`{1,2}`cap1=28GiB），小注无效（199GiB）——文档对。但 cap 是**次数**杠杆；Pluribus 用的是 **size×raise-index**（0.5 仅首次加注），不是次数 cap，是不同杠杆 | 大档区默认；救不动小注 |
| **A2 transposition** | 无损-on-dynamics，有损-on-range | ✅ **F17-free by construction，经代码核实**：`legal_actions()`（`state.rs:252-306`）是 `{actor committed_this_round&stack, max committed_this_round, raise_option_open[idx], last_full_raise_size, big_blind, status, terminal}` 的纯函数，**不读 `committed_total`、不读 `last_aggressor`**。⚠ 文档"exact committed chips"措辞不精确：key 必须含派生态 `raise_option_open`+`last_full_raise_size`（D-033 不足额 all-in），不止 chip 总额。改动面**确实最小**（只在 `nlhe_betting_tree.rs` walk() 加 memo；`pack_info_set_v2`/dense indexer **零改**；代价是 parent 指针→DAG、`distinct_paths` 测试要反转）。但 A2 比 B3 **更丢 range**（丢 last_aggressor），压缩量未实测 | 低风险无损挤压；单用大概率喂不下小注 |
| **A3 first-bet-small** | 无损路径 | 224 GiB = Pluribus 那张表的机器级（<0.5TB），**不是** 64 GiB。"无损保留收敛保证"**被夸大**（见 §4）。A3 本质≈"0.5 仅 raise-index 0"，已是 Pluribus 菜单的一种 | 可行但贵；**先别买机器** |
| **B3 摘要** | 有损，imperfect recall | `{0.5,1}` 的 ~4.9 GiB 是 **NODE_CAP=100M 未饱和下界**（cap sweep：5M→27,455 / 50M→104,377 / 100M→197,432 key，仍每翻倍 ×~2），真值可能 high-10⁵~10⁶。**"进 64 GiB"定性稳**（~2M key×500桶×~3.4动作×8B×2表≈48GiB），**具体数字偏低需重测**。pin（+3~15%）和 dense GiB 公式经 vultr 重跑**逐字复现**，正确。**字段表缺 `capped` 位**（见 §3.3） | 想保小注且只有小机时的选项，**需重设计** |
| **B4 / C5 / C6** | — | C5 是 reached-set 落地关键，应加权；C6 神经编码都是 perfect-recall 特征向量（Deep CFR 每位 1 bit+1 size float；AlphaHoldem 24-channel tensor），搬进 tabular key 会重新爆炸，仍 park | C5 加权；C6 park |

---

## 3. 调研发现的"更好 / 被漏掉"的方案

按价值排序，**均通过对抗验证（去掉夸大后）**。

### 3.1 Width cap：`max-N-players-to-later-streets`（最重要的新杠杆）

GTO Wizard AI 生产结果 ~20×。唯一打 **width** 的杠杆，文档没有。确定性存活规则
（closing-action / 投入最多者优先）须**统一施加**以保 `legal_actions` 一致（F17-safe）。
**应在 `nlhe_betting_tree_sizing` 加 probe**：枚举 `{0.5,1}` 但剪掉 preflop 后 >N 人在场的节点。
假设：直击"6 人各自再加注"组合，A1 治不了的 width 病。

### 3.2 raise-index / 街 依赖菜单 full-depth + 量 reached set（最高性价比实验）

`legal_bet_sizes` 做成 `(street, raise_index_this_street)` 纯函数，0.5 只在每街首次加注合法。
**坦白：这≈A3**（A3 已是"0.5 仅开池"，all-street 实测 224 GiB）。新的部分：
- **(a) 只在 turn/river 放 0.5**（EV 集中在河、树成本最低）；
- **(b) 量 reached 而非 enumerated**（Pluribus 62%；C5 已具备）。

Pluribus blueprint 跑这种 `{0.5,1}` 量级菜单 **full-depth 不炸**，靠的就是 0.5 对再加注非法
+ lazy alloc。这个 probe 直接测"小注不可行"是不是**平菜单 artifact** 而非小注本身。

### 3.3 B3 加 per-seat `capped` 位（~6 bit，恢复 range 的最小字段）

range 区分的关键量是"谁**跟了**加注（capped）vs 谁**加注**（uncapped）+ 加注次数"，
**不是 `last_aggressor`（谁最后开火）**——cold-caller in position 经常 range 占优、aggressor 是
range 优势的 noisy proxy。文档 B3 字段表有 `live_players`、`last_aggressor` 却**没有 capped**。
per-seat 1-bit、纯 public-state 函数。range 损失集中在 turn/river → 多余 recall 位优先放后街。

### 3.4 A2 与 B3 本是一家——统一成 "A2 exact-key + last_aggressor + capped"

Lanctot well-formed 条件 (iii) `X₋ᵢ(z)=X₋ᵢ(φ(z))`（对手动作序列必须一致）**验证 HOLDS**：
任何 transposition 要 sound，必须补能区分对手动作的字段。⇒ A2 几何精确做骨架 + 最小 range 字段
（last_aggressor + capped）= 单一设计，取代"A2 vs B3 二选一"。注意：补 last_aggressor 是**必要不充分**
（完整 Theorem-1 还要 (i)(ii)(iv) 的 future 同构），所以仍须实测裁定。

### 3.5 VR-MCCFR baselines（+ sampling-scheme 轴）——治 A3 的真瓶颈

control-variate baseline 无偏、drop-in、方差 ~3 个量级。**直击 A3 的真瓶颈——训练 wall（7.5×
infoset），文档自己说"无解"。** critic 补：sampling scheme（ES vs outcome vs public-chance-sampling
MCCFR）是独立的 wall-time 杠杆，引擎现是 ES（`trainer.rs`），6-max full-width traverser 下 ES 是否
最优未检验。最该加的工程项。

### 3.6 PHM 升级 `map_off_tree`（正交，**替代不了 key 决策**）

现是 nearest-ratio stub（`action.rs:386`，D-201，路线图 stage 6c 换 PHM）。PHM 公式
`f(x)=(B-x)(1+A)/((B-A)(1+x))` 验证正确、range-free、scale-invariant，是正确的 off-tree handler。
**但清醒**：(a) **不压缩 key**（真瓶颈），与 A1/A2/B3 正交；(b) **translation-alone 是被淘汰的一档**——
Lisy-Bowling 2017 测得 abstraction+translation 对 Slumbot fcpa **~4020 mbb/g 可剥削，比"每手弃牌"
还差 4×**，业界用 real-time search 兜底才敢用；(c) **收不回我方下小注的 EV**。文档把 off-tree map
当 AIVAT 细节——值得升 PHM，但它不替代"保不保小注"的 key 决策。

### 3.7 A-loss recall 断言 + range-skew(KL/EMD) 实测——把"有损"变成数字

对同一公共局面、不同到达序列，算对手 posterior 桶分布距离（KL/EMD），按 reach 概率加权 = B3
"糊掉多少 range"的训练前直方图（用现成 `action_probs` 日志）。A-loss 断言（同 key 的两态只差
acting player 自己的可推导动作）= per-key 违例计数。几行代码，正确性红线落地。⚠ A-loss 是结构
检验（B3 必然失败、无设计杠杆）；真正的 CFR 收敛条件是 (skew) well-formed / CRSWF 的 reward/
distribution error，那才是可操作的"哪些合并便宜"的度量。

### 3.8 战略天花板（诚实）：blueprint-only 是自设上限

所有超人系统（Pluribus / Libratus / DeepStack / ReBeL / Modicum）都靠 real-time search，且都需
**belief/range 通道**——tabular-no-range key 恰恰没有。团队已把 search park 在 S6。
**关键提醒：无 range 的 key 设计会堵死未来 search 路**（search 要 Bayesian belief，6-max 还是 5 家
joint range）。在 key 里保留 last_aggressor/capped 这类 range proxy，与未来上 search 一致——
别画进墙角。次选：QRE 是多人一般和有定义的解概念（GTO Wizard 3-way 用），可作 S4 收敛监控的
正向目标（文档降级了 Nash/exploitability 却没给替代）。

---

## 4. 必须纠正文档的事实 / 数字

1. **"imperfect recall 破坏 CFR 收敛"对 A3-vs-B3 的权重被夸大。** 6-max 一般和**本来就没有**
   Nash 保证（A3 在这点上无优势）。**但** no-regret / regret-bound 机器是**另一个**性质：perfect
   recall（A3）by construction 保留；imperfect recall（B3）**只在分桶恰好 CRSWF 时**保留（未验证，
   F17 那 ~100 万违例就是其脆弱证据）。⇒ A3 优势**不是"虚的"，而是 no-regret/correctness 优势，
   不是 Nash 优势**。"正确性优先"红线下不能当 illusory 抹掉。

2. **Pluribus 不是 per-round offline 分解**（调研多次说错，critic 纠正，回查 Science supp 一致）：
   blueprint 是 **full-depth root-to-river MCCFR**；"subgame 到本街末、叶子在下街 chance"是
   **real-time search 专属**。它跑 `{0.5,1}` 量级菜单不炸，靠 **0.5 对再加注非法（raise-index 菜单）
   + lazy alloc**。且那张表内存 **<0.5TB（512GB 级 = A3 机器）**，**不是** 64 GiB。

3. **translation 不是万能解**（见 §3.6）。

4. **board / 花色 canonicalization 已做**（`postflop.rs` `canonical_observation_id`，D-218-rev2
   Waugh isomorphism + colex；dense canonical id flop/turn/river = 1,286,792 / 13,960,050 /
   123,156,254）——critic 的"漏了 GameShrink 板面同构"**是错的**，我查了代码，S2 计数**没被**花色
   对称膨胀。

---

## 5. 推荐执行顺序（cheapest-first，正确性优先）

**Phase 0 — 先修测量，再选方案**（几天，现成 sizing 工具 + vultr）：
1. raise-index/街 菜单重测 `{0.5,1}`（0.5 仅首次加注 / 仅 turn-river）
2. 加 **width-cap**（max-N-to-flop）probe
3. 量这些菜单下的 **reached infoset set**（不只 enumerated）
4. B3 `{0.5,1}` 用更大 NODE_CAP / key-only walk **重测真 distinct-key**（现 4.9 GiB 是未饱和下界）
5. board canonicalization 已确认在做，跳过

这五个全便宜、F17-safe，可能直接推翻"必须 512GB"。

**Phase 1 — 据 Phase 0 选路：**
- 若 width-cap + raise-index + reached 把小注子集压进 64–96 GiB → 走**无损路线**（coarse 菜单 +
  A2 transposition memo 无损挤压），保 no-regret/soundness，躲 B3 无业界先例的"摘下注历史"。
- 若小注无论如何无损塞不下、且质量 probe 证明它值 → 上**重设计 B3** = A2 exact-key + last_aggressor
  + **capped 位** + legal_action_set_id pin，**先在 HU 零和管线验**（exploitability/LBR 在那里才有牙齿，
  是验证过的正确决策仪器），再移植 6-max。

**Phase 2 — 不论选哪条都做的正交收益：**
- `map_off_tree` 升 PHM；VR-MCCFR baselines 治训练 wall；A-loss 断言 + range-skew 实测。

---

## 6. 参考（调研核到原始来源的）

- **Brown & Sandholm, Science 2019 *Superhuman AI for multiplayer poker*（Pluribus）+ supp**
  （https://noambrown.com/papers/19-Science-Superhuman_Supp.pdf）：664,845,654 动作序列 / reach
  413,507,309（62%）；<0.5TB / 64-core / 8 天 / $144；blueprint **full-depth** Linear MCCFR + 负 regret
  剪枝 + lazy alloc；betting **完美回忆**（只摘牌 200 桶/街）；rounds 3-4 首次加注 `{0.5p,1p,allin}`、
  后续 `{1p,allin}`；round 1 最多 14 档（因不 search）。
- **Ganzfried & Sandholm, IJCAI 2013 *Action Translation … Pseudo-Harmonic Mapping***
  （https://www.cs.cmu.edu/~sandholm/reverse%20mapping.ijcai13.pdf）：`f(x)=(B-x)(1+A)/((B-A)(1+x))`；
  Rand-psHar 近 0 exploitability、arithmetic 可炸 ~100×。
- **Lisy & Bowling 2017 LBR/exploitability of NL bots**（poker.cs.ualberta.ca/publications/aaai17ws-lisy-lbr.pdf）：
  abstraction+translation ~4020 mbb/g 可剥削。
- **Lanctot et al., ICML 2012 *No-Regret Learning in EFGs with Imperfect Recall***（arXiv:1205.0622）：
  well-formed games + full-game regret bound；条件 (iii) `X₋ᵢ(z)=X₋ᵢ(φ(z))`。
- **Kroer & Sandholm, EC 2016 *Imperfect-Recall Abstractions with Bounds***（arXiv:1409.3302）：CRSWF；
  reward/leaf-prob/distribution error reach-weighted bound；最优化 NP-complete。
- **Johanson et al., AAMAS 2013 *Evaluating State-Space Abstractions***：IR 胜 PR，但摘的是**牌**、
  保留动作序列（与 B3 相反）。
- **Brown, Sandholm, Amos 2018 *Depth-Limited Solving*（Modicum）**（arXiv:1805.08195）：5GB/700 core-hr
  blueprint；multi-valued states / k=4 biased continuation strategies（无需 NN）；2p0s。
- **Cermak, Bosansky, Lisy, IJCAI 2017 / arXiv:1803.05392（FPIRA/CFR+IRA）**：自动 bounded-loss IR 精化；
  需遍历完整树 + 2p0s + ~7×10⁵ infoset（差我们 5 个量级）。
- **Gimbert, Paul, Srivathsan, AAMAS 2025 *Simplifying Imperfect Recall Games***（arXiv:2502.13933）：
  A-loss recall 定义；1-player IR NP-hard。
- **Fu et al. 2025 *Beyond Outcome-Based Imperfect-Recall***（arXiv:2510.15094）+ **KrwEmd**
  （arXiv:2511.12089）：outcome-only / "忘掉一切" IR 损失大，主张 higher-resolution / k-recall。
- **RL-CFR, Li et al., ICML 2024**（arXiv:2403.04344）：per-state 自适应菜单；real-time，需 PBS+value net，
  2p0s。
- **GTO Wizard 博客**：*How Solvers Work*（696,613 vs ~87M 节点）、*Do Multiple Sizes Matter?* /
  Dynamic Sizing（单 size ~0.3% pot，但自对弈+局面最优 size）、max-3-to-flop ~20×、3-way 用 QRE。
- **VR-MCCFR（Schmid et al. AAAI 2019）**；**Cepheus 压缩 regret 存储（Bowling-Tammelin，262TB→~11TB）**；
  **FOSG/PBS（Kovařík et al. *Rethinking Formal Models…*）**。
- 本项目：`docs/six_max_nlhe_target.md` §S2；`src/rules/state.rs:252-306`（legal_actions 纯函数）；
  `src/training/nlhe_betting_tree.rs`（perfect-recall 树）；`src/abstraction/postflop.rs`（canonical 板面）；
  `src/abstraction/action.rs:386`（map_off_tree PHM stub）。

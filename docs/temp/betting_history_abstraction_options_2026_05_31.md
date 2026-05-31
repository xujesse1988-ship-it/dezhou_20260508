# 下注历史抽象表示方案（6-max，betting tree 之外的选项）

工作笔记。背景：S2 实测发现显式 betting tree 在 6-max 多 bet size 下爆炸
（`{0.5,1}` > 1 亿节点 / ≥20B infoset / ≥645 GiB，见 `docs/six_max_nlhe_target.md` §S2）。
本文列出"除 betting tree 外，下注历史还能怎么抽象表示"的方案谱系，供定 6-max 抽象时取舍。

## 0. 当前做法 = 显式 betting tree（完美回忆）

infoset key = `node_id`(下注历史) + `bucket`(私牌) + `position`（InfoSetId 打包，
`src/abstraction/map/mod.rs`；`node_id` 在高位 26 bit）。

`node_id` = **预枚举抽象下注树里的位置** = 对抽象动作序列的**完美回忆**：两条不同加注序列哪怕
到达完全相同的局面（同底池、同人数、同该谁动）也是**不同 infoset**。爆炸就来自这里——re-raise
链的每条路径单独记一份。下注树由 `PublicBettingTree::build_with_abstraction` 枚举，当前**无 raise
深度上限**（`FacingRaise3Plus` 只分桶不剪树）。

下注历史不一定要这么表示。按"丢多少信息"分三类。

## A. 近乎无损的削减（不丢策略相关信息）

### A1. Raise cap（每街 re-raise 轮数上限）
直接砍掉爆炸的那条链。Pluribus/Libratus 都限制加注序列。最便宜、实现最简、能叠加 B/C。
- 实现：在树构造 / 合法动作里，本街 raise 数到 K 后只留 call/fold/all-in。
- 引擎已有 facing-raise 分桶，加个深度截断即可。

**实测（2026-05-31，vultr `df75058`，`nlhe_betting_tree_sizing` 加 `RAISE_CAP` 探针；
K = 每街 (Bet+Raise) 聚合上限、all-in 不计入且永远保留；preflop 169 / postflop 200，dense 两表 variable）**：

| 抽象 ＼ cap | K=1 | K=2 | K=3 / 无cap |
|---|---|---|---|
| `{1.0}` 1 大档 | 1.60M / 10.07 GiB | 3.83M / 23.98 GiB | 4.69M / 29.14 GiB |
| `{1.0,2.0}` 2 大档 | **4.53M / 28.27 GiB ✅** | 8.95M / 55.50 GiB ⚠ | 10.03M / 62.03 GiB ❌ |
| `{0.5,1.0}` 含小注 | 30.75M / **199.46 GiB ❌** | >100M / ≥647 GiB ❌ | >100M / ≥645 GiB ❌ |

（探针自洽：`{1.0}` cap=50 与无cap 逐字节一致；HU self-check 永不加 cap、守 240,096 节点；树规模随 K 单调。）

**结论 = A1 是"大档区的杠杆"，不是"小注的解药"**：
- **大档区有效**：`{1.0,2.0}` cap=1 = 28.27 GiB，比单档 `{1.0}` 无cap（29.14）还小、稳进 64 GiB
  → **用"每街最多 1 次加注"换来第二个 bet size**（策略表达力↑），内存不涨反降。有用档位 = K=1/2；
  K=3 起 ≈ 无cap（大注 ≤3 次加注即顶 all-in，cap 不绑）。
- **小注区无效**：`{0.5,1.0}` 最狠的 cap=1 仍 199.46 GiB（~7× 超 64 GiB），cap≥2 破亿。决定性反例：
  `{0.5,1.0}` cap=1（30.75M）比 `{1.0,2.0}` **无cap**（10.0M）还大 3×，max depth 还从 38 涨到 43
  → 小注的爆炸主因是**多路宽度 + 深筹码续局**（小注压低底池 → 6 人多路深局 → 每个 cap 后节点仍
  {fold,call,allin} 分叉 × 多街），raise cap 是**深度**杠杆、治不了**宽度**病。
- 故原"最对症、首选"只对**大档区**成立；**保留小注必须叠 B3/B4 或按 S2 弃小注，A1 单用不够**。

### A2. Public-state 规范化（transposition 折叠）
把"到达相同 `(各家已投筹码, 该谁动, 本街已加注数)`"的不同动作序列并成同一节点。完美回忆下它们
本是不同节点，合并后是 transposition 折叠。无损或近无损，但要算"状态等价"而非"路径"，实现复杂度高于 A1。

### A3. first-bet-small（小注只当开池领打，re-raise 一律大注）— 需 ~512 GB 大机，可行
想保住 postflop 领打的尺寸选择（0.5 vs 1pot，策略上值钱），又躲开小注的爆炸：preflop `{1.0}`、
postflop `{0.5,1.0}` 但**只允许首次进攻（开池 `Bet`）打 0.5 或 1pot，后续 `Raise` 一律 1pot（禁 `Raise{0.5}`）**。
用 `AbstractAction` 的 `Bet`(首次)/`Raise`(后续) 天然区分实现。

**实测（2026-05-31，vultr `8b04aee`，`FIRST_SMALL=1` 探针 commit `8b04aee`；preflop 169 / postflop 200）**：

| 配置 | 决策节点 | dense 两表 | max depth |
|---|---|---|---|
| first-bet-small 无cap | **35.13M** | **224.58 GiB** | 43 |
| + RAISE_CAP=2 | 32.46M | 208.16 GiB | 43 |

放进谱系：比 `{1.0,2.0}`（10.0M/62 GiB）大 **3.6×**，比 `{0.5,1.0}` cap=1（30.75M/199 GiB）略大。
**超出 64/96 GiB 单机预算，但落进 ~512 GB 大内存机富余**（RSS ≈ 230 GiB；512 GB = 476.8 GiB 留 ~250 GiB 余量；
256 GB = 238 GiB 偏紧不建议）。raise cap 叠加也省不下来（cap=2 仍 208 GiB，cap 不绑这种深多路续局）。

**为什么反而更大 = 小注的病根在底池几何，不在 re-raise**：砍掉 0.5 re-raise（相对全程 `{0.5,1.0}` 把
≥645 砍到 224 GiB，确实砍了 ~3×），但留下的 0.5 **开池**仍制造"小底池 → 6 人深筹码多路续局"：开池打 0.5
压低底池 → 后续 1pot re-raise 按小底池算、更小 → all-in 前多塞一轮 → 树更深（depth 43 vs `{1.0}` 的 38），
且河牌一条街就占全树 3/4（per-street River=26.2M）。**只要 0.5 存在（哪怕只当领打）就触发这套几何，结构性
限制 re-raise 删不掉它。** 印证 S2「档数不是病根、小注才是」。

**取舍（不否决，是换预算）**：
- 内存：要 ~512 GB 大机（对齐 `feedback_high_perf_host_on_demand` 报预算起机），非 64 GiB c6a.8xlarge。
- 真瓶颈是训练时长：infoset 7.02B ≈ `{1.0}`（933.9M）的 **7.5×**，收敛 update 数随之上去，wall 比
  `{1.0}`/`{1.0,2.0}` 长一截——内存放开后这才是要算的账。
- 回报：**无损**保住 postflop 领打 0.5/1pot 尺寸选择（全大档抽象给不了，策略上值钱）。
- 对照 B 类：B3/B4 在小机上也能留小注尺寸（折叠成 SPR 桶），但**有损** + 改 InfoSetId 语义；first-bet-small
  纯靠堆内存、**无损保留路径**。二选一，或先用 first-bet-small 摸质量上限，再决定要不要省成 B 类。

## B. 有损削减（用摘要替代完整序列 = 把信息抽象用在下注历史上）

本质：把"下注历史"当成另一个要做信息抽象的维度，跟对私牌做 bucket 同理。

### B3. 紧凑 betting-state 摘要（feature tuple）
不存完整路径，只存一个有界小元组，例如：
`(street, 底池桶/SPR 桶, 本街已加注数, 面对的下注尺寸桶, aggressor, 在场人数, position)`。
大量不同序列 → 同一 key。**这恰好让 `{0.5,1}` 的爆炸消失**：不管底池怎么涨到这么大，落进同一 SPR
桶就是同一 infoset。key 空间有界且远小于树。

### B4. Imperfect recall（不完美回忆）
"忘掉"前几街动作细节，只保留当前街摘要 + 粗粒度底池/SPR。**大规模求解器扩状态空间的标准手段**
（Libratus/Pluribus 的牌面抽象就是 imperfect recall，下注历史同理）。B3 是它的一种具体编码。

## C. 换表示方式（不再枚举树）

### C5. Lazy / hash 生成 infoset（无预枚举树）
我们的 **HashMap 后端本来就这样**：不预建树，采样访问到才 materialize。
- 不减少 infoset **数量**，但省掉上前枚举，且能直接吃 A/B 的摘要 key（摘要空间稀疏无妨）。
- dense 表才必须预枚举有界 key 空间——而 B 的摘要恰好给出一个**有界且小得多**的 key 空间。

### C6. 连续 / 神经编码
把下注历史编码成特征向量喂价值/策略网络（ReBeL / DeepStack / AlphaHoldem），彻底绕开 tabular 枚举。
= 之前 parked 的神经路线（与当前纯 Rust tabular-CFR 基建脱节）。

## 诚实的权衡

- **B 类有损**：bot 会"忘记"区分（如忘了底池是激进打大还是被动跟大）。摘要选不好掉策略质量；
  **CFR 在 imperfect recall 下收敛保证变弱**（2 人零和都不保证收敛 Nash，靠经验 + 好抽象）。
  艺术在于摘要保住策略相关信息。
- B 类**改 InfoSetId 语义**：`node_id`（精确路径，一对一）→ 计算出的摘要 key（多对一）。
- **A 类（raise cap / 状态规范化 / first-bet-small）风险最低、无损、能与 B/C 叠加**。对小注：单机 64 GiB
  预算下 raise cap 救不了（§A1，仍 199 GiB）；first-bet-small 无损保住小注尺寸但要 **~512 GB 大机**（§A3，
  224 GiB 两表）——即"堆内存换无损"，可行但贵。

## 对我们最对症的组合

爆炸 = **re-raise 深链（A1 可砍）+ 小注底池几何导致的多路深筹码续局（结构性削减砍不动深度，见 §A1/§A3 实测）**。
想要 postflop 小注尺寸，三条路按"机器 vs 有损"取舍：

- **(i) ~512 GB 大机 + first-bet-small（§A3）**：无损保留完整路径，纯堆内存，224 GiB 两表落进 512 GB 富余。
  代价 = 大机 + 训练 wall（infoset 7.02B ≈ `{1.0}` 的 7.5×）。**当前倾向先走这条摸质量上限**（无损、不改语义）。
- **(ii) 小机 + B3 摘要**：`(street, SPR/底池桶, 本街已加注数, 面对尺寸桶, aggressor, 在场人数, position)`
  替掉 `node_id` 完整路径 → key 空间有界、爆炸消失。本质 = 对下注历史做 imperfect-recall。代价 = **有损**
  （bot 不再区分"底池怎么变大的"）+ 改 InfoSetId 语义。是把 (i) 省成小机的路子。
- **(iii) 直接弃小注**：少量大档 + A1（`{1.0,2.0}` cap=1 已进 64 GiB），最省事，放弃 postflop 小注尺寸。

## 待办 / 下一步候选

- (a) ✅ **已做**（2026-05-31，见 §A1 实测，`RAISE_CAP` 探针 commit `df75058`）：raise cap=K 对
  `{0.5,1}` **无效**（cap=1 仍 199 GiB，cap≥2 破亿），对 `{1.0,2.0}` **有效**（cap=1 = 28.27 GiB 进
  64 GiB）。结论：A1 = 大档区杠杆，非小注解药。
- (b) 起草 **betting-state 摘要 key** 设计：列保留字段 + 估 key 空间大小 + 与 InfoSetId 打包的改动面。
  （A1 既已证救不动小注，(b) 的有损摘要从"可选"升为"想保留小注就必需"；若走"弃小注 + 大档 + A1"
  路线则可暂不做 (b)。）

## 参考

- Waugh et al. *A Practical Use of Imperfect Recall*（CFR + imperfect-recall 抽象）。
- Johanson et al. 关于 poker abstraction / bucketing。
- Brown & Sandholm Libratus / Pluribus（action abstraction + 有限 bet size + 子博弈重解）。
- 本项目：`docs/six_max_nlhe_target.md` §S2（树规模实测）；`src/training/nlhe_betting_tree.rs`；
  `src/abstraction/map/mod.rs`（InfoSetId 打包）。

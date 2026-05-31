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
直接砍掉爆炸的那条链。Pluribus/Libratus 都限制加注序列。最便宜、最对症。
- 实现：在树构造 / 合法动作里，本街 raise 数到 K 后只留 call/fold/all-in。
- 引擎已有 facing-raise 分桶，加个深度截断即可。
- 风险最低，且能叠加 B/C。**首选**。

### A2. Public-state 规范化（transposition 折叠）
把"到达相同 `(各家已投筹码, 该谁动, 本街已加注数)`"的不同动作序列并成同一节点。完美回忆下它们
本是不同节点，合并后是 transposition 折叠。无损或近无损，但要算"状态等价"而非"路径"，实现复杂度高于 A1。

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
- **A 类（raise cap / 状态规范化）风险最低**，且能与 B/C 叠加。

## 对我们最对症的组合

爆炸 = **re-raise 链 × 完美回忆**。最高杠杆、最低风险 =
**A1 raise cap + B3 底池/SPR 桶化的 betting 摘要替掉 `node_id` 的完整路径**。
本质 = 对下注历史做 imperfect-recall 抽象，也正是大规模求解器能把 6-max 塞进有界内存的原因——
代价是 bot 不再区分"底池怎么变大的"。

## 待办 / 下一步候选

- (a) 量 **raise cap=K** 对 `{0.5,1}` 树规模的效果（引擎加深度截断探针，沿用 `nlhe_betting_tree_sizing`）。
- (b) 起草 **betting-state 摘要 key** 设计：列保留字段 + 估 key 空间大小 + 与 InfoSetId 打包的改动面。
- 二者可先做 (a)（便宜、对症、验证 raise cap 单独能否压回可行区），再决定要不要上 (b) 的有损摘要。

## 参考

- Waugh et al. *A Practical Use of Imperfect Recall*（CFR + imperfect-recall 抽象）。
- Johanson et al. 关于 poker abstraction / bucketing。
- Brown & Sandholm Libratus / Pluribus（action abstraction + 有限 bet size + 子博弈重解）。
- 本项目：`docs/six_max_nlhe_target.md` §S2（树规模实测）；`src/training/nlhe_betting_tree.rs`；
  `src/abstraction/map/mod.rs`（InfoSetId 打包）。

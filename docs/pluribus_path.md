# Pluribus 可实现路径

## 文档目标

本文档给出一个可实现的 Pluribus 类多人无限注德州算法路径。目标不是复述论文，而是把 Pluribus 的核心思想拆成可工程落地、可测试、可量化验收的阶段。

最终目标：

- 实现一个支持 6-max 100BB No-Limit Texas Hold'em 的实用策略引擎。
- 使用离线自博弈训练出的 `blueprint strategy` 作为全局策略基础。
- 在实战决策时使用 `depth-limited search` 对当前局面实时重解。
- 不依赖人类牌谱和对手身份建模，保持 Pluribus 式固定策略路线。

## Pluribus 核心路线

Pluribus 的可实现路线可以概括为：

`规则环境 -> 抽象层 -> Linear MCCFR blueprint -> 训练优化 -> 实时重解 -> 评测体系 -> 服务化策略引擎`

核心模块：

- `Blueprint`：离线用自博弈训练出的基础策略，覆盖完整游戏树的抽象版本。
- `Action abstraction`：把无限注下注空间压缩成有限动作集合，例如 fold/call/check、0.5 pot、1 pot、all-in。
- `Information abstraction`：把牌面、手牌强度、听牌结构、下注历史等映射到有限 bucket。
- `Linear MCCFR`：用 Monte Carlo Counterfactual Regret Minimization 训练多人局策略，并对后期迭代赋予更高权重。
- `Depth-limited search`：实战中从当前 public state 出发，对有限深度子博弈实时搜索，用 blueprint 作为叶子节点价值近似。
- `Off-tree action handling`：真实下注不在抽象动作集合中时，把它映射到最接近或最稳健的抽象响应。

## 推荐工程架构

- 高性能核心使用 Rust 或 C++：规则引擎、手牌评估器、抽象映射、CFR 训练、实时搜索。
- Python 用于实验编排、训练调度、指标分析、评测报告生成。
- 策略表、regret 表、checkpoint 使用可分片存储，支持断点恢复和版本化。
- 高配服务器默认目标：blueprint 训练按约 `64` 核、`512GB` 内存以内设计；实时搜索按约 `28` 核、`128GB` 内存以内设计。

## 时间预算与人力假设

本路径整体复杂度对标 Pluribus（CMU + Facebook 团队多年工作产物）。任何低于以下投入的尝试都应预期阶段性目标会显著退化。

人力时间估算（全职等效）：

- 单人：`12-24` 个月。
- `3-5` 人小团队：`6-12` 个月。
- 阶段 6（实时搜索）单独占总工作量约 `30-40%`，是整个项目最难也最容易低估的阶段。

各阶段大致人月占比参考：

- 阶段 1：`1-2` 人月。
- 阶段 2：`2-3` 人月（bucket 特征工程是主要变数）。
- 阶段 3：`1` 人月。
- 阶段 4：`3-6` 人月（含训练等待时间）。
- 阶段 5：`2-3` 人月。
- 阶段 6：`4-8` 人月（拆为 6a/6b/6c，见后文）。
- 阶段 7：`1-2` 人月。
- 阶段 8：`1` 人月。

硬件假设：blueprint 训练 `64` 核 / `512GB`；实时搜索 `28` 核 / `128GB`。低于此配置必须在抽象规模和训练迭代数上做对应折扣，否则 blueprint 质量会不达预期。

## 完整实现阶段总览

### 阶段 1：规则环境与手牌评估器

目标：实现完全可信的 6-max NLHE 环境、合法动作、结算、hand history 和手牌评估器。

量化门槛：

- `1,000,000` 手牌随机模拟零非法状态。
- `200+` 合法动作固定场景全部通过。
- `100+` side pot / split pot 固定场景全部通过。
- 单线程 7-card 手牌评估吞吐达到 `1,000,000 eval/s`。
- `100,000` 手牌 hand history 回放完全一致。

### 阶段 2：抽象层

目标：实现 action abstraction 和 information abstraction，为 CFR 把真实无限注德州压缩成可训练博弈。

量化门槛：

- action abstraction 至少支持 fold/check/call、`0.5 pot`、`1 pot`、`all-in`，并可配置扩展到 `1-14` 个 raise size。
- preflop 支持 lossless `169` 起手牌类别，并能区分位置、有效筹码和前序动作。
- flop/turn/river 至少支持 `500` 个 bucket 配置，并能输出 bucket 分布报告。
- 抽象映射必须确定性可复现：同一状态重复映射 `1,000,000` 次，bucket id 完全一致。
- 抽象层每秒至少完成 `100,000` 次状态映射，满足训练采样需求。
- bucket 质量验收（仅 bucket 数量达标不算通过）：
    - 至少使用一种 potential-aware 特征参与聚类，例如 `EHS²`、OCHS、distribution-aware histogram，不能只用当前 hand strength。
    - 同一 bucket 内手牌的 expected hand strength 标准差有上限（建议 `< 0.05`），并对每个 street 出具 bucket 内方差报告。
    - 同一 bucket 内手牌的 all-in equity 分布相互之间的 EMD / KL 散度需在阈值内，证明 bucket 不是噪声聚类。

### 阶段 3：MCCFR 小规模验证

目标：先在小博弈上验证 CFR 实现正确，再迁移到德州。

量化门槛：

- Kuhn Poker exploitability 收敛到 `0.01` 以下。
- Leduc Poker 训练曲线稳定改善，固定 seed 下结果可复现。
- 2-player 简化 NLHE 能完成至少 `100,000,000` 次 sampled decision 更新。
- regret matching 输出的动作概率总和误差小于 `1e-9`。
- checkpoint 保存后恢复训练，恢复前后同一策略查询结果完全一致。

### 阶段 4：6-max Blueprint 训练

目标：实现 External-Sampling MCCFR / Linear MCCFR，训练多人局 blueprint。

量化门槛：

- 支持 `6` 个 traverser 轮流更新，所有玩家都有独立 regret 与 average strategy 累积。
- 单次训练可连续运行 `24` 小时无崩溃、无明显内存泄漏。
- 支持 checkpoint 保存/恢复，恢复后训练曲线连续。
- "first usable" blueprint 门槛：至少完成 `1,000,000,000`（`10⁹`）次 sampled decision 更新；该 blueprint 仅用于打通流水线和初步消融，不具备实战质量。
- "production" blueprint 门槛：至少完成 `100,000,000,000`（`10¹¹`）次 sampled decision 更新，对标 Pluribus 论文规模；只有 production blueprint 才能进入阶段 6 之后的实战评测。
- LBR (Local Best Response) exploitability 持续下降，最终低于既定阈值（建议 `< 100 mbb/g`，依抽象规模调整）。
- 对开源参考 bot（如 Slumbot、SlumBot 2017 或同等水平开源实现）head-to-head 至少 `100,000` 手不输，`mbb/g` 在 95% 置信区间内不显著为负。
- 多人 CFR 收敛性监控：必须实时输出 average regret 增长曲线、策略 entropy、动作概率震荡幅度。average regret 应呈 sublinear 增长；若出现持续震荡或线性增长必须能告警并定位。
- baseline sanity check（必要但非充分条件）：blueprint-only 策略在 `1,000,000` 手牌评测中稳定击败 random、call-station、tight-aggressive 三类基线。该项不能替代 LBR 和 Slumbot 评测。

### 阶段 5：训练性能与内存优化

目标：加入接近 Pluribus 的工程优化，让 blueprint 训练能扩展到更大抽象。

量化门槛：

- regret 和 average strategy 使用紧凑存储，策略表支持分片加载。
- 极负 regret 动作支持 pruning，并能周期性恢复探索。
- 同等抽象和迭代数下，训练速度相比朴素实现提升至少 `2x`。
- 内存占用相比朴素表存储下降至少 `50%`。
- pruning 开关消融实验中，策略质量不能出现不可解释的大幅退化。

### 阶段 6：实时 Depth-Limited Search

目标：在真实决策点对当前局面做实时子博弈重解，用搜索策略改进 blueprint。

阶段 6 是整个项目最复杂的部分。Pluribus 论文对 continual re-solving 和 leaf evaluation 的描述偏简略，从零复现极易写错。本阶段必须拆为 6a / 6b / 6c 三个串行子阶段，每个子阶段都有独立验收。

#### 阶段 6a：单层 subgame 重解

只在当前 public state 做一次 subgame solve，叶子节点直接用 blueprint 策略估值，不引入 biased leaf。先打通流水线、性能、正确性。

量化门槛：

- flop/turn/river 都支持从当前 public state 启动 subgame search 并返回策略。
- 当前下注轮使用更细或近似 lossless 的 action abstraction，未来下注轮使用受控 bucket。
- 单次决策搜索延迟 P95 小于 `30` 秒。
- subgame action sequence 控制在 `100-2,000` 级别，避免搜索爆炸。
- 在 fixed seed 下，同一 public state + 同一手牌 + 同一对手范围多次 solve 的策略差异在数值噪声范围内。
- 单层 search 在 `1,000,000` 手牌评测中不显著差于 blueprint-only（先保证不退化）。

#### 阶段 6b：continual re-solving + biased leaf strategies

在每个决策点重新 solve，并显式实现多个 biased leaf evaluation strategies，让对手在子博弈叶子处可以选择不同 meta-strategy，避免被剥削。这是 Pluribus 真正能赢人的关键机制。

量化门槛：

- 显式实现至少 `4` 个 leaf evaluation strategies：`unbiased`（直接使用 blueprint）、`fold-biased`、`call-biased`、`raise-biased`，对应 Pluribus 论文中给对手的 meta-strategy 选项。
- subgame solver 在叶子处对每个 biased strategy 维护一份独立的 EV 估计，并在 solve 中允许对手选择最有利的一个。
- 每条 biased strategy 都能单独消融开关，验证去掉之后策略可剥削性显著上升。
- continual re-solving 必须在每个决策点重新求解，不允许缓存上一决策点的 subgame 解。
- search 策略在 `1,000,000` 手牌评测中显著优于 blueprint-only，提升需超过统计误差且 LBR exploitability 显著下降。

#### 阶段 6c：off-tree action handling 验证

对手真实下注不在抽象动作集合中时的处理，是阶段 6 实战 bug 和可剥削性的最大来源。文档必须明确指定算法并独立验收。

量化门槛：

- 显式选定 off-tree 映射算法，例如 pseudo-harmonic mapping、nearest-action mapping、randomized rounding，并写入策略服务的版本元数据。
- fuzz 测试 `1,000,000` 个 off-tree 下注金额，映射结果稳定可复现，且不出现非法动作或越界 bucket。
- 对比 on-tree 与 off-tree 输入下相同 public state 的策略输出，量化策略差异分布。
- 对一个故意贴近抽象边界下注的简单对手做 `1,000,000` 手对战，统计该对手是否能通过卡边界获得显著正收益（视为可剥削性证据）。
- off-tree 处理路径覆盖率与 on-tree 路径覆盖率分别报告，作为评测报告的一部分。

### 阶段 7：评测体系

目标：建立可重复、可对比、可诊断的策略评测平台。

量化门槛：

- 每次正式评测至少 `1,000,000` 手牌。
- 输出 `mbb/game`、standard error、置信区间、按位置拆分的收益。
- 支持 blueprint-only、search-on/off、pruning-on/off、不同 abstraction 配置的消融对比。
- 固定 seed 的评测结果可复现；不同 seed 的结果方差可统计。
- 每个发布候选策略必须生成评测报告和策略版本哈希。
- 必须使用 AIVAT 或 DIVAT 作为标准方差缩减方法。无方差缩减时，6-max NLHE 100BB 的 `1,000,000` 手对噪声过大，难以分辨弱改进；评测报告需同时输出原始 `mbb/g` 与方差缩减后的 `mbb/g`。
- 必须把 LBR (Local Best Response) exploitability 作为评测指标之一，每个发布候选策略都需输出当前抽象下的 LBR 上界并跟踪版本间变化。
- 至少有一组评测对手是开源参考 bot（Slumbot 或同等水平），head-to-head 结果必须在评测报告中显式记录。

### 阶段 8：实用化策略引擎

目标：把训练和搜索能力封装成可调用的策略服务。

量化门槛：

- API 输入公开状态、我方私牌、筹码、下注历史，输出动作概率分布和推荐动作。
- API P95 延迟小于 `30` 秒，blueprint-only fallback P95 小于 `100ms`。
- 支持 checkpoint 热加载和版本回滚。
- 同一状态、同一 seed、同一策略版本输出完全一致。
- 服务只面向自有测试环境、研究环境或合规产品形态，不接入线上牌局自动化。

## 最终验收标准

- 完成 6-max 100BB NLHE 端到端闭环：训练、加载、搜索、评测、服务化。
- blueprint-only 能稳定击败基础基线 Bot（必要但非充分条件）。
- search 策略显著优于 blueprint-only，且提升超过统计误差，并在 LBR exploitability 上显著低于 blueprint-only。
- 对开源参考 bot（Slumbot 或同等水平）head-to-head 在置信区间内不输。
- LBR exploitability 在已知抽象下达到稳定低位，不随时间漂移。
- 长局数评测结果稳定，至少覆盖 `10,000,000` 手牌累计评测，且使用 AIVAT/DIVAT 方差缩减后结果一致性可复现。
- 每个策略版本都有可复现的训练配置、checkpoint、评测报告（含 LBR 与方差缩减后的 `mbb/g`）和版本哈希。

## 详细阶段文档

- 阶段 1：规则环境与手牌评估器的量化验证方式，见 [pluribus_stage1_validation.md](./pluribus_stage1_validation.md)。
- 阶段 2-8：后续按阶段 1 的格式继续拆分，每个阶段单独维护目标、验证方式、通过标准和进入下一阶段的门槛。

## 参考资料

- Pluribus 主论文：https://noambrown.github.io/papers/19-Science-Superhuman.pdf
- Pluribus 补充材料：https://noambrown.github.io/papers/19-Science-Superhuman_Supp.pdf

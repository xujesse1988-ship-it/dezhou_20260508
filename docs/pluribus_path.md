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
- 第一个可用 blueprint 至少完成 `1,000,000,000` 次 sampled decision 更新。
- blueprint-only 策略在 `1,000,000` 手牌评测中稳定击败 random、call-station、tight-aggressive 三类基线。

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

量化门槛：

- flop/turn/river 都支持从当前 public state 启动 subgame search。
- 当前下注轮使用更细或近似 lossless 的 action abstraction，未来下注轮使用受控 bucket。
- 单次决策搜索延迟 P95 小于 `30` 秒。
- subgame action sequence 控制在 `100-2,000` 级别，避免搜索爆炸。
- search 策略在 `1,000,000` 手牌评测中显著优于 blueprint-only，提升需超过统计误差。

### 阶段 7：评测体系

目标：建立可重复、可对比、可诊断的策略评测平台。

量化门槛：

- 每次正式评测至少 `1,000,000` 手牌。
- 输出 `mbb/game`、standard error、置信区间、按位置拆分的收益。
- 支持 blueprint-only、search-on/off、pruning-on/off、不同 abstraction 配置的消融对比。
- 固定 seed 的评测结果可复现；不同 seed 的结果方差可统计。
- 每个发布候选策略必须生成评测报告和策略版本哈希。

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
- blueprint-only 能稳定击败基础基线 Bot。
- search 策略显著优于 blueprint-only，且提升超过统计误差。
- 长局数评测结果稳定，至少覆盖 `10,000,000` 手牌累计评测。
- 每个策略版本都有可复现的训练配置、checkpoint、评测报告和版本哈希。

## 详细阶段文档

- 阶段 1：规则环境与手牌评估器的量化验证方式，见 [pluribus_stage1_validation.md](./pluribus_stage1_validation.md)。
- 阶段 2-8：后续按阶段 1 的格式继续拆分，每个阶段单独维护目标、验证方式、通过标准和进入下一阶段的门槛。

## 参考资料

- Pluribus 主论文：https://noambrown.github.io/papers/19-Science-Superhuman.pdf
- Pluribus 补充材料：https://noambrown.github.io/papers/19-Science-Superhuman_Supp.pdf

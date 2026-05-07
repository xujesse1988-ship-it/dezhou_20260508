# 阶段 1：规则环境与手牌评估器的量化验证方式

## 阶段目标

阶段 1 的目标是先做出一个完全可信的 6-max No-Limit Texas Hold'em 环境。这个阶段不训练 AI，只验证游戏状态、合法动作、发牌、下注、结算、回放、手牌评估都正确，否则后续 CFR 训练结果没有可信度。

阶段 1 需要支持：

- 6 人桌、100BB、大小盲、按钮轮转、preflop/flop/turn/river 四轮下注。
- fold、check、call、raise、all-in、最小加注、多人 all-in、side pot、split pot。
- 完整 hand history 记录与回放。
- 手牌评估器能正确比较 5-7 张牌中的最佳 5 张组合。
- 环境必须确定性可复现：相同 seed、相同动作序列，得到完全一致结果。

## 量化验证方式

### 1. 规则状态机验证

- 随机模拟 `1,000,000` 手牌，不能出现非法状态、负筹码、重复牌、无人获胜、pot 金额不守恒。
- 每手结束时验证：`所有玩家筹码总和 + 桌面未结算筹码 = 初始总筹码`。
- 每个 betting round 结束时验证：未 fold 且未 all-in 的玩家投入额相同。
- 按钮轮转连续运行 `10,000` 手牌，每个座位拿到 button / SB / BB 的次数差异不超过 `1`。

### 2. 合法动作生成验证

- 构造至少 `200` 个固定场景，覆盖 open raise、3-bet、短码 all-in、不足额 all-in、多人 call、check-back、最后行动位。
- 每个场景断言合法动作集合完全匹配预期。
- 随机 fuzz `1,000,000` 个中间状态，生成的 raise 必须满足最小加注和剩余筹码约束。
- 非法动作必须被拒绝，并返回明确错误原因。

### 3. Side pot / split pot 验证

- 构造至少 `100` 个多人 all-in 场景，覆盖 2-6 人、多个不同 all-in 金额、多人并列获胜。
- 每个场景断言主池、边池、获胜者、分池结果完全匹配手算结果。
- 分池产生不能丢失筹码；如存在不可整除筹码，必须按固定规则处理并可复现。

### 4. 手牌评估器验证

- 使用公开 poker hand ranking 样例测试，正确率必须 `100%`。
- 覆盖 10 类牌型：high card、one pair、two pair、trips、straight、flush、full house、quads、straight flush、royal flush。
- 随机生成 `10,000,000` 组 7-card hand，不允许出现重复牌或比较结果不稳定。
- 性能目标：单线程每秒至少 `1,000,000` 次 7-card 评估；多线程吞吐接近线性扩展。

### 5. Hand history 回放验证

- 随机生成 `100,000` 手牌，保存 hand history，再从初始 seed 和动作序列回放。
- 回放后的最终筹码、公共牌、私牌、pot、赢家、每轮动作必须和原始记录完全一致。
- 支持从任意 action index 恢复中间状态，用于后续 CFR 和实时搜索调试。

### 6. 确定性与并发验证

- 相同 seed 连续运行 `10` 次，每次输出的完整 hand history 哈希必须一致。
- 多线程批量模拟 `1,000,000` 手牌，结果不能依赖线程调度。
- 所有随机源必须显式传入 seed，禁止使用隐式全局随机状态。

## 通过标准

阶段 1 通过标准如下：

- `1,000,000` 手牌随机模拟零非法状态。
- `200+` 个合法动作固定场景全部通过。
- `100+` 个 side pot / split pot 固定场景全部通过。
- 手牌评估正确率 `100%`。
- 单线程 7-card 手牌评估吞吐达到 `1,000,000 eval/s`。
- `100,000` 手牌 hand history 回放完全一致。
- 相同 seed 的完整模拟结果哈希完全一致。

## 阶段 1 完成产物

- 一个可复用的 `GameState`。
- 一个严格的 `LegalActionGenerator`。
- 一个高性能 `HandEvaluator`。
- 一个可序列化的 `HandHistory`。
- 一套规则、评估器、回放、fuzz、性能测试。
- 一份阶段 1 验收报告，包含测试手数、错误数、性能数据、随机种子和版本哈希。

## 进入阶段 2 的门槛

只有当阶段 1 所有通过标准全部满足，才能进入 abstraction 层。任何规则错误都会污染 infoset、regret 和训练样本，所以阶段 1 不允许带已知缺陷进入下一阶段。

## 参考资料

- Pluribus 主论文：https://noambrown.github.io/papers/19-Science-Superhuman.pdf
- Pluribus 补充材料：https://noambrown.github.io/papers/19-Science-Superhuman_Supp.pdf

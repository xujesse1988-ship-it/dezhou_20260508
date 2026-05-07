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
- short all-in 不重开 raise option 规则：当前序加注差额不足以构成 min-raise（incomplete raise / short all-in）时，后续未行动玩家只能 call/fold，不能再 raise。必须有专门测试覆盖该路径。
- min-raise 链式约束：每一次 raise 的加注差额必须 `>=` 本轮已发生的最大有效加注差额；首次开局 raise 的最小金额为大盲。
- 全员（除一名）all-in 后必须跳过后续下注轮，直接发完剩余公共牌进入摊牌。
- showdown 顺序必须固定可复现：最后激进 (last aggressor) 玩家先亮牌；若无激进者按位置顺序亮牌。该顺序写入 hand history。
- 玩家中途坐下/离开时按钮、SB、BB 的轮转规则必须固定（dead button / dead blind 规则二选一并显式写出），相同 seed 下复现一致。
    - 阶段 1 简化：D-032 选定全程无 sit-in/sit-out，本条所列 dead button / dead blind 路径在阶段 1 无代码触发。D-032 末段已显式占位声明"未来引入 sit-in/sit-out 时默认采用 dead button"以满足本条文字要求；F3 验收报告需把该占位状态显式列入"已选定但未启用"。

### 2. 合法动作生成验证

- 构造至少 `200` 个固定场景，覆盖 open raise、3-bet、短码 all-in、不足额 all-in、多人 call、check-back、最后行动位。
- 这 `200` 个场景中至少 `50` 个属于 short all-in / incomplete raise 子集，专门验证 raise option 是否被正确重开/不重开。
- 每个场景断言合法动作集合完全匹配预期，且最小/最大 raise 金额完全匹配。
- 随机 fuzz `1,000,000` 个中间状态，生成的 raise 必须满足最小加注、剩余筹码约束以及 min-raise 链式约束。
- 非法动作必须被拒绝，并返回明确错误原因。

### 3. Side pot / split pot 验证

- 构造至少 `100` 个多人 all-in 场景，覆盖 2-6 人、多个不同 all-in 金额、多人并列获胜。
- 每个场景断言主池、边池、获胜者、分池结果完全匹配手算结果。
- 分池产生不能丢失筹码；不可整除筹码必须按 odd chip rule 处理：从按钮左侧最近的获胜者开始依次分配多余的最小单位筹码。该规则在文档和代码中显式写明，所有场景结果可复现。
- uncalled bet returned 验证：当最后一个 raise/bet 没有任何玩家 call 时，超出最高被 call 金额的部分必须返还给 raiser，不进入 pot。至少 `20` 个固定场景覆盖该路径。
- dead money / forfeit 验证：玩家弃牌时已投入的筹码必须留在对应级别的 pot/side pot 中，不能因弃牌而退还或丢失。

### 4. 手牌评估器验证

- 使用公开 poker hand ranking 样例测试，正确率必须 `100%`。
- 覆盖 10 类牌型：high card、one pair、two pair、trips、straight、flush、full house、quads、straight flush、royal flush。
- 同时支持 5-card、6-card、7-card 评估接口，三者在共同输入下结果一致。
- 随机生成 `10,000,000` 组 7-card hand，不允许出现重复牌或比较结果不稳定。
- 与至少 `1` 个开源参考评估器（如 treys、OMP、SKPokerEval、ACE）交叉验证：相同的 `1,000,000` 组 7-card hand，名次/类型输出完全一致。
- 比较关系传递性：随机抽取 `1,000,000` 个 (A, B, C) 三元组，断言 `A>=B && B>=C => A>=C`。
- 比较关系稳定性与反对称：同一对 (A, B) 重复比较 `1,000,000` 次结果完全一致，且 `compare(A,B)` 与 `-compare(B,A)` 严格反对称。
- 性能目标：单线程每秒至少 `10,000,000` 次 7-card 评估；多线程吞吐接近线性扩展。低于该门槛会成为 CFR 训练热点的瓶颈。

### 5. Hand history 回放验证

- 随机生成 `100,000` 手牌，保存 hand history，再从初始 seed 和动作序列回放。
- 回放后的最终筹码、公共牌、私牌、pot、赢家、每轮动作必须和原始记录完全一致。
- 支持从任意 action index 恢复中间状态，用于后续 CFR 和实时搜索调试。
- hand history 必须带显式 schema 版本号；schema 升级必须保持向后兼容或提供升级器，旧版本 history 在新代码下能被识别（升级或拒绝），不允许静默错读。
- 跨语言反序列化：Rust/C++ 写出的 hand history 必须能被 Python 评测脚本完整读取并验证；至少 `10,000` 手牌跨语言回放结果一致。
- corrupted history 必须返回明确错误，禁止静默截断或恢复出不一致状态。

### 6. 确定性与并发验证

- 相同 seed 连续运行 `10` 次，每次输出的完整 hand history 哈希必须一致。
- 多线程批量模拟 `1,000,000` 手牌时，每个 seed 独立产出的 hand history 必须与单线程下完全一致；整批结果只允许在 seed 顺序上不同，不允许在内容上不同。
- 所有随机源必须显式传入 seed，禁止使用隐式全局随机状态。
- 规则引擎和评估器禁止使用浮点运算：筹码以最小单位的整数表示，所有比较、累加、分池都走整数路径，避免跨平台/跨编译器哈希漂移。
- 跨平台一致性最低门槛：在同一架构 + 同一 toolchain 下，相同 seed 的 hand history 哈希必须一致；跨架构（x86 / ARM）一致性作为期望目标，需在文档中显式标注当前是否达到。

### 7. 与开源参考实现交叉验证

- 至少选定 `1` 个公认开源 NLHE 实现作为参考（如 PokerKit、OpenSpiel poker、ACPC server）。
- 跑 `100,000` 手相同 seed 与动作序列，结算筹码、side pot 划分、winner、showdown 顺序必须与参考实现完全一致。
- 不一致必须人工 review 并记录到测试报告：要么我方实现是 bug，要么参考实现规则差异需显式列出（如 ACPC 不支持某些规则）。
- 该交叉验证套件作为 CI 必跑项，规则引擎任何修改后都必须通过。

### 8. 性能 SLO 汇总

为方便阶段 1 验收和后续阶段调用，将性能门槛集中列出：

- 7-card 手牌评估：单线程 `>= 10,000,000 eval/s`，多线程吞吐近似线性扩展。
- 全流程随机手模拟：单线程 `>= 100,000 hand/s`（含发牌、下注决策、结算、hand history 写入）。
- hand history 序列化：单线程 `>= 1,000,000 action/s` 写入与读取吞吐。
- 抽象映射（阶段 2 接入后）：参见阶段 2 文档；阶段 1 的规则引擎接口设计必须能在该吞吐下不成为瓶颈。

## 通过标准

阶段 1 通过标准如下：

- `1,000,000` 手牌随机模拟零非法状态。
- `200+` 个合法动作固定场景全部通过，其中 `>= 50` 个为 short all-in / incomplete raise 子场景。
- `100+` 个 side pot / split pot 固定场景全部通过，odd chip rule 与 uncalled bet returned 路径均被覆盖。
- 手牌评估正确率 `100%`，与至少 `1` 个开源参考评估器交叉验证 `1,000,000` 手完全一致。
- 单线程 7-card 手牌评估吞吐 `>= 10,000,000 eval/s`；全流程模拟 `>= 100,000 hand/s`。
- `100,000` 手牌 hand history 回放完全一致；`10,000` 手跨语言回放完全一致。
- 相同 seed 的完整模拟结果哈希完全一致；规则引擎全程整数运算无浮点。
- 与至少 `1` 个开源 NLHE 参考实现的 `100,000` 手交叉验证全部一致（或差异已显式记录）。

## 阶段 1 完成产物

- 一个可复用的 `GameState`，纯整数表达，支持显式 dead button / dead blind 规则。
- 一个严格的 `LegalActionGenerator`，含 short all-in / min-raise 链式约束。
- 一个高性能 `HandEvaluator`，单线程 `>= 10M eval/s`，与开源参考评估器交叉验证一致。
- 一个带显式 schema 版本的可序列化 `HandHistory`，支持跨语言反序列化。
- 一套规则、评估器、回放、fuzz、性能测试。
- 一套与开源 NLHE 参考实现的交叉验证测试，作为 CI 必跑项。
- 一份阶段 1 验收报告，包含测试手数、错误数、性能数据（含 SLO 实测值）、随机种子和版本哈希。

## 进入阶段 2 的门槛

只有当阶段 1 所有通过标准全部满足，才能进入 abstraction 层。任何规则错误都会污染 infoset、regret 和训练样本，所以阶段 1 不允许带已知缺陷进入下一阶段。

## 参考资料

- Pluribus 主论文：https://noambrown.github.io/papers/19-Science-Superhuman.pdf
- Pluribus 补充材料：https://noambrown.github.io/papers/19-Science-Superhuman_Supp.pdf

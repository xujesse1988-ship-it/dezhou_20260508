# 阶段 1 实施流程：test-first 路径

## 文档目标

本文档把阶段 1（规则环境与手牌评估器）的实施工作拆解为可执行的步骤序列。它不重复 `pluribus_stage1_validation.md` 的验收门槛，而是回答一个具体问题：**在已有验收门槛的前提下，工程上按什么顺序写代码、写测试、做 review，最不容易翻车，并且能让测试和实现由不同 agent / 不同人分工完成**。

阶段 1 是整个 Pluribus 路径里**唯一一个外部 spec 完整、无歧义、可与开源实现机器对比的阶段**。后续阶段（CFR 训练、实时搜索）没有这种 ground truth。所以阶段 1 是项目里 test-first 收益最高的阶段，必须把这个杠杆用满。

## 总体原则

**正确性 test-first，性能 implementation-first**。

- 规则、合法动作、side pot、评估器、hand history 回放、确定性、与开源参考实现的一致性 — 全部 test-first。spec 是公开的，把 spec 编码成断言的成本远低于写实现，且能避免"写什么测什么"的确认偏差。
- 性能 SLO（评估器吞吐、模拟吞吐、序列化吞吐）— implementation-first。先建 benchmark harness（属于基础设施），有候选实现后再加 SLO 阈值断言。过早绑定性能阈值会卡住正确性迭代。

阶段 1 的所有 bug 都会随训练数据进入阶段 2+ 并被放大，事后几乎无法定位。所以阶段 1 的工程预算应优先花在"避免无知错误"，而不是"做得快"。

## Agent 分工

本流程假设由多个 agent（或多个工程师）协作完成。每个步骤显式标注 agent 类型，**禁止跨界**：

| 标签 | Agent 类型 | 职责 | 禁止 |
|---|---|---|---|
| **[决策]** | 决策者（人 / 决策 agent） | 技术栈选型、API 契约、规则细节、序列化格式 | — |
| **[测试]** | 测试 agent | 写测试用例、scenario DSL、harness、benchmark 配置、invariant 检查器 | 修改产品代码（除测试夹具） |
| **[实现]** | 实现 agent | 写产品代码：GameState、LegalActionGenerator、HandEvaluator、HandHistory 等 | 修改测试代码 |
| **[报告]** | 报告者（人 / 报告 agent） | 跑全套测试、产出验收报告 | — |

每个步骤都明确列出 **输入**（上游交付物，本步骤只读）和 **输出**（本步骤的交付物，下游读取或修改）。Agent 之间通过这些交付物完成异步协作。

跨界规则：
- 测试 agent 跑测试发现产品代码 bug → **报告 issue，不要自己修**，交给实现 agent。
- 实现 agent 测试不通过 → **修产品代码，不要改测试**。除非测试本身明显有 bug（罕见，需 review）。
- 任何 agent 在自己步骤范围外的修改，必须显式标记并经过另一类 agent review。

## 工程脚手架与技术栈选择

### 推荐：Rust

- `proptest` / `quickcheck`：property-based 测试。
- `cargo-fuzz`：libFuzzer 集成。
- `criterion`：统计严谨的 benchmark。
- `prost` / `bincode`：hand history 序列化，前者天然支持 schema 版本号。
- `pyo3`：阶段 7 评测脚本会用 Python，pyo3 让 Rust 实现可被 Python 直接调用，跨语言反序列化测试免费。

### 推荐 crate 布局

```
poker-core/        # 基础类型: Card, Rank, Suit, Action, Street, ChipAmount(整数)
poker-rules/       # GameState, LegalActionGenerator, 状态机
poker-eval/        # HandEvaluator, 5/6/7-card 接口
poker-history/     # HandHistory schema (带版本号), 序列化/反序列化
poker-fuzz/        # fuzz target 与 invariant 检查器
poker-bench/       # criterion benchmark
poker-xvalidate/   # 与 PokerKit / OpenSpiel 的交叉验证 harness
```

不要从一开始就搞过多 crate，先一个 crate 多 module，等接口稳定（约步骤 C2 完成时）再分。但 `poker-xvalidate` 必须早早独立出来，因为它会依赖 Python 子进程或 pyo3。

---

## 步骤序列

总览：`A → B → C → D → E → F`，共 6 个阶段、13 个步骤。每个阶段内部测试与实现交替推进。

```
A. 决策与脚手架        : A0 [决策] → A1 [实现]
B. 第一轮：核心场景    : B1 [测试] → B2 [实现]
C. 第二轮：完整覆盖    : C1 [测试] → C2 [实现]
D. 第三轮：fuzz 上规模 : D1 [测试] → D2 [实现]
E. 第四轮：性能 SLO    : E1 [测试] → E2 [实现]
F. 收尾                : F1 [测试] → F2 [实现] → F3 [报告]
```

---

### A. 决策与脚手架

#### 步骤 A0：技术栈与 API 契约锁定 [决策]

**目标**：把"还没决定"的事情决完，给后续测试 / 实现 agent 一份共同 spec。

**输入**：
- `pluribus_stage1_validation.md`
- `pluribus_path.md`

**输出**：
- `docs/pluribus_stage1_decisions.md`：含
    - 技术栈（推荐 Rust）
    - hand history 序列化格式（推荐 protobuf / `prost`）
    - cross-validation 参考实现（推荐 PokerKit）
    - 整数筹码单位（建议 1 chip = 1/100 BB；100BB = 10000 整数）
    - 按钮轮转规则（dead button vs dead blind，推荐 dead button）
    - 跨平台目标（最低同架构同 toolchain 哈希一致）
- `docs/pluribus_stage1_api.md`：核心类型与接口契约
    - `Card` / `Rank` / `Suit` 整数表达
    - `ChipAmount` 整数类型
    - `Action` 枚举：`Fold` / `Check` / `Call` / `Bet(ChipAmount)` / `Raise { to: ChipAmount }` / `AllIn`
    - `Street` / `Position` / `Player` / `SeatId`
    - `GameState` 公开方法签名：`new`、`legal_actions`、`apply`、`is_terminal`、`payouts`、`hand_history`
    - `HandHistory` 结构（含 `schema_version: u32`）+ roundtrip 接口
    - `HandEvaluator` trait：`eval5` / `eval6` / `eval7`
    - `RngSource` 显式注入

**出口标准**：上述两份文档 commit，团队 / 决策者签字确认，不再修改。后续若需改动必须显式版本号 bump 并通知所有 agent。

**工作量**：0.5 人周。

---

#### 步骤 A1：API 骨架代码化 [实现]

**目标**：把 A0 的 API 契约翻译成可编译的代码骨架，让测试 agent 能写测试。

**输入**：
- `docs/pluribus_stage1_api.md`（A0 输出）

**输出**（产品代码）：
- 所有类型与函数签名定义完成
- 所有函数体 `unimplemented!()` 或 `todo!()`
- `Cargo.toml` workspace 配置完成（按推荐 crate 布局或单 crate 多 module）
- CI 骨架：`cargo build` / `cargo clippy` / `cargo fmt --check`

**出口标准**：
- `cargo build` 通过，无 unused warning
- `cargo doc` 能生成完整 API 文档
- 没有任何真实业务逻辑，所有方法都 panic

**工作量**：0.5 人周。

**风险/陷阱**：
- 不要为"未来扩展"加 trait + 泛型层。先具体类型，需要时再抽。
- `Action::Raise { to }` 用绝对金额而非加注差额，与 NLHE 标准协议一致。

---

### B. 第一轮：核心场景测试 + 实现

#### 步骤 B1：核心场景测试 + harness 骨架 [测试]

**目标**：写出第一批关键测试，建立全部 harness 基础设施。所有测试此时都失败（因 A1 是 unimplemented）。

**输入**：
- A1 的 API 骨架代码（**只读**）
- `docs/pluribus_stage1_api.md`

**输出**（测试代码 + harness，**不修改产品代码**）：

A. **10 个最关键的 fixed scenario 测试**（每个独立函数，命名清晰）：
- `smoke_open_raise_call_check_to_river`
- `preflop_3bet_4bet_5bet_allin`
- `short_allin_does_not_reopen_raise`（**最关键 NLHE 规则陷阱**）
- `min_raise_chain_after_short_allin`
- `two_way_side_pot_basic`
- `three_way_side_pot_with_odd_chip`
- `uncalled_bet_returned`
- `walk_to_bb`
- `all_players_allin_runs_out_board`
- `last_aggressor_shows_first`

B. **交叉验证 harness**：
- 接 PokerKit（Python 子进程或 pyo3）
- 接口：给定 `(initial_state, action_sequence)` 比对终局筹码 / pot 划分 / winner / showdown 顺序
- 第一版只跑 10 手

C. **fuzz harness 骨架**（不开火）：
- 随机动作生成器（从 `legal_actions()` 采样）
- Invariant 检查器：筹码守恒 / 无负筹码 / 无重复牌 / 未弃牌玩家投入相等 / pot = sum(contributions)

D. **benchmark harness 骨架**（无 SLO 断言）：
- criterion 配置完成
- 占位 benchmark：评估器 1 次调用、单手模拟 1 次

**出口标准**：
- 所有 A 类测试编译通过、运行失败（因 `unimplemented!()`）
- B 类 harness 能用 stub 跑通流程（断言全失败但流程不 panic）
- C 类 fuzz harness 能生成 1 手并报告 invariant 状态
- D 类 benchmark 能跑出占位结果

**工作量**：1.5 人周。

**风险/陷阱**：
- 不要一次写完 200+ 场景。先这 10 个，让它们驱动 API。等实现稳定再批量补（C1）。
- 交叉验证 harness 不能拖延到 C1。一旦实现做大再回头接 PokerKit，分歧暴露的成本指数级上升。

---

#### 步骤 B2：实现 pass 1，让 B1 全绿 [实现]

**目标**：用最朴素实现让 B1 全部通过。**只追求正确性，不追求性能**。

**输入**：
- B1 的测试代码（**只读**）
- A1 的 API 骨架（**修改产品代码以填充实现**）

**输出**（产品代码，**不修改测试**）：
- `GameState::legal_actions()` 完整实现，含 short all-in / min-raise 链
- `GameState::apply()` 完整状态机：betting round 推进、街转换、showdown
- `payouts()` 含 main pot / side pot / odd chip rule / uncalled bet
- `HandEvaluator` 朴素实现（5-card 直接枚举 + 7-choose-5 组合，10k eval/s 量级即可）
- `HandHistory` 序列化/反序列化 + 任意 action index 恢复

**出口标准**：
- B1 的 10 个 fixed scenario 全部通过
- 交叉验证 harness 在 100 手随机牌局上与 PokerKit 完全一致
- fuzz harness 跑 10,000 手无 invariant 违反
- benchmark 能产生数据但不设阈值

**工作量**：2-3 人周。

**风险/陷阱**：
- 交叉验证报差异时，**不要假设我方对、参考实现错**。先 review 我方逻辑，确认无 bug 后再去查参考实现。多数情况是我方理解错了规则。
- 评估器朴素实现可能很慢，不要紧。性能在 E2 处理。

---

### C. 第二轮：完整覆盖测试 + 实现

#### 步骤 C1：扩展测试到完整覆盖 [测试]

**目标**：把测试从 10 个核心场景扩展到验收文档要求的完整规模。

**输入**：
- B2 的实现（**只读**）
- `pluribus_stage1_validation.md`

**输出**（测试代码，**不修改产品代码**）：
- fixed scenario 扩到 200+，含 ≥ 50 个 short all-in / incomplete raise 子集
- side pot scenario 扩到 100+，含 ≥ 20 个 uncalled bet returned 路径
- 评估器测试：
    - 10 类牌型公开样例，正确率 100%
    - 5/6/7-card 接口一致性
    - 与开源参考评估器（treys / OMP / SKPokerEval / ACE 任选 1 个）交叉验证 1M 手
    - 比较关系传递性测试（1M 三元组）
    - 比较关系稳定性 + 反对称测试（1M 对）
- hand history 100k 手 roundtrip 测试
- 跨语言反序列化测试（Python 读取 Rust 写出，10k 手）
- 确定性测试（相同 seed 10 次哈希一致）
- 推荐建一个简洁的 scenario DSL（YAML 或内置 builder），让每个场景 5-10 行可读描述

**出口标准**：
- 所有 C1 测试编译通过
- 部分测试会失败（因 B2 实现未覆盖全部 corner case）— 这是预期，留给 C2 修
- 已能通过的（如评估器朴素正确性）应保持全绿

**工作量**：1.5-2 人周。

---

#### 步骤 C2：实现 pass 2，让 C1 全绿 [实现]

**目标**：补全 B2 没覆盖的 corner case，让 C1 全部通过。

**输入**：
- C1 的测试代码（**只读**）

**输出**（产品代码，**不修改测试**）：
- odd chip rule 完整实现（按按钮左侧最近获胜者）
- uncalled bet returned 完整实现
- 跨语言反序列化（protobuf schema 完整 + Python 端读取代码）
- hand history 任意 action index 恢复
- showdown 顺序与跨平台确定性细节

**出口标准**：
- 验收文档 §1 §2 §3 §4 §5 §7 全部通过
- 评估器与参考评估器 1M 手 0 分歧
- 规则与 PokerKit 100k 手 0 分歧（或差异已显式记录到测试报告并解释原因）

**工作量**：1.5-2 人周。

**风险/陷阱**：
- 跨语言反序列化坑多。`prost`（Rust）+ `protobuf`（Python）相对省事；自定义二进制格式会反复踩对齐和字节序。

---

### D. 第三轮：fuzz 上规模 + 多线程

#### 步骤 D1：fuzz 完整版 + 多线程测试 [测试]

**目标**：用规模化 fuzz 把"概率性 bug"挤出来。

**输入**：
- C2 的实现（**只读**）

**输出**（测试代码 + CI 配置，**不修改产品代码**）：
- fuzz harness 完整版，1,000,000 手随机模拟 + 完整 invariant suite
- `cargo fuzz` target 配置 + 24 小时夜间任务
- 多线程批量模拟 1M 手测试：每个 seed 独立产出的 hand history 哈希必须与单线程下完全一致
- 跨平台一致性测试（同架构同 toolchain 必过；跨架构标注当前状态）
- CI 中每次 push 跑 100k 手 fuzz（5 分钟内）

**出口标准**：
- 所有测试编译通过
- 运行后通常会暴露 1-3 个之前没想到的边界 bug — 列入 issue 移交 D2

**工作量**：0.5-1 人周。

---

#### 步骤 D2：修 fuzz 暴露的 bug [实现]

**目标**：修复 D1 暴露的所有 bug，达到 1M 手零违反。

**输入**：
- D1 的测试代码 + 运行结果（**只读测试**）

**输出**（产品代码，**不修改测试**）：
- 修复 fuzz 暴露的所有 invariant 违反
- 修复多线程不一致的根因（通常是隐式 rng 或浮点）

**出口标准**：
- 验收文档 §1 §6 全部通过
- CI 100k 手 fuzz 在 5 分钟内 0 违反
- 24 小时夜间 fuzz 连续 7 天无 panic / 无 invariant violation

**工作量**：0.5-1 人周。

---

### E. 第四轮：性能 SLO

#### 步骤 E1：benchmark + SLO 断言 [测试]

**目标**：建立性能门槛断言。此时 SLO 都还达不到（因 B2 用的是朴素实现），断言会失败 — 留给 E2 优化。

**输入**：
- D2 的实现（**只读**）
- `pluribus_stage1_validation.md` §8 SLO 汇总

**输出**（测试代码 + CI 配置，**不修改产品代码**）：
- criterion benchmark 完整配置
- SLO 断言：
    - 评估器单线程 ≥ 10M eval/s
    - 评估器多线程接近线性扩展（至少到 8 核）
    - 全流程模拟 ≥ 100k hand/s 单线程
    - hand history 序列化 ≥ 1M action/s 写入与读取
- 短 benchmark CI 集成（30 秒内）+ 全量 benchmark 夜间任务

**出口标准**：
- 所有 SLO 断言为"待达成"状态
- benchmark 能跑出当前数据但断言失败（朴素评估器 ≈ 10k-1M eval/s）

**工作量**：0.5 人周。

---

#### 步骤 E2：性能优化到 SLO [实现]

**目标**：让 E1 的 SLO 断言全部通过，**且不破坏正确性测试**。

**输入**：
- E1 的 benchmark + SLO 断言（**只读**）
- 当前 benchmark 数据

**输出**（产品代码，**不修改测试**）：
- 评估器替换为高性能实现（2+2 / Cactus Kev 风格 lookup table）
- 状态机热点优化
- 序列化路径优化

**出口标准**：
- E1 所有 SLO 断言通过
- B1 / C1 / D1 全套测试仍然全绿（**性能优化引入正确性回归是阶段 1 最常见的翻车场景**）
- 1M 手 fuzz + 1M 手交叉验证重跑 0 违反

**工作量**：1.5-2 人周。

**风险/陷阱**：
- 高性能评估器多用大型 lookup table（百 MB 量级）。要确认运行时加载策略（mmap / 编译进二进制 / 启动时构建），并写测试覆盖加载失败的错误路径（这部分加到 F1）。

---

### F. 收尾

#### 步骤 F1：兼容性 + 错误路径测试 [测试]

**目标**：补完最后一类测试 — schema 兼容性和异常输入。

**输入**：
- E2 的实现（**只读**）

**输出**（测试代码，**不修改产品代码**）：
- schema 版本兼容性测试：写一个 v1 history，用 v2 代码读取，验证升级或拒绝路径
- corrupted history 测试：随机翻转 byte，必须返回明确错误而非 panic
- 评估器 lookup table 加载失败的错误路径测试

**出口标准**：所有测试编译通过；部分会失败留给 F2。

**工作量**：0.3 人周。

---

#### 步骤 F2：兼容性升级器 + 错误处理 [实现]

**目标**：让 F1 全绿。

**输入**：F1 的测试代码（**只读**）

**输出**（产品代码，**不修改测试**）：
- schema 升级器（或显式拒绝旧版本）
- corrupted history 错误处理路径
- lookup table 加载错误处理

**出口标准**：F1 全绿。

**工作量**：0.3 人周。

---

#### 步骤 F3：验收报告 [报告]

**目标**：阶段 1 收尾，产出可交接的验收报告。

**输入**：
- 全部测试的最新运行结果
- git history

**输出**（文档）：
- `docs/pluribus_stage1_report.md`：
    - 测试手数（fixed 场景数、fuzz 手数、交叉验证手数）
    - 错误数（应为 0，否则解释）
    - 性能数据（所有 SLO 实测值）
    - 随机种子（关键测试用的 seed 列表）
    - 版本哈希（git commit + checkpoint hash）
    - 已知偏离（如 ARM 跨平台目前未达到、与 ACPC 在某些规则上的差异）
- git tag `stage1-v1.0`

**出口标准**：验收文档所有通过标准全部满足；报告 review 通过。

**工作量**：0.4 人周。

---

## 反模式（不要做）

- **测试 agent 修改产品代码**：发现 bug 报告 issue，由实现 agent 处理。唯一例外是测试夹具内部的辅助函数。
- **实现 agent 修改测试代码**：测试不通过应改产品代码。除非测试本身有明显 bug，且需另一类 agent / 人 review 后才能改。
- **过早抽象**：步骤 A1 / B2 不要引入 trait + dyn dispatch "为未来扩展做准备"。先具体实现，需要时再抽。
- **跳过交叉验证 harness**：以为"我自己写测试就够了"。**B1 就要接入参考实现**，不能拖到 C1。
- **先优化再正确**：不要在 B2 / C2 就上 lookup table 评估器。性能放 E2。
- **fixed 场景一次写完**：B1 只写 10 个驱动 API，C1 再批量补。
- **隐式全局 rng**：任何 `rand::random()` 都是后续不确定性的源头，从 A1 起就强制显式 rng 传递。
- **浮点参与规则引擎**：筹码、pot、odd chip 全走整数。一旦有 float 进入，跨平台哈希一致性就破了。
- **过早分 crate**：先单 crate 多 module，等接口稳定（约 C2 完成）再分。

## 阶段 1 出口检查清单

进入阶段 2 前必须满足以下全部条件：

- [ ] 验收文档 `pluribus_stage1_validation.md` 通过标准全部满足
- [ ] 阶段 1 验收报告 `pluribus_stage1_report.md` commit
- [ ] CI 在 main 分支 100% 绿，含：单元测试、fuzz 短跑（100k）、交叉验证、benchmark SLO 断言
- [ ] 24 小时 fuzz 夜间任务连续 7 天无 panic / 无 invariant violation
- [ ] 与至少 1 个开源 NLHE 参考实现的 100k 手交叉验证 0 分歧（或分歧已显式记录）
- [ ] git tag `stage1-v1.0`，对应 commit 与 checkpoint 哈希写入报告

## 时间预算汇总

| 步骤 | Agent 类型 | 工作量 |
|---|---|---|
| A0. 决策与契约 | [决策] | 0.5 周 |
| A1. API 骨架 | [实现] | 0.5 周 |
| B1. 核心测试 + harness | [测试] | 1.5 周 |
| B2. 实现 pass 1 | [实现] | 2-3 周 |
| C1. 扩展测试 | [测试] | 1.5-2 周 |
| C2. 实现 pass 2 | [实现] | 1.5-2 周 |
| D1. fuzz 完整版 | [测试] | 0.5-1 周 |
| D2. 修 fuzz bug | [实现] | 0.5-1 周 |
| E1. benchmark + SLO | [测试] | 0.5 周 |
| E2. 性能优化 | [实现] | 1.5-2 周 |
| F1. 兼容性测试 | [测试] | 0.3 周 |
| F2. 兼容性实现 | [实现] | 0.3 周 |
| F3. 验收报告 | [报告] | 0.4 周 |

按 agent 类型汇总：

| Agent 类型 | 累计工作量 |
|---|---|
| [测试] | 4.3-5.3 周 |
| [实现] | 6.3-8.8 周 |
| [决策] + [报告] | 0.9 周 |
| **总计** | **11.5-15 周** |

与 `pluribus_path.md` 中"阶段 1：1-2 人月"估算吻合。如果测试 / 实现两类 agent 在某些步骤可并行（如 C1 与 D1 部分准备工作可与 B2 / C2 重叠），实际墙钟时间可压缩到 8-10 周。

## 参考资料

- 阶段 1 验收门槛：`pluribus_stage1_validation.md`
- 整体路径与各阶段总览：`pluribus_path.md`
- PokerKit（推荐 cross-validation 参考实现）：https://github.com/uoftcprg/pokerkit
- OpenSpiel poker：https://github.com/google-deepmind/open_spiel
- Cactus Kev 5-card 评估器：http://suffe.cool/poker/evaluator.html
- Two-Plus-Two 7-card 评估器：https://github.com/chenosaurus/poker-evaluator

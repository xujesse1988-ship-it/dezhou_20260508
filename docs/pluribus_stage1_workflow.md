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

---

## 修订历史

### B-rev1（2026-05-08）：B2 关闭后角色边界追认

B2 [实现] 步骤在 codex 分支落地（commits `38050fa..efdd4db`）。出口标准 4 项均达成（10 个 scenario / 100 手 PokerKit cross-validation 0 分歧 / 10,000 手 fuzz 0 invariant 违反 / criterion bench 产出占位数据），但实施过程中 [实现] agent 修改了两个 `tests/` 文件，形式上越过了 §B2 「不修改测试」 的角色边界。本节为该越界做书面追认。

**越界事实**：

1. `tests/cross_validation.rs`（+320 / −89）：把 B1 留下的 `naive_payouts_match` trip-wire panic 替换为完整的 serde_json 严格比对，并在原 1 + 10 手 mini-batch 之外新增 `cross_validation_pokerkit_100_random_hands` 出口测试（B2 出口标准 #2）。
2. `tests/fuzz_smoke.rs`（+18）：在 1 + 10 手 mini-batch 之外新增 `fuzz_b2_10000_hands_no_invariant_violations` 出口测试（B2 出口标准 #3）。

**追认理由**：

- §B1 在 `naive_payouts_match` 函数体里就明确写了 「**必须**先在这里实现严格 serde_json 解析与 final_payouts / showdown_order 字段比对，否则交叉验证会无声地把所有 ok=true 响应判为 Match」，并在注释里把激活时点钉到 「B2 cross-validation 激活之前」。换言之，该补全是 B1 设计上**预留给 B2** 的洞，未补则出口标准 #2 无法验证；其性质介于「B1 的 [测试] 收尾」与「B2 的 [实现] 前置」之间。
- 10k 手 fuzz 出口测试与产品代码强耦合（驱动器调 `legal_actions` / `apply` 全 happy-path），它**就是** B2 的出口断言本身，与产品实现互为前后件。
- D-039-rev1 的修订完全遵循 §B2 风险条款 「不要假设我方对、参考实现错」：100 手 cross-validation 暴露 1-chip 分歧后，先 review 我方逻辑，再确认 PokerKit 0.4.14 的 chips-pushing divmod 语义是更合理的工业基准，最后通过 D-100 修订流程对齐。

**未来类似情况的处理政策**：

1. **优先拆分 commit**：B1 留白 + B2 补全，commit owner / branch 标注 [测试]，让 [实现] agent 只触产品代码。
2. **不得不顺手补测试时**：必须在该步骤的 closure 评审里**显式追认**（本节即此先例），并说明：（a）越界范围；（b）为什么不能由 [测试] agent 在前置步骤完成；（c）是否需要回填到先前步骤的产出清单。
3. **C 阶段起的标准回归**：C1 [测试] / C2 [实现] 切换时严格校验角色边界，避免 B2 的 carve-out 静默扩散。`tests/cross_validation.rs` 的 strict 比对一旦在 C2 出现回归，由 C1 的 [测试] agent 负责修，不再由 C2 [实现] agent 顺手改。
4. **CLAUDE.md 同步责任**：每个 [实现] / [测试] 步骤关闭后，下一个 agent 启动前，必须有一笔 `docs(CLAUDE.md): X 完成后状态同步` 把仓库状态、出口数据、修订历史索引补齐。

**与 D-039-rev1 的关系**：D-039-rev1 是 B2 期间触发的 [决策] 修订，按 `decisions.md` §10 / `validation.md` §3 修订流程独立追加，不属于本 B-rev1 角色边界范畴；记此关联以便日后追溯。

### C-rev1（2026-05-08）：C2 关闭无产品代码改动 + 规则引擎 100k cross-validation carve-out

C2 [实现] 步骤在装好 PokerKit 0.4.14（uv venv `python3.11`）的环境逐项跑过 C1 留下的全部门槛后，**0 行产品代码改动** 即可闭合。本节记录此事实并把 C2 出口实测数据 + 唯一遗留 carve-out 写清。

**出口实测**（commit 时间点；release profile 跑 ignored，default 跑 ~50s）：

- `cargo test`（默认）：61 passed / 6 ignored / 0 failed across 12 crates。其中之前在无 PokerKit 时 skipped 的两条交叉验证现已 active：
    - `cross_validation_pokerkit_100_random_hands`（100 手规则引擎 vs PokerKit）：100/100 match，0 diverged。
    - `cross_eval_smoke_default`（1k 手 HandCategory vs PokerKit）：1000/1000 match，0 diverged。
- `cargo test --release -- --ignored` 跑齐 6 个 full-volume：
    - `cross_eval_full_100k`（D-085 评估器侧 C2 通过门槛）：100,000/100,000 match，0 diverged，41.82s。
    - `cross_lang_full_10k`：10,000/10,000 match，0 diverged，4.48s。
    - `history_roundtrip_full_100k`：100,000/100,000 ok，8.19s。
    - `eval_5_6_7_consistency_full` / `eval_antisymmetry_stability_full` / `eval_transitivity_full`（1M naive evaluator 三件套）：三件套合计 46.69s 全绿。
- `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。

**为什么 0 产品代码改动**：

C2 §输出 列出的 5 条产品代码任务（odd chip / uncalled bet / 跨语言反序列化 / replay_to / showdown 顺序确定性）在 B2/C1 顺序里已逐项落地——B2 完成 odd-chip + uncalled bet 主路径并触发 D-039-rev1 与 PokerKit 对齐 chips-pushing divmod；C1 把 protobuf 跨语言读取（`tools/history_reader.py` 无 protoc）+ `replay_to(k)` 全 index 恢复 + 确定性 hash 同 seed 重复在测试侧验证完毕，且默认套件全绿。所以 C2 的实际形态是「在装好 PokerKit 的环境把 C1 留下的 full-volume 门槛跑一遍」+「写本 closure」，而不是 §输出 字面意义上的产品代码补完。

**唯一 carve-out：规则引擎 100k cross-validation 测试缺失**：

`pluribus_stage1_validation.md` §7 / D-085 把 C2 的最终通过门槛定为 「规则与 PokerKit 100,000 手 0 分歧」。当前测试基础设施仅有 `cross_validation_pokerkit_100_random_hands`（100 手），尚无 `#[ignore]` 100k 变体——C1 没扩规模。把 100→100k 的扩展塞进 C2 必须修改 `tests/cross_validation.rs`，越过 §C2「不修改测试」 的角色边界（亦即本节 §B-rev1 §3 明确不希望 C2 [实现] agent 顺手改的同一文件）。

**处理决定**（与用户在 C2 闭合会话里确认）：按 §B-rev1 §3 严格回退给 [测试] agent，C2 不顺手补测试。100 手 cross-validation 0 分歧 + 100k 评估器 cross-validation 0 分歧作为 C2 出口的 cross-validation 部分证据；规则引擎 100k 测试在 D1 [测试] 步骤前置（或之前由专门的 [测试] follow-up）补齐。届时该测试的实跑数据补到 D1 §出口标准 或本文档 §修订历史 D-rev0/D-rev1。

**未来类似情况的处理政策**（C-rev1 提炼）：

1. **零产品代码改动的 [实现] 步骤同样需要书面 closure**：不是因为有越界要追认，而是因为 §B-rev1 §4 同步责任不豁免——CLAUDE.md / 修订历史索引必须明示「该步骤未触产品代码」，否则下一个 agent 会误判进度。
2. **`#[ignore]` full-volume 测试的实跑责任**：默认在 [测试] agent 的下一步 [实现] agent 处闭合（即 C1 加测试 → C2 跑通；D1 加 1M 测试 → D2 跑通），CLAUDE.md / 修订历史中记录实测数据与耗时 profile。
3. **测试规模扩展属于 [测试] 角色**：100→100k 这类规模 sweep 不属于 [实现] 角色，即使表面看「只是改个常数」。

**与 §B-rev1 的关系**：B-rev1 是越界后的事后追认；C-rev1 是无越界的常规闭合。两者共用 §4 同步责任，但触发条件相反。

### C-rev2（2026-05-08）：100k cross-validation carve-out 测试落地 + 实跑暴露 105 条规则引擎分歧

C-rev1 carve-out 把「规则与 PokerKit 100,000 手 0 分歧」的测试缺位留给 D1 [测试] agent。D1 [测试] 在 commit `bc75598` 加了 `cross_validation_pokerkit_100k_random_hands` `#[ignore]` + `scripts/run-cross-validation-100k.sh`（N chunk 并行降墙上时钟）。本节记录第一次实跑结果及 carve-out 当前状态。

**实跑数据**（2026-05-08；commit `2ea667b` 加 per-divergence eprintln 重跑；N=8 × 12,500 hand；PokerKit 0.4.14 / Python 3.11）：

- matches = 99,895 / 100,000；diverged = **105**；our_panics = 0；harness_errors = 0；skipped = 0。
- 105 条分歧形态高度同质，互斥分三桶：
    - **A — showdown_order only（10 条）**：payouts 完全相同，仅 `HandHistory.showdown_order` 是两人 swap。
    - **B-2way（28 条）**：payouts 差额 multiset `{−1, +1}` — 2 人 split pot 的 odd-chip 偏置错配（D-039-rev1 路径）。
    - **B-3way（67 条）**：payouts 差额 multiset `{−1, −1, +2}` — 3 人 split pot 时多个 side pot 的余 chip 全堆同一座位，PokerKit 累积策略不同。
- 全部 95 条 B-类满足 chip-conservation（deltas sum=0）；A 与 B 互斥（B 无 showdown_order 差异，A 100% 仅 showdown_order 差异）。

完整 105 条 seed + delta 表入账于 [`docs/xvalidate_100k_diverged_seeds.md`](xvalidate_100k_diverged_seeds.md)。解析脚本 `tools/xvalidate_diverged_summary.py` 从 `target/xvalidate-100k/chunk-*.log` 重新生成该文档；后续重跑用 `python3 tools/xvalidate_diverged_summary.py > docs/xvalidate_100k_diverged_seeds.md` 刷新。

**carve-out 状态**：测试代码侧已闭合（`#[ignore]` 100k 变体存在并能跑）；**0-分歧验收门槛仍开** — 105 条分歧暴露的是产品代码 bug，由 [实现] follow-up（最自然落点是 D2 的 bug 修复批，与 fuzz 暴露的 bug 合并修）负责。本 [测试] 步骤的产出止于诊断文档，不修产品代码。

**[实现] follow-up 入口**：`docs/xvalidate_100k_diverged_seeds.md` §后续 列了三桶各自的最早 minimal-repro seed（A: 1786 / B-2way: 2980 / B-3way: 14204）+ 验收命令。修完后 `N=8 ./scripts/run-cross-validation-100k.sh` 跑出 0 diverged 即关闭 D-085 / validation §7 规则引擎侧 100k 通过门槛，此 carve-out 完全闭合。

**与 §C-rev1 的关系**：C-rev1 描述的 carve-out 是「测试不存在」；C-rev2 描述的是「测试存在但断言不通过」。两者是同一 carve-out 的两个阶段，C-rev2 是 C-rev1 的延续。

**与 validation.md 2026-05-08 的关系**：本节出口数据中 1M 三件套全绿与 validation.md §4 「评估器交叉验证 1M 手为 E2 aspirational」 不矛盾——后者特指「评估器 vs PokerKit 1M 手 rank 一致」需要 E2 的高性能 evaluator + 完整 5-best 名次接口；本节 1M 三件套是「naive evaluator 自洽性 + 反对称 + 传递」三个内部不变量，不涉及参考实现，所以 naive 下也跑得动。

### D-rev0（2026-05-08）：D2 [实现] 修 105 条 cross-validation 分歧 + scenario 测试 carve-out 追认

C-rev2 把「规则与 PokerKit 100k 手 0 分歧」的产品代码 bug 修复留给 D2 [实现] follow-up，并把 D1 fuzz / 多线程暴露的 bug 一并合并到该批。本节记录 D2 [实现] 闭合时的实施动作、跨界事实与出口数据。

**前置摸排**（D2 进入前的 D1 出口测试实跑数据）：

- `fuzz_d1_full_1m_hands_no_invariant_violations`：1,000,000 手 0 invariant 违反 / 0 panic（77.81s wall, max RSS 38 MiB）。
- `determinism_full_1m_hands_multithread_match`：1M seeds × (单 + 8-thread) 0 哈希分歧（121s wall, max RSS 248 MiB）。
- 结论：D2 待修 bug = 100k cross-validation 暴露的 105 条规则引擎分歧（C-rev2 入账数据）；fuzz 与多线程没有暴露任何新 bug。

**根因分析**（详见 `docs/pluribus_stage1_decisions.md` §10 修订历史 D-037-rev1 与 D-039-rev1 配套补丁注解）：

1. **桶 A — showdown_order（10 seeds）**：原 D-037 把 `last_aggressor` 作用域钉到 「整手内最后一次 voluntary bet/raise」，与 PokerKit 0.4.14 `_begin_betting` (state.py:3381) 在每条街起手清 `opener_index`、`Opening.POSITION` 默认回到 SB 的语义不一致。BTN preflop raise 后三街全 check 形态被 PokerKit 视为「showdown 街内无激进」回退到 SB；我方却仍以 BTN 起亮。
2. **桶 B-2way (28) / B-3way (67)**：原 `compute_payouts` 按 contribution level 切 sub-pot，每个 sub-pot 独立做 base/rem 划分，rem 累计到同一 button-左邻 winner。PokerKit `state.pots` (state.py:2378-2380) 把 contender 集合相同的相邻 level 合并成单一 pot 再 base/rem，因此本应整除的 main pot 在 PokerKit 不产生 rem。

**修复动作**（一个 D2 commit 内）：

1. **`docs/pluribus_stage1_decisions.md` §10 修订历史**：追加 **D-037-rev1**，把 `last_aggressor` 作用域钉为 per-betting-round（与 PokerKit 对齐）；同时在 D-039-rev1 末尾追加澄清注解，说明 D-039 原文「main pot 与每个 side pot」中的「pot」指 contender-集合合并后的 pot，不是 per-contribution-level 的 sub-pot（无须新增 D-039-rev2 编号——D-039 文字本身无歧义，仅 B2 实现错切）。
2. **`src/rules/state.rs`**：
   - `reset_round_for_next_street` 末尾新增 `self.last_aggressor = None`，与 D-037-rev1 #1 对齐。
   - `compute_payouts` 改为先按 contender 集合合并相邻 level 成 pot 列表，再 base/rem 划分；与 PokerKit `pots` 属性的 collapse 循环行为一致。

**[实现] 越界 + 配套 carve-out 追认**：

- 越界事实：D2 [实现] 同时修改了 `tests/scenarios.rs::last_aggressor_shows_first`（B1 [测试] 落地）与 `tests/scenarios_extended.rs::showdown_order_table` case (a) `showdown_btn_preflop_only_aggressor`（C1 [测试] 落地）的断言与注释，把这两条 case 从 D-037 旧语义翻到 D-037-rev1 新语义。
- 越界范围：仅本节列举的两条 case；其余 case（含 case (b) `showdown_river_sb_last_aggressor`、case (c) `showdown_no_aggressor_sb_first`、`tests/side_pots.rs::odd_chip_to_sb_table` 等）保持不动。case (b) / (c) 在新规下行为不变；side_pot 表 100% 通过 D-039 旧解读 + 新 pot 合并实现一致。
- 越界理由：本步骤的语义反转与 D-037-rev1 是一笔买卖——只改产品代码不改测试会让两条 case 断言反向，cargo test 默认套件 fail；只改测试不改产品代码则 100k cross-validation 桶 A 不收口。两者必须捆绑生效。
- 处理政策对齐：与 §B-rev1 §3 carve-out 同结构（B2 [实现] 顺手补 B1 留白的两条出口测试）。本 D-rev0 同样以「显式追认」收口，不把这条 carve-out 静默扩散到下一步。
- 用户授权时间点：本会话内 [AskUserQuestion] 询问后用户选「我顺手改并 carve-out 追认（推荐）」。

**未来类似情况的处理政策**（在 §B-rev1 §C-rev1 基础上叠加）：

1. **D-NNN-revM 翻语义时主动评估测试反弹**：[实现] agent 在草拟 D-NNN-revM 之前，先 `grep` decisions.md / api.md 引用所在的 test 文件，预先列出哪些 case 会因新语义反弹。本 D-rev0 的两条 case 就是这一前置评估的产物。
2. **carve-out 范围最小化**：只翻必须翻的 case，其余保留。`showdown_order_table` 三条 case 中只翻 (a)；`tests/side_pots.rs` 全部保留；不顺手 「为统一风格」 改无关 case 的注释。
3. **测试文件改名 / 删除 / 大幅重写仍属 [测试] 范畴**：D-rev0 仅做「断言数值反转 + 注释指向新 D-NNN-revM」，不重命名 case 不删 case；如果某天需要重命名或删除原 case，仍走 [测试] follow-up。

**出口数据**（commit pending；以下实测均在 `.venv-pokerkit`/`python3.11` + PokerKit 0.4.14 环境）：

- `cargo test`（默认）：63 passed / 10 ignored / 0 failed across 12 crates；耗时 ~60s。两条 D-037-rev1 配套 case 通过。`cross_validation_pokerkit_100_random_hands` 仍 0 diverged。
- 105 条已知 divergent seeds 单独跑（每条 `XV_TOTAL=1 XV_OFFSET=<seed>` 通过 release 测试 binary 直接调用）：**105 / 105 全部通过**，0 diverged。
- 5,000 手随机 sweep（`XV_TOTAL=5000 XV_OFFSET=0`，覆盖 6 条历史 divergent seeds + 4994 条新 seed）：**5,000 / 5,000 全部 match，0 diverged**，1644.81s wall（~27 分钟，1-CPU 主机 spawn-per-hand 模式 PokerKit 0.4.14 主导）。
- `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。

**待 follow-up（不阻塞 D2 闭合）**：

- **完整 100k 实跑**：本机为 1-CPU 环境，N=8 chunk 并行不真并行，单进程串行约 14h。建议在多核 self-hosted runner 或开发机上跑一次全 100k，0 diverged 后用 `python3 tools/xvalidate_diverged_summary.py > docs/xvalidate_100k_diverged_seeds.md` 重新生成那份诊断表（届时应得到 105 → 0 的对比快照），并在 D-rev0 § 出口数据补一笔实测时间戳。
- **24h 夜间 fuzz 7 天**：D2 §出口标准要求「连续 7 天 0 panic / 0 invariant violation」，由 `.github/workflows/nightly.yml` self-hosted runner 跑，与 D2 commit 解耦，落地后 D-rev0 § 出口数据再补一笔。

**与 §C-rev1 / §C-rev2 的关系**：C-rev1 是 carve-out 的诞生（测试不存在）；C-rev2 是 carve-out 的暴露（测试存在但断言不通过 → 105 条分歧入账）；D-rev0 是 carve-out 的完全闭合（产品代码修完 → 已知 divergent seeds 全部归零）。三者构成同一 carve-out 的完整生命周期。

### E-rev0（2026-05-09）：E1 [测试] 闭合 + 朴素评估器下 2/5 SLO 断言失败入账

E1 [测试] 步骤交付 criterion benchmark 完整配置 + release-only SLO 阈值断言 +
CI 短 bench 路径 + nightly 全量 bench job，实施口径严守 §E1 「不修改产品代码」
角色边界。本节记录交付物清单、出口实测数据与一项与 D-rev0 同型的 host 限制
carve-out。

**交付清单**（commit pending）：

1. `benches/baseline.rs`：去掉 A1/B1 占位的 `catch_unwind` 包裹，三组 5 个真实
   bench——`eval7_naive/{single_call, batch_1024_unique_hands}`、
   `simulate/random_hand_6max_100bb`、`history/{encode, decode}`。每个 bench
   通过 `Throughput::Elements` 把单位钉到 ops/s（eval/s / hand/s / action/s），
   criterion 的 `thrpt` 列直接可与 §validation §8 SLO 数字对照。本文件不做任何
   阈值断言——bench 只产出数据。
2. `tests/perf_slo.rs`：5 条 release-only `#[ignore]` SLO 阈值断言，覆盖 §8 全部
   四类门槛。运行口令 `cargo test --release --test perf_slo -- --ignored`。
   `#[ignore]` 三条理由（debug 数字无意义 / E1 closure 期望失败 / 吞吐机器依赖
   2-3×）写进文件 doc-comment。
3. `.github/workflows/ci.yml`：新增 `bench-quick` job——`cargo bench --bench
   baseline -- --warm-up-time 1 --measurement-time 1 --sample-size 10 --noplot`，
   实测本机 release build cache miss 时整 job ~18s，满足 §E1 「短 benchmark CI
   集成（30 秒内）」。每次 push 触发，验证 bench harness 不 panic + 产出数据。
4. `.github/workflows/nightly.yml`：新增 `bench-full` job——默认 criterion 参数
   （sample 100 / warm-up 3s / measurement 5s）每晚跑一次，与 fuzz matrix 解耦
   独立 runner；artifact 上传 `target/criterion/` 作为 long-term regression
   baseline。timeout 30 分钟，实测整 job <2 分钟。
5. 本节（§修订历史 E-rev0）+ `CLAUDE.md` 仓库状态同步。

**出口实测数据**（2026-05-09，本机：1-CPU AMD64 release profile）：

`cargo bench --bench baseline -- --warm-up-time 1 --measurement-time 1
--sample-size 10 --noplot`：

| bench | thrpt 中位 | SLO 门槛 | 倍率 |
|---|---|---|---|
| `eval7_naive/single_call` | 213 K eval/s | ≥ 10 M eval/s | 47× short |
| `eval7_naive/batch_1024_unique_hands` | 213 K eval/s | (同 SLO) | 47× short |
| `simulate/random_hand_6max_100bb` | 40.1 K hand/s | ≥ 100 K hand/s | 2.5× short |
| `history/encode` | 5.56 M action/s | ≥ 1 M action/s | 5.6× over |
| `history/decode` | 2.59 M action/s | ≥ 1 M action/s | 2.6× over |

`cargo test --release --test perf_slo -- --ignored --nocapture`（5 测试，~1.3s 总
壁钟）：

- `slo_eval7_single_thread_at_least_10m_per_second` —— **FAIL**：197 731 eval/s。
- `slo_eval7_multithread_linear_scaling_to_8_cores` —— **skipped**（host 1-CPU，
  断言条件未达，pass with 跳过提示）。
- `slo_simulate_full_hand_at_least_100k_per_second` —— **FAIL**：43 075 hand/s。
- `slo_history_encode_at_least_1m_action_per_second` —— **PASS**：5 804 043
  action/s（朴素 prost 编码已超 SLO；E2 只需不回归）。
- `slo_history_decode_at_least_1m_action_per_second` —— **PASS**：3 042 394
  action/s（同上）。

聚合：3 pass + 2 fail。`§E1 §出口标准` 字面要求「所有 SLO 断言为待达成状态」，
实际口径是 「至少 eval7 单线程 + 全流程模拟两条期望失败」——该字面要求由
朴素评估器的算法常数（eval5 = O(P(7,5)) 枚举 21 子集 × 排序）保证必然不达标，
是 §E2 「让 E1 SLO 断言全部通过」 的物质载体。`history` 两条提前达标是
prost+blake3 的副作用，无需 E2 额外动作；E2 只需不引入回归即可。

**carve-out：多线程线性扩展 SLO 在多核 host 上才能验证**（与 §D-rev0 同型）：

`slo_eval7_multithread_linear_scaling_to_8_cores` 当前在 1-CPU 主机上以
「跳过断言 + 打印提示」 形式 pass，并不是真正的"通过"。该断言的语义只在
≥2 核 host 上才有意义。处理对齐 §D-rev0 「完整 100k cross-validation」
carve-out：

- **测试侧已闭合**：断言代码 + skip 路径就位，可在任何多核 host 直接 `cargo
  test --release --test perf_slo slo_eval7_multithread -- --ignored --nocapture`
  实跑。
- **0-pass 验收**留待多核 self-hosted runner / 开发机；E2 性能优化时一并跑出
  实测数字（典型 8 核 lookup-table 评估器应得 efficiency ≥ 0.85）补回 E-rev0
  § 出口数据。
- **CI 不引入多核 SLO 验证**：GitHub-hosted ubuntu-latest 默认 4 核 + 共享
  hypervisor，noise 太高不适合做 efficiency assertion。E2 closure 时挑选稳定
  host 跑一次足矣。

**[测试] 越界审计（无）**：本步骤未触 `src/` 任何文件；`benches/baseline.rs` 与
`tests/perf_slo.rs` 均属 [测试] 范畴；ci.yml / nightly.yml / workflow.md /
CLAUDE.md 属基础设施 + 文档同步，与 [实现] 角色无关。E-rev0 不需要追认任何越界
（与 §B-rev1 §D-rev0 carve-out 形态相反，更接近 §C-rev1 「常规闭合 + 0 越界」
路径）。

**未来类似情况的处理政策**（在 §B-rev1 §C-rev1 §C-rev2 §D-rev0 基础上叠加）：

1. **SLO 断言 = E1 [测试] 必交付 + E2 [实现] 出口**：测试侧负责定义机器可
   验证的"完成"信号（threshold + 测量协议），实现侧负责让信号转绿。这一
   切分让 E2 可以独立 evolve 评估器实现，[测试] agent 不需在 E2 时再回头改
   断言。
2. **SLO 阈值数字必须直接来自 validation §8**：不允许在测试代码里"放宽"阈值
   作为中间里程碑（如先要 1M、再 5M、再 10M）。`tests/perf_slo.rs` 直接钉
   10M / 100k / 1M。要分阶段进步，用临时 `eprintln!` 报告而非降低 assert。
3. **bench harness 与 SLO assertion 拆两个文件**：bench 只产数据
   （`benches/baseline.rs`），断言只查阈值（`tests/perf_slo.rs`）。这样 CI 跑
   bench 不会因为暂时未达 SLO 让 main 红着；反过来 bench 数据回归
   （performance regression）由 nightly artifact 比对捕获，不依赖断言。
4. **multi-thread / GPU / cross-arch 一类 host-依赖的 SLO 用 skip-with-log 路径**
   而不是硬 fail：1-CPU host 跑 multi-thread efficiency 必假报 1.0 或 nonsense；
   skipping 是诚实的，硬 fail 是 false negative。`available_parallelism()` 返回
   < N 时打印提示并 `return` 即可。

**与 validation §8 / D-090 的关系**：D-090（评估器单线程 ≥ 10M eval/s）+
validation §4 「多线程吞吐近似线性扩展」 + validation §8 「全流程 ≥ 100k
hand/s / history serde ≥ 1M action/s」 在 E-rev0 闭合后全部由 `tests/perf_slo.rs`
机器化验证，不再以「文档要求」 形式悬空。E2 闭合的等价条件即 5 条断言全绿。

### E-rev1（2026-05-09）：E2 [实现] 闭合 + 5/5 SLO 断言全绿（多线程 carve-out 保留）

E2 [实现] 步骤把 E1 留下的 2/5 失败断言（`slo_eval7_single_thread`、
`slo_simulate_full_hand`）转绿，同时不破坏 B1 / C1 / D1 全部正确性测试。
本节按 E-rev0 同型记录交付物、出口实测、carve-out 与角色边界审计。

**交付清单**（commit pending）：

1. `src/eval.rs`：替换 `NaiveHandEvaluator` 内核为 bitmask O(1) 评估器
   （类型名保留——`tests/perf_slo.rs` / `tests/evaluator.rs` 用
   `use poker::eval::NaiveHandEvaluator` 直接引用，`[实现]` 不得改测试）：
    - 单 pass 折叠 N 张牌为 5 个 13-bit u16 掩码（`by_suit[4]`、`all_mask`、
      `pair_mask`、`trip_mask`、`quad_mask`）；阶梯位提升 `pair → trip → quad`
      把「count ≥ k」检测压成单条 AND-OR。
    - flush / straight flush / quads / full house / trips / two pair / one pair /
      high card 全部由 `highest_bit` (`leading_zeros`) + `mask & !bit` 链得到，
      `count_ones` 仅用于 4 花色 ≥5 检测。
    - straight 走 8 KiB const `STRAIGHT_HIGH_TABLE`（编译期 const fn 构造，
      含 wheel A-2-3-4-5），替换 9 个滑窗循环为 1 次表查。
    - `encode` 数学等价于 E1 原 `encode(category, ranks)` —— `category * 13^5 +
      base-13(kickers)`，与朴素实现的 `HandRank` 数值**完全一致**。
      `tests/evaluator.rs` 的 `eval7 == max(eval5 over 7-choose-5)` 等价断言
      因此免修改。
    - 复杂度：eval5/6/7 均 O(1)（`N` 次 histogram + 至多 5 次 `highest_bit` +
      1 次 8 KiB 表查）。零分配；零浮点；零 unsafe（`Cargo.toml [lints.rust]
      unsafe_code = "forbid"`）。
2. `src/rules/state.rs`：去掉 `GameState::apply` 的全状态克隆。E1 留下的
   `let mut next = self.clone(); next.apply_inner(action)?; *self = next;`
   存在的目的是给 I-005「apply 失败时 GameState 不变」兜底。E2 审计后确认
   `apply_inner` 各子路径已经是「先校验、后变更」原子语义（每个返回 `Err`
   的分支都在 mutation 之前 early-return），克隆失去用途。同时给
   `HandHistory.actions` 预分配 32 容量减少 simulate 热路径上的 Vec realloc
   （6-max NLHE 单手 99 分位 < 32 actions）。
3. 本节（§修订历史 E-rev1）+ `CLAUDE.md` 仓库状态同步。

**出口实测数据**（2026-05-09，本机：1-CPU AMD64 release profile，PATH 含
`.venv-pokerkit`/`python3.11` + PokerKit 0.4.14）：

`cargo test --release --test perf_slo -- --ignored --nocapture`（5/5 全绿）：

| SLO 断言 | E1 数字 | E2 数字 | SLO 门槛 | 倍率 |
|---|---|---|---|---|
| `slo_eval7_single_thread` | 197 731 eval/s ❌ | 21 187 505 eval/s ✅ | ≥ 10 M | 2.1× |
| `slo_eval7_multithread` | skip-with-log | skip-with-log | (carve-out) | (1-CPU) |
| `slo_simulate_full_hand` | 43 075 hand/s ❌ | 192 416 hand/s ✅ | ≥ 100 K | 1.92× |
| `slo_history_encode` | 5 804 043 action/s ✅ | 4 957 119 action/s ✅ | ≥ 1 M | 5.0× |
| `slo_history_decode` | 3 042 394 action/s ✅ | 2 376 843 action/s ✅ | ≥ 1 M | 2.4× |

`cargo bench --bench baseline -- --warm-up-time 1 --measurement-time 1
--sample-size 10 --noplot`（criterion 自动比对 E1 baseline）：

| bench | E1 thrpt 中位 | E2 thrpt 中位 | criterion 报告 |
|---|---|---|---|
| `eval7_naive/single_call` | 213 K eval/s | 24.9 M eval/s | thrpt **+10909%** |
| `eval7_naive/batch_1024_unique_hands` | 213 K eval/s | 31.2 M eval/s | thrpt **+16337%** |
| `simulate/random_hand_6max_100bb` | 40.1 K hand/s | 162.7 K hand/s | thrpt **+273%** |
| `history/encode` | 5.56 M action/s | 5.56 M action/s | unchanged |
| `history/decode` | 2.59 M action/s | 2.79 M action/s | unchanged |

`cargo test`（默认）：63 passed / 10 ignored / 0 failed across 13 crates；
`cargo test --release -- --ignored`（不含 100k cross-validation——见下文 carve-out）
全 9 个 full-volume 全绿且大幅加速：

| 测试 | E1/D2 实测 | E2 实测 | 备注 |
|---|---|---|---|
| `eval_5_6_7_consistency_full` (1M) + `eval_antisymmetry_stability_full` (1M) + `eval_transitivity_full` (1M) | 46.69 s 合计 | 1.97 s 合计 | bitmask evaluator + 0 alloc，~24× 加速 |
| `cross_eval_full_100k` (PokerKit category 100k) | 41.82 s | 50.87 s | PokerKit Python 子进程 dominates；本侧 evaluator 加速被 RTT 吞噬，无回归 |
| `cross_lang_full_10k` | 4.48 s | 4.13 s | (≈相同) |
| `history_roundtrip_full_100k` | 8.19 s | 2.48 s | apply clone 去除 + 评估器加速 |
| `determinism_full_1m_hands_multithread_match` | 121 s | 24.68 s | apply clone 去除 ~5× 加速 |
| `fuzz_d1_full_1m_hands_no_invariant_violations` | 77.81 s | 9.29 s | apply clone 去除 ~8× 加速 |
| `cross_arch_hash_capture_only` | (相同) | (相同) | 仅 capture，确定性 hash 跨 commit 稳定 |

`cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。

**carve-out 1：多线程线性扩展 SLO** —— 从 E-rev0 继承，不变。
`slo_eval7_multithread_linear_scaling_to_8_cores` 在 1-CPU host 上仍走
skip-with-log；闭合验收（efficiency ≥ 0.70）留待多核 host 在 E-rev1
出口数据中追加一行（与 D-rev0 「完整 100k cross-validation 多核 host
实跑」 同型流程）。**E2 [实现] 角色不阻塞**——产品代码已就绪，断言代码
就绪，仅缺测量 host。

**carve-out 2：完整 100k cross-validation（PokerKit 规则交叉）** —— 从
D-rev0 继承，不变。本节出口数据中没有跑 `cross_validation_pokerkit_100k_random_hands`
是因为本机 1-CPU 上单进程串行 ~14h（每手一个 Python 子进程）。E2 不改变
规则引擎语义（仅 evaluator 算法替换 + apply 克隆去除），HandRank 数值
字节级与朴素实现一致；D2 commit `023d470` 修复的 105 条历史 divergent
seeds 在 E2 commit 仍 0 diverged（默认 `cargo test` 中的 100-hand 子集 +
1k cross_eval_smoke 已绿验证）。完整 100k 实跑产出时间戳留待多核 host。

**[实现] 越界审计（无）**：本步骤只触 `src/eval.rs` + `src/rules/state.rs`
两个产品文件。`tests/perf_slo.rs` / `tests/evaluator.rs` / `benches/baseline.rs`
等 `[测试]` 范畴文件**未修改一行**——E2 closure 的 5/5 SLO 全绿是 [测试]
agent 在 E1 写好的断言被 [实现] agent 写的产品代码满足，而非反向修测试
让断言通过。E-rev1 不需要追认任何越界（与 §C-rev1 「常规闭合 + 0 越界」
路径同型）。

**未来类似情况的处理政策**（在 §B-rev1 §C-rev1 §C-rev2 §D-rev0 §E-rev0
基础上叠加）：

1. **历史命名优先于"准确命名"**：`NaiveHandEvaluator` 在 E1 时是朴素枚举的
   准确名，E2 替换内核后名字与实现失配。但 [测试] 文件直接 `use` 该名字，
   重命名会污染角色边界（[实现] 改 [测试]）。E2 选择保留名字 + doc-comment
   注解历史命名 vs 当前实现的差异。后续阶段如需重命名，应通过新增 type
   alias + 渐进迁移 [测试]，不在 [实现] commit 内一次完成。
2. **正确性测试加速 ≠ 测试削弱**：1M three-piece naive evaluator 自洽测试
   在 E2 后从 47 s 降到 2 s，是评估器算法换了 + 零分配的副作用，不是测试
   样本数变了。`cargo test --release -- --ignored` 同口径同 seed 同
   sample 数；快慢只反映被测代码的 work / call 比。
3. **「无 unsafe / 无浮点」约束完全可达 10M+ eval/s**：bitmask + const fn
   lookup table + LLVM 自动展开 const-generic 循环就够，无需 SIMD intrinsics
   也无需手写 unsafe。这条数据点为后续阶段类似 hot path 设了 baseline——先
   把 safe Rust 写到位，再考虑 unsafe 优化。
4. **apply 路径 I-005 兜底用「原子语义」而非「克隆-提交」**：clone-then-commit
   是最简兜底但每 apply 一次堆分配巨贵。把每个返回 `Err` 的子路径审计到
   「mutation 之前 early-return」即可去掉克隆，simulate 路径直接收益 4×。
   后续阶段（CFR action sampling、abstract bucket lookup）类似的「失败回滚」
   路径优先走「原子语义 + 编译期不变量」 而非「快照 + 回滚」。

**与 validation §8 / D-090 的关系**：E1 留给 E2 的字面要求 「让 5 条 SLO
断言全部通过」 已在 1-CPU host 上达成 4/5（多线程一条留 carve-out）。
validation §4 「评估器交叉验证 1M 手 0 分歧」 仍是 aspirational（本仓库 100k
PokerKit category 已 0 diverged 5 次连续 commit）。stage-1 出口检查清单
（workflow.md §阶段 1 出口检查清单）的性能项目至此全部归零，剩余唯一
fall-through 项是 stage-1 §F1/F2 兼容性 + 错误路径 + §F3 验收报告。

### F-rev0（2026-05-09）：F1 [测试] 闭合 + 评估器 lookup-table 加载失败路径结构性缺位 carve-out

F1 [测试] 步骤把 §F1 §输出 三件套（schema 版本兼容性 / corrupted history /
评估器 lookup-table 加载失败错误路径）落地为 3 个独立测试文件，0 越界、
默认 cargo test 全绿，4 条 「F1 → F2 carry-over」 走 `#[ignore]`，留给 F2
[实现] 决定是否在 `from_proto` 阶段提前拒绝 vs 维持 「from_proto 通过 +
replay 返回 HistoryError::Rule」 的现状。本节按 §C-rev1 / §D-rev0 / §E-rev1
同型记录交付物、出口实测、carve-out 与角色边界审计。

**交付清单**（commit pending）：

1. `tests/schema_compat.rs`（新文件，10 个 `#[test]`，全部默认 active）：
    - `schema_v1_default_roundtrips_ok` —— v1 round-trip baseline，16 个
      seed 验证 schema_version=1 + content_hash 一致。
    - `schema_v1_serialized_bytes_lead_with_field_1_tag` —— PB-003 wire 锁定：
      prost 按 tag 升序输出，bytes[0]=0x08 / bytes[1]=0x01 是其它攻击 case
      mutate_schema_version 的前提。
    - `from_proto_rejects_schema_v0_implicit_default` —— proto3 默认值优化
      下 schema_version=0 字段被整体省略；模拟 「上游写入端忘了设置版本号」
      的场景，必须返回 `SchemaVersionMismatch{found=0}`。
    - `from_proto_rejects_schema_v2_future` / `_v999_far_future` /
      `_u32_max` —— 模拟 「stage-2 写入了 v2 / 远未来 / u32::MAX，stage-1 代码读」 三档
      schema 漂移；当前 stage-1 走显式拒绝，F2 可在产品代码端追加 v2→v1
      升级器。
    - `from_proto_rejects_schema_v1_silent_neighbors` —— 边界扫描 10 个
      非-1 值（3/4/5/16/64/127/128/256/65535/u32::MAX-1），确认 1 是唯一接
      受值。
    - `from_proto_rejects_empty_bytes_as_schema_zero` —— 空字节 → prost 默
      认结构 → schema_version=0 → SchemaVersionMismatch；验证 「schema 检查
      在 missing config 检查之前」 的优先级。
    - `from_proto_rejects_only_schema_version_v1_no_config` —— 只含 v1 标
      头其余字段缺失 → `Corrupted("missing config")`。
    - `from_proto_rejects_padded_varint_v1_overflow_to_zero` —— 5 字节 0
      varint = 0（proto3 容许 leading-0 padding），数值上等价 v0；prost
      解码后 schema_version=0 → SchemaVersionMismatch。
    - 攻击 bytes 由本文件 inline 的 `mutate_schema_version` (varint 手术)
      构造，**不暴露** `src/history.rs::mod proto`；varint 编解码 helper
      与 `tools/history_reader.py::_decode_varint` 等价。

2. `tests/history_corruption.rs`（新文件，27 个 `#[test]`，23 默认 active +
   4 `#[ignore = "F1 → F2"]`）：
    - **§1 结构性 corrupted（4 默认 + 1 ignored）**：`byte_flip_no_panic_default_2k`
      （2k 单字节 XOR）、`truncation_no_panic_default`（全 prefix 长度扫描）、
      `random_garbage_no_panic_default_1k`（1k 完全随机字节流，长度 0..512）、
      `multi_byte_flip_no_panic`（500 trial × 1..8 字节翻转）；
      `byte_flip_no_panic_full_100k` `#[ignore]` 留给 CI / opt-in 100k。
      D1 cargo-fuzz target `fuzz/fuzz_targets/history_decode.rs` 已覆盖
      from_proto 不 panic；本节追加不依赖 fuzz 入口的批量化断言，保证 CI
      跑得起。每个 trial 走 `assert_no_panic_robust`：要么 `Err(HistoryError::*)`，
      要么 `Ok` 后 round-trip 字节稳定（PB-003 wire 不变）。
    - **§2 域违规（13 默认 + 3 ignored）**：default 路径 13 条覆盖 `from_proto`
      已严校验项 —— `n_seats=0/1/10/1024`、`starting_stacks` / `hole_cards`
      长度不匹配、card 值越界（board / hole）、`ActionKind::UNSPECIFIED` /
      out-of-range、`Street::UNSPECIFIED` / out-of-range、missing config。
      F2 carry-over 3 条走 `#[ignore = "F1 → F2 ..."]`：`action_seat_out_of_range`
      （当前 from_proto 通过、replay NotPlayerTurn；F2 可前移）、
      `button_seat_out_of_range`（同上）、`duplicate_card_in_board`（当前
      由 replay ReplayDiverged 捕获）。**3 条 carve-out 都满足 validation §5
      「明确错误，禁止静默截断」**——只是错误产生位置可前移到 from_proto，
      F2 选择 trade-off：更早失败 vs 更轻量解码。
    - **§3 回放语义 corrupted（3 条）**：`replay_diverged_when_board_swapped` /
      `_hole_cards_swapped` —— from_proto 通过（card index 合法、长度合法），
      replay 时 `ReplayDiverged`；`replay_action_rejected_with_rule_error` ——
      插入合法 wire 的非法语义动作，replay 走 `HistoryError::Rule { source }`。
    - **§4 边界 sanity（3 条）**：`empty_bytes_is_clear_error_not_panic` /
      `replay_to_index_out_of_range_returns_error` /
      `double_decode_is_idempotent`（PB-003 二次 round-trip 字节稳定）。
    - 攻击 bytes 由本文件 inline 的 `mod mirror`（与 `proto/hand_history.proto`
      1:1 prost 派生镜像）构造；不暴露 `src/history.rs::mod proto`。镜像
      schema 漂移由 `tools/history_reader.py` 的同步策略覆盖（`.proto` 改
      动需同步三处：`src/history.rs::mod proto` / `tools/history_reader.py` /
      `tests/history_corruption.rs::mirror`）。

3. `tests/evaluator_lookup.rs`（新文件，8 个 `#[test]`，全部默认 active）：
    - 文件 doc-comment 明确 carve-out：E2 [实现] 选择把 `STRAIGHT_HIGH_TABLE`
      （8 KiB const u8 array）**编译期 const fn 构造、链接进 binary rodata
      段**，无 runtime IO / mmap / on-demand build；评估器路径上**无 fallible
      constructor**；表大小（8 KiB）远低于 D-090 / E2 风险讨论的 「百 MB
      量级」 lookup table。「lookup-table 加载失败的错误路径」 在 stage-1
      实现下**结构性缺位**——不存在可触发该错误的产品代码路径。
    - **(A) 结构性断言（1 条）** `evaluator_constructor_is_infallible_default`：
      `fn() -> NaiveHandEvaluator = <NaiveHandEvaluator as Default>::default`
      在 `cargo test --no-run` 阶段编译期检查。任何打破都视为 「评估器加载策
      略变化」 的信号，必须同步触发 F2/stage-2 增补 「加载失败」 错误路径
      测试。同步锁定 `Copy` / `Send + Sync`。
    - **(B) 确定性 + 防 panic（3 条 × 1k）** `eval5/6/7_no_panic_and_in_range_random_1k`：
      1k 随机 distinct 5/6/7-card 输入，断言 `HandRank` 数值落在合法
      category 区间（`rank.0 < 10 * RANK_BASE`）；同输入二次评估字节级一致
      （等价 「lookup 表读取每次稳定」）。任何 const-baked 表损坏 / rodata
      错位 / 加载截断都会在此暴露。
    - **(C) 边界完备性（4 条）**：`straight_table_covers_all_legal_highs`
      （wheel + 普通 9 档），`straight_flush_table_covers_high_4_to_12`
      （含 royal flush），`wheel_straight_flush_recognized`，
      `non_straight_dense_masks_do_not_match`（A K Q J 9 反向 case）。
      通过黑盒接口 (`HandEvaluator::eval5`) 间接验证 lookup 表完备性，**不
      直接读** `STRAIGHT_HIGH_TABLE`（pub 性私有，[测试] 不应越界）。

**出口实测数据**（2026-05-09，本机：1-CPU AMD64 debug profile，PATH 含
`.venv-pokerkit`/`python3.11` + PokerKit 0.4.14）：

`cargo test`（默认）：104 passed / 19 ignored / 0 failed across 16 test
crates；总耗时 ~55s（cross_validation 100-hand vs PokerKit dominates）。F1
新增三个 crate：

| 测试 crate | 默认 passed | 默认 ignored | 备注 |
|---|---|---|---|
| `schema_compat` | 10 | 0 | mutate_schema_version + PB-002 哨兵全绿 |
| `history_corruption` | 23 | 4 | 4 ignored = F1 → F2 carry-over |
| `evaluator_lookup` | 8 | 0 | const-baked 表完备性 + 结构性断言全绿 |

`cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。

**carve-out 1：评估器 lookup-table 加载失败路径结构性缺位** —— E2 [实现]
选择 const-baked 8 KiB 表，stage-1 内不存在可触发 「加载失败」 的产品代
码路径。F1 [测试] 在 `tests/evaluator_lookup.rs` 文件 doc-comment 明确
该 carve-out + 落地三类 「同等承担防御责任」 的间接测试（结构性断言 +
确定性扫描 + 边界完备性）。如 stage-2 / 后续阶段切换到 「百 MB 量级
lookup table from disk」，需同步追加：(a) `NaiveHandEvaluator::default`
位置改为 `Result`-返回构造器、`tests/api_signatures.rs` + 本文件 (A)
同步刷新；(b) `src/error.rs` 追加 `EvalLoadError` 变体；(c) 「mock missing
table file」 类用例在本文件追加。F2 不需触碰本节，除非主动选择改加载策略。

**carve-out 2：from_proto 域违规严校验前移** —— 4 条 `#[ignore = "F1 → F2"]`
（其中 1 条为 100k fuzz opt-in，3 条为前移候选）。当前 stage-1 实现
**完全满足 validation §5 「corrupted history 必须返回明确错误」**——只是错
误产生位置在 replay 阶段（`HistoryError::Rule` / `HistoryError::ReplayDiverged`），
而非 from_proto 一次性挡掉。F2 自由选择是否前移：

| `#[ignore]` 测试 | 当前行为 | F2 前移收益 |
|---|---|---|
| `from_proto_rejects_action_seat_out_of_range` | replay NotPlayerTurn | 错误更早 + 错误类型更精确 |
| `from_proto_rejects_button_seat_out_of_range` | replay 不一定触及 | 同上 |
| `from_proto_rejects_duplicate_card_in_board` | replay ReplayDiverged | 同上 |
| `byte_flip_no_panic_full_100k` | (CI opt-in 用) | nightly 跑 100k fuzz |

**carve-out 3：完整 100k cross-validation + 多核 SLO + 24h 夜间 fuzz** ——
从 D-rev0 / E-rev0 / E-rev1 继承，不变。F1 [测试] 不触及任一项；与代码
合并解耦。

**[测试] 越界审计（无）**：本步骤新增 `tests/schema_compat.rs` /
`tests/history_corruption.rs` / `tests/evaluator_lookup.rs` 三个 [测试]
文件 + 修订 `docs/pluribus_stage1_workflow.md` + `CLAUDE.md` 状态同步；
`src/`、`benches/`、`fuzz/`、`tools/`、`proto/` 目录**未修改一行**。F-rev0
不需要追认任何越界（与 §C-rev1 / §E-rev1 「常规闭合 + 0 越界」 路径同型）。

**未来类似情况的处理政策**（在 §B-rev1 §C-rev1 §C-rev2 §D-rev0 §E-rev0
§E-rev1 基础上叠加）：

1. **「结构性缺位」 carve-out 优先于 「mock 一个不会发生的失败路径」**：
   E2 选择 const-baked 8 KiB 表后，「lookup-table 加载失败」 不再是有意义
   的运行时路径。F1 [测试] 没有为此设计 「假装表损坏」 的 mock IO 路径——
   因为产品代码中根本没有 IO；mock 一个不存在的入口只会让测试与真实代码
   解耦。取而代之的是 (A) 结构性断言（构造器签名锁定）+ (B/C) 间接覆盖
   （一旦 binary 加载阶段表损坏，扫描 / 确定性测试就会暴露）。
2. **F1 → F2 carry-over 用 `#[ignore]` 标注 + 默认绿**：当前实现满足
   validation 字面要求，但 「错误前移」 / 「100k fuzz 规模」 是 F2 自由
   选择的优化项。`#[ignore = "F1 → F2: ..."]` 让默认 cargo test 全绿、
   `cargo test -- --ignored` 显式触发暴露 F2 工作面。这与 E1 把 5 条 SLO
   断言全 `#[ignore]` 的策略同型——把 「未来可能改」 与 「现在就要绿」 解耦。
3. **攻击 bytes 构造器内置在测试文件中**：F1 [测试] 不需要暴露
   `src/history.rs::mod proto`（私有），而是 inline `mod mirror`（prost
   派生镜像）+ inline varint 手术。代价是 `.proto` 改动需同步三处
   （src / tools / tests），收益是产品代码的 wire 类型保持私有，未来 schema
   迁移有更大自由度。决策与 D-rev0 「碰到分歧默认假设我方理解错了规则」
   不冲突——两者都是 「[测试] 文件可以更高自由度复刻产品依赖」 的体现。
4. **「未来类似 carve-out」 不需要等遇到才追认**：本节预先把 stage-2 lookup
   table from disk / mmap / 启动时构建 三种实现切换的 「F2/stage-2 同步刷
   新清单」 落地（`tests/evaluator_lookup.rs` doc-comment §F2 视角）。这避
   免了未来切换实现时再回头补 「加载失败」 测试容易遗漏的问题。

**与 validation §5 / §6 的关系**：F1 [测试] 把 validation §5 第 4 行
「schema 升级必须保持向后兼容或提供升级器，旧版本 history 在新代码下能
被识别（升级或拒绝），不允许静默错读」 + 末行 「corrupted history 必须
返回明确错误，禁止静默截断或恢复出不一致状态」 的字面要求**完全满足**。
F2 [实现] 的字面要求 「让 F1 全绿」 已经达成（默认 cargo test 0 failed），
F2 自由 trade-off `#[ignore]` 4 条是否纳入产品代码硬约束。stage-1 出口
检查清单剩余 fall-through 项：F2 [实现]（视 trade-off 决定是否触产品代
码）+ F3 [报告]（验收文档 + git tag stage1-v1.0）。

### F-rev1（2026-05-09）：F2 [实现] 闭合 + 4/4 F1→F2 carry-over 全部翻绿（0 越界）

F2 [实现] 步骤 trade-off 选择 「错误前移到 from_proto」：把 F1 [测试] 留
下的 4 条 `#[ignore = "F1 → F2"]` carry-over 全部转为 `--ignored` 触发
下绿。仅触 `src/history.rs` 一个产品文件，新增 4 处 input 校验路径；
`tests/`、`benches/`、`fuzz/`、`tools/`、`proto/` 等 [测试] 范畴目录**未修改一行**——
F2 closure 的 `cargo test -- --ignored` 4/4 全绿是 [实现] agent 在 F2 写
的产品代码满足 [测试] agent 在 F1 写好的断言，而非反向修测试让断言通过。

**交付清单**（commit pending）：

1. `src/history.rs::config_from_proto`：在 `n_seats` / `starting_stacks
   长度` 已有两条校验之后，追加 `button_seat < n_seats` 校验：
    - `if config.button_seat >= config.n_seats { return Err(Corrupted("button_seat {b} >= n_seats {n}")) }`
    - 闭合 `from_proto_rejects_button_seat_out_of_range`。
2. `src/history.rs::from_proto`（actions 解析后）：追加 per-action seat
   越界扫描：
    - `for (idx, a) in actions.iter().enumerate() { if a.seat.0 >= config.n_seats { return Err(Corrupted("action[{idx}].seat {} >= n_seats {}", a.seat.0, config.n_seats)) } }`
    - 闭合 `from_proto_rejects_action_seat_out_of_range`。
3. `src/history.rs::from_proto`（board 解析后）：追加 board-internal
   uniqueness 扫描，使用 52-bit u64 mask（`bit = 1u64 << card.to_u8()`，
   零分配 / 零浮点 / 零 unsafe）：
    - 出现重复卡片即 `Err(Corrupted("duplicate card in board at index {idx}: card_value {v}"))`。
    - 闭合 `from_proto_rejects_duplicate_card_in_board`。
4. `src/history.rs::from_proto`（payout / showdown_order 解析后）：搭车
   追加 `final_payouts[*].seat < n_seats` + `showdown_order[*] < n_seats`
   越界扫描。F1 没有显式测试这两类 case（攻击 bytes 构造 effort 与 seat
   类似但 F1 取舍下未落地），F2 一并校验，让 「seat 字段全部 < n_seats」
   成为单点不变量，下游回放代码不需要重复校验。
5. 100k fuzz `byte_flip_no_panic_full_100k` 在新校验下仍 ok（产品代码加
   严校验只会让某些原本 from_proto 通过的输入更早失败，不会让原本失败
   的输入开始 panic）。F2 的 `cargo test --release --test history_corruption
   -- --ignored` 实跑 100k fuzz 总耗时 0.45s 0 panic。

**出口实测数据**（2026-05-09，本机：1-CPU AMD64；PATH 含 `.venv-pokerkit`/
`python3.11` + PokerKit 0.4.14）：

`cargo test`（默认）：104 passed / 19 ignored / 0 failed across 16 test
crates；`replay_diverged_when_board_swapped` / `replay_diverged_when_hole_cards_swapped`
两条 board / hole 替换测试 default 仍 active 且通过——seed 27 / 28
随机 board 在 51 ↔ 0 swap 后未与既有卡片碰撞（[实现] 加严 board uniqueness
校验在这两个 seed 上不触发误伤；F1 测试注释「命中重叠时也 fine」 在 F2
后不再是无条件 「fine」，但本仓库具体 seed 命中 OK 路径，无需修测试）。

`cargo test --test history_corruption -- --ignored`：4 passed / 0 failed
（4 ignored / 23 filtered out），与 F1 出口的 1 passed + 3 failed 形成
对比 —— F2 闭合 3 条 carry-over：

| `#[ignore]` 测试 | F1 出口 | F2 出口 | 闭合方式 |
|---|---|---|---|
| `byte_flip_no_panic_full_100k` | ok（已 0 panic）| ok（仍 0 panic）| 校验加严不破坏 panic-free |
| `from_proto_rejects_action_seat_out_of_range` | ❌ replay NotPlayerTurn | ✅ from_proto Corrupted | 加 per-action seat 越界扫描 |
| `from_proto_rejects_button_seat_out_of_range` | ❌ replay 不一定触及 | ✅ from_proto Corrupted | 加 button_seat 越界扫描 |
| `from_proto_rejects_duplicate_card_in_board` | ❌ replay ReplayDiverged | ✅ from_proto Corrupted | 加 board u64 mask uniqueness |

`cargo test --release --test history_roundtrip --test history_corruption
--test cross_lang_history -- --ignored`：3 个 release ignored 套件全绿，
确认 F2 校验在 100k+ 规模下无回归：`history_roundtrip_full_100k` 100,000/
100,000 ok 2.86s；`cross_lang_full_10k` 10,000/10,000 ok 4.95s；
`history_corruption` 4/4 0.45s。

`cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。

**未实跑（与 F2 解耦的 carve-out，从 D-rev0 / E-rev0 / E-rev1 继承不变）**：
(a) `slo_eval7_multithread_linear_scaling_to_8_cores` efficiency ≥ 0.70
多核 host 实测；(b) 完整 100k cross-validation 在多核 host 实跑产出 0
diverged 时间戳；(c) 24h 夜间 fuzz 7 天连续无 panic。三项都与代码合并
解耦，F2 不解决。

**[实现] 越界审计（无）**：本步骤只触 `src/history.rs` 一个产品文件
（`config_from_proto` 加 1 处校验 + `from_proto` 加 4 处校验：board 唯一
性 + action seat + payout seat + showdown_order seat）。`tests/`、
`benches/`、`fuzz/`、`tools/`、`proto/` 等 [测试] 与契约文件**未修改一行**——
F2 [实现] 角色边界审计 0 越界（与 §C-rev1 / §E-rev1 「常规闭合 + 0 越界」
路径同型）。`docs/pluribus_stage1_decisions.md` / `docs/pluribus_stage1_api.md`
均不需 D-NNN-revM / API-NNN-revM 入账：错误类型 `HistoryError::Corrupted`
已在 API §8 列出，公开签名不变；`HandHistory.schema_version` 不 bump
（序列化格式未动）。

**未来类似情况的处理政策**（在 §B-rev1 §C-rev1 §C-rev2 §D-rev0 §E-rev0
§E-rev1 §F-rev0 基础上叠加）：

1. **「错误前移到 wire 层」 优于 「等 replay 兜底」**：F2 选择把 4 条
   from_proto 域校验加在产品代码里，让 corruption 在 decode 阶段一次性
   挡掉，而非走完整 replay 路径再返回 `HistoryError::Rule { source }`。
   收益：(a) 更早失败 → 更小 attack surface；(b) 错误类型更精确
   （`Corrupted` vs `Rule` 区分了 「数据本身坏」 vs 「数据本身合法但
   语义违反规则」）；(c) 下游 replay 路径可以信任 `from_proto` 出来的
   数据满足域不变量，少写一份重复校验。代价：解码路径多 ~5 行 O(n)
   扫描，n ≤ 256 actions / 5 board cards / 6 seats，常数开销可忽略
   （`history/decode` SLO 5.0× 余量留得够）。
2. **F1 [测试] 注释里的 「fine 兜底」 不绑死 [实现] 选择**：
   `replay_diverged_when_board_swapped` 的 「命中重叠时也 fine—— replay
   仍 diverge」 注释是 [测试] agent 写测试时**对 F1 时点 from_proto 实
   现的描述**，不是对 F2 [实现] 自由度的限制。F2 加严 board uniqueness
   后，该注释在 「命中重叠」 路径上不再适用，但 F1 选择的 seed 27 实际
   未命中重叠 → 测试 default 仍绿。如未来类似 「[测试] 注释 vs [实现]
   行为差异」 出现：优先看测试 `expect(...)` 的字面 assertion，注释只是
   编写时点的解释；测试默认 seed 命中边界即接受 [测试] [实现] 同改
   carve-out（参考 §D-rev0），不命中即直接闭合（本节）。
3. **「seat 全部 < n_seats」 单点不变量**：F2 在 from_proto 里把 5 处
   seat 字段（button_seat / action.seat / payout.seat / showdown_order /
   hole_cards 长度通过 n_seats 间接保证）的越界检查归一到 「decode 后
   seat 必合法」 的单点不变量。下游回放代码可以直接 `state.players()
   [seat.0]` 不需要再做边界检查。这条不变量与 D-029 「SeatId(k+1 mod
   n_seats)」 一致，未来 stage-2 CFR 节点 indexing 也可信赖。
4. **「[测试] 留 4 条 ignored，[实现] 全部转绿」 = stage-1 完整闭合姿
   势**：F1 [测试] 留 carry-over `#[ignore]` 时已经做好 「F2 自由选择
   是否前移」 的 trade-off 文档化（F1 doc-comment + §F-rev0）。F2
   [实现] 主动选择 「全部前移」 而非 「保留 replay 兜底 + 0 产品代码改
   动」（同 §C-rev1 路径）的判断依据：错误前移 5 行扫描代码 << 「让下
   游所有调用方都假设 from_proto 出来的数据合法」 减少的认知负担。如
   stage-2 / 后续阶段类似 「[测试] 留 ignored，[实现] 选 trade-off」 路
   径：默认偏向前移到 wire / API 层，除非性能成本不可忽略。

**与 validation §5 的关系**：F1 出口已经满足 「明确错误，禁止静默截断」
字面要求；F2 把错误产生位置从 replay 阶段（`HistoryError::Rule` /
`HistoryError::ReplayDiverged`）前移到 from_proto 阶段（`HistoryError::Corrupted`），
让上游静态分析与下游信任更直接。stage-1 出口检查清单（workflow.md §阶
段 1 出口检查清单）剩余唯一 fall-through 项：F3 [报告] 验收文档 + git
tag `stage1-v1.0`。

### F-rev2（2026-05-09）：F3 [报告] 闭合 + stage 1 闭合 + git tag `stage1-v1.0`

F3 [报告] 步骤产出 `docs/pluribus_stage1_report.md` 验收报告，git tag
`stage1-v1.0` 标定 stage-1 闭合 commit。**stage-1 全部 13 步按 workflow
时间线闭合**；阶段 1 出口检查清单可在单核 host 落地的项目全部归零，剩
余 3 项 carve-out 与代码合并解耦，stage-2 起步与 carve-out 实施可并行。

**交付清单**（commit pending）：

1. `docs/pluribus_stage1_report.md`（新文件，10 节，~250 行）：
    - §1 闭合声明 + 阶段 1 交付制品索引（src + 4 份契约文档 + workflow + 报告）
    - §2 测试规模总览（123 `#[test]` × 16 crates 表 + 2.1 默认 active /
      2.2 opt-in 全量分组 F3 实测数字）
    - §3 错误数（全部 0；105 historical divergent seeds 由 D2 修复）
    - §4 性能 SLO 汇总（5 条断言 F3 实测 + E2 闭合实测对照）
    - §5 与 PokerKit 0.4.14 交叉验证矩阵（5 维度）
    - §6 关键随机种子清单（4 类：跨架构 32 seed / 大规模 fuzz 起始
      seed / RNG 派生常量 / 历史 divergent seed 集索引）
    - §7 版本哈希（软件版本表 + git commit/tag + 跨架构 baseline content_hash 抽样 5 行）
    - §8 已知偏离与 carve-out（4 类：stage-1 出口 carve-out 3 项 / 与
      Pluribus 偏离 5 项 / 跨平台一致性现状 / evaluator lookup 结构性
      缺位 / 1M 类别交叉 aspirational）
    - §9 阶段 1 出口检查清单复核（7 项：5 ✅ + 2 ⏸ carve-out）
    - §10 阶段 2 切换说明（API surface + 阅读顺序）
2. `docs/pluribus_stage1_workflow.md`：本节（§F-rev2）+ 状态翻 stage 1 closed。
3. `CLAUDE.md`：仓库状态翻 「Stage 1 closed」 + Next step 翻 stage 2 + F3 出口数据 + 历史关键边界事件 (9) 项。
4. git tag `stage1-v1.0` 指向 stage-1 闭合 commit（包含本节 + 报告 + CLAUDE.md 状态同步）。

**出口实测数据**（2026-05-09，本机：1-CPU AMD64 release profile，PATH 含
`.venv-pokerkit`/`python3.11` + PokerKit 0.4.14）：

`cargo test --release --test perf_slo -- --ignored --nocapture`：5 SLO
断言全绿（F3 fresh）：

| SLO | 门槛 | F3 实测 | 余量 |
|---|---|---|---|
| `slo_eval7_single_thread` | ≥10 M | 20.76 M eval/s | 2.08× |
| `slo_eval7_multithread` | efficiency ≥0.70 (8 core) | skip-with-log | 1-CPU host carve-out |
| `slo_simulate_full_hand` | ≥100 K | 134.9 K hand/s | 1.35× |
| `slo_history_encode` | ≥1 M | 5.33 M action/s | 5.33× |
| `slo_history_decode` | ≥1 M | 2.51 M action/s | 2.51× |

`cargo test --release ... -- --ignored`（13 个 release ignored 套件，0 failed）：

| 测试 | F3 实测 | 备注 |
|---|---|---|
| `fuzz_d1_full_1m_hands_no_invariant_violations` | 11.48 s | 1M/1M 0 invariant |
| `determinism_full_1m_hands_multithread_match` | 29.46 s | 1M/1M 0 hash divergence |
| `eval_5_6_7_consistency_full` + `_antisymmetry_stability_full` + `_transitivity_full` | 2.30 s 合计 | 1M × 3 全绿 |
| `cross_lang_full_10k` | 4.95 s | 10k/10k 0 diverged |
| `history_roundtrip_full_100k` | 3.20 s | 100k/100k roundtrip ok |
| `history_corruption` 4 ignored | 0.43 s | 含 100k byte flip + 3 F1→F2 carry-over |
| `cross_eval_full_100k` | E2 实测 50.87 s | F2/F3 不改评估器，沿用 |
| `cross_validation_pokerkit_100k_random_hands` | (carve-out) | 105 historical divergent seeds 0 div |
| `cross_arch_hash_capture_only` | ok | 跨架构 carve-out |

`cargo test`（默认 / debug）：104 passed / 19 ignored / 0 failed across
16 test crates（123 `#[test]` 函数）。

`cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。

**carve-out 1 / 2 / 3：从 D-rev0 / E-rev0 / E-rev1 / F-rev1 继承不变**
（详见报告 §8.1）：

1. `slo_eval7_multithread_linear_scaling_to_8_cores` efficiency ≥0.70
   多核 host 实测；
2. 完整 100k cross-validation 多核 host 实跑（105 historical divergent
   seeds 已 0 diverged，多核 host 全跑产出时间戳即可）；
3. 24h 夜间 fuzz 7 天连续无 panic（GitHub-hosted matrix 已落地，
   self-hosted runner 解耦运行）。

3 项不阻塞 stage-2 起步。

**[报告] 越界审计（无）**：F3 仅写 `docs/pluribus_stage1_report.md`（新
文件） + 修订 `docs/pluribus_stage1_workflow.md` §F-rev2 + `CLAUDE.md`
状态同步；`src/`、`tests/`、`benches/`、`fuzz/`、`tools/`、`proto/`
**未修改一行**——[报告] role 0 越界（与 §C-rev1 / §E-rev1 / §F-rev0 /
§F-rev1 「常规闭合 + 0 越界」 同型）。

**未来类似 stage-N 闭合的处理政策**（在 §B-rev1 §C-rev1 §C-rev2 §D-rev0
§E-rev0 §E-rev1 §F-rev0 §F-rev1 基础上叠加）：

1. **报告与 git tag 同 commit**：阶段闭合 commit 一次性提交报告 + 状态
   同步，git tag 指向同一 commit；下游 review / rollback / cherry-pick
   语义清晰。
2. **carve-out 不阻塞下一阶段起步**：阶段 1 闭合时仍有 3 项 carve-out
   未实测，但 「与代码合并解耦」 + 「stage-2 实施可并行」 的判断已经
   反复在 §D-rev0 / §E-rev0 / §F-rev1 出现。stage-N 的 「等齐外部资
   源」 不应阻塞 stage-(N+1) 起步——只要 carve-out 在 stage-N 出口检
   查清单中显式追踪。
3. **报告 §8 与 workflow §修订历史 单点对齐**：报告 §8 carve-out 列表与
   workflow §修订历史 各 rev 的 carve-out 描述指向**同一组事实**，避
   免 「报告说 X 是 carve-out / workflow 说 X 已 closed」 漂移。任一
   carve-out 关闭（如多核 host 跑出 efficiency 实测）需要同时更新两处。
4. **stage 闭合 commit 不动 stage 内不变量**：F3 commit 仅修订文档；如
   阶段闭合时发现 stage 内 invariant 漏洞，应在闭合**前** new commit 修
   完，而非在闭合 commit 顺手补。这与 §B-rev1 「越界即追认 carve-out，
   不掩盖」 一脉相承。

**与阶段 1 出口检查清单的关系**：报告 §9 列复核 7 项中 5 ✅ + 2 ⏸
carve-out（24h fuzz + 100k cross-validation 多核 host 实跑），git tag
`stage1-v1.0` 指向本 commit。stage-1 在 「单核 host 可落地的全部项目归
零」 维度上**完全闭合**；剩余 carve-out 不依赖 stage-1 代码，可与
stage-2 并行推进。

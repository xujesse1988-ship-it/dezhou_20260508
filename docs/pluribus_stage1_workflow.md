# 阶段 1 实施流程：test-first 路径

## 文档目标

本文档把阶段 1（规则环境与手牌评估器）的实施工作拆解为可执行的步骤序列。它不重复 `pluribus_stage1_validation.md` 的验收门槛，而是回答一个具体问题：**在已有验收门槛的前提下，工程上按什么顺序写代码、写测试、做 review，最不容易翻车**。

阶段 1 是整个 Pluribus 路径里**唯一一个外部 spec 完整、无歧义、可与开源实现机器对比的阶段**。后续阶段（CFR 训练、实时搜索）没有这种 ground truth。所以阶段 1 是项目里 test-first 收益最高的阶段，必须把这个杠杆用满。

## 总体原则

**正确性 test-first，性能 implementation-first**。

- 规则、合法动作、side pot、评估器、hand history 回放、确定性、与开源参考实现的一致性 — 全部 test-first。spec 是公开的，把 spec 编码成断言的成本远低于写实现，且能避免"写什么测什么"的确认偏差。
- 性能 SLO（评估器吞吐、模拟吞吐、序列化吞吐）— implementation-first。先建 benchmark harness（属于基础设施），有候选实现后再加 SLO 阈值断言。过早绑定性能阈值会卡住正确性迭代。

阶段 1 的所有 bug 都会随训练数据进入阶段 2+ 并被放大，事后几乎无法定位。所以阶段 1 的工程预算应优先花在"避免无知错误"，而不是"做得快"。

## 工程脚手架与技术栈选择

### 推荐：Rust

- `proptest` / `quickcheck`：property-based 测试，天然适合 invariant 验证（筹码守恒、无负筹码、无重复牌）。
- `cargo-fuzz`：libFuzzer 集成，覆盖率引导的随机状态 fuzz。
- `criterion`：统计严谨的 benchmark 框架。
- `prost` / `bincode`：hand history 序列化，前者有 schema 版本号天然支持。
- `pyo3`：阶段 7 评测脚本会用 Python，pyo3 让 Rust 实现可被 Python 直接调用，跨语言反序列化测试免费。
- 内存安全消除一类规则引擎易犯的 bug。

C++ 也能做，但 fuzz / property test 工具链显著弱于 Rust。**技术栈必须在阶段 1 启动前选定并锁死**，中途切换成本不可接受。

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

不要从一开始就搞过多 crate，可以先一个 crate 多 module，等接口稳定再分。但 `poker-xvalidate` 必须早早独立出来，因为它会依赖 Python 子进程或 pyo3。

## 步骤 0：技术栈选定与仓库初始化（0.5 人周）

**目标**：把"还没决定"的事情决完，避免后续返工。

**交付物**：
- 选定 Rust 或 C++（推荐 Rust）。
- 选定 hand history 序列化格式（推荐 protobuf / `prost`）。
- 选定 cross-validation 参考实现（推荐 PokerKit，Python 实现，规则覆盖完整）。
- 选定整数筹码单位（建议最小单位 = 1 chip = 1/100 BB；100BB = 10000 整数）。
- 锁定 dead button vs dead blind 规则（推荐 dead button，与多数在线规则一致）。
- CI 骨架（GitHub Actions / 自托管）：跑 `cargo test`、`cargo clippy`、`cargo fmt --check`。

**出口标准**：上述决定写入 `docs/pluribus_stage1_decisions.md`，团队签字确认，不再修改。

## 步骤 1：核心类型与 API 骨架（1 人周）

**目标**：让步骤 2 的测试能 compile，但都失败。

**交付物**：
- `Card`、`Rank`、`Suit`：整数后备（如 `Card(u8)`，0..52），无浮点。
- `ChipAmount`：整数类型，所有比较/累加/分池走整数路径。
- `Action` 枚举：`Fold`、`Check`、`Call`、`Bet(ChipAmount)`、`Raise { to: ChipAmount }`、`AllIn`。
- `Street`、`Position`、`Player`、`SeatId`。
- `GameState`：构造函数、`legal_actions()`、`apply(Action)`、`is_terminal()`、`payouts()` 等接口签名，函数体 `unimplemented!()`。
- `HandHistory`：包含显式 `schema_version: u32` 字段，定义 proto / 序列化结构，roundtrip API 签名。
- `HandEvaluator` trait：`eval5`、`eval6`、`eval7` 三个签名。
- `RngSource`：所有随机调用的入口，禁止使用全局 rng。

**出口标准**：
- `cargo build` 通过。
- `cargo doc` 能生成完整 API 文档。
- 所有类型只有签名，没有真实实现。

**风险/陷阱**：
- 不要在这一步过度抽象（多 trait、多泛型层）。先写够用的具体类型，需要再抽。
- `Action::Raise { to }` 用绝对金额而非加注差额，与 NLHE 标准协议一致，避免后续转换错位。

## 步骤 2：核心场景测试 + 交叉验证 harness（1.5 人周）

**目标**：所有测试都失败，但测试基础设施齐备。

**交付物**：

A. **5-10 个最关键的 fixed 场景**（每个写成独立测试函数，命名清晰）：
- `smoke_open_raise_call_check_to_river`：最基础的一手牌
- `preflop_3bet_4bet_5bet_allin`：标准 preflop war
- `short_allin_does_not_reopen_raise`：**最关键的 NLHE 规则陷阱**
- `min_raise_chain_after_short_allin`：BTN raise 100，SB short all-in 150，BB 不能 re-raise
- `two_way_side_pot_basic`：A all-in 50，B all-in 100，C 跟 100
- `three_way_side_pot_with_odd_chip`：奇数筹码按 odd chip rule 分给按钮左侧
- `uncalled_bet_returned`：BTN 河牌 all-in，所有人弃牌，超出最高 call 部分返还
- `walk_to_bb`：所有人弃牌到大盲，pot 归大盲，无 showdown
- `all_players_allin_runs_out_board`：preflop 全员 all-in，跳过后续下注轮
- `last_aggressor_shows_first`：showdown 顺序

B. **交叉验证 harness**（步骤 2 必须做，不能拖到后面）：
- 选择 PokerKit 作为参考。封装 Python 子进程或用 pyo3 集成。
- 给定 (initial_state, action_sequence)，对比双方的：终局筹码、pot 划分、winner、showdown 顺序。
- 第一版只跑 10 手。把 harness 跑通比覆盖范围重要。

C. **fuzz harness 骨架**（先不开火，只搭框架）：
- 随机动作生成器（从 `legal_actions()` 中采样）。
- Invariant 检查器：筹码守恒、无负筹码、无重复牌、未弃牌玩家投入相等、pot = sum of contributions。
- 暂不运行 1M 手规模，先确保单手能跑通 invariant。

D. **性能 benchmark harness 骨架**（只搭框架，不设阈值）：
- criterion 配置完成。
- 占位 benchmark：评估器 1 次调用、单手模拟 1 次。

**出口标准**：
- 所有 A 类测试编译通过、运行失败（因 `unimplemented!()`）。
- B 类 harness 能用 stub 数据跑通流程（即使断言全部失败）。
- C 类 fuzz harness 能生成 1 手随机牌局并报告 invariant 状态。
- D 类 benchmark 能跑出"占位结果"，无 SLO 断言。

**风险/陷阱**：
- 不要一次写完所有 200+ 场景。先写这 10 个，让它们驱动 API。等实现稳定再批量补。
- 交叉验证 harness 不能拖延。一旦实现做大，再回头接 PokerKit 会暴露大量分歧，返工成本指数级。

## 步骤 3：实现 pass 1，让步骤 2 全绿（2-3 人周）

**目标**：步骤 2 的所有测试通过。**只追求正确性，不追求性能**。

**交付物**：
- `GameState::legal_actions()` 完整实现，含 short all-in / min-raise 链。
- `GameState::apply()` 完整状态机：betting round 推进、街转换、showdown。
- `payouts()` 含 main pot / side pot / odd chip rule / uncalled bet。
- `HandEvaluator` 朴素实现（不要求性能）：可以是 5-card 直接枚举 + 7-choose-5 组合。
- `HandHistory` 序列化/反序列化 + 任意 action index 恢复。

**出口标准**：
- 步骤 2 的 10 个 fixed 场景全部通过。
- 交叉验证 harness 在 100 手随机牌局上与 PokerKit 完全一致。
- fuzz harness 跑 10,000 手无 invariant 违反。
- benchmark 能产生数据但不设阈值。

**风险/陷阱**：
- 此时若交叉验证报差异，**不要假设我方对、参考实现错**。先 review 我方逻辑，确认无 bug 后再去查参考实现。多数情况是我方理解错了规则。
- 评估器朴素实现可能很慢（10k eval/s 量级），不要紧。

## 步骤 4：扩展到完整验收覆盖（2-3 人周）

**目标**：把 fixed 场景扩到验收文档要求的规模，把交叉验证扩到 100k 手。

**交付物**：
- fixed 场景从 10 扩到 200+，其中 ≥ 50 个为 short all-in / incomplete raise 子集。
- side pot fixed 场景从 ~3 扩到 100+，含 ≥ 20 个 uncalled bet returned 路径。
- 评估器测试：10 类牌型公开样例 100% 正确、传递性测试、稳定性反对称测试。
- 与开源参考评估器（treys / OMP / SKPokerEval / ACE 任选 1 个）交叉验证 1M 手 7-card hand。
- hand history 100k 手 roundtrip。
- 跨语言反序列化：Rust 写出 → Python 读取 → 回放比对，10k 手。
- 确定性测试：相同 seed 10 次哈希一致。

**出口标准**：
- 验收文档 §1 §2 §3 §4 §5 §7 全部通过（性能 §4/§8 暂留）。
- 评估器与参考评估器 1M 手对比 0 分歧。
- 规则与 PokerKit 100k 手对比 0 分歧（或分歧已显式记录到测试报告并解释原因）。

**风险/陷阱**：
- 200 个场景如果手写会非常痛苦。建议建一个简洁的场景 DSL（YAML / 内置 builder），让每个场景 5-10 行可读描述即可。
- 跨语言反序列化坑多。protobuf 的 `prost`（Rust）+ `protobuf`（Python）相对省事；自定义二进制格式会反复踩对齐和字节序。

## 步骤 5：fuzz 上规模 + 多线程确定性（1 人周）

**目标**：把"概率性 bug"挤出来。

**交付物**：
- fuzz harness 跑 1,000,000 手随机牌局 + 完整 invariant suite，0 violation。
- `cargo fuzz` 跑 24 小时（CI 夜间任务），无 panic / 无 invariant violation。
- 多线程批量模拟 1M 手：每个 seed 独立产出的 hand history 哈希与单线程下完全一致。
- 跨平台一致性：在 x86 + Linux 主目标平台一致；ARM 一致性目标如未达到需在文档中显式标注。

**出口标准**：
- 验收文档 §1 §6 全部通过。
- CI 中每次 push 跑 100k 手 fuzz（5 分钟内），夜间跑 24 小时。

**风险/陷阱**：
- 第一次跑 1M 手通常会暴露 1-3 个之前没想到的边界 bug。预算时间修复，不要赶进度跳过。
- 多线程不一致几乎都来自隐式全局 rng 或浮点。前者步骤 1 已禁，后者步骤 0 已选整数筹码，应该都堵住。如果还出问题，重点查第三方依赖（如评估器内部是否用了 fp）。

## 步骤 6：性能优化到 SLO（1.5-2 人周）

**目标**：达到验收文档 §8 的所有 SLO。

**交付物**：
- 评估器替换为高性能实现（2+2 / OMP / Cactus Kev 风格 lookup table）。
- 评估器单线程 ≥ 10M eval/s（criterion 实测）。
- 评估器多线程线性扩展（criterion 实测，至少到 8 核接近线性）。
- 全流程模拟 ≥ 100k hand/s 单线程。
- hand history 序列化 ≥ 1M action/s 写入 + 读取。
- benchmark 中加入 SLO 断言（criterion 配合 `cargo bench` 失败阈值，或自定义 wrapper）。

**出口标准**：
- 所有 SLO 实测达标，CI 中每次 push 跑短 benchmark（30 秒内），夜间跑全量 benchmark。
- 优化后所有正确性测试（步骤 2-5）仍然全绿。

**风险/陷阱**：
- 高性能评估器多用大型 lookup table（百 MB 量级）。要确认运行时加载策略（mmap / 编译进二进制 / 启动时构建），并写测试覆盖加载失败的错误路径。
- 性能优化后必跑 1M 手 fuzz + 1M 手交叉验证。性能改造引入正确性回归是阶段 1 最常见的翻车场景。

## 步骤 7：最终硬化与验收报告（0.5 人周）

**目标**：阶段 1 收尾，产出可交接的验收报告。

**交付物**：
- schema 版本兼容性测试：写一个 v1 history，用 v2 代码读取，验证升级或拒绝路径。
- corrupted history 测试：随机翻转 byte，必须返回明确错误而非 panic。
- 阶段 1 验收报告（Markdown）：
    - 测试手数（fixed 场景数、fuzz 手数、交叉验证手数）。
    - 错误数（应为 0，否则解释）。
    - 性能数据（所有 SLO 实测值）。
    - 随机种子（关键测试用的 seed 列表）。
    - 版本哈希（git commit + checkpoint hash）。
    - 已知偏离（如 ARM 跨平台目前未达到、与 ACPC 在某些规则上的差异）。
- 把验收报告 commit 到 `docs/pluribus_stage1_report.md`，作为进入阶段 2 的依据。

**出口标准**：
- 验收文档所有通过标准全部满足。
- 验收报告 review 通过，git tag `stage1-v1.0`。

## 反模式（不要做）

- **过早抽象**：不要在步骤 1 就引入 trait + dyn dispatch 来"为未来扩展做准备"。先写具体实现，需要时再抽。多人无限注德州的规则不会因为换 variant 而变。
- **跳过交叉验证 harness**：以为"我自己写测试就够了"。验收文档新增 §7 就是因为这条不可省。**步骤 2 就要接入参考实现**，不能拖到步骤 4。
- **先优化再正确**：不要在步骤 3 就上 lookup table 评估器。性能放步骤 6。先用最朴素实现拿到正确性。
- **fixed 场景一次写完**：200 个场景一次写完会让步骤 2 拖延数周。先 10 个驱动 API，再批量补。
- **隐式全局 rng**：任何 `rand::random()` 调用都是后续不确定性的源头。从步骤 1 起就强制显式 rng 传递。
- **浮点参与规则引擎**：筹码、pot、odd chip 全走整数。一旦有 float 进入，跨平台哈希一致性就破了。
- **过早分 crate**：crate 边界一旦定下来，重构成本高。先一个 crate 多 module，等接口稳定（约步骤 4 完成时）再分。

## 阶段 1 出口检查清单

进入阶段 2 前必须满足以下全部条件：

- [ ] 验收文档 `pluribus_stage1_validation.md` 通过标准全部满足。
- [ ] 阶段 1 验收报告 `pluribus_stage1_report.md` commit。
- [ ] CI 在 main 分支 100% 绿，含：单元测试、fuzz 短跑（100k）、交叉验证、benchmark SLO 断言。
- [ ] 24 小时 fuzz 夜间任务连续 7 天无 panic / 无 invariant violation。
- [ ] 与至少 1 个开源 NLHE 参考实现的 100k 手交叉验证 0 分歧（或分歧已显式记录）。
- [ ] git tag `stage1-v1.0`，对应 commit 与 checkpoint 哈希写入报告。

## 时间预算汇总

按本流程，阶段 1 总工作量约 `8-11` 人周。与 `pluribus_path.md` 中 "阶段 1：1-2 人月" 的估算吻合。

| 步骤 | 工作量 |
|---|---|
| 0. 技术栈选定 | 0.5 人周 |
| 1. 核心类型骨架 | 1 人周 |
| 2. 核心场景测试 + 交叉验证 harness | 1.5 人周 |
| 3. 实现 pass 1 让 10 个场景全绿 | 2-3 人周 |
| 4. 扩展到完整验收覆盖 | 2-3 人周 |
| 5. fuzz 上规模 + 多线程确定性 | 1 人周 |
| 6. 性能优化到 SLO | 1.5-2 人周 |
| 7. 最终硬化与验收报告 | 0.5 人周 |

## 参考资料

- 阶段 1 验收门槛：`pluribus_stage1_validation.md`
- 整体路径与各阶段总览：`pluribus_path.md`
- PokerKit（推荐 cross-validation 参考实现）：https://github.com/uoftcprg/pokerkit
- OpenSpiel poker：https://github.com/google-deepmind/open_spiel
- Cactus Kev 5-card 评估器：http://suffe.cool/poker/evaluator.html
- Two-Plus-Two 7-card 评估器：https://github.com/chenosaurus/poker-evaluator

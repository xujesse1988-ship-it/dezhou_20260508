# 阶段 1 决策记录

## 文档地位

本文档记录阶段 1 的全部技术与规则决策。一旦 commit，后续步骤（A1 / B1 / B2 / ... / F3）的所有 agent 必须严格按此 spec 执行。

任何决策修改必须：
1. 在本文档以 `D-NNN-revM` 形式追加新条目（不删除原条目）
2. 必要时 bump `HandHistory.schema_version`
3. 通知所有正在工作的 agent（在工作流 issue / PR 中显式标注）

未在本文档列出的细节，agent 应在 PR 中显式标注"超出 A0 决策范围"，由决策者补充决策后再实施。

---

## 1. 技术栈

| 编号 | 决策项 | 选定值 | 理由 |
|---|---|---|---|
| D-001 | 实现语言 | **Rust** | cargo-fuzz / proptest 生态最完整；pyo3 让阶段 7 Python 评测脚本免费跨语言；整数筹码 + 无浮点容易做 |
| D-002 | 测试框架 | 内置 `#[test]` + `proptest` + `cargo-fuzz` | proptest 做 property test、cargo-fuzz 做覆盖率引导 fuzz |
| D-003 | benchmark 框架 | `criterion` | 统计严谨、CI 友好 |
| D-004 | hand history 序列化 | protobuf via `prost` | schema 版本号字段天然支持，跨语言反序列化几乎零成本 |
| D-005 | 跨语言桥 | `pyo3`（主） + Python subprocess（备） | pyo3 优先；某些第三方 Python 库不便嵌入时退到 subprocess |
| D-006 | CI 平台 | GitHub Actions | 与公开仓库标配；如需自托管再调整 |
| D-007 | Rust toolchain | stable channel（pin 在 `Cargo.toml` `rust-version`） | 避免 nightly 引入不确定性 |
| D-008 | 操作系统主目标 | Linux x86_64 | 与训练集群一致；macOS / ARM 作为期望目标 |

---

## 2. Crate 布局

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-010 | 起步布局 | 单 crate 多 module，crate 名 `poker` |
| D-011 | Module 划分 | `core` / `rules` / `eval` / `history` / `error` / `fuzz` / `bench` / `xvalidate`（其中 `error` 仅含公开错误类型 `RuleError` / `HistoryError`，详见 `pluribus_stage1_api.md` §8；`fuzz` / `bench` / `xvalidate` 为测试 / 性能 / 交叉验证模块，不属公开 API）|
| D-012 | 拆 crate 时机 | C2 完成、API 稳定后再拆为 workspace |
| D-013 | feature gate | `xvalidate` 模块的 PokerKit 依赖通过 feature 隔离，默认关闭 |

---

## 3. 数值与单位

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-020 | 整数筹码单位 | 1 chip = 1/100 BB |
| D-021 | 默认起始筹码 | 100 BB = 10,000 chips |
| D-022 | 默认大盲 | BB = 100 chips |
| D-022b | `default_6max_100bb` 默认按钮位 | `button_seat = SeatId(0)`；`SeatId(1)` 为 SB、`SeatId(2)` 为 BB（按 D-032 推导） |
| D-023 | 默认小盲 | SB = 50 chips |
| D-024 | 默认 ante | 0 chips（接口预留扩展） |
| D-025 | `ChipAmount` 后备类型 | `u64`（远超 stage 1 上限） |
| D-025b | 筹码 vs 盈亏的有符号区分 | 绝对筹码量（stack / pot / committed / blind / ante / bet `to`）一律用 `ChipAmount(u64)`；盈亏 / payout（可正可负）一律用 `i64`。`ChipAmount → i64` 转换：值必须 ≤ `i64::MAX`，否则 panic（阶段 1 起始 stack ≤ 100 BB · 100 chips/BB · 9 seats = 90,000，远在 `i64::MAX` 内）。`i64 → ChipAmount` 仅在调用方已证明非负时允许，并显式断言。 |
| D-026 | 浮点禁用范围 | 规则引擎 / 评估器 / hand history / 抽象映射全程整数；任何 PR 引入 `f32` / `f64` 必须 reject |
| D-026b | `ChipAmount` Sub 下溢策略 | `ChipAmount` 的 `Sub` / `SubAssign` 在下溢时 **debug 与 release 都 panic**（不使用 saturating，不使用 wrapping）；下溢即视为规则引擎 bug，必须立即终止以便定位。需要 saturating 语义的调用方必须显式用 `checked_sub` 或先比较再相减 |
| D-027 | RNG 显式注入 | 禁止全局 rng；所有随机调用显式接受 `&mut dyn RngSource` |

**核心数值速查表**：

| 名称 | 值 | 单位 |
|---|---|---|
| 1 chip | 1/100 BB | — |
| BB | 100 | chips |
| SB | 50 | chips |
| 默认起始筹码 | 10,000 | chips（= 100 BB） |
| 默认桌大小 | 6 | seats |
| 桌大小可配置范围 | 2..=9 | seats |
| HandHistory 初始 schema_version | 1 | — |

---

## 4. NLHE 规则

| 编号 | 决策项 | 选定值 / 描述 |
|---|---|---|
| D-030 | 桌大小 | 默认 6-max；`TableConfig` 接受 2..=9 用于测试 / fuzz |
| D-031 | ante | 默认 0；接口字段保留以备未来扩展 |
| D-032 | 按钮轮转 | **简化方案**：阶段 1 假定**全程**无玩家坐入坐出 —— 既不允许 mid-hand sit-in/sit-out，也不允许 hand-boundary sit-in/sit-out。所有 `n_seats` 座位从模拟开始到结束全程在场。按钮每手向左移动一格（即下一手按钮 = 当前按钮左侧第一个座位，模 `n_seats`）。盲注位由按钮位机械推导：SB = 按钮左 1，BB = 按钮左 2。dead button / dead blind corner case（短暂缺席导致的悬空盲）不在阶段 1 scope，留待支持坐入坐出时再补 D-032-revM。**该简化对 cross-validation harness 的额外约束见 D-086**。 |
| D-033 | 短码 all-in 重开规则 | incomplete raise（短码 all-in 加注差额 < 本轮最大有效加注差额）**不重开** raise option |
| D-034 | min raise（首次） | 首次开局 raise 的最小金额 = BB |
| D-035 | min raise（链式） | 后续每次 raise 的加注差额 ≥ 本轮已发生的最大有效加注差额 |
| D-036 | 全员 all-in 跳轮 | 除一名玩家外全员 all-in 后，跳过后续下注轮，直接发完剩余公共牌进入摊牌 |
| D-037 | showdown 顺序 | `last_aggressor` = 本手内最后一次 **voluntary** bet 或 raise 的玩家。SB / BB / ante 等强制盲注 / 强制下注 **不算** voluntary aggression；preflop limp（call BB）也不算 aggression。若 `last_aggressor` 存在，由其先亮，其余未弃牌玩家从 `last_aggressor` 起向左依次轮流（每次取下一个未弃牌座位，跳过弃牌者）；若 `last_aggressor` 不存在（典型场景：preflop 多人 limp 进入 flop 后各街全员 check 至 river 摊牌），则从 SB 起向左依次轮流（SB 已弃则取下一个未弃牌座位）。注：**BB walk**（所有非 BB 玩家 preflop 弃牌）不进入 showdown，无 D-037 适用问题；该术语在 D-040 / D-037 内不应被举为 showdown 例子。 |
| D-038 | side pot 排序 | 按 all-in 金额升序，最低 all-in 形成 main pot，依次形成 side pot |
| D-039 | odd chip rule | 每个 pot（main pot 与每个 side pot）独立计算零头：先在该 pot 的获胜者集合内均分，余数（< 获胜人数）按"按钮左侧最近"的顺序在该 pot 获胜者集合中依次分配最小单位（1 chip）。不同 pot 之间互不影响；同时存在多个 side pot 时各自独立执行。例：3-way 摊牌，main pot = 301 chips，3 人皆赢 → 各得 100，余 1 chip 给按钮左侧最近的获胜者；同手 side pot = 7 chips，2 人赢 → 各得 3，余 1 chip 给该 side pot 获胜者中按钮左侧最近者（与 main pot 的零头去向独立判断） |
| D-040 | uncalled bet returned | 最后一个 raise/bet 没有 caller 时，超出"最高被 call 金额"的部分返还 raiser，不进入 pot |
| D-041 | dead money | 弃牌玩家已投入的筹码留在对应级别 pot/side pot，不退还 |
| D-042 | string bet | 不允许（统一接受单一 Action 调用，不分多步） |
| D-043 | 时间限制 | 阶段 1 不实现；接口字段保留 |

---

## 5. 确定性与跨平台

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-050 | RNG 注入 | 显式 `RngSource` trait，禁用全局 rng |
| D-051 | 跨平台必过门槛 | 同架构 + 同 toolchain 下，相同 seed 产生的 HandHistory hash 一致 |
| D-052 | 跨架构期望目标 | x86_64 vs ARM64 hash 一致；阶段 1 验收报告需显式标注当前是否达成 |
| D-053 | 哈希算法 | BLAKE3（用于 HandHistory 内容指纹与版本哈希） |
| D-054 | 多线程一致性 | 每个 seed 独立产出的 HandHistory 必须与单线程下完全一致；只允许 seed 顺序变化，不允许内容变化 |

---

## 6. Hand history schema

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-060 | 序列化 | protobuf via `prost` |
| D-061 | schema 起始版本 | `schema_version = 1` |
| D-062 | schema 升级策略 | 必须保持向后兼容 OR 显式提供升级器；旧版本禁止被新代码静默错读 |
| D-063 | 必含字段 | schema_version / config / seed / actions（含序号） / board / hole_cards（按 seat） / final_payouts / showdown_order |
| D-064 | 可恢复性 | 支持从任意 action index 回放到中间状态 |
| D-065 | proto 文件位置 | `proto/hand_history.proto`，由 `prost-build` 生成 Rust 代码 |
| D-066 | Python 端读取 | `proto/hand_history.proto` 由 Python `protoc` 生成绑定，与 Rust 端共用同一份 proto 文件。Python 绑定输出位置：`python/poker_proto/`，由 `make python-proto` target 生成 |

---

## 7. 评估器

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-070 | 接口 | 5-card / 6-card / 7-card 三个独立函数 |
| D-071 | 返回类型 | 不透明 `HandRank`（`u32`，数值越大越好；同值表示同强度） |
| D-072 | 公开 category | `HandCategory` 枚举可从 `HandRank` 派生 |
| D-073 | B2/C2 实现 | 朴素实现（5-card 直接枚举 + 7-choose-5 组合）。**B2/C2 临时门槛：10k eval/s**，仅用于功能正确性验证，**不阻塞 B2 出口**；最终性能 SLO 由 D-090（≥ 10M eval/s，E2 后达成）规定 |
| D-074 | E2 实现 | 高性能 lookup table（2+2 风格或 Cactus Kev 衍生） |
| D-075 | 参考评估器 | C1 阶段从 `treys` / `OMP` / `SKPokerEval` / `ACE` 任选 1 个做 1M 手交叉验证 |

---

## 8. 交叉验证

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-080 | 主参考实现 | **PokerKit** (https://github.com/uoftcprg/pokerkit) |
| D-081 | 集成方式 | pyo3 优先；若 PokerKit 与 pyo3 集成有阻碍则退回 subprocess + JSON |
| D-082 | 比对粒度 | 终局筹码 / main pot / side pot 划分 / winner / showdown 顺序 |
| D-083 | 分歧处理 | 默认假设我方 bug；review 后才能记为参考实现差异 |
| D-084 | 第二参考（可选） | OpenSpiel poker 作为补充 — 阶段 1 不强制接入，C2 完成后视情况补 |
| D-085 | 交叉验证规模 | B2: 100 手（仅功能正确性）；C2: 100k 手（**最终通过门槛**，零分歧）；E2 后回归: 1M 手（性能 + 稳定性巩固） |
| D-086 | cross-validation harness 配置约束 | 因 D-032 假定全程无玩家坐入坐出，所有调用 PokerKit（或任何参考实现）的 cross-validation 用例必须把参考实现配置为"全程 `n_seats` 座全部在场、无 sit-in/sit-out、按钮机械每手左移、SB/BB 由按钮位推导"模式。若参考实现默认行为引入空座、dead button 或 dead blind，必须在 harness 中显式禁用，并把使用的参考实现版本号与配置参数记入测试报告。任何因参考实现配置差异导致的分歧不算 D-083 意义上的"我方 bug"，但必须在测试报告中显式列出。 |

---

## 9. 性能 SLO（最终目标，E2 后达到）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-090 | 评估器单线程吞吐 | ≥ 10,000,000 eval/s |
| D-091 | 评估器多线程扩展 | 接近线性扩展（至少到 8 核） |
| D-092 | 全流程模拟单线程吞吐 | ≥ 100,000 hand/s |
| D-093 | hand history 序列化 | ≥ 1,000,000 action/s 写入 + 读取 |
| D-094 | benchmark 工具 | criterion，CI 短跑 30 秒内、夜间全量 |

---

## 10. 决策修改流程

- D-100 任何决策修改必须在本文档以追加 `D-NNN-revM` 条目的形式记录，**不删除原条目**
- D-101 修改若影响 HandHistory 兼容性，必须 bump `schema_version` 并提供升级器
- D-102 修改若影响 API 签名，必须同步修改 `pluribus_stage1_api.md` 并通知正在工作的 agent
- D-103 决策修改 PR 必须经过决策者 review 后合入

---

## 11. 已知未决项（不阻塞 A1）

以下事项目前未做最终决策，留待后续步骤再确认。在敲定前 agent 不应基于这些做强假设：

- 高性能评估器具体算法选型（2+2 vs Cactus Kev vs 自研）— 由 E2 决定
- 跨架构（ARM64）哈希一致性是否作为阶段 1 通过门槛 — 由 F3 验收时确认
- OpenSpiel 是否作为第二交叉验证参考 — 由 C2 完成后视情况确认

---

## 参考资料

- 阶段 1 验收门槛：`pluribus_stage1_validation.md`
- 阶段 1 实施流程：`pluribus_stage1_workflow.md`
- 整体路径：`pluribus_path.md`
- PokerKit：https://github.com/uoftcprg/pokerkit
- prost：https://github.com/tokio-rs/prost
- pyo3：https://github.com/PyO3/pyo3

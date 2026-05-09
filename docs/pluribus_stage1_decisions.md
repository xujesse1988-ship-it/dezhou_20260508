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
| D-024 | 默认 ante | 0 chips（接口预留扩展）。语义约定：`TableConfig.starting_stacks` 是发盲注 / ante **之前** 的座位栈；引擎在开手时把盲注 / ante 从对应座位的 `stack` 转入 pot，因此 `sum(stacks) + pot = sum(starting_stacks)` 在牌局任意时刻、任意 apply 前后都必须成立（与 API I-001 一致）。 |
| D-025 | `ChipAmount` 后备类型 | `u64`（远超 stage 1 上限） |
| D-025b | 筹码 vs 盈亏的有符号区分 | 绝对筹码量（stack / pot / committed / blind / ante / bet `to`）一律用 `ChipAmount(u64)`；盈亏 / payout（可正可负）一律用 `i64`。`ChipAmount → i64` 转换：值必须 ≤ `i64::MAX`，否则 panic（阶段 1 起始 stack ≤ 100 BB · 100 chips/BB · 9 seats = 90,000，远在 `i64::MAX` 内）。`i64 → ChipAmount` 仅在调用方已证明非负时允许，并显式断言。 |
| D-026 | 浮点禁用范围 | 规则引擎 / 评估器 / hand history / 抽象映射全程整数；任何 PR 引入 `f32` / `f64` 必须 reject |
| D-026b | `ChipAmount` Sub 下溢策略 | `ChipAmount` 的 `Sub` / `SubAssign` 在下溢时 **debug 与 release 都 panic**（不使用 saturating，不使用 wrapping）；下溢即视为规则引擎 bug，必须立即终止以便定位。需要 saturating 语义的调用方必须显式用 `checked_sub` 或先比较再相减 |
| D-027 | RNG 显式注入 | 禁止全局 rng；所有随机调用显式接受 `&mut dyn RngSource` |
| D-028 | RngSource → deck 发牌协议 | `GameState::new` / `GameState::with_rng` 必须按以下确定性协议发牌，**任何实现偏离视为违反 API 契约**：① 初始化 `deck = [Card::from_u8(0), Card::from_u8(1), ..., Card::from_u8(51)]`（共 52 张，按 `Card::to_u8` 升序）；② Fisher-Yates 洗牌：`for i in 0..51 { let j = i + (rng.next_u64() % ((52 - i) as u64)) as usize; deck.swap(i, j); }`，恰消费 51 次 `next_u64`；③ 发牌索引（`n = config.n_seats`，发牌起点 = SB 即按钮左 1，按 D-029 升 SeatId 方向环绕）：`deck[k]` 为发牌顺序中第 k 个座位的第 1 张底牌（k = 0..n），`deck[n + k]` 为同一座位的第 2 张底牌（k = 0..n），`deck[2n .. 2n+3]` 为 flop（按出现顺序），`deck[2n+3]` 为 turn，`deck[2n+4]` 为 river；④ **不发烧牌**（burn cards），`deck[2n+5..]` 在阶段 1 不被引用。该协议作为公开契约，testers 可基于此构造 stacked `RngSource` 实现来产生指定牌序（B1 fixed scenario 主要使用该路径）。任何修改必须走 D-100 / API-NNN-revM 流程，并 bump `HandHistory.schema_version`（因为相同 seed 将产生不同的 `board` / `hole_cards`，破坏跨版本回放）。 |
| D-029 | 座位方向约定 | `SeatId(k+1 mod n_seats)` 是 `SeatId(k)` 的左邻。按钮轮转（D-032）、盲注推导（D-022b / D-032）、odd chip 分配（D-039）、showdown 顺序（D-037）、D-028 发牌起点环绕方向中"向左" / "按钮左侧" 均按此理解。该约定与 API §1 `SeatId` 的注释保持一致。 |

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
| D-032 | 按钮轮转 | **简化方案**：阶段 1 假定**全程**无玩家坐入坐出 —— 既不允许 mid-hand sit-in/sit-out，也不允许 hand-boundary sit-in/sit-out。所有 `n_seats` 座位从模拟开始到结束全程在场。按钮每手向左移动一格（即下一手按钮 = 当前按钮左侧第一个座位，模 `n_seats`，方向定义见 D-029）。盲注位由按钮位机械推导：SB = 按钮左 1，BB = 按钮左 2。dead button / dead blind corner case（短暂缺席导致的悬空盲）不在阶段 1 scope，留待支持坐入坐出时再补 D-032-revM。**该简化对 cross-validation harness 的额外约束见 D-086**。**未来扩展占位**：当未来 D-032-revM 引入 sit-in/sit-out 支持时，**默认采用 dead button 规则**（按钮在原座位停留一手，盲注按规则推导），不采用 dead blind。该占位决定先行写入以满足 `pluribus_stage1_validation.md` §1 "dead button / dead blind 规则二选一并显式写出" 的硬性条款；阶段 1 因 sit-in/sit-out 不在 scope，该规则当前无任何代码路径触发，仅在 F3 验收报告中作为"已选定但未启用"列出。`validation.md` §1 末段对应一行注释指向本占位。 |
| D-033 | 短码 all-in 重开规则 | incomplete raise（短码 all-in 加注差额 < 本轮最大有效加注差额）**不重开** raise option |
| D-034 | min raise（首次） | 首次开局 raise 的最小金额 = BB |
| D-035 | min raise（链式） | 后续每次 raise 的加注差额 ≥ 本轮已发生的最大有效加注差额 |
| D-036 | 全员 all-in 跳轮 | 除一名玩家外全员 all-in 后，跳过后续下注轮，直接发完剩余公共牌进入摊牌 |
| D-037 | showdown 顺序 | `last_aggressor` = 本手内最后一次 **voluntary** bet 或 raise 的玩家。SB / BB / ante 等强制盲注 / 强制下注 **不算** voluntary aggression；preflop limp（call BB）也不算 aggression。若 `last_aggressor` 存在，由其先亮，其余未弃牌玩家从 `last_aggressor` 起向左依次轮流（每次取下一个未弃牌座位，跳过弃牌者）；若 `last_aggressor` 不存在（典型场景：preflop 多人 limp 进入 flop 后各街全员 check 至 river 摊牌），则从 SB 起向左依次轮流（SB 已弃则取下一个未弃牌座位）。注：**BB walk**（所有非 BB 玩家 preflop 弃牌）不进入 showdown，无 D-037 适用问题；该术语在 D-040 / D-037 内不应被举为 showdown 例子。 |
| D-038 | side pot 排序 | 按 all-in 金额升序，最低 all-in 形成 main pot，依次形成 side pot |
| D-039 | odd chip rule | 每个 pot（main pot 与每个 side pot）独立计算零头：先在该 pot 的获胜者集合内均分，余数（< 获胜人数）按"按钮左侧最近"的顺序在该 pot 获胜者集合中依次分配最小单位（1 chip）。不同 pot 之间互不影响；同时存在多个 side pot 时各自独立执行。例：3-way 摊牌，main pot = 301 chips，3 人皆赢 → 各得 100，余 1 chip 给按钮左侧最近的获胜者；同手 side pot = 7 chips，2 人赢 → 各得 3，余 1 chip 给该 side pot 获胜者中按钮左侧最近者（与 main pot 的零头去向独立判断）。**Corner case：BTN 自身为获胜者**：环绕计数从 `BTN+1`（按 D-029 即按钮左 1）起、不从 BTN 起；即 BTN **不优先**获得余 chip，仅当所有非 BTN 的获胜座位都已分到、最终环绕回到 BTN 时才落到 BTN。该约定与 PokerKit 等开源参考实现一致。等价表述：余 chip 分配的迭代起点 = 按钮位的下一个座位，逐一遍历 `(BTN+1) mod n`、`(BTN+2) mod n`、…、`(BTN+n-1) mod n`、`BTN`，跳过非该 pot 获胜者，直到余 chip 为 0。 |
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

### 修订历史

- **D-033-rev1** (2026-05-08)：把 D-033 + `pluribus_stage1_validation.md` §1 第 22 行
  "incomplete raise 不重开 raise option" 的精确语义钉为 **TDA Rule 41 / PokerKit
  一致** 解读，澄清 "哪类玩家在 incomplete 之后仍可加注"。
  - **背景**：原 D-033 文字仅说 "incomplete 不重开"；validation §1 第 22 行说
    "后续未行动玩家只能 call/fold"。两句对 **已行动玩家** 无显式约束、对
    "未行动" 一词不区分两种状态：(a) "本轮 betting 内尚未做过任何动作"；
    (b) "已做过动作但尚未对当前最高 full raise 作出回应"。B1 评审第二轮
    发现 `tests/scenarios.rs` #3 与 #4 在不同解读下分别为真，互相矛盾。
  - **新规则（TDA-41 等价表述）**：
    1. **raise option 状态** 为 per-player 一比特：`true` = 仍可加注；
       `false` = 已关闭。
       - 初值：postflop 街起手所有 `Active` 玩家 = `true`；preflop BB 在
         `max_committed_this_round` 仍 == BB（即面对 limp / 无人加注）时
         = `true`，否则 = `false`；其余玩家 preflop 起手 = `true`。
    2. **full raise**（差额 ≥ `last_full_raise_size`）发生时：raiser 自身
       置 `false`；所有 `Active` 且 **尚未对该 full raise 行动**
       （`committed_this_round` < 新 `max_committed_this_round`）的玩家置
       `true`。同时更新 `last_full_raise_size = 新差额`。
    3. **call / fold**：不影响他人；自身置 `false`（call 后已回应；fold
       后退出）。
    4. **incomplete raise / short all-in**（差额 < `last_full_raise_size`）：
       a) **不更新** `last_full_raise_size`（D-035 链条上限不动）。
       b) **不修改** 任何玩家的 raise option 标志。
       c) 仅推升 `max_committed_this_round` 并把推升者标为 `AllIn`。
    5. `legal_actions().raise_range` 仅当当前玩家的 raise option 标志
       = `true` 且 `stack > 0` 时为 `Some`，`min_to = max_committed_this_round
       + last_full_raise_size`（D-035）。
  - **等价口语化表述**："incomplete 不重开 raise option" 仅意味着 incomplete
    本身 **不充当 reopen 事件**——它 **不** 意味着所有人此后都不能加注；
    在 incomplete 之前 raise option 仍是开启状态的玩家不受影响。
  - **影响**：
    - `tests/scenarios.rs` #3 / #4 的 setup 完全保留，但因 actor 不同断言
      互换：
      - **#3 `short_allin_does_not_reopen_raise`**：执行到 BTN Call 450 之后
        测 SB 的视角——SB 已-acted 且其后无 full raise，`raise_range = None`，
        显式 `Action::Raise` 返回 `RuleError::RaiseOptionNotReopened`。
      - **#4 `min_raise_chain_after_short_allin`**：执行到 BB AllIn 之后即
        停，测 BTN 的视角——BTN 仍持有 SB full raise 开启的 raise option，
        `raise_range = Some`，`min_to = 650 = max_committed(450) +
        last_full_raise(200)`。
      详见同 PR `tests/scenarios.rs` patch。
    - `pluribus_stage1_validation.md` §1 第 22 行措辞同步收紧（同 PR 改），
      新条文显式指向 D-033-rev1。
    - **不** 影响 `Action` / `LegalActionSet` / `RuleError` 公开签名，无需
      `API-NNN-revM`。
    - **不** 影响 `HandHistory` protobuf schema，不 bump `schema_version`。
    - 实现侧（B2 起）：`GameState` 内部需维护 per-player
      `raise_option_open: bool` 标志，按上述规则 1–4 维护；`legal_actions`
      按规则 5 暴露。
  - **与 PokerKit 的对齐**：本解读与 PokerKit 默认行为一致，期望 C2 阶段
    100k 手 cross-validation 在 short all-in 路径上 0 分歧；若实测发现
    PokerKit 偏离 TDA-41，追加 D-033-rev2 重新对齐并在 D-086 显式标注配置
    差异。
  - **撤销条件**：若决策者后续选择 "严格 / 简化" 语义（任何人在 incomplete
    之后都不能 raise），需追加 D-033-rev2、翻转 `tests/scenarios.rs` #4
    的 `raise_range` 断言、并在 D-086 给 PokerKit 引入显式配置差异碳证
    （D-083 之外的合法分歧来源）。

- **D-039-rev1** (2026-05-08)：B2 PokerKit 0.4.14 cross-validation 将
  odd chip 余数分配钉为 **PokerKit 默认 chips-pushing divmod 语义**：每个
  pot 仍独立计算零头，先在该 pot 的获胜者集合内均分；若存在余数，则把该
  pot 的**全部余数**给按钮左侧顺序中最近的获胜者，而不是逐个分给多个赢家。
  - **背景**：seed 72 的 5-way preflop all-in 中，3 名玩家平分 50,000
    chips，余数为 2。PokerKit 0.4.14 将 2 chips 全部分给按钮左侧顺序中
    第一个赢家；原 D-039 的逐个分配解释会分给前两个赢家，导致
    `cross_validation_pokerkit_100_random_hands` 出现 1-chip 分歧。
  - **新规则**：余 chip 分配起点仍为 `(BTN+1) mod n`，按 D-029 向左查找
    该 pot 的获胜者集合；找到第一个获胜者后，把该 pot 的全部余数加给该
    座位。不同 pot 之间仍独立执行，因此同一手多个 pot 可以各自把余数给
    各自的第一个获胜者。
  - **影响**：`payouts()` 语义改变但公开签名不变；`HandHistory` schema
    不变，不 bump `schema_version`。`pluribus_stage1_validation.md` §3
    与实现注释同步改为该语义。
  - **澄清（D2 [实现] / D-039-rev1 配套补丁，2026-05-08）**：D-039 原文
    "main pot 与每个 side pot" 中的 "pot" 指 **按 contender 集合合并后的
    pot**（标准 main pot + 各 side pot），不是 "每个 contribution level 一个
    sub-pot"。即：建 pot 时若两条相邻 contribution level 的 contender 集合
    完全相同，须合并为单一 pot 再做 base/rem 划分；rem 在合并后的总额上
    算一次。该解读与 PokerKit `state.pots` 属性一致（`state.py` 2378-2380
    `while pots and pots[-1].player_indices == tuple(player_indices): amount
    += pots.pop().amount`）。该澄清不新增 D-NNN-revM 编号——D-039 文字本身
    无歧义，仅 B2 实现按 contribution level 切 sub-pot 是错的；100k
    cross-validation 桶 B-2way (28 seeds) / 桶 B-3way (67 seeds) 由该 bug
    产生，详见 `docs/xvalidate_100k_diverged_seeds.md`。

- **D-037-rev1** (2026-05-08)：D-037 原文 "last_aggressor = **本手内最后一次**
  voluntary bet 或 raise 的玩家" 中的 **作用域** 钉为 **本手最后一次 betting
  round（即 showdown 前的最后一条街）内的最后一次 voluntary bet/raise**，
  而不是 "整手内最后一次"。若该 betting round 内无 voluntary bet/raise，
  按 D-037 原 fallback 走（从 SB 起向左）。
  - **背景**：D-085 / `cross_validation_pokerkit_100k_random_hands` 第一次
    实跑（commit `2ea667b`，N=8 × 12,500 hand）暴露 10 条 showdown_order
    分歧（桶 A，最早 seed=1786），形态全为 2-人 swap：典型场景是 BTN
    preflop raise → 三街全 check 到 river → 摊牌时 BTN 应该不再 "先亮"。
    PokerKit 0.4.14 的 `opener_index` 在每条街起 `_begin_betting`
    (`state.py:3381`) 时被重置为 None，按位置 (`Opening.POSITION`) 算回
    SB；之后每次 `complete_bet_or_raise_to` (`state.py:4100`)
    `opener_index = player_index` 更新到本街最新的加注者。等到摊牌
    `_begin_showdown` (`state.py:4135-4145`) 用最终 `opener_index` 确定
    showdown 起点。换言之 PokerKit 的 "last aggressor" 天然就是 per-betting
    round 而非 per-hand。原 D-037 把作用域钉到 "本手内"，与 PokerKit 不
    一致；按 workflow §B2 风险条款 "默认假设我方理解错了规则" 应对齐
    PokerKit。
  - **新规则**：
    1. `last_aggressor` 状态在 `GameState` 中维护，但每条街起手 (preflop
       初始化、flop / turn / river 起手 `_begin_betting` 等价点)
       重置为 `None`。
    2. 每次 voluntary bet 或 raise（含 incomplete short all-in）将
       `last_aggressor` 置为 actor seat；与 D-033-rev1 #4(b) 不冲突——后者
       不动 `raise_option_open`，本条不动 `raise_option_open`，仅改
       `last_aggressor`。
    3. Showdown 起点：取摊牌时 `last_aggressor`（即 river / 最后一条
       betting round 的最后一次 voluntary bet/raise actor）；若为 `None`
       则 fallback 到 SB（D-037 原 fallback）。
    4. 强制盲注 / ante / preflop limp 仍不算 voluntary aggression（D-037
       原条款不变）。
  - **等价口语化表述**："谁先亮" 由 **最后一条街** 的最后一次主动加注者
    决定；如果最后一条街全部 check / call，那条街没人 "激进"，回退到
    SB 起摊。前几条街的加注不再 "粘连" 到摊牌起点。
  - **影响**：
    - `src/rules/state.rs::reset_round_for_next_street`（街间过渡）需新增
      `self.last_aggressor = None`；其它 last_aggressor 设值点
      （`apply_bet` / `apply_raise` 含 incomplete 路径）保持不变。
    - `tests/scenarios.rs::last_aggressor_shows_first`（B1 [测试]）与
      `tests/scenarios_extended.rs::showdown_order_table` case (a)
      `showdown_btn_preflop_only_aggressor`（C1 [测试]）原 expect 直接
      落在 D-037 旧语义上，需要更新到 D-037-rev1 语义；该改动作为本
      revM 的配套补丁随同提交，并在 `docs/pluribus_stage1_workflow.md`
      §修订历史 D-rev0 中显式追认 [实现] agent 触碰 tests 的角色边界
      carve-out。
    - `pluribus_stage1_validation.md` §1 第 25 行 "最后激进 (last
      aggressor) 玩家先亮牌" 表面说法兼容新解读（"最后激进" 在新规
      下作用域为 "最后一条街"），不强制改文字；如未来再有歧义可再
      追加澄清。
    - **不** 影响 `Action` / `LegalActionSet` / `RuleError` 公开签名，
      无需 `API-NNN-revM`。
    - **不** 影响 `HandHistory` protobuf schema（`showdown_order` 字段
      值改变，但字段本身未变），不 bump `schema_version`。
  - **撤销条件**：若决策者后续选择 "整手最后一次激进" 语义（与 PokerKit
    显式分歧），需追加 D-037-rev2、翻转两个 scenario 测试断言、并在
    D-086 给 PokerKit 引入显式配置差异碳证（D-083 之外的合法分歧来源）。
    届时 `cross_validation_pokerkit_100k_random_hands` 桶 A 会重新出现
    10 条分歧，需在 D-085 / validation §7 显式 carve-out。

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

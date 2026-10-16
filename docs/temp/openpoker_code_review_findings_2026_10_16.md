# OpenPoker 实盘策略链路代码审查（2026-10-16）

起因：想知道实盘打牌策略还有哪些可以改进的地方。过往牌局不一定是用当前程序策略打的，所以这次审查**只看当前代码**
（和描述代码的设计文档），不看任何打牌记录 / 实盘日志。由子 agent 只读完成，验证用的小程序没进仓库。

标注约定：**[核实]** = 读代码推演或跑小测试确认；**[推断]** = 推理，未直接验证。文中位置以函数名为准（行号会随改动漂移）。
审查对象是当前实盘配置实际走到的路径：

```
--seats 9 --reshape all-1pot --postflop-cap 4 --search --search-trigger all-postflop
--search-flop-prefer-blueprint on --search-deep-menu --search-prewarm --search-max-nodes 100000
--search-live-traversers on --search-solve-threads 4 --search-iterations 100000
--search-time-budget-ms 3000 --search-limp-as-fold on
```

## 状态一览

| # | 类型 | 问题 | 状态 |
|---|---|---|---|
| 1 | 正确性 | blueprint 路径按均匀分布出牌 | **已修**（`859ba8f`） |
| 2 | 正确性 | driver 漏算不带 amount 的全下投入 | **已修**（`859ba8f`） |
| 3 | 策略 | 翻牌 blueprint 只有 1pot 一档，任何尺寸都按满池注应对 | 未做 |
| 4 | 策略 + 工程 | 子树超节点上限直接放弃，不退窄菜单；5–6 人池建完树才拒绝 | 未做 |
| 5 | 策略 | 翻前面对短码全下按 100BB 全下应对 | 未做 |
| 6 | 策略 | 兜底太保守（HU 翻后不走 floor；免费时从不下注） | 未做 |
| 7 | 策略 | 跟进平跟后遇到加注几乎全弃 | 未做 |
| 8 | 参数 | driver 离场码深上限 180BB 偏宽 | 未做 |
| 9 | 参数 | 实盘没开 `--search-lcfr` | 未做 |

建议先做 6（改动最小）、8（改一个常量）、4。

## 1. blueprint 路径按均匀分布出牌 [核实]（已修）

- **场景**：开 `--search-flop-prefer-blueprint` 时翻牌锚定面、过码深闸 → 决策走 blueprint；翻前同理。
- **问题**：`tools/openpoker_advisor.rs` `blueprint_distribution` 拿到空 Vec 就按均匀出牌；crop 的
  `average_strategy`（`src/training/nlhe_dense_crop.rs`）对「碰过但 strategy_sum 为 0」的行直接返回 1/n。
  9624c5c 修的 `read_current_strategy` 只管搜索路径。
- **量级**（子 agent 流式扫 8B crop）：只看「hero 本街已行动、现在面对下注」的节点，按 hero 自身到达权重加权，真实底牌落在
  占位行的比例：2 人池 0.95%、3 人池 12.2%、4 人池 63.9%。按节点访问量加权：翻牌面对下注 2/3/4 人池 4.7% / 16% / 54%。
  动作集 {fold, call, 1pot, allin} → 命中即 25% 概率随机推全下。翻前加权约 1%，抽查多是自己不会走到的垃圾牌。
- **修法（859ba8f）**：`Blueprint::strategy` 对恰好均匀 / 未访问的行返回空；`blueprint_distribution` 返回 `Option`。
  占位时翻后（开搜索）改走锚定搜索（`source=search:bp_placeholder`，没有预热、现解），翻前 / 未开搜索走兜底
  （`fallback:blueprint_placeholder`）。range 估计对空 σ 本就按 1/n，不变。
- **实盘印证**：run3（00561b6）一手 JJ 在 7d8sJc 三人 3-bet 池拿到 check / bet1pot / allin 各 0.3333。

## 2. driver 漏算不带 amount 的全下 [核实]（已修）

- **问题**：`tools/openpoker_play.py` `_commit_step` 遇到没有 amount 的 all_in，投入增量记 0；之后 your_turn 把该座剩余栈
  更新成 0 → `hand_start_stacks` 回推的起始栈只剩已投入部分。离线 HH 解析（`src/training/openpoker_hh.rs`，用 `stack_before`
  覆盖）早修过，实盘请求路径没修。
- **服务端字段**（查 run1–3 全部 5 次 all_in）：不带 amount 时 `stack` / `stack_before` / `stack_after` 都是全下**前**的剩余栈，
  `contribution_delta=0`。
- **后果**：真栈重放到这个座位对不上（`auth_seat_mismatch`）或底池错 → 翻后搜索放弃，只能 check / fold。
  run3 一手 BB 翻牌全下后被算成 44。
- **修法（859ba8f）**：不带 amount 的 all_in 按 `stack_before` 补投入、该座剩余栈记 0；新增 driver selftest 场景 6b
  （同一消息序列，修前算出 SB 起始栈 10，修后 2000）。

## 3. 翻牌 blueprint 只有 1pot 一档 [核实]

- **现状**：all-1pot 翻牌只有 {1pot}；`src/abstraction/action.rs` 的 off-tree 映射里小于最小档的注直接取最小档。
  子 agent 小测试：在 {1pot} 菜单下 0.2–1.5 pot 的下注全部映射成 `Bet(1000)`；生产搜索宽菜单里 0.2–0.66 pot 映射成 0.5 pot。
- **问题**：prefer-blueprint 开着时，面对 1/3 池 c-bet 用的是「面对满池注」的弃牌频率，必然弃多；自己下注也只有满池或全下。
- **建议**：prefer-blueprint 只用在本街没人下注、或映射误差小（真实 / 映射尺寸在 0.75–1.33 之间）的节点，其余走搜索。
  判断条件放进 `flop_blueprint_stack_gate` 一类的闸里，预热的 skip 判断同源。
- **风险**：翻牌搜索是否优于训练充分的 blueprint 没有定论，需要离线对比 + A/B。

## 4. 子树超节点上限直接放弃 [核实]

- **现状**：`deep_menu_for`（`src/training/nlhe_betting_tree.rs`）的 SPR 阈值是按 4M 节点上限调的，实盘上限是 100k；
  超 cap 时 `solve_subgame`（`src/training/subgame.rs`）报错放弃 → check / fold。
- **子 agent 实测**：4 人单加注池 200BB 宽菜单 160,744 节点（超限放弃），{1pot} 只有 15,224；5 人平跟池 100BB {1pot} 也有
  140,030。反方向：单挑平跟池 SPR>40 被强制 {1pot}，整棵树才 460 节点，宽菜单完全负担得起。
- **工程浪费**：5–6 人池先完整建树、再在 `solve_subgame` 里拒绝；建树前计数只对 ≥7 人生效，且「建后超限」不进缓存 →
  预热和每次决策都重建一遍。设计文档里 6 人平跟池 557k 节点约 100MB、1.4 s 一次（来自文档，本次未核实）；机器没有 swap。
- **建议**：按节点预算选菜单（先数宽菜单，超了退 {1pot}，再超才放弃）；计数门槛降到 ≥4 人；缓存「建后超限」。选菜单的函数
  必须在 subgame 建树处和 advisor outgoing 处共用一份（否则出牌尺寸与子树菜单不一致）。

## 5. 翻前面对短码全下 [路径核实，影响推断]

- **现状**：lockstep 重放用对称 100BB，`all_in` 直接映射成 AllIn（`src/training/openpoker_hh.rs` `hist_to_concrete`）。
  30BB 的 3bet 全下被当成 100BB 全下 → 跟注范围严重偏紧。开池全下在树里没有节点（`drop_preflop_open_allin`），走兜底：
  只跟 JJ+ / AK / AQ，底池赔率 floor 在这种局面几乎不触发。
- **建议**：全下者真栈 < 约 60BB 时用规则：hero 对「按码深定宽的全下范围」的胜率 ≥ 所需底池赔率就跟。第 2 条修好后真栈可信，
  可以做了。全下范围宽度是猜的，需要标注并留参数。

## 6. 兜底太保守 [核实]

- HU 翻后直接 `safe_fallback`（`decide` 里 `smap.hu && !board.is_empty()` 分支），连坚果度 floor 和底池赔率 floor 都不走；
  改成 `fallback_with_floor` 一行（driver 遇两人桌打一手就离场，影响面小）。
- `fallback_with_floor` 能免费行动时从不下注：搜索放弃后拿着河牌坚果也只过牌。可加：坚果度 ≥ 0.97 时下注半池到 3/4 池。

## 7. 跟进平跟后遇到加注几乎全弃 [核实]

- `limp_heuristic` 有人加注的分支只有 P 档（JJ+ / AK / AQ）跟注，从不 3bet。BTN 45% 的跟进平跟范围被隔离加注时基本白送 1BB。
- **建议**：hero 已经跟进平跟、面对单次加注时，按位置和底池赔率放宽跟注档（例如跟进档里的对子 / 同花连张在赔率够时跟）。
  属于 2026-10-16 平跟方案 A 的遗留（`openpoker_limp_handling_research_2026_10_16.md` §6.2）。

## 8. 离场码深上限 180BB 偏宽 [推断]

- driver `STACK_LEAVE_HI = 180BB`（`tools/openpoker_play.py`），比 blueprint 的 100BB 假设和翻牌闸 [60, 140]BB 都宽。
  hero 有 180BB 时，翻前 blueprint 选 AllIn 会推整个筹码，用的却是 100BB 的全下范围。
- **建议**：上限收到约 130BB，改一个常量。

## 9. 实盘没开 `--search-lcfr` [推断]

- 设计文档把 LCFR 称为限时下的第一杠杆，实盘是 10 万次 vanilla ES-MCCFR。
- **建议**：在离线重放集上 A/B，比较不同 seed 之间策略的稳定性。

## 看过、认为没问题的关键路径

- **缓存 key**：覆盖 solve 全部输入；预热和决策共用同一个 lockstep、同一份前缀函数，`actor_override` 让 range 平滑口径一致；
  `hand_seed` 不含 actions / board，碰撞可忽略。
- **搜索读策略**：均匀占位检查已在（`read_current_strategy`）；真栈下加注档塌进全下时，策略投影保住总概率。
- **seat_map / 幻影座**：有逐引擎重放的测试；crop 守卫有洞时作废本次输出并清缓存；`raise_to_op` 对 min > max 有保护。
- **范围估计**：card removal、跨街后验的身份校验（同手 + board 前缀）没发现问题。小偏差：σ 缺失或占位时按 1/n 处理，影响不大。

## 需要实验数据才能判断

- 锚定 3 / 4 人池翻牌面对下注的实际频率（第 1 条的占位比例是「到达该节点的条件概率」，没算走到这条线的频率）。
- 翻牌搜索 vs blueprint 的质量（第 3 条）。
- LCFR、`range_uniform_mix=0.25`、平跟范围这些数值是否合适。

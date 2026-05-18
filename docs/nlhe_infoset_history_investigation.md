# 简化 NLHE InfoSet 是否需要动作历史 — 调查路径

本文档只记录"接下来要验证什么 / 怎么验证"，不记录结论。结论落地后改 `docs/status.md`。

## 出发点

`src/training/nlhe.rs::SimplifiedNlheGame::info_set` 把 `InfoSetId` 编码为：

- `bucket_id` — hole+board 的牌力桶（与下注线无关）
- `position_bucket` — 座位
- `stack_bucket` — **starting stack / BB**，整手不变（见 `src/abstraction/preflop.rs:106`）
- `betting_state` — 仅本街 raise 计数
- `street_tag` — 街
- `action_signature` — 本次决策的可选动作 role mask + bet sizing 金额桶

没有任何字段编码"前面街发生过什么"。同一 `street_tag + betting_state=Open` 下，
preflop 的 aggressor 身份、raise 深度、是否 limp 都被丢掉。

## 假设

存在两条不同的 preflop 线，到达 flop 同一 actor 决策点时，`info_set` 返回相同的
`InfoSetId`。但两条线下双方 range 分布显著不同，单一 average strategy 不可能同时
对两个 range 最优。

## 验证路径（按序执行）

### Step 1 — 写 collision 测试，证实 / 证伪假设

新增 `tests/nlhe_infoset_history_collision.rs`。构造两条 preflop 线：

- 线 A（SB 攻击）: SB raise → BB call → 进 flop。
- 线 B（BB 攻击）: SB call (limp) → BB raise → SB call → 进 flop。

使用同一 RNG seed，让发牌（hole + board）完全相同。

断言：

1. 两条线 flop 起手的 actor 相同（BB / player 1）。
2. 两条线在该决策点上 `info_set(state, actor)` 返回**相同**的 `InfoSetId`
   — 这是当前实现的行为，测试 `assert_eq` 是为了把 collision 钉成 regression。
3. 两条线 `state.action_history` 实际不同 — 证明 collision 不是因为状态本身
   退化，而是 InfoSet 编码丢信息。

通过 = collision 实锤；失败 = 假设不成立，要么 stack/pot 状态本身就把两条线
区分了（要看具体哪个字段在区分），要么测试构造错了。

### Step 2 — 决定要不要加 history

如果 Step 1 通过：

- collision 是 hard evidence，按 `CLAUDE.md` §1 这是正确性问题，必须修。
- 在动代码前先决定"加哪些信息、加在哪儿"。候选最小集：

  - 每条 previous street 一个 (aggressor: None/P0/P1, raise_depth_bucket: 0..n)
    的小字段，4 街总计 ~12 bit。可塞进现有 `action_signature` 的 26 bit 字段
    （需要重新切位）。
  - 或者直接把 `InfoSetId` 扩到 128 bit，避免位预算挤压。

  这一步需要外部对照证据支撑选择（OpenSpiel 简化 HU NLHE / Pluribus blueprint
  的 history bucketing 描述），不要按直觉拍板。

### Step 3 — 加完之后用什么证明加对了

不能只靠"测试现在 assert_ne 通过了"。需要：

- Leduc / Kuhn 类 ground truth 不适用（它们不分多街 aggressor）。
- 简化 NLHE 没有 closed-form Nash。
- 唯一可行的外部对照是：在加完 history 的版本上跑 LBR / best response
  exploitability 曲线，对比加之前。`tools/nlhe_h3_report.rs` 已经有 LBR proxy 入口。
- 量化门槛：完整 H3 gate（按 `docs/status.md` 是 100M update + 1M hands 评测）下
  exploitability 显著下降才能说"加 history 修了正确性问题"。否则可能是修了一个
  不影响收敛的死字段。

## 范围外

- 不在本文档讨论 6-max history。stage 3 简化 NLHE 是 HU 范围（D-313）。
- 不在本文档讨论 perf。按 `CLAUDE.md` 反模式 §1，正确性确认前不做 perf。
- 不在本文档讨论 checkpoint schema 兼容。如果改 `InfoSetId` 位 layout，旧
  checkpoint 全部作废，按 `CLAUDE.md` §2 "不写已废弃保留"直接弃。

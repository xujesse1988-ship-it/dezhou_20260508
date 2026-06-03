# OpenPoker 接入设计（2026-06-02 草案 → 2026-06-03 已实现 + live 校准）

> 目的：把 6-max blueprint 挂上 `openpoker.ai` 的真实对手场，做 S5「绝对强度」实测。
> **状态（2026-06-03）：已实现 + live 连通性 smoke 通过**——`tools/openpoker_advisor.rs`（Rust）+
> `tools/openpoker_play.py`（WS driver）落地，账号 jesse_xu 实跑 4 手全 blueprint 驱动、0 报错。
> **§1 协议表是 06-02 草案，部分字段经 06-03 实测修正——以末「§9 实现状态 + live 校准」为准。**
> 与 S5「相对强度」互评（`six_max_nlhe_target.md` S5①）**共用同一个 off-tree advisor 引擎**，见末「§6 共用引擎」。

## §0 为什么是它

- `six_max_nlhe_target.md` line 36 记的缺口：「没有强 6-max 公开参考 bot（不像 Slumbot 之于 HUNL）」。OpenPoker 正好补：
  6-max 真实 bot 场 + 排行榜 + WebSocket API + 免费 + 支持 Rust。
- **格式与 `default_6max_100bb()` 高度对齐**（这是能用的前提）：

  | 维度 | OpenPoker | 本 solver | 对齐? |
  |---|---|---|---|
  | 人数 | 2–6（主 6-max） | 6-max | ✓ |
  | 盲注 | 10 / 20 | SB/BB 比例同 | ✓ |
  | 起始码深 | 默认买入 2000 = **100BB** | 100BB | ✓（买入锁 2000） |
  | rake | **无**（chip-balance 计分） | 无（chip-EV） | ✓ |
  | 牌编码 | `Ah`/`Ts`/`2c` | 同（`slumbot_advisor::parse_card` 直接吃） | ✓ |

- 对手 = 其他开发者 bot 池 + 排行榜，**非固定强基准** → 给「活的竞争场排名」，不是 Slumbot 式可复现绝对基准。
  对 6-max 这反而更接近真实质量信号（D-6M-003：质量以实测对战为主）。

## §1 协议摘要（来源：docs.openpoker.ai/llms-full.txt，2026-06-02 抓取）

连接：`wss://openpoker.ai/ws`，HTTP header `Authorization: Bearer <api_key>`（**不支持** query 参数鉴权）。
注册：`POST https://api.openpoker.ai/api/register {"name","email","terms_accepted":true}` → 返回 api_key（只显示一次）。
成功后 server 发 `connected{agent_id,name}`；失败 `error{code:auth_failed}` 关连接 4001。

**server → bot**：

| type | 关键字段 |
|---|---|
| `hand_start` | `hand_id`, `seat`（我方座）, `dealer_seat`（button）, `blinds{small_blind,big_blind}` |
| `hole_cards` | `cards:["Ah","Kd"]` |
| `your_turn` | `hand_id`, `turn_token`, `valid_actions:[{action:"fold"},{action:"call",amount},{action:"raise",min,max}]`, `pot`, `community_cards`, `players:[{seat,name,stack}]`, `min_raise`, `max_raise` |
| `player_action` | `seat`, `action`, `amount`（check/fold 为 null）, `street`, `stack`, `pot` |
| `community_cards` | `cards`, `street`（flop=3/turn=1/river=1） |
| `hand_result` | `winners:[{seat,stack,amount,hand_description}]`, `pot`, `final_stacks{seat:stack}`, `shown_cards{seat:[..]}` |

**bot → server**：

| type | 字段 |
|---|---|
| `join_lobby` | `buy_in`（1000–5000，**锁 2000**） |
| `action` | `hand_id`, `action`, `amount`, `turn_token`, `client_action_id` |
| `rebuy` / `leave_table` / `set_auto_rebuy{enabled}` | — |

**action 语义**：`fold`/`check`/`call`/`all_in` 无 amount；`raise` 的 `amount` = **总 raise-to 额**（不是增量），须 ∈ [min,max]（取自 valid_actions）。
turn_token 一次性（重用 → `action_rejected`）；缺 `hand_id`/`turn_token`/`client_action_id` → `legacy_action_protocol`。
超时 120s（自动 fold，不能 fold 则 check）；限速 20 msg/s、10 conn/min/IP；断线 120s 内 `resync_request{table_id,last_table_seq}` 补；连续 miss 3 手移出桌。

## §2 架构：复用 Slumbot 的「driver + resident advisor」拆分

沿用 `docs/temp/slumbot_api_bridge_plan_2026_05_29.md` 的成功结构，**保持 crate 零网络依赖**（invariant）：

```
openpoker_play.py (driver)                     resident advisor (Rust, 6-max 版)
  - WS 连接 / Bearer 鉴权 / 重连 / resync    每决策一行 JSON in → 一行 JSON out（无状态）：
  - join_lobby{buy_in:2000}                    in : {hole, board, button_seat, my_seat,
  - 累计每手 betting 历史（见 §3）  ──────▶          blinds, stacks[6], action_history}
  - your_turn → 组请求 → 调 advisor  ◀──────  out: {action:"raise", amount:<to>} | {"call"} | ...
  - 回 action{turn_token, client_action_id}
```

- driver 用 Python（`websocket-client`），负责所有 IO/token/重连/限速；**无策略逻辑**。
- advisor = §6 的 6-max off-tree 引擎，**stateless per 决策**（请求自带完整手局 → 可重放、可单测，与 slumbot_advisor 同哲学）。
- 为什么不全 Rust：WS 全 Rust 要引 tokio-tungstenite，破「crate 零网络依赖」；Slumbot 已验 driver+advisor 拆分够用。

## §3 状态映射：OpenPoker 事件流 → solver

OpenPoker 是**有状态推送**（不像 Slumbot 每决策给完整 action 串）→ driver 必须自己**累计**：

- `hand_start` → 记 `button=dealer_seat`、`my_seat=seat`、blinds、清空本手历史。
- `hole_cards` → 记我方手牌。
- `player_action`（每条）→ 追加到本街历史。由 `action`+`amount`+各座 `committed_this_round`（driver 自己跟）还原成
  advisor 要的 token 序（`c`/`k`/`f`/`b<to>`，**to = 本街累计到额**，与 `slumbot_advisor` 的 `Token` 同语义）。
- `community_cards` → 进下一街、追加 board、各座 committed_this_round 归零。
- `your_turn` → 组装请求调 advisor；advisor 返回的抽象动作 → 用 `valid_actions` 的 min/max 夹 outgoing to 额（§5）。

**座位/位置**：OpenPoker 0-indexed、button=`dealer_seat`。solver 内部按「相对 button 的 offset」定位置
（offset 0=BTN…见 `nlhe_eval` per-position）。advisor 入参直接给 `button_seat`+`my_seat`，内部建 N 座 `GameState` 即对齐。

## §4 ⚠ 最大风险：码深漂移（blueprint-only 的固有限制）

- blueprint 在**全员 100BB 对称**码深下解出。OpenPoker 是 cash 桌、**筹码跨手累积** + rebuy（busto 补 75BB）+ 买入区间 50–250BB
  → 实战里**每座码深各异、且常 ≠ 100BB**。
- off-tree 映射只解决**下注尺寸**不在抽象里的问题，**解决不了码深 ≠ 100BB**（树深度/SPR 都变了）。这是 blueprint-only 无 re-solve 的已知短板。
- 缓解（都不根治）：
  1. **买入锁 2000=100BB**（开局对齐）；
  2. **每手后若我方栈漂出 [80,125]BB → `leave_table` 重 join 取 2000**（控我方栈；控不了对手栈）；
  3. 接受偏深/偏浅时精度下降，**评测报告标注码深分布**（按我方开局有效栈分桶出 mbb/100，别把深码污染算进 100BB 成绩）。
- 这条决定了 OpenPoker 成绩是「近 100BB 场的近似强度」，不是干净的 100BB GTO 实测。**必须在报告里诚实标注**。

## §5 off-tree 翻译（复用，不新写算法）

- **incoming**（对手下了抽象里没有的尺寸，如 2.5x cold-call、任意 raise）：`ActionAbstraction::map_off_tree(&abs.game_state, ChipAmount(to))`
  → `project_tag_onto(legal_abs, tag)`（塌 AllIn 兜底）→ 推进抽象影子。**逐字复用** `slumbot_advisor::resolve_actions` 的 `BetTo` 分支。
- **outgoing**（blueprint 选了抽象动作 → 真实 to 额）：`outgoing_*` 以真实 `GameState` pot 算同 ratio 档的 to → 夹进 `your_turn` 的
  `[min_raise, max_raise]`（OpenPoker 会校验）→ 填 action.amount。复用 `slumbot_advisor::outgoing_incr` 的 to-额逻辑，只是输出换成 OpenPoker `{action,amount}`。

## §6 共用引擎（①互评 + ②OpenPoker 的共同底座）

把 `slumbot_advisor.rs` 的 off-tree 核从 HU binary 抽进可复用 `src/` 模块（建议 `src/training/blueprint_advisor.rs`），并去 HU 硬编码：

- 抽出：`replay`/`resolve_actions`/`project_tag_onto`/`find_tag`/`outgoing_*`/`bet_or_call`/`parse_card`。
- 去硬编码：`TableConfig::default_hu_200bb()` → 传入（6-max 用 `default_6max_100bb()`）；座位映射 `1 - pos` → 通用 `button_seat`/`my_seat`；
  单一抽象影子 → 支持「一张权威 real `GameState` + K 个抽象影子（每个 distinct blueprint 一份，各自 tree）」。
- 之上两个薄壳：
  - **①互评工具** `tools/six_max_blueprint_h2h.rs`：N 座自对弈，每座 advisor 驱动，循环赛输出按位置 mbb/g + CI。
  - **②OpenPoker advisor** `tools/openpoker_advisor.rs`：stateless per 决策，driver 喂请求。
- **正确性门（必过才信结果）**：① vultr 跑 HU 回归——抽核后 `slumbot_advisor` 行为 byte-equal（现有 T2..T5 全绿）；
  ② 新 6-max 单测——已知手局逐步 replay 的 real/abs lockstep + off-tree 投影 + payoff 守恒。**本机仅 build/fmt/clippy，行为正确性一律 vultr**（`feedback_tests_on_vultr`）。

## §7 实现顺序（建议）

1. 抽核 → `src/training/blueprint_advisor.rs`（HU 回归守住），vultr 测。← 解锁①②
2. ①互评工具 + vultr 跑 {baseline,nolimp,preopen} 循环赛 → **回答「reshape 有没有用」**（S5 第一要务）。
3. 注册 OpenPoker 账号（**需用户**：邮箱 + api_key）。
4. ②driver + advisor 薄壳 → 先 1 手连通性 smoke，再挂场跑、读排行榜。
5. 多对手 AIVAT（S5③）后置，先裸 mbb/100。

## §8 未决 / 需用户

- ~~OpenPoker 账号注册（api_key）— 需用户邮箱。~~ **已注册**（账号 jesse_xu，2026-06-03）。
- 免费号 1 bot → 三变体上场只能分时段轮换（场会变、噪声大）；要并行比较得 Pro。→ **变体之争优先用①互评**（受控），OpenPoker 验最强那个。
- 码深漂移（§4）无根治 → 报告诚实标注，别当干净 100BB 成绩。

## §9 实现状态 + live 校准（2026-06-03）

**落地（commits `798b279`/`ebcab41`/`9d30d92`，branch 6max）**：
- `tools/openpoker_advisor.rs`：常驻 Rust advisor，复用 `blueprint_advisor`（`advance_shadow_by_applied` incoming /
  `outgoing_action` outgoing / `parse_card`），泛化 6 座（座位相对 button rotate 到 `default_6max_100bb` 座 0、
  筹码 ×scale=5）。**鲁棒兜底**：任何重放失败（码深漂移 / 结构性 gap / 非 6 人桌 / 非法历史）→ 安全合法动作
  （能 check 就 check、否则 fold）+ `source=fallback:<reason>`，live 不崩。
- `tools/openpoker_play.py`：WS driver（websocket-client），`--selftest` 离线验 IPC。
- **验证（vultr）**：advisor 3 单测 + driver `--selftest` 真 preopen 3 场景（开池 raise45=2.25BB / open-limp→
  fallback:structural_gap / flop bet0.5pot）全绿；**live smoke 4 手全 blueprint 驱动、0 兜底、0 报错、干净退**。

**§1 协议草案的实测修正（live 观测 + `docs.openpoker.ai/llms-full.txt`，2026-06-03）**：
- **action 报文**：`client_action_id` 必须是**字符串**（早先 int 被服务端拒 `invalid_message`）；`amount` 始终在场
  （raise 为 float 总 to 额、fold/check/call/all_in 为 null）。
- **player_action**：带 `amount_mode` 字段——raise 是 `"to_total"`（`amount` = 总 to 额，与草案一致）+
  `contribution_delta` / `to_call_before` / `stack_before/after`（可据此精确还原 committed，比草案的 driver 自跟更稳）。
- **消息分双流**：`stream:"event"`（离散事件 hand_start/hole_cards/player_action/community_cards/your_turn/
  hand_result，driver 用）+ `stream:"state"`（`table_state` 全量快照，含 `actor_seat`/`to_call`/`min_raise_to`/
  `max_raise_to`/`hero.valid_actions`/全座栈——driver 当前忽略，**未来可改用它取 valid_actions/还原更鲁棒**）。
- `hand_start{seat, dealer_seat, blinds{small_blind,big_blind}}`；`hole_cards{cards}`；`your_turn{valid_actions
  (含 raise 的 min/max), pot, community_cards, players, min_raise, max_raise, turn_token, seat}`。`hand_id` 是 UUID 串。
- `table_joined{seat, players[], ...}` + `lobby_joined{position, estimated_wait}`（草案未列，入座流程）。

**§4 码深漂移实测（坐实「严重」）**：实测同桌 6 座栈 **14BB–800BB**（远超草案设想的「买入 50–250BB」）——真实
arena 是深 / 极不均衡的 cash 桌。blueprint 假设 100BB → 短栈 all-in / 深栈大注会让重放 desync 走兜底；本 4 手恰
近 100BB 故 0 兜底，**长跑兜底率必升**。成绩只能是「近 100BB 场近似」，报告须按我方开局有效栈分桶 + 标注兜底率。

**剩绝对强度量化（非连通性）**：挂场跑数百+手累积 mbb/100 + 排行榜位次。须用户授权时长（账号公开在线）+ 码深漂移
分桶标注；CI 太宽再上多对手 AIVAT（S5③）。部署 blueprint 默认 `preopen`（范围更近 GTO，S4续⑥）。

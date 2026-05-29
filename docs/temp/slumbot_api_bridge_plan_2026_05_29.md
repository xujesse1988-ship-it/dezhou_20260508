# Slumbot API 对接实施计划（2026-05-29）

把 `train_cfr` 训练出的 blueprint 接上 Slumbot HUNL API 对战的具体实施计划。
本文是工作笔记（`docs/temp/`），实现落地后相关结论再回写 `docs/status_v2.md`。

## 0. 三项已定决策

1. **传输** = Python driver（跑 HTTP/TLS/token）+ 常驻 Rust advisor（stdio JSON-lines）。
   理由：正确性逻辑全留在 `cargo test`、crate 保持零网络依赖、白嫖 `sample_api.py`
   的网络/会话/解析代码。
2. **B5（抽象 vs 真实 pot）** = 抽象影子 + 真实 pot 旁路（两个 lockstep 状态）。
3. **off-tree 映射** = v1 用现有 `map_off_tree`（nearest ratio），不做 pseudo-harmonic；
   升级 PHM 的触发条件见 §9。

## 1. profile 对齐（无需缩放）

| 维度 | Slumbot | 求解器 `TableConfig::default_hu_200bb` |
|---|---|---|
| 玩家数 | 2 | 2 |
| 盲注 | 50 / 100 | 50 / 100 |
| 起手栈 | 20,000 (200BB) | `20_000` ×2 |
| 每手重置 | 是 | 是 |

筹码 "to" 语义一致：Slumbot `b<N>` 的 `N = street_last_bet_to`（本街累计下注到，
preflop 从 BB=100 起算）；求解器 `AbstractAction::Bet/Raise{to}` 的 `to` 也是
`max_committed_this_round + 增量`（同样本街累计、preflop 含盲注）。→ `to` 基本 1:1。

## 2. 进程拓扑

```
Python driver (fork sample_api.py)
  HTTP/TLS / token / 重连 / new_hand / act 循环
  每个我方决策点：{hole,board,client_pos,action} 一行 JSON → advisor stdin，读回 {incr}
        │ stdio JSON-lines
Rust advisor (tools/slumbot_advisor.rs，常驻)
  启动：加载 dense blueprint + v4 bucket 表一次
  每行：从 root 重放该手 → 查 blueprint → 出 incr
  每决策无状态（消息自带完整手局），可重放、可单测
```

## 3. stdio 协议

**启动 argv**：
`--checkpoint <dense.ckpt> --bucket-table <v4_cafebabe_500.bin> --dense
--fallback-policy hybrid --seed <u64>`
就绪后输出一行：`{"ready":true,"update_count":100000000,"strategy_blake3":"2fab8a…"}`

**请求**（Python→advisor，每决策一行）：
```json
{"hole_cards":["Ac","9d"],"board":["7h","2c","Ks"],"client_pos":1,"action":"b200c/kk/"}
```
**响应**（advisor→Python）：
```json
{"incr":"b450"}
```
错误：`{"error":"...."}`；Python 收到 error 即中止该手并打日志，不静默继续。

## 4. 唯一的 crate 改动（in-lib，最小面）

`src/training/nlhe.rs` 加一个公开方法，函数体复用现有 `info_set` 的 bucket lookup +
packing（要用 `pub(crate)` 的 `pack_info_set_v2`，必须在 lib 内）：

```rust
/// 用注入的真实 hole+board 为指定 node_id 构造 InfoSetId（绕过 Game::info_set
/// 对随机发牌 state 的依赖）。preflop 走 PreflopLossless169；postflop 走
/// canonical_observation_id + BucketTable::lookup —— 与 info_set 同一路径。
pub fn info_set_for_cards(&self, node_id: NodeId, hole: [Card; 2], board: &[Card]) -> InfoSetId
```

`street_tag` 由 `self.tree.node(node_id).street` 取；其余与 `info_set` 逐行对应。
**不碰发牌 / checkpoint schema / 训练路径。**

其余 advisor 所需 API 均已公开：`SimplifiedNlheGame::{root, next, legal_actions,
info_set}`、`SimplifiedNlheState.current_node_id`（pub 字段）、
`StreetActionAbstraction::{default_6_action, abstract_actions, map_off_tree}`、
`GameState::{new, apply, legal_actions, pot}`、`Card::new` + `Rank/Suit::from_u8`、
`DenseNlheEsMccfrTrainer::load_checkpoint`。Card 字符串解析（"Ac"→Card）放 advisor
bin（~20 行），无需进 lib。

## 5. 重放/决策算法（advisor 核心，B4+B5 落地）

每个请求：
1. 解析 `hole_cards`/`board` → `Card`；`action` 串 → token 序列（`k`/`c`/`f`/`b<N>`，
   `/` 跳过）。
2. 建**两个 lockstep 状态**，都从 root 起：
   - 抽象影子 `abs: SimplifiedNlheState = game.root(dummy_rng)`（随机牌，只用树位置）。
   - 真实态 `real: GameState = GameState::new(&cfg, seed)`，真实筹码驱动（只用来算真实
     下注尺寸 + 合法区间）。
3. 按 token 顺序，对当前 actor（由 `real.current_player()` 给出）同步推进两态：
   - `k`→Check / `c`→Call / `f`→Fold：两态 apply 对应动作（abs 从 `legal_actions(abs)`
     取带 `to` 的 Call）。
   - `b<N>`：
     - incoming 映射：`tag = map_off_tree(&abs.game_state, ChipAmount(N))` 选最近 ratio
       （**以抽象影子为参考系**，保证选出 tag 一定存在于 abs 当前节点 → 永不 off-tree、
       计数永远对齐；real-pot 漂移仅影响选哪个 bucket，可接受）。
     - abs 侧 apply 该 tag 对应抽象动作；real 侧 apply 真实 `Bet/Raise{to:N}`
       （N≥all-in 时 AllIn）。
     - 投影兜底：若选中 ratio 因抽象 pot 较小已塌进 AllIn，两态都走 AllIn
       （单独 `project_tag_onto(abs)` 函数 + 测试）。
4. 重放完，**断言** `abs.current_player() == real.current_player() == (1 - client_pos)`。
   **B2 座位映射：Slumbot pos 1 = SB/button = solver SeatId(0)；pos 0 = BB = SeatId(1)。**
   不符 → `{"error":"seat/parse desync"}`。
5. 查策略：
   - `node_id = abs.current_node_id`；`legal = legal_actions(abs)`（顺序=训练序）。
   - `info = game.info_set_for_cards(node_id, my_hole, board)`（**真实牌**）。
   - `dist = strategy_fn(info, legal.len())`（复用 `nlhe_h3_report` 的 hybrid
     `make_strategy_fn`）。
   - 从 `dist` **采样**索引 `i`（per-decision seed = hash(action 串, hole, board, --seed)
     → 确定性、可复现、仍保留混合策略）。
6. 出 incr（outgoing 翻译，以**真实 pot** 算尺寸）：
   - `legal[i]` = Fold→`f` / Check→`k` / Call→`c`。
   - AllIn → `b<real.legal_actions().all_in_amount>`。
   - Bet/Raise{ratio_label r} → 在 `abstract_actions(&real)`（同一抽象作用在真实 pot 上）
     里找同 `r` 的动作，取其 `to`，发 `b<to>`；若该档在真实 pot 下已塌成 AllIn 就发
     all-in。`to` 由 `DefaultActionAbstraction` 自带 floor-to-min / cap-to-allin 保证落
     在 Slumbot 合法区间。

> 计数对齐是**构造性保证**：abs 永远走在 betting tree 上，节点合法动作数训练时即固定，
> 真实 pot 不影响它 → 永不触发 `StrategyLengthMismatch`。漂移只影响 incoming 选档与
> outgoing 尺寸，二者皆为可度量近似。

## 6. Python driver

fork `sample_api.py`：`Login/NewHand/Act` 原样保留；把"naive check/call"那段换成"调
advisor 拿 incr"。启动时 `subprocess.Popen` 拉起常驻 advisor、读 ready 行；每手循环把
响应字段喂进去。累计 winnings，输出 mbb/100 + 方差。

## 7. 验证测试（cargo，按规则在 vultr 跑）

| # | 测试 | 钉死什么 |
|---|---|---|
| T1 | `info_set_for_cards(node_id, state 真实牌, board) == Game::info_set(state, actor)` | 注入路径与训练路径 byte-equal |
| T2 | Card 解析 "Ac/Td/9h/2s" round-trip + 全 52 张 | 字符映射（含 Suit 字符序确认） |
| T3 | 给定 action 串+client_pos，重放后 `current_player()`/street/terminal 与独立 `ParseAction` 移植对齐 | **B2 座位反转** + B4 重放 |
| T4 | 各档 ratio 的 outgoing `to` ∈ [min-raise, stack]；all-in 串 `b20000c///` 正常 | outgoing 合法化 |
| T5 | 喂 canned 手局（含空串/我先动、含 `/`、含 all-in 空街）→ 无 panic、出合法 incr | 端到端 smoke（无网络） |
| T6（集成） | vultr 上实打 N 手 vs Slumbot，记 mbb/100 | 真实强度度量 |

## 8. 部署与 blueprint 选型

- **用 dense 100M blueprint**（`run_dense_lcfr_100m`，4.7 GiB，LBR 1143）+ **v4 cafebabe
  500/500/500 bucket 表**。理由：与 HashMap 500M（1126）同质量（差 < SE），但 dense ckpt
  加载 RAM ~4.6 GiB，能塞进 vultr 7.7 GiB；HashMap 500M 在 vultr 会 OOM。K=1000/2000
  高桶表 suspect，不用。
- advisor 跑 **vultr**（有出网 + artifact 都在本地）。**开工前验证**：dense ckpt 推理态
  RSS < 7.7 GiB（若紧张，按"高性能机按需申请"规则先问用户再起大机）。
- ⚠️ **对外服务**（把牌局发 slumbot.com）。T6 第一次真实联机前先找用户确认；大样本可能要
  `/api/login` 账号（免登录够打小样本）。

## 9. 里程碑（每步独立可测）

| M | 内容 | 产出/验收 |
|---|---|---|
| M1 | crate 加 `info_set_for_cards` + T1 | 最小改动，解锁后续 |
| M2 | advisor 骨架：加载 blueprint、stdio、Card 解析、ready | vultr RAM check 通过 |
| M3 | 重放引擎（两态 lockstep + 座位 + 决策点检测）+ T3/T4 | 座位映射钉死 |
| M4 | 策略查询+采样+outgoing 翻译 + T5 | 无网络端到端出合法 incr |
| M5 | Python driver 接通 advisor | 本地 mock 跑通一手 |
| M6 | vultr 实打 N 手 + mbb/100 | 真实强度数字（需用户点头联机） |

## 10. 已知近似 / 延后项

- incoming 用 nearest（`map_off_tree`），非 PHM —— 已决策。升级 PHM 触发条件：实测看到
  Slumbot 在 gap 区间系统性下注且我们在这些 spot 显著失血，再上，并用 nearest vs PHM
  A/B 对照 mbb/g 验证收益 > 噪声。
- 抽象/真实 pot 漂移 → incoming 选档 + outgoing 尺寸偏差；HU 每手注少、漂移小，可接受且
  可度量。
- map_off_tree 的"塌进 AllIn"投影兜底，单测覆盖。

## 11. Slumbot 协议关键事实（来源 ericgjackson sample_api.py）

- 端点：`POST /api/new_hand`（`{token}`，首次 token 可缺）、`POST /api/act`
  （`{token, incr}`）、可选 `POST /api/login`。`incr` 是**单步**动作。
- 响应字段：`old_action`、`action`（至今完整动作串）、`client_pos`(0/1)、
  `hole_cards`、`board`、`token`（可能更换，需透传）、`winnings`（手结束才有）。
- action 串：`k`=check、`c`=call、`f`=fold、`b<N>`=下注到本街 N、`/`=分街。
  all-in 可有空街，如 `b20000c///`。
- 座位（从 ParseAction 推）：初值 `pos=1, last_bettor=0`，翻牌后 `pos=0` →
  **pos 1 = SB/button（preflop 先动）、pos 0 = BB（postflop 先动）**。

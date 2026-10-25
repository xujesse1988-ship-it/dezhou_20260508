# 实时搜索：hero 真实手牌单独信息集 + 按比例强制发牌 —— 实现方案（2026-10-25）

来源：`docs/temp/openpoker_realtime_search_efficiency_research_2026_10_19.md` 第 1 条 / §4（调研时只读、未实现）。
本文把它落成具体改动、验证和上线步骤。**还没动代码**。

标注约定同调研文档：**[事实]** = 代码逐行确认；**[推断]** = 有代码或数据支撑但没直接验证。

## 0. 要解决什么

子树求解每次迭代都按 hero 范围重抽 hero 底牌 [事实，`SubgameNlheGame::root`]。hero 真实手牌的信息集只在「抽到同桶的牌」且
「对手正好走出这条线」时才被访问，常常只有几次到几十次，低于 `min_hero_visits=200` 就不用这份解：
翻牌退 blueprint，转牌 / 河牌放弃搜索走兜底（能过牌就过牌，否则弃牌，坚果 / 赔率 / 听牌地板除外）[事实]。

两类场景会撞上：

1. **常规不收敛**：10-19 实盘 65c，同一请求 5 个种子访问 27 / 3188 / 2 / 4579 / 5079 次，allin 概率 0.55 / 0.09 / 0.002 [事实]。
2. **wopo 偏离**：hero 实际没按建议打时，下一街 hero 范围按「σ(真实动作 | 牌)」条件化，真实手牌的权重可能 ≈ 0；
   hero 范围不做均匀混合（`mix_lambda_for_seat`），真实手牌所在桶在解里几乎访问不到 [事实，`subgame.rs:1323`]。

## 1. 做法（选 §4.2 版：单独信息集，不塞回原桶）

- **单独信息集**：hero 拿着真实那两张牌时，信息集的桶号换成一个专用值 `real_slot = 该街桶数`（500 桶的街就是 500，
  正常桶是 0..499）。只看牌、不看是怎么发到的：自然抽到真实组合也进这个信息集。其余组合仍按原桶。
  - 等于只给 hero 的这一手牌把抽象分细，解的仍是一个定义清楚的博弈。原桶少了这一个组合，其余组合权重不变。
  - 对手的信息集不变，看不到 hero 的牌。
- **强制发牌**：hero 当遍历者时，以概率 `q` 直接发真实底牌，其余座位在去掉这两张牌后照原规则抽（有范围按范围、
  无范围均匀）；以概率 `1−q` 照旧从范围抽。对手当遍历者时完全照旧。
- **为什么没有偏差** [推断，§4.2 的论证]：
  - ES-MCCFR 的 regret 和平均策略只在 `actor == traverser` 的节点累积 [事实，`recurse_es` / `subgame_trainer::recurse`]。
    强制发牌只发生在 hero 当遍历者时 → **对手的 regret / 平均策略完全不受影响**，只是对手学习时面对的 hero 策略收敛得慢一点。
  - 真实手牌信息集里只有一个组合：每次 hero 遍历落到它的概率恒为 `q + (1−q)·r(h)`，是常数；给定 hero 拿这手牌，
    其余隐藏信息（对手底牌、后续公共牌）的分布不变 → 只是整体乘一个常数，遗憾匹配和平均策略不受影响。
  - 其余 hero 桶：每次被采到的概率整体乘 `1−q`，同样只是常数缩放。
  - 已知的近似：其余座位是「按座位顺序逐个去掉已占用牌」抽的，和精确联合分布有一点差别 [事实]；这是现有做法，
    强制发牌时 hero 先占位、其余座位按原顺序抽，和自然抽样下「hero 不在第一位」时的条件分布有同量级的差别。验证 V3 会覆盖到。
- **代价**：hero 其余手牌只拿到 `1−q` 的更新，对手学习时面对的 hero 策略更粗 → 对手模型收敛变慢 [推断]。`q` 先试 0.5，再比 0.25 / 0.75。
- **解决不了的**（不在本方案范围）：
  - 本街对手先行动过的落点，访问量还乘着「对手走出这条线的概率」（调研 §5，办法是按真实路线强制对手动作 + 概率加权）。
  - wopo 偏离时解里的对手仍以为 hero 没有这手牌（对手遍历时 hero 照旧从推错的范围抽）。真实手牌的策略 = 「对手这样以为」时的最好打法。

## 2. 代码改动

### 2.1 发牌入口带上遍历者

| 位置 | 改动 |
|---|---|
| `src/training/game.rs` `Game` trait | 加默认方法 `fn root_for_traverser(&self, traverser: PlayerId, rng) -> State { self.root(rng) }`。Kuhn / Leduc / 蓝图 NLHE 不覆盖 → 行为逐位不变。 |
| `src/training/trainer.rs` `EsMccfrTrainer::step` / `step_parallel` | `game.root(rng)` → `game.root_for_traverser(traverser, rng)`（两处遍历者都已在调用前算好 [事实]）。 |
| `src/training/subgame_trainer.rs` `SubgameTrainer::step` / `step_parallel` | 同上两处。HashMap 版和数组版都走同一入口，现有「两条路径逐位相同」的测试在 `q>0` 下照样能用。 |

### 2.2 子树游戏：真实手牌配置 + 强制发牌

`src/training/subgame.rs` `SubgameNlheGame`：

- 新字段 `real_hand: Option<RealHand>`，`RealHand { seat: PlayerId, hole: [Card; 2], q: f64, slot_by_street: [u32; 4] }`
  （`slot_by_street[s]` = 该街桶数，即专用桶号）。构造器加 builder `with_real_hand(seat, hole, q)`；`None` = 现状。
- 覆盖 `root_for_traverser`：
  - `real_hand` 为 `Some` 且 `q > 0` 且 `traverser == seat` 时，抽一次均匀数 `u`；`u < q` → 强制发牌，否则 `self.root(rng)`。
  - 其余情况直接 `self.root(rng)`，**不消耗随机数** → 关闭时随机数流和现在完全相同。
  - 强制发牌：新函数 `sample_holes_with_fixed(seat, hole, rng)`：`used` 先放 board + hero 两张，其余未弃牌座位按原顺序抽
    （有范围走现有 CDF + 拒绝采样 + 精确兜底；无范围在可用组合里均匀抽），然后 `template.resample_hidden_with_holes`。

### 2.3 信息集：只在子树游戏里换桶号

- `src/training/nlhe.rs` `SimplifiedNlheState` 加字段 `pub(crate) real_hand: Option<RealHandTag>`（`{ seat, hole, slot_by_street }`，Copy）。
  全部 13 处构造补 `real_hand: None` [事实，数过]；只有 `SubgameNlheGame::root` / `root_for_traverser` / `query_at` 填 `Some`。
- `SubgameNlheGame::info_set`（现在直接转调 `SimplifiedNlheGame::info_set`）：先照旧算出 `InfoSetId`，如果 `actor == tag.seat`
  且该座底牌 == `tag.hole`，用 `pack_info_set_v2(slot_by_street[street], node, street)` 重新打包。
  - **不改** `SimplifiedNlheGame::info_set` 和 `cached_hand_bucket`：蓝图训练 / 查表路径逐位不变；depth-limit 叶子按桶查续局值
    也继续拿到真实桶号（专用桶号在值表里没有对应项）。
  - 桶号字段 24 位，500 / 169 都放得下 [事实，`pack_info_set_id`]。
- `query_at`（现在直接调 `SimplifiedNlheGame::info_set`）改成走上面的覆盖逻辑 → 读数和访问量门槛自动读专用信息集。
- `info_set_for_cards(node, hole, board)`（跨街复用读上一街 σ 用）：节点决策者 == hero 且 `hole` == 真实底牌时返回专用信息集。
  这样下一街推 hero 范围时，真实手牌用的是它自己的策略。

### 2.4 数组表多留一格

`SubgameTrainer::new`：`real_hand` 为 `Some` 时每街桶数 +1 传给 `SubgameTables::new`（每个被访问的节点多一个行位，内存可忽略）。
`find` 里的越界断言保留，专用桶号恰好是最后一格。

### 2.5 配置、缓存 key、预热

- `SubgameSearchConfig` 加 `real_hand_q: f64`（默认 0.0 = 关）。`> 0` 时搜索两条路径（锚定 `subgame_search` / 脱锚
  `subgame_search_unanchored`）建子树后调 `with_real_hand(hero_seat, hole, q)`；hero 座和底牌调用方本来就有。
  - 「关」= `real_hand: None`，不开专用信息集。`q = 0` 但想开专用信息集（验证 V3 的慢参照）单独给一个测试用开关，不进 CLI。
- `solve_cache_key`：加 `real_hand_q`、开关位、hero 座、hero 底牌（root 几何里已哈希各座 hole_cards [事实]，这里再显式加，免得依赖上游）。
- 预热：预热请求带同样的底牌，走同一个建子树函数 → 同 key。加一个单测钉住「预热与决策算出同一个 key」。

### 2.6 advisor / driver

- `tools/openpoker_advisor.rs`：CLI `--search-real-hand-q <0..1>`（默认 0），启动日志 `search ON:` 行加 `real_hand_q=`；
  决策调试日志里记 `hero_visits` 和是否开了专用信息集。
- `tools/poker_live/advisor_client.py`：`add_advisor_args` 加定义 + `advisor_extra_args` 透传。
  （10-24 教训：advisor 加了参数而 driver 没透传，driver 启动即退出。）

## 3. 验证（全部过了才合并，`invariants.md` §7 / §8）

| 编号 | 内容 | 通过标准（建议，需你拍板） |
|---|---|---|
| V0 | 关闭时逐位不变 | 现有全部测试（含各 byte-equal 测试）通过；新增：`real_hand_q=0` 的子树 solve 输出与改动前逐位相同。 |
| V1 | 小游戏外部对照 | Leduc 测试专用包一层游戏，覆盖 `root_for_traverser`：玩家 0 遍历时以 q=0.5 强制发某张牌（Leduc 信息集本来就精确到牌）。用现有 `LeducBestResponse` 算可剥削度，5 个种子：强制发牌版的最终可剥削度不高于基线均值 + 2 倍标准差，且两条曲线同趋势下降。 |
| V2 | 两种存表实现一致 | `q>0` 下 `EsMccfrTrainer`（HashMap）与 `SubgameTrainer`（数组）逐位相同（沿用现有对照测试，加 q=0.5 用例）。 |
| V3 | NLHE 上无偏 | 生产参数 fixture（9 座 crop blueprint + 500 桶）：开专用信息集、q=0、迭代 ×20 作慢参照；q=0.5 正常迭代。各 5 个种子，真实手牌策略的 L1 距离 ≤ 慢参照自身种子间 L1 的 1.5 倍。 |
| V4 | 效果 | 65c 那手 5 个种子：`hero_visits` 最小值 ≥ 1000（现在最小 2），allin 概率种子间极差 ≤ 0.15（现在 0.002–0.55）。再造一个 wopo 偏离点（hero 慢打、真实手牌在下一街范围里权重 ≈ 0）：改前 `low_hero_visits`，改后访问 ≥ 200 且出搜索结果。 |
| V5 | 性能 | 生产 Mac 上同 8 秒预算的迭代数下降 ≤ 5%（强制发牌省掉 hero 采样，预期持平或略快）。 |

## 4. 上线

1. 代码合并后默认 `q=0`，生产行为不变。
2. 实盘 A/B：一台 `--search-real-hand-q 0.5`，一台 0。比较 `low_hero_visits` / `search_giveup` 比例、同一请求换种子的建议稳定性、
   长期盈亏（要几千手才有统计意义）。
3. 稳定后再决定默认值，并在 `docs/status_v3.md` 记结论。

## 5. 工作量与顺序

| 步骤 | 内容 | 估计 |
|---|---|---|
| 1 | 2.1 发牌入口 + V0（关闭逐位不变） | 0.5 天 |
| 2 | 2.2–2.4 强制发牌 + 专用信息集 + 表 +1 格，单测（发牌分布、信息集换号、读数） | 1–1.5 天 |
| 3 | V1 Leduc 外部对照、V2 两种实现一致 | 0.5–1 天 |
| 4 | 2.5 配置 / 缓存 key / 预热，2.6 CLI + driver 透传 | 0.5 天 |
| 5 | V3 / V4 fixture 实验（本机 2 核跑得慢，可能要到生产 Mac） | 1–2 天（机器时间为主） |
| 6 | V5 生产 Mac 测速 + 实盘 A/B | 视手数 |

## 6. 需要你拍板的

1. 验证表里的通过标准（尤其 V3 的 1.5 倍、V4 的 1000 次 / 0.15）。
2. `q` 的初始值：建议 0.5，V3 / V4 时同时跑 0.25 / 0.75。
3. 是否接着做调研 §5（同街第二次决策的路线加权）。本方案只解决「hero 抽不到真实手牌」，不解决「对手很少走到这条线」。

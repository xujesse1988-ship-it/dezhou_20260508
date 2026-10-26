# 实时搜索：hero 真实手牌单独信息集 + 按比例强制发牌 —— 实现方案（2026-10-25）

来源：`docs/temp/openpoker_realtime_search_efficiency_research_2026_10_19.md` 第 1 条 / §4（调研时只读、未实现）。
本文把它落成具体改动、验证和上线步骤。**已实现**（分支 `feat/real-hand-forced-deal`），实现与验证结果见 §7。

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

- `SubgameSearchConfig` 加 `real_hand_q: Option<f64>`（默认 `None` = 关）。`Some(q)` 时搜索两条路径（锚定 `subgame_search` / 脱锚
  `subgame_search_unanchored`）建子树后调 `with_real_hand(hero_seat, hole, q)`；hero 座和底牌调用方本来就有。
  - **实现时改了**：不另设测试开关，`Some(0.0)` 就是「只开专用信息集、不强制发牌」（V3 慢参照），CLI 传 `--search-real-hand-q 0` 即可；
    不传 = 关。
- `solve_cache_key`：加 `real_hand_q`、开关位、hero 座、hero 底牌（root 几何里已哈希各座 hole_cards [事实]，这里再显式加，免得依赖上游）。
- 预热：预热请求带同样的底牌，走同一个建子树函数 → 同 key。加一个单测钉住「预热与决策算出同一个 key」。

### 2.6 advisor / driver

- `tools/openpoker_advisor.rs`：CLI `--search-real-hand-q <0..1>`（不传 = 关），启动日志 `search ON:` 行加 `real_hand_q=`；
  决策响应里本来就有 `hero_visits`，开了以后读的就是专用信息集的访问数。
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

## 7. 实现与验证结果（2026-10-25）

### 7.1 实现

按 §2 落地，偏差只有 §2.5 / §2.6 标出的两处（`Some(0.0)` 当慢参照、CLI 不传 = 关）。改动文件：
`src/training/{game,trainer,subgame_trainer,nlhe,subgame,blueprint_advisor}.rs`、`tools/openpoker_advisor.rs`、
`tools/poker_live/advisor_client.py`，新增 `tests/real_hand_forced_deal_leduc.rs`。

新增测试：

| 测试 | 管什么 |
|---|---|
| `subgame_trainer::tests::trainer_matches_hashmap_trainer`（加 2 个开启用例） | V2：开启后 HashMap / 数组两种存表逐位相同 |
| `root_for_traverser_without_forced_deal_matches_root` | 关闭 / 对手遍历 / q=0 时发牌与 `root` 逐位相同、随机数流不变 |
| `forced_deal_gives_hero_real_hole` | q=1 每次都发真实底牌、不撞牌、公共牌不变；q=0.5 强制比例 0.46–0.54 |
| `real_hand_infoset_uses_dedicated_slot` | 真实底牌 → 专用桶号；其它底牌 / 其它座位不变；`query_at` 与 `info_set_for_cards` 一致 |
| `forced_deal_raises_hero_visits` | 专用信息集访问数 ≥ 20 倍 |
| `subgame::tests::real_hand_q_keys_solve_and_reads_dedicated_slot` | 真实搜索路径：q 进缓存 key、读专用信息集、预热与决策同 key |
| `openpoker_advisor parse_args_real_hand_q` | CLI 解析 / 越界拒收 / 未开 --search 拒收 |
| `forced_deal_matches_natural_draw_reference`（`--ignored`） | V3（河牌小子树版，见 7.2） |
| `tests/real_hand_forced_deal_leduc.rs`（2 个常规 + 1 个 `--ignored`） | V1 |

### 7.2 验证结果（AWS 8 核 / 15 GB，`~/rh_exp/`）

| 编号 | 结果 | 结论 |
|---|---|---|
| V0 | 改动前（355e88e）与改动后（关闭）的 advisor，同请求同种子：65c 3 个决策 × 2 种子、2dQd 2 个决策 × 2 种子，**10 个完整响应逐字节相同**（含 blueprint 与搜索决策）。相关现有测试全部通过：子树 / 训练器 / NLHE 模块 88 个、`cfr_simplified_nlhe`（含 2 个长跑）、`cfr_kuhn`、advisor 搜索相关 34 个；fmt / clippy 通过。 | 通过 |
| V1 | Leduc，5 种子 × 2000 万次。vanilla：基线 0.150±0.140，q=0.5 0.129±0.061；LCFR（period 40 万）：基线 0.154±0.108，q=0.5 0.195±0.077。 | 按门槛通过，但**检验力弱**：基线自己在种子间 0.03–0.40 乱跳（见 7.3） |
| V2 | 见 7.1 | 通过 |
| V3（原设计） | 65c：只开专用信息集 + 200 倍迭代（1.12 亿次）作慢参照，访问仍只有约 1300 次，3 个种子 fold 0.94 / 0.53 / 0.81，参照本身没收敛。 | **原设计不可行**（成本太高），改用下面的小子树版 |
| V3（小子树版） | 桩桶表 3 人河牌子树，两种抽样同样总迭代（对手训练量相同），各 4 种子。2400 万次：自然抽样访问 ~7400、种子间 L1 1.05；q=0.5 访问 ~400 万、种子间 L1 0.37；交叉 L1 1.04（比值 1.47）。1.2 亿次：自然抽样访问 ~3.7 万、种子间 0.86；q=0.5 访问 ~2000 万、种子间 0.80；交叉 0.85（比值 1.02）。第 4 个动作均值：自然 0.35 → 0.45，强制 0.77 → 0.71。 | 通过。迭代加 5 倍后自然抽样往强制发牌靠拢、交叉距离 ≈ 组内距离，看不出系统性偏差。强制发牌组 2000 万次访问下也有一个种子落到另一个解：这个粗糙小博弈本身可能有多个均衡 |
| V4 | 65c（关闭 → q=0.5）：`hero_visits` 102–132 → 4070–5465；fold 0.34–0.97 → 0.96–1.00；allin 0.009–0.123 → 0.000–0.033。q=0.25 / 0.75：访问 2.0–3.0 千 / 6.0–7.4 千，fold 都在 0.97–1.00（结果不随 q 变，没有偏差的旁证）。 | 65c 两个门槛都过（访问 ≥1000、allin 极差 ≤0.15） |
| V4（偏离点） | wopo 实盘 98 条决策里只有一个「偏离且原策略几乎不选」：2dQd 翻前 blueprint fold ≈100%、hero 跟注。下一街翻牌搜索关闭时访问就有 5–6 千（同桶其它牌提供访问），**实盘数据里没出现偏离导致访问不足**。q=0.5 访问 3.8–4.2 万，但 check 概率种子间 0.75–0.99，比关闭（0.89–1.00，读的是整桶平均）更散。 | 原定「改前 low_hero_visits」的场景在实盘数据里不存在；访问多了不等于读数一定更稳 |
| V5 | 固定迭代（561216 次）求解耗时：关闭 5.7 秒，q=0.25 / 0.5 / 0.75 都是 5.6–5.8 秒。 | AWS 上持平；生产 Mac 未测 |

### 7.3 和本功能无关、但要单独查的

Leduc 上这套 ES-MCCFR 跑 2000 万次，可剥削度在种子间 0.03–0.40，vanilla / LCFR 都一样。印象中外部采样在 Leduc 上同等计算量能到 0.01 量级，
差了一个数量级左右（`invariants.md` §8：偏离 10 倍以上要停下来怀疑算法）。怀疑点 [推测]：平均策略在遍历者自己的节点上按遍历者 reach 累加，
而访问到该节点的频率还带着对手和机会的 reach，后者随迭代变化，平均策略的时间权重可能因此有偏。没有验证，需要单独立项查。
详细记录与验证计划见 `es_mccfr_leduc_convergence_issue_2026_10_25.md`。

### 7.4 结论与下一步

- 实现正确性：V0（关闭逐位不变）、V2（两种存表一致）、单测全部通过。
- 效果：65c 那类「对手下注后才轮到 hero」的点，访问涨约 40 倍、种子间策略明显稳定（V4）；求解耗时不变（V5，AWS）。
- 无偏：理论论证（§1）+ V3 小子树版 + V4 不同 q 结果一致，三方面都没发现偏差；V1（Leduc）检验力弱，只能说明没把收敛搞坏。
- 实盘数据里 wopo 偏离导致访问不足的情况没出现过（同桶其它牌已提供足够访问），这个功能对 wopo 偏离的实际收益比预想小，
  主要收益在 65c 这类深层落点。另外读专用信息集后，策略只代表这一手牌、不再是整桶平均，在近似无差别的点上种子间可能更散（2dQd）。
- 上线建议：按 §4 做实盘 A/B（`--search-real-hand-q 0.5` 对照不开）。生产 Mac 的耗时（V5）上线前测一次。

### 7.5 平均策略改法后的变化（2026-10-26）

`es_mccfr_leduc_convergence_issue_2026_10_25.md` §9.6：子树根所在街的所有行（含真实手牌专用格）改为定期扫描、直接累加精确平均，
不再在遍历者节点按自身 reach 累加。对本功能的影响：
- 真实手牌信息集的平均策略不再依赖采样次数，强制发牌只负责给 regret 多喂样本（这正是 §1 的设计意图）；访问量门槛读的仍是遍历者访问数。
- 7.2 的 V2「两种存表逐位相同」现在只对 regret / 当前策略 / 访问计数成立，平均策略由新测试 `exact_average_is_sequence_weighted_average` 对定义值验证。
- 65c / 2dQd 重跑：访问数逐个不变，读数在种子波动内一致，求解更快（65c 5.7 → 3.5 s，q=0.5）。


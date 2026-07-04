# Heuristic9 策略详解（代码级走读）

> 本文逐层讲清 `src/heuristic/` 里的实际决策逻辑，把「文档怎么说」对齐到「代码怎么做」。
> 想看架构和迭代闭环，去 [`heuristic9_design.md`](heuristic9_design.md)；本文只聚焦**策略本身**。
> 每处引用都带 `file:line`，点开即可对照源码。
>
> ⚠ **Phase B（2026-07-04）重构提示**：policy 层已重写为**显式场景森林**（节点 enum +
> `match` 分派，`preflop_decide`/`postflop_decide` 不复存在；各节点阈值移入
> `HeuristicConfig.nodes` 子树；`i_have_initiative` 过期主动权已修）。本文的策略**语义**
> 走读仍基本成立（节点响应逐规则移植），但涉及 `policy.rs`/`config.rs` 的行号与字段名以
> [`scenario_heuristic_design.md`](scenario_heuristic_design.md) §3.2.1 + §6 Phase B
> 落地状态与源码为准；本文全面更新排在 Phase C 首轮迭代后。

## 一句话概括

**它是什么。** 一套纯启发式引擎——不是 CFR，也不是 GTO。三个关键词：**分层**、**可解释**、**座位无关**（2–9 人共用同一套规则）。

**它的目标：通过启发式策略达到职业级玩家水平。**

## 决策流水线（每个决策点）

`decide()`（`src/heuristic/policy.rs:170`）把一次决策拆成六步，依次执行。注意它的签名现在多穿了一个 `ml: Option<&MlRange>`（`policy.rs:175`）——`Some` 走 ML range 加权 equity，`None` 走标量阈值 equity：

```
Request → [1]GameView重建 → [2]Features信号 → [3]对手Range(ML默认) → [4]Policy打分 → [5]Exploit剥削 → [6]合法化 → Response
```

`decide` 先取 `exploit_adjust`（`policy.rs:153`）和标量 `opp_thr = build_opp_thresholds`（`policy.rs:186`，永远算，代价极低、只在兜底路径消费），再按街分派 `preflop_decide` / `postflop_decide`，最后 `legalize`。`ml` 由 `HeuristicBot.ml_range.as_ref()` 传入（`mod.rs:79`）。

### 主货币 = 多人蒙特卡洛 equity（`equity.rs`）

equity 是整个引擎的地基，分两层算：

1. **翻前 169 强度表**（`EquityEngine::new`，`src/heuristic/equity.rs:92`，启动时一次性算好）：让每一类起手牌对 1 个随机对手跑蒙特卡洛，得到一个 `strength ∈ [0,1]`（随机两张约等于 0.5）。翻前所有阈值、以及对手 range 的「松紧标度」都以这把尺子为准。（ML 路径载入时，这张强度表改由 `strength169.json` 经 `EquityEngine::from_strength`（`equity.rs:106`）重建，保证与离线训练同轴。）
2. **翻后实时 equity**：对每个还在场的对手，先建出他的 range，然后反复采样——给对手发一手隐藏牌、补全剩余公共牌、摊牌比大小，统计 hero 的胜率（含平分）。这一步**与人数无关**，正是「同一套规则吃 2–9 人」的根基。

实时 equity 现在有**两个消费函数**，对应两条 range 路径：

- **标量路径** `equity(&[thr])`（`src/heuristic/equity.rs:156`）：每个对手一个「最低强度阈值」，**硬过滤**出 `strength ≥ thr` 的组合放进采样池，均匀抽。
- **ML 路径** `equity_weighted(&[RangeVec])`（`src/heuristic/equity.rs:284`）：每个对手一条 169 类**加权分布**，把类质量平摊到「去死牌后存活的具体 combo」，建一条累积权重 CDF，再二分采样（`sample_cdf`，`equity.rs:431`）。听牌 / 极化形态能进 range，这是标量硬阈值表达不了的。

两者摊牌评估逐字同构，唯一区别就是「对手手牌从哪来」。分发由 `equity_dispatch`（`src/heuristic/policy.rs:134`）统一：`Some(ml)` → `equity_weighted`，`None` → `equity`。

`threshold_for_top(frac)`（`src/heuristic/equity.rs:136`）负责把「对手 range 松紧的百分位」反向换算成最低强度阈值——标量路径直接用它，ML 路径的角色模板先验也用它（同一套 frac，下详）。

### 对手 range 建模（`build_opp_ranges_ml` / `build_opp_thresholds`）

详见后面的专章[「对手 range 建模详解」](#对手-range-建模详解)。这里先给一句话：为每个还在场的对手估他「可能拿着什么牌」，喂给蒙特卡洛。

- **live 默认（ML）**：`build_opp_ranges_ml`（`policy.rs:119`）为每个对手用在线 ML 流水线预测一个 169 类 `RangeVec`，喂 `equity_weighted`。
- **兜底（标量）**：`build_opp_thresholds`（`policy.rs:55`）给每个对手估一个**最低强度阈值**，喂 `equity()`。这是 `--no-ml-range` 或 ML 产物缺失时的退化锚。

两条路径**座位序完全一致**（同样跳过 `my_seat` 与已弃座、`0..num_seats` 升序），保证对手一一对应。

## 翻前策略（`preflop_decide`，`src/heuristic/policy.rs:260`）

翻前主要是纯阈值决策，用的是 hand_strength（`equity.rs:131`），基本不受 ML range 影响——唯一例外是「面对加注的平跟」分支要算真实 equity，那里会走 `equity_dispatch`（`policy.rs:375`，ML 开则用加权 equity，半 samples）。位置不用固定的 EP/MP/CO/BTN，而是换算成 `my_pos_bucket`（`view.rs:727`）——**「相对按钮还剩几个人没行动」的桶**（Early / Mid / Late / SB / BB），这样才能座位无关地泛化到任意人数。

- **短栈（<25bb）只推或弃**：强度 ≥ shove 阈值就直接全下（`policy.rs:286-289`）。
- **前面没人加注**：强度 ≥ 该位置的开池阈值（Early 0.585 / Late 0.505 …，`config.rs:131`）就 open，尺寸是 2.3bb，每多一个 limper 再加 1bb（isolation raise，`policy.rs:308-314`）。强度够不上、但便宜又多人时，用投机牌 limp 进场博 set（小对、同花连张，`policy.rs:316-327`）。
- **面对加注**：强度 ≥ 3bet/4bet 阈值就做价值再加注（尺寸为面对额的 IP ×3 / OOP ×4，`policy.rs:355-370`）；够不上就**按底池赔率决定是否平跟**（`policy.rs:374-399`）。这里专门修过一个 BB 防守漏洞：只要赔率划算就防守，不再硬卡强度门；深栈的投机牌还额外给一点隐含赔率折让。

## 翻后策略（`postflop_decide`，`src/heuristic/policy.rs:424`）

翻后第一件事就是算 equity——`equity_dispatch`（`policy.rs:435`）：live 默认拿 ML range 的加权 equity，兜底拿标量阈值 equity。再算三个正交信号（`features.rs`）：

- **made 档位**：成手强度分级，从 Air（空气）→ WeakPair → … → Monster（`src/heuristic/features.rs:12`，判定在 `made_class`，`features.rs:98`）；
- **听牌**：同花听、两头顺、后门、combo（多重听牌）（`draws`，`features.rs:193`）；
- **质地 wetness**：把「是否成对 / 两色 / 单色 / 连张」综合成一个 `[0,1]` 的湿度值（`texture`，`features.rs:239`）。

equity 仍是主货币，这三个信号的作用是**区分 equity 相同、但性质不同的牌**——同样是 50% 胜率，可能是价值牌，可能是半诈唬，也可能是空气，打法完全不同。

**没人下注时**（在下注和过牌之间选，`policy.rs:464`），按优先级从高到低：

1. equity ≥ 价值门槛，且 made ≥ 中对 → **价值下注**（`policy.rs:467`）；
2. 低 SPR + 顶对 → **承诺下注**（准备 stack-off，`policy.rs:472`）；
3. 有听牌且 equity 够 → **半诈唬**（频率门约 75%，`policy.rs:477`）；
4. 有主动权 + 干燥牌面的空气 → **c-bet 诈唬**（干面 0.80、湿面 0.55，按湿度线性插值，`policy.rs:485-492`）；无主动权则极低频 float（`policy.rs:493-499`）；
5. 其余情况 → 过牌控池，或直接放弃。

**面对下注时**（在弃 / 跟 / 加之间选，`policy.rs:509`）：

- equity ≥ 价值加注门、且顶对以上 → **价值加注**（`policy.rs:527-533`）；
- 拿到 combo 听牌 → 低频**半诈唬加注**（`policy.rs:535-541`）；
- equity ≥ 底池赔率所需（听牌有隐含赔率折让）→ **跟注**（`policy.rs:543`）；
- 非河牌 + 纯空气 → 极低频**诈唬加注**（主要用来打紧手，`policy.rs:552-562`）；
- 都不满足 → 弃牌。

**下注尺寸**（`pick_bet_size`，`policy.rs:589`；价值动作再过 `value_size`，`policy.rs:611`）：干面下小注、湿面下大注（随湿度插值）；低 SPR 时加大（为承诺做准备）；坚果 + 深 SPR + 湿面时，偶尔来一发 overbet。**人越多自动越收紧**：对手数量上升，价值门槛随之提高、诈唬频率随之下降（`multiway_*` 系数）。

## 对手 range 建模详解

这一环最容易被忽略，却决定了翻后每一个 equity 数值。目标是：**在不知道对手底牌的前提下，用他这一手的一路行动，反推他「可能拿着什么牌」**，再把这份推断喂给蒙特卡洛。推得准，翻后所有价值 / 跟注 / 诈唬阈值才站得住；推得糙，equity 就全是噪声。

现在有两条路，**共享同一套角色骨架**（动作角色 → top-frac：3bet 0.08 / open 0.17 / coldcall 0.34 / limp 0.55 / bbcheck 0.80），但表达力天差地别：

- **ML range（live 默认）** 输出一个完整的 169 类**分布**——能表达听牌、极化、board 相关性。
- **标量阈值（兜底）** 只输出一个**最低强度阈值**——把 range 压成一条「从坚果往下取到某条线」的线性截断。

### ML range 流水线（live 默认，`range_online.rs`）

每个决策点、对每个活跃对手座位，`MlRange::predict(view, opp_seat)`（`src/heuristic/range_online.rs:49`）四步走出一个归一化 169 类 `RangeVec`：

**① 采样构造 `GameView::to_range_sample(opp_seat, prof)`（`src/heuristic/view.rs:821`）。** 从 live 的 `GameView` 直接拼出一个 `range::Sample`：核心是**因果事件流 `ev[]`**——每个动作一条，字段 `st/s/a/c/p/tc/hb`（街 / 座位 / 动作 / 动作后本街累计 / 动作后底池 / 动作前 to-call / 动作前 high-bet），在重放循环里逐动作记录（`view.rs:509-517`），**口径逐字段对齐离线 featurizer**（`tools/range_dataset_build.py` 的 `finalize_live_hand`），防训练/服务特征漂移（train/serve skew）。其余上下文（board / 盲注 / 栈「盲注已扣」/ hero 底牌 / `tseat` 目标座）从 `GameView` 直取。

> ⚠ **`prof` 当前传 `None`**（`range_online.rs:50`）——ML range **暂不吃 per-opponent 画像**，因果先验里的 `range_threshold_shift` 恒为 0。也就是说，累积对手画像目前只经由[剥削层](#剥削层opponentrs-弱场最大-ev-来源)影响 **hero 自己的打法**（价值/诈唬/尺寸），还没接进 ML 的**对手 range** 建模。这是明确的后续项。

**② token 化 `tokenizer::tokenize(&Sample)`（`src/heuristic/tokenizer.rs:196`）。** 把样本铺成一串 **72 维 per-token 特征序列**（`FDIM=72`，`tokenizer.rs:18`）：CTX token（人数/栈/位置桶）+ PROF token（画像 15 维）+ 逐街 board token + 逐事件 action token。这是离线 featurizer 的 `feat` 张量的逐义 Rust 复刻（字节级对拍），与 range 侧的因果先验 `pidx[t]` 逐 token 对齐。

**③ 组合先验 + 残差 + 牌面移除。** `predict` 把三样东西拧成 logits（`mlrange::predict`，`src/heuristic/mlrange.rs:216`）：

- **因果 m_prior**（`RangeModel::causal_logpriors(&Sample)`，`src/heuristic/range.rs:486`）= 一个**手写贝叶斯追踪器**（原 multi_field 草案 §2，该文档已由 `docs/scenario_heuristic_design.md` 代替）：翻前按角色（`role_now`，`range.rs:668`）取模板先验（`RangeModel::prior`，`range.rs:380`），翻后按对手每个动作乘一个**似然算子**（`RangeModel::op`，`range.rs:399`——bet/raise/call/check 各有极性：大注抬价值、跟注偏中段、check 偏两极）。角色模板阈值就是上面那套 `TPL_FRAC`（`range.rs:34`），经 `EquityEngine::threshold_for_top` 折成强度线。强度表由 `strength169.json` 经 `EquityEngine::from_strength` 载入。这是「B2b」基线，可解释、零数据可用，充当**先验锚**。
- **GRU 学习残差**（`GruForward::residual(feat)`，`src/heuristic/mlrange.rs:137`）= 一个小的单层 GRU，吃 token 序列、在 m_prior 之上输出一个**学到的残差**。它是纯 f64 手写前向，逐义移植自离线 torch 模型（golden 字节级对拍）。
- **合成** `mlrange::predict(logprior, residual, alive, rscale)`：`logits = logprior + rscale·residual + log(alive)`，`softmax` 归一。`rscale` 默认 **0.8**（`RangeConfig.ml_rscale`，`config.rs:60`）；**死类做牌面移除**——去死牌（hero 底牌 + 当前 board，`alive_counts`，`range_online.rs:109`）后存活 combo 数为 0 的类掩 `−1e30`（`mlrange.rs:229`），不可能的手严格零质量，部分阻断的手按存活 combo 折减。

结果就是**对手在当前决策点的预测手牌分布**——一个归一化 169 类 `RangeVec`（`range.rs:294`）。

**④ 消费。** `build_opp_ranges_ml`（`policy.rs:119`）为每个活跃对手收一个 `RangeVec`（座位序与标量 `build_opp_thresholds` 完全一致），`equity_dispatch` 路由到 `EquityEngine::equity_weighted(hole, board, &ranges, samples, seed)`（`equity.rs:284`）——加权 CDF 采样取代标量硬过滤。

**优雅降级：** `MlRange::load`（`range_online.rs:29`）任一产物缺失/维度不符 → `None`；构造期日志 + 回落标量路径（`mod.rs:47-54`），绝不 panic。

**出处 / 证据（简）。** ML range 的 Rust 前向与离线 golden 字节级对拍通过（2026-07-02）；离线评估里 GRU 残差模型的**分街 top-5 命中率显著超过 B2b 手写基线**（翻后三街 p≤0.012）。live-EV 的定论数字尚未落，接线为 opt-in、sim 安全门把关。详见 `docs/opponent_range_ml_design.md`（M1/M3）。

### 标量阈值兜底（`build_opp_thresholds`，`--no-ml-range` 的退化锚）

关掉 ML（或产物缺失）时，对手 range 退回老模型：`build_opp_thresholds()`（`policy.rs:55`）返回一个 `Vec<f64>`——**每个还在场的对手对应一个 `thr ∈ [0, 0.96]`**，代表该对手 range 的「最低起手强度」。这个强度用的是 `EquityEngine` 那把 **equity-vs-1-random 尺子**（随机两张约等于 0.5），和 hero 自己翻前的 `hand_strength`、翻前 open 阈值（0.505–0.585）**同一把尺**，可以直接比较。

> **它和 ML 是什么关系？** `RangeVec::from_threshold(thr)`（`range.rs:322`）把一个标量阈值精确复刻成「硬 membership」的 169 类向量（`strength ≥ thr` 的类给权重、否则 0，再归一）。也就是说，**标量路径 = ML 加权 range 的退化特例**——把连续分布压成 0/1 的门。ML 只是把这条硬门放宽成一条有听牌、有极化的软分布。

记住 `thr` 的方向：

- `thr` **越高** → 对手 range 越**窄、越强**（只剩强牌）→ hero 打他的胜率越低 → 越该弃牌、越该缩减价值；
- `thr` **越低** → 对手 range 越**宽**（灌进大量弱牌，但**顶端照样有 AA**，不是「没有强牌」）→ hero 平均胜率更高 → 敢打更薄的价值、跟更宽的牌。

> 注意「宽 ≠ 弱」：调低 `thr` 只是把 range 的**下限**放低，让手数变多、平均变弱，**顶端永远还是坚果**。

#### 阈值怎么组装（四层叠加）

对每个对手 `s`，`build_opp_thresholds`（`policy.rs:80-111`）按顺序叠这四层：

```
thr = threshold_for_top(frac)          # 第①层：动作角色 → 起始百分位
    + postflop_tighten                 # 第②层：街越晚越紧
    + aggressor_size_tighten           # 第③层：注越大越紧（仅本街下注者）
    + range_threshold_shift(画像)       # 第④层：nit/station 偏移
    → clamp[0.0, 0.96]
```

**第①层：动作角色 → `frac`（top 百分位，`policy.rs:88-100`）。** 先判断这个对手在本手扮演什么角色，映射到「他的 range 大概是最强的百分之几」——用的正是和 ML 角色模板同一套 frac：

| 角色 | 判据 | `frac`（top%） | 含义 |
|---|---|---|---|
| 3bet+ 池的激进者 | 是翻前/本街加注者，**且** `preflop_num_raises ≥ 2` | **0.08** | 最紧，约等于顶 8% 强牌 |
| 开池 / 单加注者 / 翻后首个下注者 | 是翻前/本街加注者，**且** `preflop_num_raises ≤ 1` | **0.17** | 约顶 17%（对照 hero 的 `open_mid`） |
| 冷跟一个加注 | 非加注者，`committed_total ≥ 2.5bb` | **0.34** | 跟进但不主动，中等偏宽 |
| limp / 补盲 | 非加注者，`committed_total ≥ 1.5bb` | **0.55** | 很宽 |
| 盲位过牌 option | 其余（`< 1.5bb`） | **0.80** | 几乎是随机两张 |

> 关键点：`preflop_num_raises` 会**一直保留到翻后**。所以 3bet 池里的翻后下注者仍然吃 0.08 的紧读，单加注池的翻后下注者吃 0.17——「3bet 池 range 更强」这条常识就自动成立了。角色判断里**加注者优先**，后面那几档 `committed_bb` 只落到「投了钱但没加注」的对手身上。

**第②层：街越晚越紧**（`policy.rs:64-70`）：`postflop_tighten = 0.03 × street_idx`，也就是翻牌 **+0.03** / 转牌 **+0.06** / 河牌 **+0.09**。人到越晚的街还愿意投钱，range 就越诚实。翻前这一层为 0。

**第③层：本街注越大越紧**（`policy.rs:71-79, 101-105`）：只对**本街最后一个下注者**（`street_aggressor`）、且翻后有下注时才生效。`bet_frac = high_bet / pot`（夹到 `[0, 1.5]`），`aggressor_size_tighten = 0.12 × bet_frac`。下注占底池越大，就越假设他是价值牌，range 收得越紧。

> ⚠ **这是刻意的剥削假设，不是 GTO。** GTO 里大注 range 反而更 polar（掺很多诈唬），但弱场真人的习惯是「下大注 = 有牌、很少诈唬」，所以这里对大注**收紧**而不是放宽（`policy.rs:71-72` 注释点明了这点）。碰上真会诈唬的激进对手，就靠第④层的画像偏移把这份「过度尊重」扳回来。

**第④层：对手画像偏移**（`range_threshold_shift`，`opponent.rs:136`）：观测够 18 手才触发，按分类加减 `adjust_cap`（默认 0.12）：nit **+cap**（收紧，方便 hero 弃牌）、station **−cap**（放宽）、maniac **−0.8cap**、lag **−0.4cap**、tag / unknown **0**。

最后统一 `clamp[0.0, 0.96]`——**永远不到 1.0**。再紧也要给对手留一点 range，避免「假设对手只有绝对坚果」导致 hero 疯狂弃牌。

#### `frac` 和 `thr` 怎么换算（`threshold_for_top`，`equity.rs:136`）

**用「考试分数线」来打比方最好懂。** 把全部 1326 手起手牌**从强到弱排成一队**：

- `frac`（「要前百分之几」）= 你想圈住队伍最前面多大一截，比如「前 8%」或「前 80%」；
- `thr`（强度门）= 卡在这一截末尾的那道**分数线**。

`threshold_for_top` 就是**「给比例，返回分数线」**：想要的人越少（前 8%），线划得越高 → `thr` 大；想要的人越多（前 80%），线越低 → `thr` 小。所以 `frac` 越小 ⇔ `thr` 越大，都表示「越强」。代码把牌按强度**从高到低**累加组合数（pair 6 / suited 4 / offsuit 12，合计 1326），累到 `frac × 1326` 手时返回停在那手的强度。

| `frac` | ≈ 强度门 | ≈ 手牌范围 |
|---|---|---|
| 0.08 | ~0.62 | 99+、AQs+、AKo 一档的强牌 |
| 0.17 | ~0.57（≈ `open_mid`） | 66+、A9s+、ATo+、KJs+，较宽开池 |
| 0.34 | ~0.53 | 再加入小对、宽同花连张、宽 Ax |
| 0.55 | ~0.50（≈ 随机） | 半数牌 |
| 0.80 | ~0.45 | 几乎任意两张 |

#### 阈值怎么被消费（`equity.rs:179-201`）

每个对手的 `thr` 进入 `equity()` 之后：先枚举去掉 dead 牌（hero 手牌 + 已知公共牌）之后所有可能的对手组合，**只保留 `strength ≥ thr` 的**放进该对手的采样池；蒙特卡洛每一轮从各池各抽一手、补齐公共牌、摊牌。两个鲁棒性兜底：

- **阈值太紧、把池筛空了** → 退化成「任意两张」（`equity.rs:192-199`），绝不 panic；
- **每个对手各自采样自己的池** → 多人池里「加注者 top-17% + 冷跟者 top-34%」是两条不同的 range，hero 的 equity 是**同时打赢两者**的胜率。多人收紧在 range 这一层就自然发生了。（ML 路径的 `equity_weighted` 同理，只是每人换成一条完整分布。）

#### 例子（以标量锚为准，ML 会把每条 range 细化成分布）

**例 A — 翻前面对 3bet。** 100bb，hero 在 BTN open 2.3bb，BB 3bet 到 8bb，轮到 hero。

- BB 是加注者、`preflop_num_raises = 2` → `frac = 0.08`；还在翻前，②③层都是 0。
- BB 未知画像 → `thr ≈ 0.62`，range 约等于顶 8%（99+ / AQs+ / AK）。hero 的 AJs 对这条 range 胜率偏低 → flat / fold，而不是 4bet。
- 若 BB 是 **nit**（VPIP<0.16、观测 ≥18 手）→ `+0.12` → `thr ≈ 0.74` → range 收到约顶 2–3%（QQ+ / AK）→ hero 更倾向弃牌。
- 若 BB 是 **maniac** → `−0.096` → `thr ≈ 0.52` → range 放宽到约顶 40% → hero 敢用更宽的牌 4bet / call 抓诈。

**例 B — 单加注池的翻牌 c-bet。** 6-max，BTN（villain）open 2.5bb、BB（hero）跟注，翻牌底池约 5.5bb。villain 在 A♠7♦2♣ 干面下注 3.5bb。

- villain 既是翻前加注者又是本街下注者，`preflop_num_raises = 1` → `frac = 0.17`（`thr` 起点约 0.57）。
- 到翻牌 → `postflop_tighten = +0.03`；`bet_frac = 3.5/9 ≈ 0.39` → `aggressor_size_tighten ≈ 0.047`。
- 合计 `thr ≈ 0.65`——**比 villain 翻前开池还紧**。弱场里「开池后又在 A 高干面继续开火」值得给足价值信用 → hero 中低对子胜率不够 → 弃牌。若 villain 是 maniac，第④层 `−0.096` 把 `thr` 拉回约 0.55 → 边缘对子转跟注抓诈。

**例 C — 多人池里同时给不同角色建模。** 翻牌三人：villain1 翻前 open（`frac 0.17`），villain2 冷跟（`frac 0.34`）。翻牌 villain1 下注、villain2 跟、轮到 hero。

- villain1 `thr ≈ 0.57 + 0.03 + 注大小项`（本街下注者，吃第③层）；villain2 `thr ≈ 0.53 + 0.03`（只跟注，不吃第③层）。
- `equity()` 用**两条不同 range** 同时采样，hero 的 equity = 同时击败两者的胜率——比只对付 villain1 时低得多，多人纪律自然逼出。

**例 D — 同一个动作、不同对手，得到不同 range。** 两人都翻前 open 2.3bb（都 `frac 0.17`、起点 `thr ≈ 0.57`）：**nit** → `+0.12` → `thr ≈ 0.69`（当顶 ~5%，防守极紧）；**station** → `−0.12` → `thr ≈ 0.45`（当「任意两张」，敢用宽牌打价值）。同一个 `open`，因累积画像不同被翻译成截然不同的 range——「把累积经验变现」落到 range 层。（注意：这一层画像变现目前**只在标量路径**生效；ML 路径 `prof=None`，暂不吃画像。）

## 剥削层（`opponent.rs`）—— 弱场最大 EV 来源

跨手、跨 session 地累计每个对手名下的 **VPIP / PFR / AF**，攒够 18 手就分类成 nit / tag / station / maniac / lag（`classify`，`src/heuristic/opponent.rs:113`），再翻译成两类调整：

1. **改对手的 range 模型**（`range_threshold_shift`，`opponent.rs:136`）：nit 收紧（方便我弃牌）、station 放宽。（**当前只作用于标量路径**——ML 路径尚未接画像。）
2. **改我方的打法**（`aggregate_exploit`，`opponent.rs:176`，经 `exploit_adjust` 汇总，`policy.rs:153`）：
   - 场上有 **station** → 价值打薄 + 诈唬砍半 + **价值注尺寸 ×1.30**（`station_value_size_mult`，`config.rs:257`——站着跟大注的对手，就把价值注做大多榨一点；诈唬不放大，因为 station 根本不弃）；
   - 场上有 **maniac** → 放松抓诈的跟注门槛，自己少诈唬；
   - **全是 nit** → 多偷盲 + 多诈唬（×1.3）。

这一路是「无论 ML 开不开」都吃 hero 自身打法的：它调的是价值/诈唬门槛与尺寸，不是对手 range。剥削可门控，默认关闭时**逐字节等于历史基线**，没有 station 时乘子恒为 1.0、零影响。调整幅度 `adjust_cap` 卡在 0.12（≥0.16 时对 nit 过度偷盲会断崖式被惩罚）。

## 安全网 & 混合

- **合法化**（`legalize`，`policy.rs:198`）：最后一道网，保证输出严格落在 `valid` 集合内。不能 check 就保守退化（不能 call 就 fold），raise-to 夹到 `[min_raise, max_raise]`。
- **异常兜底**（`safe_fallback`，`policy.rs:241`）：碰到任何异常（如无手牌信息），能 check 就 check，否则 fold——sim 实测 0 次 fallback、0 次非法输出。
- **受控随机**：cbet / semibluff / bluff 的频率门用的是**以手牌状态为种子的确定性 RNG**（`situation_seed`，`policy.rs:33`），既可复现，又不会被简单套路针对。equity MC 的 seed 也同源混合（`equity.rs:204` 起）。

## 参数面（`config.rs`）

所有会影响 EV 的魔法数都收进 `HeuristicConfig`（`src/heuristic/config.rs`），带默认值、可被 JSON 覆盖，分成六组：`equity` / `preflop` / `postflop` / `mixing` / `exploit` / **`range`**。单位约定：筹码量一律用 **big blind 倍数**，和桌子大小无关 → 同一份 config 通吃任意 stake。改启发式 = 改这里的数 + 跑 `heuristic9_sim` 回归门。

新增的 `RangeConfig`（`config.rs:41`）是 ML range 的旋钮面：

| 字段 | 默认 | 含义 |
|---|---|---|
| `use_ml_range` | `false`（config 层）／`true`（**live advisor**） | 开则每对手建 169 类 `RangeVec` 喂 `equity_weighted`，取代标量 `equity` |
| `ml_weights_path` | `artifacts/range_ml/range_v1_gru_weights.json` | GRU 残差权重 JSON |
| `ml_strength_path` | `artifacts/range_ml/strength169.json` | 169 强度表（→ `from_strength` → `RangeModel`） |
| `ml_rscale` | `0.8` | 残差温度（与离线 golden 一致） |

> **关于默认值的两处口径别搞混：** `RangeConfig::default().use_ml_range = false`（`config.rs:57`）——这是为守「默认 config 逐字节等于历史标量基线」的纪律（sim/单测走这条）。但 **live advisor 二进制显式把它翻成 `true`**（`heuristic9_advisor.rs:139`），`--no-ml-range` 才退回。所以「**当前 live 运行时的对手 range = ML 路径**」，而「**默认 config = 标量基线**」，两句都对，看的是哪一层。

> 另注：`config.rs` 里标了 `⚠ 暂未接线` 的字段（`postflop.size_mid`、`postflop.checkraise_freq`、`mixing.temperature`）目前不影响 EV，是给后续功能（三档尺寸 / check-raise 专家 / 边界 softmax 混合）预留的位置。

## 设计取舍

`heuristic9_design.md §0` 说得很直白：CFR 会被「训练时假设的人数」绑死、不可解释、改一处就要重训；而启发式在弱场里这些反倒全是优点——座位无关、旋钮即调、每个决策都能回溯到是哪个信号在驱动。ML range 的引入没有背离这条哲学：它接在同一条**「所有维度折算成 equity」**的主货币上，只把「对手 range」这一个输入从标量升级成分布；标量锚（`from_threshold`）随时可退回，`safe_fallback` / `legalize` 作为共享地板不变。升级仍靠那条闭环：**打 1000 手 → analyze 找漏洞 → 调 config / 接线 → 跑 sim 回归门，不退化才上线**。ML range 的接线正是这条纪律的最新一环——离线先行、golden 对拍、opt-in 门控、sim 安全门把关。

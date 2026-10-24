# limp 池里 hero 自己的范围：按 limp 规则算（方案 A）设计

日期：2026-10-24。状态：设计，未实现。

相关文档：
- `docs/temp/openpoker_limp_handling_research_2026_10_16.md`（limp 规则与平跟当弃牌映射的由来）
- `docs/temp/unanchored_range_design_2026_06_10.md`（脱锚搜索档一前缀 reach）
- `docs/temp/poker_manager_live_20261023_bp2_115653_unanchored_exploit_analysis_2026_10_23.md`（115653 批次分析，limp 池放弃搜索的数据来源）

## 0. 结论

hero 翻前由 `limp_heuristic` 决定的每一步，算 hero 范围时不再乘 blueprint 的 σ，改成拿 limp 规则本身对 1326 个组合各算一遍：
做出的动作和实际一致记 1，否则记 0，用这个系数乘进 hero 的范围。

limp 规则是确定性的，所以这样得到的 0/1 范围就是 hero 的真实范围，不是近似。不给 hero 加权重下限，也不混均匀分布。

## 1. 原理：估对手和估 hero 是同一件事

两边的范围都是「只看得到这个人的动作，反推他拿每个组合的概率」：

P(拿 h 走到这里) ∝ 每一步「拿 h 时做出实际那个动作的概率」相乘

hero 的真实手牌不参与这个计算。两边唯一的差别是公式里那一项从哪来：

| | 对手 | hero |
|---|---|---|
| 每步动作概率从哪来 | 不知道，只能用 blueprint、VPIP 统计或假设去近似 | 就是我们自己执行的策略（blueprint、limp 规则、PM-17），代码里都有 |
| 误差从哪来 | 对手实际打法和模型不一样 | 只会出在「算范围用的策略」和「实际执行的策略」不一致 |
| 要不要权重下限 / 混均匀 | 要，给模型误差留余量（Loki 的 0.01 下限、我们的 `range_uniform_mix`） | 不要；用对了策略就没有模型误差，加了反而人为制造误差 |

hero 知道自己的牌，但求解里的对手不知道。对手针对 hero 的整个范围找应对，hero 拿真实手牌的最好打法又取决于这些应对，所以 hero 也必须有范围，而且必须是 hero 真实打法对应的范围。范围里混进假牌，对手的应对就是针对一个不存在的 hero 算的，hero 真实手牌的策略又针对这些应对去优化，错会传两层。`mix_lambda_for_seat` 不给 hero 混均匀分布也是这个原因：2026-06-12 实测对称混合后方向直接反掉。

业界做法：DeepStack 每次重新求解时，自己的范围从均匀开始，每次自己行动后「用算出的策略按贝叶斯规则更新」，发公共牌后把不可能的组合清零。用的是自己**实际执行**的策略，不是另一套参考策略。方案 A 就是照这个规则做。

关于「要不要用对手以为的 hero 范围」：不要。用真实范围等于假设对手知道 hero 的打法，这是最保守的假设，对手猜对了也不会吃亏。改用「对手以为的范围」属于利用对手的错误认识，要先给对手对 hero 的看法建模，我们没有这份数据，猜错了反而会亏。

## 2. 现状：错在哪

### 2.1 例子 #580（115653 批次）

经过：dd 在按钮位平跟，SB 加注到 40，hero 在 BB 按 limp 规则跟注；dd 再 3bet（失同步 `limp_as_fold:limper_continued`），hero 又按 limp 规则跟注。翻牌走脱锚搜索。

| hero 这一步 | 实际谁决定 | 现在算 hero 范围时乘的是什么 |
|---|---|---|
| BB 跟注 SB 的加注 | limp 规则（P 档才跟） | **blueprint 里 BB 跟注 SB 开池的概率**，QQ 这类强牌在 blueprint 里基本 3bet，跟注概率很低 |
| 跟注 dd 的 3bet | limp 规则 | 什么都没乘（失同步之后的动作不进前缀，按 1 处理） |

hero 的范围里装的是「blueprint 会平跟 SB 开池的那些中等牌」，hero 的真实手牌被压到接近 0，正好和真实范围反过来。
按 limp 规则，hero 这时的真实范围是 P 档：JJ+、AK、AQ，共 56 个组合，每个权重相同。

参考数据（实盘日志统计的 blueprint 面对开池跟注概率）：BB 拿强牌 13.5%、中对 68%；SB 拿强牌 7.2%、中对 18.5%；AA 0.0001，KK 0.01。

注意：#580 里 hero 真实手牌的权重具体是多少，因为服务器连不上，没能用 `--debug-log` 重放核实。上面是按代码路径推出来的。

### 2.2 后果

- ES-MCCFR 按范围抽 hero 的手牌，真实手牌权重接近 0 就几乎抽不到，访问次数达不到 `min_hero_visits`（200），最后放弃搜索。
  115653 批次 23 次放弃搜索里有 14 次在 dd 的 limp 池；上一批的 KK 未访问、AKo 只有 8 次访问，也都在 limp 池。
- 没放弃的 limp 池解，hero 范围也是错的，解里对手的应对针对的是一个偏弱的 hero。

### 2.3 现有代码的处理

- `tools/openpoker_advisor.rs` 的 `unanchored_prefix_decisions` 只去掉了「hero 自己平跟被映射成弃牌」那一步（按 1 处理）。hero 在已同步前缀里由 limp 规则做的其他动作，乘的仍是 blueprint σ。
- `src/training/subgame.rs` 的 `estimate_range` 对每个座位都用 blueprint σ 累乘（同质 blueprint 假设），不知道 hero 哪些步不是 blueprint 做的。

## 3. 设计

### 3.1 找出 hero 哪些步是 limp 规则做的

逐个看 hero 的翻前动作，对每一步截出「这一步之前的历史」，用和 `decide` 相同的分流逻辑判断当时走的是哪条路：

- `lockstep_replay` 成功、`limp.mapped()` 且在翻前 → limp 规则；
- `lockstep_replay` 失同步、原因是 `structural_gap` 或 `limp.mapped()`、且在翻前 → limp 规则；
- 前面先命中翻前全下启发式（`allin_spot`）的 → 不是 limp 规则（见 §5）；
- 其余 → blueprint，照旧乘 σ。

分流只看动作历史，和底牌无关，每一步判断一次即可。

### 3.2 每一步对 1326 个组合各算一遍

- 用截断后的历史构造当时的请求，调用**同一个** `limp_heuristic`，不另写一份「范围版」，免得以后改规则时两边不一致。
- `limp_heuristic` 会读当时的 `valid`（`can_raise` / `can_check` / `can_call` / 加注上下限），请求里只有当前这一步的 `valid`。
  所以要用真实筹码重放到那一步，从 `legal_actions` 重建。
- 底池赔率（`preflop_pot_odds` → `pot_odds_floor_hit`）和底牌无关，每一步只算一次。命中赔率下限时所有牌都跟，这一步系数全是 1，等于没收窄，这本身就是对的。
- 加注额 `(3 + 平跟人数 + 是否不在位置) × BB` 和底牌无关，只比较动作类型（加注 / 跟注 / 过牌 / 弃牌）。
- 和公共牌冲突的组合照旧由 `estimate_range` 清零。

### 3.3 和 blueprint 前缀合并

- 从前缀决策里**去掉**这些步，避免 blueprint σ 和 0/1 系数重复相乘。现在「去掉 hero 平跟映射成弃牌那一步」的逻辑并进来：那一步改乘 limp 规则的系数（跟着平跟 / 补齐那批牌），比现在按 1 处理更准。
- 失同步之后 hero 再由 limp 规则做的动作（#580 的跟注 3bet），前缀里本来没有，系数直接乘上。
- hero 其他由 blueprint 做的步照旧乘 σ。
- 接口：`PrefixReach` 加一个只作用于 hero 座位的系数向量（`Option<&[f64]>`，下标对齐 `all_hole_combos()`）。`estimate_range` 算完 hero 后乘上它再归一化；hero 照旧不混均匀分布（`mix_lambda_for_seat` 不变）。

### 3.4 一致性检查

hero 真实手牌的系数必须是 1（实际就是用这手牌按规则做出了这个动作）。如果算出 0，说明分流判断和当时不一样，这一步退回现状做法，并在调试日志里记一条，不硬套。

## 4. 要注意的地方

1. **预热和实时决策必须算出同一份。** 两边共用 `unanchored_prefix_decisions`，hero 系数也放进同一个函数，否则缓存永远对不上，预热白做。范围本来就进 solve 缓存 key，范围变了自动重解，不会串用旧解。
2. **前缀为空时不能退均匀。** 现在 `subgame.rs` 里 `prior.is_empty()` 时直接返回 `None`（均匀，跳过整个范围计算）。有 hero 系数后，即使 blueprint 前缀被去空，也要照样构造范围。这里写错会让修改悄悄失效。没有 hero 系数时保持现有行为逐位不变。
3. **转牌、河牌自动修好。** 后面的街用跨街后验（档二′），从翻牌解的根部范围沿实际动作推下来，翻牌这一层修对了后面就对了。
4. **计算量。** `chen_top_fraction` 每次把 1326 个组合扫一遍，1326 手各调一次约 176 万次比较，每步几十毫秒。改成先算一次排名表再查表，可以降到可以忽略。
5. **会改变解里对手的打法，这是想要的。** hero 范围从一堆中等牌变成 P 档，解里的对手会更尊重 hero 的下注。6 月那次方向反掉是往 hero 范围里塞了假的强牌；这次是把范围改成真的，性质不同。
6. **锚定路径不覆盖。** 平跟者面对加注弃牌后整手回到锚定路径，那里 hero 范围同样乘 blueprint σ（`estimate_range(skip_all_in=false)`）。实盘开着 `--search-force-unanchored`，翻后都走脱锚路径，能覆盖到；关掉时覆盖不到，在注释里写明。

## 5. 这次不做的

- **翻前全下启发式**（`allin_response`，按 `ALLIN_RANGE_TABLE` 取 Chen 序前 x%）：同样是 hero 不按 blueprint 打，也是确定性规则，可以照搬这套办法，第二步再做。
- **PM-17 的 4bet**：在 blueprint 基础上挪一部分弃牌概率去 4bet，系数不是 0/1，要按挪过之后的分布算，单独处理。
- **兜底里的 premium_call**（`fallback_with_floor`）：同上，后续再看。
- **对手平跟者的范围**：现在是「blueprint 开池范围之外的牌」再混一点均匀。改成 VPIP 和 PFR 之间那一段（剥削先验里已有 `CallBand`）可能对 dd 这种 VPIP 66% / PFR 53% 的人更准，但这是另一个问题，要单独 A/B。

接口按「hero 某一步的系数向量」设计，以后接全下启发式和 PM-17 不用再改 `subgame.rs`。

## 6. 验证

1. 单元测试（按测试名逐个跑，套 1.2G 内存限制）：
   - 面对加注时的系数正好是 P 档那 56 个组合；
   - 未加注的平跟池里，隔离加注档、跟着平跟档的牌数和 `limp_ranges_for` 一致；
   - hero 真实手牌系数为 1；
   - 赔率下限命中时系数全是 1；
   - 没有平跟映射的手牌，范围和现在逐位一样；
   - 预热和决策算出同一份范围（同 key）。
2. 离线重放：115653 批次 limp 池里放弃搜索的 14 次决策，加上 10-21 那手 KK 和 AKo，用 `--debug-log` 比较改前改后 hero 真实手牌权重和访问次数。需要服务器，或者本机在 1.2G 限制下试跑。
3. 实盘：和 ca66745（去掉低于 5% 的弃牌）、`--search-max-nodes 1000000` 一起上，看 limp 池放弃搜索的比例。

## 7. 预期效果

- 能直接解决：limp 池里 hero 真实手牌权重接近 0 导致的放弃搜索（115653 批次 23 次里的 14 次）；没放弃的 limp 池解，质量也会提高。
- 解决不了：加注池里那 8 手放弃搜索与 hero 范围无关，分析里判断多半是噪声。
- 对每百手赢多少 BB：limp 池占比不大，单批 2000 手很可能看不出来。这是消除一个已经确认的错误，不能指望它带来明显的盈利提升。

改动量估计：代码约 150–250 行，主要在 `tools/openpoker_advisor.rs`，`src/training/subgame.rs` 改十几行，另加测试。

## 8. 参考

- DeepStack: Expert-Level Artificial Intelligence in No-Limit Poker（自己范围的贝叶斯更新）：https://ar5iv.labs.arxiv.org/html/1701.01724
- Billings et al., Opponent Modeling in Poker (AAAI-98)（对手权重表与下限）：https://www.cs.ualberta.ca/~games/poker/publications/AAAI98.pdf
- Davidson et al., Improved Opponent Modeling in Poker (ICAI 2000)：https://www.cs.ualberta.ca/~games/poker/publications/ICAI00.pdf

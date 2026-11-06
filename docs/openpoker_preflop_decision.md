# 翻前决策说明（代码口径 `562cca4`；补盲部分为 11-01 改动后）

本文讲 advisor（`tools/openpoker_advisor.rs`）在翻前怎么决定一个动作：走哪条路、每条路的规则和数字、哪些开关会影响翻前。
翻后（实时搜索、范围估计）见 `openpoker_realtime_subtree_search.md` / `openpoker_realtime_range_estimation.md`。
`openpoker_play_explainer.html` 里的翻前两节写于 10-15，PM-17、ante 放宽、全下启发式都没写进去，以本文为准。

**标注约定**：**[事实]** = 代码逐行确认；**[实测]** = wopo 实盘日志统计；**[推断]** = 读代码推出来、没有专门跑测试验证。

**对应的实盘配置**（wopo，`artifacts/wopo_ante200_9b_tilt_live_20261031_b100/common_args.sh`）：8 座、每人 ante 1BB、
200BB blueprint `run_8max_ante100_200bb_antefl2_3bet_cap4_b500` 9B 翻前 + 翻牌 crop，`--reshape ante-flop2-3bet --postflop-cap 4`，
`--search-limp-as-fold on --exploit on --exploit-tilt on`，**没开** `--exploit-4bet`（PM-17）。
wopo 上 advisor 只给建议（写行动文件），实际出牌的是人。

---

## 0. 结论

1. **翻前从不做实时搜索** [事实]。两个搜索触发面（`FlopFirstUnraised` / `AllPostflop`）都只管翻后（`src/training/subgame.rs:1056`）。
   翻前的动作只来自 5 个地方：**blueprint 查表**、**平跟池启发式**、**面对全下启发式**、**兜底**，以及（开了才有的）**PM-17 改分布**。
2. 实盘里翻前决策约一半查 blueprint，约四成走平跟池启发式 [实测，§5]。平跟池启发式的范围是手写的常识值，没有解过、也没用实盘数据校准过。
3. `--exploit` / `--exploit-tilt` / `--exploit-limp-seed` **不改翻前动作** [事实]，只用翻前动作去估翻后搜索里对手的范围。
4. 整理时确认了两个问题：补盲者过牌后 hero 会被建议用任意两张跟进去（§6.1，11-01 已修：补盲改在接入层处理）；入池人数到 5 时第 5 人的跟注被误当成平跟，落到兜底（§6.2，未修）。

---

## 1. blueprint 的翻前树

### 1.1 动作菜单（`ante-flop2-3bet`，`src/training/nlhe_betting_tree.rs:300`）[事实]

| 局面 | 树里能做的动作 |
|---|---|
| 开池（前面都弃，非 SB） | 弃牌 / 加注 1/3 底池。**没有平跟**（`no_open_limp`），**没有开池全下**（`drop_preflop_open_allin`） |
| SB 面对前面全弃 | 弃牌 / 补齐（complete）/ 加注 1/3 底池 |
| BB 没人加注 | 过牌 / 加注 |
| 面对开池（3bet） | 弃牌 / 跟注 / 加注 0.5 底池 / 加注 1 底池 / 全下 |
| 面对 3bet 及以上 | 弃牌 / 跟注 / 加注 1 底池 / 全下 |
| 入池人数会超过 4（`--postflop-cap 4`） | **删掉跟注和过牌**，只能弃牌或加注（第 5 个人起，已入池的人也一样） |

尺寸按「跟注后底池」算：加注到 = 当前最高投入 + 比例 × 跟注后底池。8 人发牌、每人 1BB ante 时，开池前底池 9.5BB，
1/3 底池开池 = 1 + (9.5 + 1)/3 = **4.5BB**（测试 `ante_flop2_open_size_and_small_open_translation`：10/20 桌开到 90）。
BB 面对 4.5BB 开池的 0.5 底池 3bet ≈ **13.25BB**（测试 `ante_flop2_3bet_half_pot_three_bet`：开到 265）。
发牌人数少时 ante 少，开池尺寸跟着变小（7 人发牌约 4.17BB）。

### 1.2 真实座位怎么放进树（`seat_map`，`tools/openpoker_advisor.rs:621`）[事实]

- 树座 0/1/2 = BTN/SB/BB，3 起按翻前行动序（8 座：3=UTG … 7=CO）。
- 短桌（k 人发牌，3 ≤ k < 8）：真实 BTN/SB/BB 放到树座 0/1/2，其余玩家占靠近 CO 的位置，空出来的最早行动位当「幻影座」，
  重放开头先替他们弃牌。也就是说，**7 人桌的 UTG 用的是 8 人树里 UTG 弃牌后 UTG+1 的策略**。幻影座不交 ante，底池与真实桌一致。
- 2 人桌只映翻前；翻后直接兜底（`short_hu_postflop`）。
- 补盲（wopo 新进场 / 回座补 1BB 的人）：**advisor 不知道有补盲**（11-01 起）。接入层（`tools/poker_live/table.py`
  `HandEngine.advisor_actions`）把补盲者当成没下过注的普通玩家发给 advisor：开局栈含补的 1BB；他翻前没人加注时的过牌改发 `call`
  （= 一次平跟，交给平跟池启发式）；跟注 / 加注 / 全下原样发；弃牌原样发，补的 1BB 在 advisor 的底池里就没有了。
  所以轮到补盲者之前、以及他弃牌之后，advisor 算的底池比真实少 1BB（底池赔率类判断略偏保守）。
  hero 自己补盲时，advisor 里他面对 1BB，策略给弃牌会因为桌面能过牌改成过牌（`:fold_as_check`）。

---

## 2. 决策顺序

`decide_with`（`tools/openpoker_advisor.rs:2764`）翻前按下面的顺序走，前一步出了结果就不往下走：

```
0. 入参校验（座位数、大盲整除、小盲 ≤ 大盲、ante、底牌）不过 ──→ safe_fallback（能过牌就过，否则弃）
1. 面对全下，且 hero 与全下者有效码深 < blueprint 码深 60%（200BB → 120BB）或 > 140%（→ 280BB）→ 全下启发式（§3.4）
2. 重放：把真实动作逐个推进 blueprint 树（「影子」），尺寸映射到最近的档（§3.2）
   ├─ 有人平跟（影子里记成弃牌，--search-limp-as-fold on）────────→ 面对全下？全下启发式 : 平跟池启发式（§3.3）
   │    （例外：开了 --search-limp-first-blueprint 且 hero 本手还没行动过 → 往下查 blueprint，§3.7）
   ├─ 推不进去（失同步）
   │    ├─ 面对全下 ─────────────────────────────────────────→ 全下启发式
   │    ├─ 原因是结构缺口（平跟进了无平跟的树）或有平跟映射 ──→ 平跟池启发式
   │    └─ 其余 ───────────────────────────────────────────→ 兜底 fallback_with_floor（§3.5）
   └─ 推进去了 → 查 blueprint 平均策略（§3.1）
        ├─ 该节点没训练到（均匀占位）─→ 面对全下？全下启发式 : 兜底
        └─ 得到动作分布
3. PM-17（只在开了 --exploit-4bet 且是 PM-17 局面时）：把弃牌概率挪给 4bet（§3.6）
3a. 样本少：hero 这手牌这一行的累计权重 < 5 万（--preflop-commit-min-visits）且 hero 还能加注 → 去掉全下和押上大半筹码的加注（§3.1）
3b. 深码：hero 与未弃牌对手中最深者的有效码深 > blueprint 码深 140% → 全下概率挪走（§3.1）
4. 抽样：弃牌 / 全下概率 < 5% 的先去掉再归一，按确定性种子抽一个（§3.1）
5. 换算成桌面动作：尺寸按真实底池算、÷ 筹码倍数、夹进 [min_raise, max_raise]（§3.1）
6. driver 合法性检查（tools/poker_live/wopo.py legalize）
```

注意第 1 步在重放之前：短码 / 深码全下不管前面有没有平跟，都先走全下启发式。

---

## 3. 各条路的规则

### 3.1 blueprint 查表 [事实]

- 信息集 = 树节点（位置 + 翻前动作线）× 169 类起手牌，取训练出来的**平均策略**（`blueprint_distribution`，`:3555`）。
  只留概率 > 0 的动作再归一。
- 该信息集只是均匀占位（没训练到，crop 里存的是空）→ 不按均匀出牌，走兜底（`fallback:blueprint_placeholder`）。
  10-16 run3 吃过亏：JJ 在 3 人池拿到过牌 / 下注 / 全下各 1/3。
- 抽样前剪枝（`prune_for_sampling`，`:5256`）：**全下概率 < 5% 去掉、弃牌概率 < 5% 去掉**，其余按比例放大。
  翻前没有 SPR，全下统一用 5% 门槛（翻后深码才用 20%）。日志里 `probs` 记的是剪枝前的分布。
- **深码不全下**（`deep_preflop_no_allin`，11-04 加）：hero 与未弃牌对手（含还没行动的）中开局码深最大者取小，
  超过 blueprint 码深 140%（200BB blueprint = 280BB，与翻牌码深闸上限同值）时，分布里全下的概率整份挪走：
  树里跟注就会全下（跟注折进了全下档）→ 给真实跟注；否则给最大的非全下加注档；没有加注档 → 给跟注档；
  分布里也没有跟注档（宽度重定向删了，或树里只剩弃牌 / 全下）而 hero 面对加注 → 给真实跟注。
  没人入池时规则上能平跟，但树里没有平跟档，不会挪成平跟。`source` 带 `:deep_no_allin`，`probs` 记挪之前的分布。
  起因：11-03 实盘 001296，hero 467BB，blueprint 建议 JJ 3bet 全下（41%）、面对 551BB 的 4bet 全下 100% 跟，输 466BB。
- **塌成全下的加注档**（`raise_goes_allin`，11-06 修）：影子树里还有 1pot 档，但按真实底池算在 blueprint 码深放不下时，
  `outgoing_action` 发的是全下，桌面上是加到 hero 全部筹码（`max_raise`），不是 blueprint 码深。这条和下一条闸门都把它当全下：
  和全下一起去掉，也不接收挪来的概率。粗算对手 3bet 到约 60–65BB 以上时 1pot 就放不下。
  修之前的表现：hero 500BB 面对 3bet 到 70BB，AKo / JJ / QQ 带 `:deep_no_allin` 却加到 499BB；实盘 001509 的 88 换 o75 后带 `:low_visits_no_commit` 却加到全部 11750。
- **样本少不押上**（`low_visits_no_commit`，11-05 加，`--preflop-commit-min-visits`，默认 5 万，0 = 关）：
  hero 这手牌这一行的累计策略和（crop 里存的原始值，每次访问加一整份 σ，等于 LCFR 加权后的访问量）低于门槛时，
  去掉全下和押上大半筹码的加注（加注新投入 ≥ 还能输的筹码 × `--preflop-commit-share`，默认 0.5；还能输的筹码 = hero 与未弃牌对手中最深者取小，再减去已投入），其余动作按比例放大（和 < 5% 的全下被剪枝时一样）；只有这些动作时给跟注；树里跟注折进全下档（菜单没有跟注、hero 面对加注）时给真实跟注。
  QQ+ 和 AK 面对加注时，这部分概率给跟注，弃牌概率不变：按比例分的话，抽样到的低权重行里 AKo 有 83/134 行弃牌过半，而样本多的相近节点这些牌几乎不弃。
  hero 只能跟注时豁免：对手已真实全下，或对手的加注已盖过 hero（桌面不给加注），这时全下档就是跟注。
  `source` 带 `:low_visits_no_commit`，`bp_row_weight` 记这一行的权重（触发与否都记），`probs` 记改之前的分布。
  - 不看动作线：同样 3bet 前 2 家跟注，权重能差 16 倍（`R1000,C,C,F,F,F,R500,F` AJo 1435，`R333,C,C,F,F,F,R500,F` AJo 22929）；
    相同权重下，跟注家数与噪声无关。
  - 门槛依据：两个 9B crop 各约 80 万行，比较每行全下概率与强度相邻手牌的均值，非对子到 5 万才稳定
    （open2 40–50k 偏差均值 / P90 .023/.039，50–70k .017/.022；3bet 40–50k .036/.091，50–70k .016/.014）。
    看原始权重、不除组合数：一行的样本是所有组合合起来的，各牌型的拐点在原始权重上一致。看本行、不看节点中位。
  - 局限：对子没有拐点（≥ 20 万仍有 .07–.08，很可能是相邻对子本来就打法不同），这个方法验证不了对子上的门槛，只是沿用同一个值。
    门槛来自 9B 的两个 blueprint，换 o75 这类新 blueprint 要重新看分布。0.5 这个比例只依据两手实盘（001450 加到 8790/10776，JJ 加到 15500/26983），不是统计出来的。低权重行的弃牌和小加注同样可能是噪声，没动。
- 种子 = hash(底牌, 公共牌, 动作序列, 基础种子)（`sample_seed`，`:5465`）：同一请求永远抽出同一个动作，混合策略照样保留。
- 出牌换算（`action_to_response`，`:3585`）：
  - 选中加注时，在**真实动作序列 + blueprint 码深**的重放局面上按同一比例算加注额，所以对手开得大时 hero 的 3bet 也按真实底池放大；
    之后 ÷ 筹码倍数四舍五入，夹进桌面给的 [min_raise, max_raise]。真实码深比 200BB 浅时加注额会被夹到 max（相当于全下）。
  - 选中弃牌但能免费过牌 → 改过牌（`:fold_as_check`，hero 补盲或在 BB 时可能遇到）。
  - 选中加注但桌面不给加注 → 改跟注或过牌（`:raise_as_call` / `:raise_as_check`）。
  - driver 层：不能加注时的 `all_in` 改跟注（`:allin_as_call`），其他不合法动作改安全动作。

### 3.2 对手的尺寸怎么放进树（`advance_shadow_by_applied`，`src/training/blueprint_advisor.rs:311`）[事实]

- 真实加注额折成「加注部分 / 跟注后底池」，落在两档之间时用 pseudo-harmonic 随机取整到上下两档之一
  （种子由局面决定，同一局面结果固定；`src/abstraction/action.rs:476`）；超过最大档但不到全下 → 按最大档；到全下 → 全下。
  然后投影到该节点合法的档。
- **开池只有 1/3 底池一档**，所以任何非全下的开池——不管真实开 5BB 还是 20BB——在影子里都是 4.5BB 开池
  （测试 `big_open_projects_down_to_small_open`）。后面的人查的是「面对 4.5BB 开池」的策略，只有 hero 自己的加注额按真实底池算。
- 3bet 在 0.5 / 1 底池之间取整；4bet 及以上只有 1 底池和全下。
- 对手全下 → 影子里是 **200BB 全下**。所以短码全下要靠第 1 步拦下来（§3.4）。

### 3.3 平跟池启发式（`limp_heuristic`，`:4089`）[事实]

**什么时候用**：首个加注之前有人跟注（非 SB 平跟、跟着平跟、SB 补齐都算）。树里删了非 SB 平跟，这类局面 blueprint 给不出策略。
`--search-limp-as-fold` 开不开，翻前都走这里——这个开关只决定翻后搜索怎么估平跟者的范围。

**牌力排序**：Chen 分为主，并列时按 对子 > 同花 > 高牌 > 踢脚 拆开（`chen_order_key`），手牌「在 top p 内」= 排在它前面的组合占比 < p。

**ante 程度** `L` = 本手 ante 总额 ÷ 8BB，封顶 1（`limp_ante_level`）。50/100/ante 100 满 8 人发牌 → L = 1；7 人发牌 → 0.875；无 ante → 0。
范围放宽倍数 = 1 + 1.1 × L（L = 1 时 2.1 倍，依据是 ante blueprint 首入范围约为无 ante 的 2.04–2.18 倍）。

**没人加注（只有平跟）**：

| hero 位置 | 隔离加注 无 ante | 被动入池上限 无 ante | 隔离加注 L=1 | 被动入池上限 L=1 |
|---|---|---|---|---|
| BTN | 30% | 45%（跟着平跟） | 63% | 94.5% |
| CO | 22% | 32%（跟着平跟） | 46.2% | 67.2% |
| HJ | 16% | — | 33.6% | — |
| LJ | 13% | — | 27.3% | — |
| 更早（UTG / UTG+1） | 11% | — | 23.1% | — |
| SB | 12% | 40%（补齐） | 25.2% | 84% |
| BB | 12% | 免费过牌 | 25.2% | 免费过牌 |

- 每多一家平跟，隔离范围 × (1 − 0.2)，最多收到一半；SB 补齐上限每多一家 +5 个百分点（放宽前封顶 55%）。
- 隔离加注额 = (3 + 平跟人数 + 盲位再加 1) BB + L BB，夹进合法区间。例：满 ante、一家平跟、BTN 隔离 → 5BB。
- 不在隔离范围：BB 过牌；在被动上限内跟（CO/BTN 跟着平跟、SB 补齐）；其余弃牌。
- 这一支**不套底池赔率规则**：一家平跟时 SB 的赔率已经 5:1，套了就是任意两张都跟。

**有人加注（hero 隔离后被 3bet，或平跟后有人加注）**：
- 跟注：P 档（JJ+、AK、AQ）；有 ante 时再加 Chen 序前 4.2% × (1 + 1.1L)（满 ante 约 8.9%，补上 TT–88、AJs、ATs、AJo、KQs、KJs、KTs、QJs、QTs、JTs、J9s、T9s）。
- 其余：底池赔率规则命中（底池 > 4 × 跟注额且跟注额 ≤ 30BB）→ 跟注，否则弃牌。
- **这一支从不再加注**：平跟池里被加注后，AA 也只跟。

### 3.4 面对全下启发式（`preflop_allin_response`，`:4429`）[事实]

**什么时候用**：轮到 hero、价格由一个已全下的对手定，并且
① hero 与全下者的有效码深（开局栈取小）< blueprint 码深的 60%（200BB blueprint = 120BB）；或
② 有效码深 > blueprint 码深的 140%（200BB blueprint = 280BB；树里的全下只是 200BB，11-04 加）；或
③ 码深在区间内但走到了平跟 / 失同步 / 占位这几条路。码深在 [60%, 140%] 内、能查 blueprint 的全下照常查 blueprint。
深码全下按表的最后一行（> 40BB：开池 8%、3bet / 4bet 5%）估范围；几百 BB 的 4bet 全下实际范围可能更紧，表没校准过。

**怎么判**：
- 全下者范围宽度（Chen 序 top 比例）按他的开局码深查表，再乘位置系数：

  | 全下者开局码深 | 开池全下 | 3bet / 4bet 全下 |
  |---|---|---|
  | ≤ 10BB | 40% | 25% |
  | ≤ 15BB | 30% | 18% |
  | ≤ 25BB | 20% | 12% |
  | ≤ 40BB | 12% | 8% |
  | > 40BB | 8% | 5% |

  位置系数：BTN / SB 1.2，BB / CO 1.0，HJ 0.85，更早 0.7。已经跟了这个全下的其他对手，范围按全下者的一半算。
- Monte Carlo 1 万次算 hero 的底池份额，份额 ≥ 跟注额 / 跟注后底池 + 2%（每多一个已跟注的人再 + 2%）→ 跟注，否则弃牌。
- 只跟或弃，不再加注。stderr 打一行 `preflop_allin ...` 记下判断用的数字。
- 表里的数是 10-16 和用户商定的常识值，没解过，也没用实盘校准过。

wopo 桌上开局码深常在 60–130BB，低于 120BB 的很常见，所以面对全下大多走这里，而不是 blueprint [推断]。

### 3.5 兜底 `fallback_with_floor`（`:3839`）[事实]

失同步、占位、出牌换算失败等情况的最后一道。**只有面对下注**（不能过牌、能跟）时才可能改成跟注，按顺序判：
1. 翻前 P 档（AA KK QQ JJ AK AQ）→ 跟（`:premium_call`）；
2. 底池赔率：底池 > 4 × 跟注额 且 跟注额 ≤ 30BB → 跟（`:pot_odds_call`）。翻前的底池按真实码深重放算，含 ante；
3. 其余：能过牌就过，否则弃。兜底从不加注。

### 3.6 PM-17：翻前对「3bet 多、被 4bet 就弃」的人放开 4bet（`fourbet_adjust`，`:3457`）[事实]

**wopo 实盘没开**（`--exploit-4bet` 不在参数里）。开了之后：

- 局面：hero 之前全弃、hero 开池，之后恰好一人 3bet（非全下）、其余全弃，轮到 hero，且走的是 blueprint 查表。
- 识别器（`src/training/fourbet_exploit.rs`）按玩家名在进程内攒两项统计，带衰减：3bet 率（先验 Beta(0.5, 9.5)，衰减 0.98）、
  3bet 后被 4bet 的弃牌率（先验均值 clamp(1 − 0.05 / 3bet 率, 0.45, 0.95)、权重 4，衰减 0.90）。
- 两道门都过才放开：P(3bet 率 > 20%) ≥ 0.95，且 P(弃牌率 > 保本弃牌率) ≥ 0.80。保本弃牌率 = 4bet 风险 / (风险 + 底池)。
- 码深：双方开局栈都要 ≥ 1.6 × 4bet 额。
- 放开时：blueprint 分布里弃牌的概率 × 1.0 全部挪给「加注 1 底池」，跟注和原有 4bet 概率不动。`source` 加 `:exploit_4bet`。

### 3.7 平跟池首次决策查 blueprint（`--search-limp-first-blueprint`，默认关）[事实]

需配 `--search-limp-as-fold on`。条件：影子同步、有平跟映射、hero 本手翻前还没行动过、不是面对全下、
平跟不超过 2 家（`limp_first_blueprint_applies`；家数数法同 §3.3：首个加注之前的跟注，SB 补齐也算）。
平跟达到 3 家仍走平跟池启发式。这时影子里平跟者已经记成弃牌，hero 查的就是「平跟者都弃牌后」的 blueprint 节点：

- 前面只有平跟 → 开池节点（弃牌 / 加注；没有跟着平跟），SB 是首入节点（弃牌 / 补齐 / 加注），BB 是面对 SB 补齐的节点。
  BB 面对平跟、其余全弃时影子已结束（失同步），仍走平跟池启发式。
- 平跟后有人加注 → 面对开池的节点（弃牌 / 跟注 / 3bet / 全下）。
- 加注额按含平跟者筹码的真实底池算（同 §3.1）。`source` = `blueprint:limp_first…`，driver 计入 blueprint。
- hero 行动过之后的翻前决策仍走平跟池启发式。翻后估 hero 范围时这一步按 blueprint σ 算（不再算 limp 规则系数，
  `hero_limp_route` 同口径）。

离线回放（11-03，10-31 起全部 wopo 轮里 hero 首次决策、前面有跟注的 375 条请求，open2 9B crop，开关关 / 开对比）：
280 条换成 blueprint。只有平跟的 206 条：加注 71 → 86、跟注（跟着平跟 / 补齐）33 → 23；一家平跟时平均加注额 4.96 → 5.94BB
（blueprint 在 1/3 底池约 4.2–4.9BB 和 1 底池约 11.5–12.5BB 两档之间混合）。平跟后有人加注的 74 条：跟注 12 → 16、
再加注 0 → 11（启发式在这里从不再加注）。

11-03 20/50 实盘（`wopo_ante200_open2_9b_live_20261103_3`，开关首次上线）：`limp_first` 决策 32 条，隔离加注 17 次没有一次让所有人弃牌，
常被 1–3 家跟进；1–2 家平跟时的隔离加注 9 次合计 +94.6BB，3 家以上 7 次合计 −293.4BB（样本小，主要是两三手大底池翻后的结果）。
blueprint 假设平跟者会弃牌，家数越多偏得越远，于是加了「3 家及以上退回启发式」。

11-03/04 复盘（开关开着的 5 轮，已结算的 hero 首次决策、前面只有平跟的手，扣抽水前后差别不大）：

| 平跟家数 | limp_first | 同期同条件的平跟池启发式（全部 12 轮） |
|---|---|---|
| 1 家 | 31 手 −356.8BB（加注 15） | 64 手 +58.2BB（加注 24） |
| 2 家 | 21 手 −118.7BB（加注 11） | 27 手 +35.0BB（加注 7） |
| 3 家及以上 | 7 手 −299.9BB（加注 6） | 18 手 −266.1BB（加注 5） |

blueprint 在平跟池里几乎从不弃牌（BTN 前面 2 家平跟时 9h5h 84% 加注、Ts4s 53% 加注），加注后多人跟进、翻后输掉。
1 家平跟反而亏得最多，「只在 1 家平跟时用」没有数据支持。样本小（每手标准差约 47BB），但方向一致，
**11-04 决定实盘关掉这个开关**（远端模板 `artifacts/wopo_next_common_args.sh` 去掉 `--search-limp-first-blueprint on`；代码默认本来就是关）。

---

## 4. 开关对翻前的影响

| 开关 | 实盘 | 影响翻前动作？ |
|---|---|---|
| `--reshape ante-flop2-3bet --postflop-cap 4` | 开 | 是：决定树的菜单和入池上限（§1.1） |
| `--search` 及各种 `--search-*` | 开 | 否（翻前不搜索）。`--search-limp-as-fold` 只改重放怎么记平跟，翻前动作同样交给平跟池启发式 |
| `--search-limp-first-blueprint` | 关（11-03 晚至 11-04 实盘开过，复盘后关回，§3.7） | 是：平跟池里 hero 首次决策改查 blueprint（§3.7） |
| `--exploit-4bet`（PM-17） | **关** | 是（§3.6） |
| `--preflop-commit-min-visits` | 50000 | 是：hero 这一行累计权重低于它时不全下、不做押上大半筹码的加注（§3.1）；0 = 关 |
| `--preflop-commit-share` | 0.5 | 是：上面「押上大半筹码」的比例（§3.1） |
| `--exploit` / `--exploit-tilt` / `--exploit-limp-seed` | 开 | 否，只影响翻后搜索的对手范围 |
| `--ante` / `--blueprint-ante` / `--stack-bb` | 100 / 100 / 200 | 是：ante 对不上直接兜底（`ante_mismatch`）；ante 程度决定平跟池范围；码深决定全下启发式的 120BB / 280BB 线和深码不全下的 280BB 线 |

---

## 5. 实盘里翻前决策的分布 [实测]

wopo 三轮（`artifacts/wopo_ante200_9b_exploit_live_20261029/`、`..._tilt_live_20261031/`、`..._tilt_live_20261031_b100/`）的
`wopo_actions_log.jsonl`，翻前 advisor 回复共 246 条（含过期的）。10-29 那轮部署的是 `ff4302e`，平跟池的 ante 放宽还没上，
数字只看大致比例：

| 来源 | 条数 | 占比 |
|---|---|---|
| `blueprint`（含 1 条 `:allin_as_call`） | 136 | 55% |
| `limp_heuristic:*` | 102 | 41% |
| 　其中 fold / raise / call / overlimp / check / pot_odds_call / complete | 62 / 18 / 6 / 6 / 5 / 2 / 3 | |
| `fallback:preflop_allin:*` | 4 | 2% |
| `fallback:limp_as_fold_no_limp*` / `fallback:blueprint_placeholder` | 3 / 1 | 2% |

wopo 平跟多，四成翻前决策由手写的平跟池启发式做，这块的质量对整体影响不小。

---

## 6. 已知问题与局限

### 6.1 补盲者过牌后，hero 会被建议用任意两张跟进去（bug，11-01 已修）[事实 + 实测]

`wopo_ante200_9b_exploit_live_20261029`：posts=[7]，动作 `6 fold, 7 check`（7 号补盲后过牌），hero 75o 面对 1BB，
advisor 回 `call`，`source = fallback:limp_as_fold_no_limp:pot_odds_call`。
另一例 `ante-table:hand:000564`（`_b100` 那轮）：4 人桌，3 号补盲后过牌，4 号开池、5 号 3bet，hero AQo 走的也是兜底（`:premium_call`），
而不是平跟池启发式。

原因：重放时补盲者的过牌在影子里当成平跟（`is_post_check`，`:1398`），记了平跟映射；但平跟池启发式数平跟只认 `call`、不认 `check`（`:4099`），
数出 0 家平跟 → 返回 `None` → 落到兜底；兜底的底池赔率规则看到 10.5BB 底池、跟 1BB → 跟注。
§3.3 特意不在没人加注时套底池赔率规则，就是为了避免这种「任意两张都跟」，这里绕过去了。
关掉 `--search-limp-as-fold` 也一样：失同步原因是 `post_check_gap`，同样落到兜底 [推断]。

**已修（11-01）**：补盲改在接入层处理，advisor 去掉了全部补盲逻辑（请求的 `posts` 字段、`SeatMap::live_posts`、
`is_post_check`、种子里的补盲）。补盲者的过牌发成 `call`，advisor 里就是一家平跟，走平跟池启发式
（测试 `poster_check_sent_as_call_goes_limp_heuristic`：CO 72o 面对补盲过牌 → `limp_heuristic:fold`）。
旧 driver 如果还发补盲者的原始 `check`，advisor 重放对不上，落兜底弃牌，也不会再按底池赔率跟。
留下的代价：翻后估补盲者范围时按主动平跟算，比他真实的「免费看牌」范围窄。

### 6.2 入池人数到 5 时，第 5 人的跟注被当成平跟，落到兜底 [事实 + 实测]

`--postflop-cap 4` 让树里第 5 个入池者（含已入池的人面对 squeeze 再跟）没有跟注选项。真实桌上有人这时跟了，
`is_limp_gap`（`:1372`）只看「影子当前节点没有跟注、有弃牌」，把它当成平跟记进映射（注释说影子缺跟注只会出现在没人加注的节点，
没考虑入池上限）。接着轮到 hero：平跟池启发式发现首个加注之前没有跟注 → `None` → 兜底，`source = fallback:limp_as_fold_no_limp`。
代码注释（`:2901`）说这条「不可达」，实盘出现了 3 次（1 次是本条，2 次是 §6.1）。

例：10-29 那轮，`7 raise 450, 0(hero) call, 4 call, 5 call, 6 raise 4100, 7 call`，轮到 hero 99 → 兜底弃牌。
动作本身未必错（面对 41BB 的 squeeze），但走的是兜底而不是任何策略。

另外，hero 自己是第 5 个入池者时，blueprint 只给弃牌或加注——**多人池里 hero 不会冷跟**，这是树的设计，不是 bug。

### 6.3 翻前不看真实码深（全下以外）[事实]

blueprint 是 200BB 对称码深训的。除了面对全下（§3.4）和深码时的 hero 全下（§3.1「深码不全下」），翻前所有决策都按 200BB 查表：hero 只有 60BB 时开池、3bet 范围和 200BB 一样，
只是加注额被夹到 max_raise。对手码深也不进信息集。

### 6.4 对手开池尺寸全部压成 4.5BB [事实]

§3.2：开池只有一档，真实开 10BB、20BB 的人在影子里都是 4.5BB 开池。hero 的跟注 / 3bet 范围按面对 4.5BB 开池查，
赔率差很多时范围会偏宽 [推断]。hero 的 3bet 额倒是按真实底池算的。

### 6.5 两张手写表没有校准 [事实]

平跟池启发式的位置范围（§3.3）和全下启发式的范围宽度（§3.4）都是常识估计；ante 放宽倍数 2.1 来自两份 blueprint 的首入范围比值。
加上平跟池里被加注后从不再加注（AA 只跟），这是四成翻前决策的依据。要改进翻前，这里比 blueprint 本身更值得先看。

---

## 附：代码位置（`tools/openpoker_advisor.rs`，`562cca4`）

| 内容 | 位置 |
|---|---|
| 主决策 | `decide_with` :2764 |
| 重放 / 平跟映射 | `lockstep_replay`，`is_limp_gap` |
| 补盲改写（接入层） | `tools/poker_live/table.py` `HandEngine.advisor_actions` |
| blueprint 分布 / 抽样剪枝 / 出牌换算 | `blueprint_distribution` :3555，`prune_for_sampling` :5256，`action_to_response` :3585 |
| 平跟池启发式 | `limp_heuristic` :4089，`limp_ranges` :3933，`limp_ranges_for` :3949，`limp_call_width` :3979 |
| 全下启发式 | `preflop_allin_spot` :4339，`preflop_allin_response` :4429，`ALLIN_RANGE_TABLE` :4187 |
| 兜底 | `fallback_with_floor` :3839，`limp_hand_tier` :3884 |
| PM-17 | `fourbet_spot` :3410，`fourbet_adjust` :3457，`src/training/fourbet_exploit.rs` |
| 树菜单 | `src/training/nlhe_betting_tree.rs` `ante_flop2_3bet_6max` :300，`filter_actions` :936 |

# 翻后决策说明（代码口径 `c7a9e2c`）

> 文中行号都按 `c7a9e2c`。§8.1 的修复和 §8.3 的 `--exploit-anchored`（2026-11-02）是之后加的，相关文件的行号会往后错开，按函数名查。

本文讲 advisor（`tools/openpoker_advisor.rs`）在翻牌、转牌、河牌怎么决定一个动作：「锚定 / 脱锚」到底指什么、两条路在每条街各做什么、
求解和出牌的共同步骤、画像在哪里起作用。翻前见 `openpoker_preflop_decision.md`（姊妹篇）。范围估计的数学细节见
`openpoker_realtime_range_estimation.md`，影子树映射见 `openpoker_shadow_tree_mapping.md`，本文只讲到能看懂决策为止。

**标注约定**：**[事实]** = 代码逐行确认；**[实测]** = wopo 实盘日志统计；**[推断]** = 读代码 / 日志推出来、没有专门跑测试验证；
**[计算]** = 按代码公式手算的数（本文没有跑 advisor）。

**对应的实盘配置**（wopo，`artifacts/wopo_ante200_9b_tilt_live_20261101/common_args.sh`，部署 `c7a9e2c`）：8 座、每人 ante 1BB、
200BB blueprint `run_8max_ante100_200bb_antefl2_3bet_cap4_b500` 9B 的**翻前 + 翻牌 crop**（`keep_through=Flop`，不含转牌 / 河牌策略），
`--search --search-trigger all-postflop --search-flop-prefer-blueprint on --search-deep-menu --search-prewarm`，
`--search-time-budget-ms 5000`，迭代上限 翻牌 100 万 / 转牌 200 万 / 河牌 300 万，`--search-solve-threads 8`，`--search-max-nodes 1000000`，
`--search-limp-as-fold on --exploit on --exploit-tilt on`。没写在参数里、取默认值的：`--search-min-hero-visits 200`、
`--search-range-uniform-mix 0.25`、`--search-weak-commit-guard on`、`--search-unanchored-prefix-reach on`、`--search-unanchored-cross-street on`、
`--search-force-unanchored off`（启动日志 `driver.log` 的 `search ON:` 一行逐项打印了这些值）。wopo 上 advisor 只给建议，实际出牌的是人。

---

## 0. 结论

1. **「锚定 / 脱锚」和「查 blueprint / 实时搜索」是两件不相干的事** [事实]。锚定 = 本手到现在的真实动作能一步步走进 blueprint 的**树结构**
   （crop 只删了转牌 / 河牌的策略数，树是按参数在内存里完整建的，四条街都在）。转牌、河牌**两条路都一定实时搜索**，
   求解的是**同一种子树**：按真实筹码、从本街起点建（`build_real_auth`），菜单由 `deep_menu_for` 按真实底池和筹码选。
   两条路真正不同的只有一件事：**子树起点的各家范围从哪来**。
2. 「河牌哪来的锚定」：河牌锚定 = 一直到河牌，每个真实动作在树上都有对应节点。它的用处是给范围估计提供一条精确的 blueprint 路径。
   正常情况下锚定河牌**一个 blueprint 策略数都不读**：范围来自转牌子树解的跨街后验，转牌的范围又来自翻牌（§3.3）。
3. 「锚定还不如脱锚的真实子树准」**不成立** [事实 + 实测]。子树两边一样真实；锚定估出的起点范围在亮牌补算里是最准的一类
   （中位排位 16%，§3.5）。让锚定局面也走脱锚（`--search-force-unanchored`）在 11-01 补算里转牌 / 河牌明显变差，所以没开；
   原因不在求解路径（同样的根范围两条路解出来逐位相同），而在画像倾斜后的翻牌范围往后带（§3.5）。
   锚定路径真正的缺口是**不读画像**（§5）；11-02 加了 `--exploit-anchored`（默认关，§8.3）补这个缺口，还没有用亮牌验证。
4. 一手牌翻后锚定还是脱锚，**基本在翻前就定了** [实测，§7]：124 手有翻后决策的牌里，63 手翻牌就脱锚、58 手全程锚定、只有 3 手先锚定后脱锚，
   没有一手能从脱锚回到锚定。脱锚的 66 手里约 55 手翻前有平跟（平跟池在树里没有节点）。
5. 翻牌「优先 blueprint」在 wopo 上很少起作用 [实测]：81 次翻牌锚定决策只有 24 次过闸直接查 blueprint。其余 57 次去搜索，其中约 51 次是码深闸不过
   （wopo 筹码常不在 120–280BB），约 6 次是尺寸闸不过（本文例子 `000621` 就是一次）。
6. 整理时新发现一个 bug（§8.1，已修）：翻牌街内有对手弃牌后，hero 同一街的第二次脱锚决策会换一份范围重新求解，丢掉该对手的画像倾斜、多花一次 5 秒，
   实盘 2 次，两次都因此访问数不足、放弃搜索。

---

## 1. 两个独立维度

### 1.1 锚定是什么（`lockstep_replay`，`tools/openpoker_advisor.rs:1419`）[事实]

每次决策 advisor 都从头重放本手动作，同时推两份状态：

- **200BB 重放态**（代码里叫 `real`）：真实动作按真实金额 apply，但**每人筹码按 blueprint 的 200BB 对称码深**，不是真实筹码；
- **影子**（`abs`）：blueprint 树上的节点。每个真实下注 / 加注按影子自己的底池折成池比例，再用 pseudo-harmonic 随机取整到相邻档
  （`advance_shadow_by_applied`，`src/training/blueprint_advisor.rs:311`）。

两份状态每一步都走得通、当前行动者一致、最后停在 hero 决策点且街与公共牌张数对得上 → **锚定**（lockstep Ok），得到影子节点 `node_id`；
任何一步走不通 → **脱锚**（lockstep Err），记下断点前最后一个对上的影子节点 `synced_node`。

### 1.2 第二个维度：查 blueprint 还是实时搜索

| 街 | 锚定 | 脱锚 |
|---|---|---|
| 翻牌 | 两道闸都过 → 查 blueprint；否则实时搜索 | 实时搜索 |
| 转牌 | 实时搜索 | 实时搜索 |
| 河牌 | 实时搜索 | 实时搜索 |

（crop 下转牌 / 河牌没有策略数，启动时 `validate_crop_runtime`（`:5575`）强制 `all-postflop` 触发 + 跨街后验开，保证被裁的街都搜索。）

### 1.3 锚定在各街到底用了 blueprint 的什么 [事实]

| 用途 | 翻牌（查表） | 翻牌（搜索） | 转牌 | 河牌 |
|---|---|---|---|---|
| 判定锚定（树结构） | 是 | 是 | 是 | 是 |
| 直接出牌用的策略 | 翻牌 σ | 否 | 否 | 否 |
| 起点范围 | — | 翻前 σ 沿影子路径（`estimate_range`） | 翻牌搜索过：翻牌子树后验，blueprint 只在翻牌根范围里间接起作用；翻牌查表：翻前 + 翻牌 σ 沿影子路径 | 转牌子树后验，不读 σ |
| 访问数不足时的退路 | — | 同一影子节点的翻牌 σ（`blueprint:low_hero_visits`） | 无 | 无 |
| 子树、菜单、出牌金额 | 200BB 重放态的底池 | 真实筹码 | 真实筹码 | 真实筹码 |

另外两处小用途：当前街影子路径上已有动作时，子树内导航改用真实动作重放（`navigate_subtree_by_real_actions`，`src/training/subgame.rs:3392`）；
`resp.info_set` 记的是影子节点的信息集。

河牌「不读 σ」有硬保证：锚定范围估计前先过 `check_blueprint_coverage`（`subgame.rs:1256`），要查被裁的转牌 σ 就报 `blueprint_cropped`，
`search_with_crop_backfill`（`:1078`）补解转牌子树后重试，补不成标 `search_giveup:crop_no_posterior:*`。实盘没出现过这个标签 [实测]。

### 1.4 两条路的范围有什么不同（`subgame_search_cached_inner` `subgame.rs:2655` vs `subgame_search_unanchored_cached_inner` `:3622`）[事实]

| | 锚定 | 脱锚 |
|---|---|---|
| 优先级 1 | 跨街后验 `cross_street_posterior_range` | 同左 |
| 优先级 2 | `estimate_range`：本街之前的**全部**影子路径决策 × blueprint σ（不跳过全下） | 前缀 reach：断点前的影子路径决策（`unanchored_prefix_decisions`，跳过全下标签），再乘 hero 平跟系数、对手倾斜系数 |
| 优先级 3 | 均匀 | 均匀 |
| 对手混 25% 均匀 | 是（hero 不混） | 是（hero 不混） |
| 画像 | 默认**不读**；开 `--exploit-anchored` 时 blueprint estimate 里对手翻前步改乘倾斜系数（不做宽度修正），跨街后验命中时不加 | 倾斜（`exploit_tilt`）+ 宽度修正（`apply_exploit_width_prior`），跨街后验命中时都不再加 |
| 建树前规模闸 | 无（建完再查节点数） | 在手 ≥ 7 人时先数 |
| 访问数不足 | 翻牌可退回 blueprint（码深闸过时） | 只能兜底（§4.6：能过就过，面对下注多半弃） |

跨街后验 = 上一街子树解的根范围 × 上一街每个真实动作在**那次解**里的概率（`subgame.rs:1952`）。它只认「同一手、紧前一街」，
不管上一街是锚定还是脱锚（`prev.hand_seed` + 公共牌前缀 + 导航三道检查）。

---

## 2. 什么时候脱锚

### 2.1 失同步原因（`lockstep_replay` :1419、`lockstep_step` :1266）[事实]

| `reason` | 含义 | 牌局例子 |
|---|---|---|
| `structural_gap` | 影子节点上没有这个动作 | ① 关着 `--search-limp-as-fold` 时任何平跟（树里删了开池平跟）；② 对手能免费过牌却弃牌：`_b100` 那轮 `000549`，转牌 1 号面对无人下注直接 `fold`，影子节点没有弃牌 [推断：日志里 hero 的转牌决策随后报「真栈导航：tag Fold 不在子树节点 0」，说明真实引擎接受了这个弃牌、影子不接受]；③ 树里没有的被动跟注 |
| `replay_illegal` | 动作在 200BB 重放态里不合法 | 对手下注额超过 200BB：10-29 那轮 Q♣T♣，4 号（开局 1131BB）转牌加到 26000（260BB）→ 200BB 重放里他没那么多筹码 |
| `replay_seat_mismatch` | 轮到的人和历史里的行动者对不上 | 短码跟注全下：10-28 那轮 A♥8♣，1 号开局 13.5BB，翻牌跟注后已全下；200BB 重放里他还有筹码、还在行动，河牌第一个动作（6 号过牌）时重放态轮到的是 1 号 |
| `lockstep_drift` | 一步之后影子和重放态的当前行动者不同 | 平跟当弃牌后其余人全弃：影子里这手已经结束，真实局还轮到大盲（代码注释举的例子） |
| `limp_as_fold:limper_continued` | 平跟者（影子里记成弃牌）之后又跟注或加注 | `000651`：CO 平跟、hero 隔离加注、平跟者跟注（§6.2） |
| `limp_as_fold:limper_in_pot` | 平跟者没被加注、直接带进翻牌 | SB 补齐 + BB 过牌；多家平跟后 BB 过牌 |
| `limp_as_fold:*` | 其余原因，有「影子已弃、真实在手」的平跟者时加这个前缀 | — |
| `phantom_*` | 短桌开局替幻影座弃牌时出错 | 座位映射有误才会出现，测试守着，实盘没见过 [推断] |
| `actor_not_dealt` / `bad_hist_action` | 动作的座位没发牌 / 动作字符串认不出、加注缺金额 | 脏请求 |
| `not_my_turn` | 重放完不是 hero 行动 | 请求和动作历史不一致 |
| `street_board_mismatch` | 重放完所在街和公共牌张数不符 | 请求漏了动作或公共牌 [推断] |
| `empty_legal` | 影子节点没有合法动作（终局） | — |

**平跟当弃牌**（`--search-limp-as-fold on`，`is_limp_gap` :1338）：翻前非全下的跟注、影子节点只有弃牌没有跟注时，影子记弃牌、真实局照常跟注，
平跟者成了「影子已弃、真实在手」的人。之后他**面对加注弃牌** → 两边重新一致、整手回到锚定（6 月数据约 90% 是这种）；
**又跟注 / 加注** → `limper_continued`；**带着进了翻牌** → `limper_in_pot`。这是唯一能「先失同步后回到锚定」的情况，而且只发生在翻前。

### 2.2 脱锚之后一直脱锚 [事实 + 实测]

每次决策都从头重放，历史只会变长、前缀不变，所以第一次失败的那一步之后每次都在同一步失败、`synced_node` 不变。
翻后没有「重新对上」的机制。实盘 124 手里没有一手从脱锚回到锚定（§7）。

`synced_node` 决定脱锚范围用到哪：断点那一步和之后的翻前动作都按「无信息」（因子 1）处理，倾斜（§5）会补上其中有名字对手的翻前步。

---

## 3. 按街、按路径的决策流程

### 3.1 总流程（`decide_with` :2700，翻后部分）[事实]

```
0. 入参校验 / 座位映射；2 人桌翻后直接兜底（short_hu_postflop）
1. lockstep_replay（:2803）
   ├─ Err（脱锚）+ 开了搜索 + 翻后 ────────────────→ decide_search_unanchored（§3.4）
   └─ Ok（锚定）
2. 翻牌且开 --search-flop-prefer-blueprint：码深闸 + 尺寸闸（:2931）
   ├─ 都过 → 查 blueprint 翻牌 σ
   │    └─ 信息集是均匀占位 → 改走锚定搜索（source = search:bp_placeholder）
   └─ 不过 / 转牌 / 河牌 → 锚定搜索（§3.2 / §3.3）
        ├─ 解出来 → 分布
        ├─ 翻牌 + 访问数不足 + 码深闸过 → 同一影子节点的 blueprint σ（blueprint:low_hero_visits）
        └─ 其余失败 → 兜底 fallback_with_floor（search_giveup:unsolved:*，§4.6）
3. 弱牌护栏 weak_commit_guard（§4.4）
4. 剪枝 prune_for_sampling（§4.5）→ 按确定性种子抽一个
5. 换算成桌面动作（§4.7）
```

### 3.2 翻牌·锚定

**两道闸**（只看翻牌、只管锚定；`--search-flop-prefer-blueprint on`）：

- **码深闸** `flop_blueprint_stack_gate`（:4517）：本手到现在**没人全下**，且 hero 与**每个**未弃牌对手的**开局**有效码深（两人开局栈取小）
  都在 blueprint 码深的 60%–140% 内。200BB blueprint = **120–280BB**。看的是开局栈，不是当前剩余栈。
- **尺寸闸** `flop_blueprint_size_gate`（:4554）：本街每次下注 / 加注（含 hero 自己的），「真实池比例 ÷ 影子里映射到的池比例」都在 0.75–1.333 内。
  真实池比例按 200BB 重放态的底池算，映射池比例按**影子自己的底池**算；本街没人下注恒过。

都过 → 查 blueprint（`source = blueprint`），不搜索，街起点的预热也跳过（`prewarm:skip:flop_prefer_blueprint`）。
任一不过 → 锚定搜索。注意预热时只看得到街起点之前的动作，街内后来出现的全下或偏离菜单的下注要在决策时现解（例子 §6.1 就是这样）。

**锚定搜索**：子树根 = 翻牌起点（真实筹码），范围 = 翻前影子路径 × blueprint 翻前 σ（翻牌没有跨街后验）。

**访问数不足**（`min_hero_visits = 200`，hero 真实底牌所在信息集被遍历的次数）：码深闸过 → 退回影子节点的 blueprint σ（:3173），
`source = blueprint:low_hero_visits`。只看码深闸、**不看尺寸闸**：尺寸闸不过时退回去的 blueprint 是在应对另一个尺寸 [事实]。
码深闸不过 → `search_giveup:unsolved:low_hero_visits:*`。

### 3.3 转牌 / 河牌·锚定

一定搜索。子树根 = 本街起点（真实筹码）。起点范围按优先级：

1. **跨街后验**：上一街有子树解在缓存里（主 slot 或 `prev` slot），且同手、紧前一街、上一街真实动作线能在那棵子树上走通 → 用它。
2. **blueprint estimate**：本街之前的影子路径 × blueprint σ。crop 下要查转牌 σ 时报 `blueprint_cropped` → 补解上一街再重试（§1.3）。
3. 均匀。

实际会走到哪一条 [事实 + 实测，range 日志]：

| 情况 | 转牌范围 | 河牌范围 |
|---|---|---|
| 翻牌搜索过（锚定或脱锚都行） | 翻牌子树后验 | 转牌子树后验 |
| 翻牌查了 blueprint（没有翻牌子树） | blueprint estimate：翻前 σ + 翻牌 σ（影子映射后的尺寸） | 转牌子树后验 |

实盘 range 日志里锚定河牌全是 `cross_street_posterior`，锚定转牌两种都有。

### 3.4 翻牌 / 转牌 / 河牌·脱锚（`decide_search_unanchored` :4755）[事实]

1. 真实筹码重放（`build_real_auth`）。失败 → `search_giveup:unanchored_build:*`。
2. 真实状态上判触发面（`all-postflop` 下翻后恒触发）；不触发或街与公共牌不符 → `fallback:<失同步原因>`。
3. **前缀 reach 的输入**（`unanchored_range_inputs` :2595，预热和决策同一函数，保证缓存 key 相同）：
   - 前缀 = `synced_node` 之前的影子路径决策；去掉：hero 被平跟映射的那一步、hero 由平跟池启发式决定的翻前步（改由系数表达）、
     `limp_raisers`（平跟后又在翻前加注的对手）的映射弃牌步（`unanchored_prefix_decisions` :1555）；
   - 估范围时跳过全下标签（200BB 全下的 σ 套到短码全下上是错的）；
   - **hero 平跟系数** `hero_limp_coef`（:1671）：hero 每个由平跟池启发式决定的翻前步，对 1326 个组合各跑一遍同一个启发式，
     和实际动作类型一致记 1、否则 0，相乘。hero 真实底牌算出 0（人没照建议打）→ 该步不用系数、退回旧做法，`source` 追加 `:hero_coef_mismatch`；
   - **对手倾斜** `exploit_tilt`（:2430，`--exploit-tilt on`）：每个有名字、没弃牌的对手，翻前每一步「参考策略 × 按画像频率倾斜」逐组合连乘，
     替换他在前缀里的全部翻前步；主动全下那一步用全下宽度表（`shove_coef`）。细节见 `docs/temp/preflop_range_offanchor_research_2026_10_29.md` §4、§7。
4. 范围优先级：跨街后验 > 前缀 reach（再乘系数）> 均匀。对手混 25% 均匀，hero 不混。
5. **宽度修正** `apply_exploit_width_prior`（`subgame.rs:1722`）：有画像、没被倾斜接管的对手，范围向「按 VPIP / PFR 取 Chen 序前若干」凸混合
   （α = 0.5 × 样本权重）；3bet 及以上、盲位过牌 / 补齐 / 防守的线不修正（`pick_shape` :4677）。**跨街后验命中时不做**（`subgame.rs:3834`）。
6. 子树：根 = 本街起点，菜单 `deep_menu_for`，宽度改道关（真实 5 人以上底池也能建）。在手 ≥ 7 人时先数节点（`SUBTREE_PRECOUNT_MIN_ACTIVE`，
   `subgame.rs:1336`），超上限直接拒绝，不先建一棵上千万节点的树。
7. 本街真实动作在子树上重放导航（`navigate_subtree_by_real_actions`），读 hero 真实底牌的平均策略。
8. 解不出来 → 兜底 `fallback_with_floor`（§4.6：能过就过，面对下注只在接近坚果 / 赔率好 / 强听牌时跟，否则弃），
   `source = search_giveup:unanchored:*`。**没有 blueprint 退路**（没有可用的影子节点）。
9. 解出来 → 护栏 → 剪枝 → 抽样 → 换算，`source = search:unanchored[:weak_commit_guard][:hero_coef_mismatch]`。

### 3.5 `--search-force-unanchored`（:191 / :2997）[事实 + 实测]

开了以后，锚定且命中触发面的搜索决策改走脱锚路径（`synced_node` = 当前影子节点，前缀就是完整历史）。目的是让画像覆盖所有搜索决策。
不改「哪些决策搜索」：翻牌过闸的仍查 blueprint。代价：翻牌访问数不足时没有 blueprint 退路，直接落兜底（§4.6）。
锚定本来也只在码深闸过时才退 blueprint（实盘开着翻牌优先 blueprint，这时去搜索说明尺寸闸没过，退回的是应对另一个尺寸的策略），
所以两者只在「码深合适、尺寸映射失败、访问数又不够」的翻牌上有区别：一边是尺寸对不上的 blueprint，一边是很紧的兜底。实盘 310 条里这种只有 3 次。

**生产没开**。11-01 亮牌补算（`docs/temp/preflop_range_offanchor_research_2026_10_29.md` §7.6，539 个样本，对数比越高越准）：
原来走锚定的 240 个样本里，全部走脱锚后翻牌更准（+0.454 vs +0.404，画像在翻牌起点起作用），但转牌 / 河牌明显变差
（+0.212 / +0.344 vs 锚定 +0.360 / +0.417），总体变差。锚定路径本身是最准的一类（blueprint estimate 中位排位 16%、67% 落在前 25%）。

**变差的原因不是脱锚路径解得差** [事实 + 推断]。根范围相同时，锚定和脱锚解的是同一个子博弈、策略逐位相同
（测试 `prewarm_anchored_exploit_tilt_hits`，2026-11-02 补测）；转牌 / 河牌的范围两边都是上一街子树的跨街后验，算法也相同。
所以两组的差别只来自翻牌根范围的输入：对手翻前步按画像倾斜（主要的）、没被接管的有名字对手多一次宽度修正、翻前全下那一步跳过。
转牌 / 河牌变差应理解为：**画像倾斜后的翻牌范围往后带，后验反而变差** [推断]——倾斜让翻牌起点更准，但 solver 假设对手拿着
这份范围翻后按均衡打，推出来的后验与真人翻后的实际打法对不上（与调研 §7.6「翻前被动、翻后发力」那类估错同方向）。
另外那组对照同时开了自适应平跟种子，不是只差这一个开关；锚定样本里很少有平跟步，影响应不大。

含义：
- `--exploit-anchored`（§8.3）给锚定路径的根范围和 force 在这里给的基本一样，**很可能有同样的「翻牌更准、转牌 / 河牌更差」**，
  它并不能「保住后验」。它和 force 的区别只剩：翻牌访问数不足时还能退 blueprint、没被接管的有名字对手不做宽度修正、没名字对手的翻前全下按 200BB 全下估。
- 所以真正要回答的是「锚定局面用不用画像」，不是走哪条路。实测上目前是不用更好（转牌 / 河牌占搜索决策一半以上，§7）。
  force 继续不开；`--exploit-anchored` 默认关，等样本够了用亮牌补算比开 / 关，重点看转牌 / 河牌。

---

## 4. 求解与出牌的共同部分

### 4.1 子树和菜单 [事实]

- 根 = 本街起点（`ResolveRoot::RoundStart`），真实筹码、真实底池、本街开始时仍在手的人；锚定和脱锚相同。
- 菜单 `deep_menu_for`（`src/training/nlhe_betting_tree.rs:445`）：看根上**在手人数**和「第二大在手剩余栈 ÷ 底池」：
  在手 ≤ 3 人且该比值 ≤ 40，或在手 4 人且 ≤ 20 → **本街** {0.5, 1} 池两档、之后的街 {1} 池一档（`deep_wide_then_single_pot`）；
  否则所有街只有 {1} 池。每档之外都有过牌 / 跟注 / 弃牌 / 全下。日志 `menu` 字段 = 本街菜单（`0.5+1pot` / `1pot`）。
- 每条街各解一棵自己的子树，所以河牌子树的河牌也是两档。

### 4.2 时间、迭代、预热、缓存 [事实]

- 每次求解墙钟上限 5 秒，迭代上限翻牌 100 万 / 转牌 200 万 / 河牌 300 万，到哪个先停哪个；8 线程；只让根上仍在手的人轮流当遍历者（live traversers）。
  节点上限 100 万，超了 `subtree 节点数 N 越界`。
- **预热**（`prewarm_inner` :788）：driver 在每条街起点、hero 行动前发预热请求，advisor 按同样的锚定 / 脱锚分类把本街子树先解进缓存。
  翻牌锚定且两道闸都过时跳过。
- **within-round 缓存**（`SubgameSolveCache`，`subgame.rs:2146`）：key 覆盖求解的全部输入（根状态、范围、菜单、种子……）。
  同一街第二次决策 key 相同 → 不重解，只在同一份解上重新导航，保证一条街只有一份解。换街时旧解挪到 `prev` slot 供跨街后验用。
  范围有任何不同 key 就不同、会重解（§8.1 的 bug 就是这样触发的，已修）。

### 4.3 读数 [事实]

导航到 hero 的决策节点，读 hero **真实底牌所在桶**的平均策略（`read_current_strategy`，`subgame.rs:3209`）。
访问次数 < 200 → 拒读（锚定翻牌可退 blueprint，其余兜底，§4.6）；从没被访问 / 平均策略无定义也拒读。

### 4.4 弱牌护栏 `weak_commit_guard`（:5318）[事实]

只管翻后，锚定、脱锚、翻牌 blueprint 三条路都过它。

- **弱牌**：当前公共牌上的坚果度（hero 打赢或打平的对手两张组合占比，对手按**全部剩余组合**算，不是按对手范围）< `WEAK_HAND_MAX_NUTTINESS = 0.75`，
  且没有强听牌（下一张成顺子或更好的张数 ≥ 8；河牌不看听牌）。中对约 0.80、顶对 0.85 以上不算弱。
- **押上全部筹码的动作**：全下，或这次新投入 ≥ `WEAK_COMMIT_STACK_SHARE = 0.5` × 行动前剩余筹码（按出牌基准态算：搜索用真实筹码，翻牌 blueprint 用 200BB 重放态）。
- 弱牌时把这些动作的概率按比例挪给过牌 / 跟注 / 弃牌；面对下注而菜单里没有跟注档（跟注被折进了全下槽）→ 整份给跟注。
- `source` 加 `:weak_commit_guard`。**只要挪了概率就加**，哪怕只有 0.01%：实盘 125 次带这个标签，只有 9 次被挪走的全下概率 ≥ 5% [实测]——
  其余即使不挪也会被下一步的剪枝去掉。

### 4.5 抽样前剪枝 `prune_for_sampling`（:5185）[事实]

在护栏之后、抽样之前：

- **弃牌** 概率 < 5% → 去掉；
- **全下** 概率 < 5% → 去掉；翻后 SPR ≥ 6 时门槛改为 **20%**（`postflop_spr` :5160：hero 剩余栈与在手对手最大剩余栈取小，÷ 跟注后底池；
  基准态同护栏）；
- 下注 / 加注 / 跟注 / 过牌不论多小都保留；去掉后按比例放大。日志 `probs` 记的是**剪枝和护栏之前**的分布。
- 种子 = hash(底牌, 公共牌, 动作序列, 基础种子)，同一请求永远抽出同一个动作。

### 4.6 解不出来：`search_giveup` 与兜底 `fallback_with_floor`（:3776）[事实]

| `source` 前缀 | 来源 |
|---|---|
| `search_giveup:build:*` / `search_giveup:unanchored_build:*` | 真实筹码重放失败 |
| `search_giveup:unsolved:*` | 锚定搜索失败（节点超限、访问数不足且不能退 blueprint、没访问到、hero 自身 reach 为 0 等） |
| `search_giveup:unanchored:*` | 脱锚搜索失败（同上，另有 `subtree_precount:*`、真栈导航失败） |
| `search_giveup:crop_no_posterior:*` | crop 下补解上一街也拿不到范围 |
| `fallback:*` | 脱锚但不在触发面、出牌换算失败（`outgoing_failed`）等 |

**兜底怎么打**（`fallback_with_floor`）：上表任何一种失败都落到这里。它不看任何策略、不看对手范围，只看 hero 的牌、公共牌和赔率。

1. **能免费过牌 → 过牌。** 不论牌多强都过牌，坚果也不下注。
2. **面对下注**，按顺序判下面三条，命中任意一条就**跟注**（`source` 加后缀）：
   - ① **接近坚果**：坚果度 > 0.93 → `:nut_call`。坚果度 = 在当前公共牌上，hero 打赢或打平的对手两张组合占比，对手按**所有剩余组合**算，
     不是按他的范围算（顶对好踢脚约 0.85–0.9，通常还不够；两对、三条以上一般够）；
   - ② **赔率好、钱不多**：底池 > 4 × 跟注额（只需约 20% 胜率）且跟注额 ≤ 30BB → `:pot_odds_call`。底池按 hero 跟注后**能赢到的部分**算，
     对手超出 hero 跟注额的余注不算。这一条不看牌力，任意两张都跟；
   - ③ **范围胜率够赔率**（2026-11-02 加）：搜索跑过、只是读数被拒（hero 访问数不足 / 信息集没访问到）时，本街的解还在缓存里。
     取它的根范围，沿本街真实动作线乘上对手每步动作的概率（`current_street_ranges`，同跨街后验的做法），得到对手**此刻**的范围；
     按范围抽对手底牌、发完剩余公共牌，蒙特卡洛 2 万局算 hero 胜率（`range_equity`，种子固定）。
     胜率 × 0.85^在手对手数 ≥ 需要的胜率 `跟注额 ÷ (底池 + 跟注额)` → `:range_equity_call`。胜率同时含成牌和听牌。
     0.85 是因为模型里的对手范围通常比真人弱（翻前范围估宽、假设按均衡打），多人时每人一次、误差叠加。
     回复里另记 `range_equity` / `range_equity_opps`。有范围胜率时它**替代**下面的听牌规则；
   - ③′ **强听牌够赔率**（拿不到范围时，如建树失败、节点超限）：翻牌 / 转牌，听顺子或更好的张数 ≥ 8（同花听 9 张、两头顺 8 张，
     卡顺不算），命中率 × 0.85 ≥ 需要的胜率 → `:draw_call`。命中率翻牌按两张牌、转牌按一张牌算；河牌没有听牌。没有注额上限，大注会被赔率挡住。
     它只数听牌张数、不算成牌的胜率：000754 转牌顶对 + 同花听面对全下（需要 33%），只按同花 9 张算 17% 而弃牌，范围胜率是 70%。
3. 三条都不中 → **弃牌**。

兜底**从不下注、从不加注**，也不回落 blueprint（例外只有锚定翻牌访问数不足、码深闸又过的那一种，§3.2）。打牌上它是一套**很紧、很被动**的打法：
可以过牌时放弃所有价值和诈唬；面对下注时除了上面三种情况全弃，中对、顶对弱踢脚面对大注都会弃，比任何下注尺寸下的最小防守频率都紧得多。
设计意图是「解不出来时别乱打、不漏筹码」，代价是被下注时放弃太多。

实盘 [实测，§7 同一批 310 条]：兜底 34 次——能过牌 9 次全部过牌；面对下注 25 次，跟注 9 次（`nut_call` / `pot_odds_call` / `draw_call` 各 3），弃牌 16 次。

范围胜率规则的离线重放 [实测，2026-11-02]：10-31 起各轮搜索放弃且面对下注的 16 次，本地按翻前 crop 重解（转牌 / 河牌的范围与实盘不完全相同）。
2 次由弃牌改跟注：000611（顶对 + 两头顺，单挑，需要 29%，胜率 65%）、000754（顶对 + 同花听，单挑，需要 33%，胜率 70%）；
其余 14 次结论不变（胜率不够，或拿不到范围）。只按单挑折扣时 000730（底对面对一注一加，两个对手，胜率 41%）也会跟，
按人数乘折扣后弃——这是多人时折扣乘人数的原因。

### 4.7 尺寸换算成真实下注额（`outgoing_action` `blueprint_advisor.rs:244`，`action_to_response` :3521）[事实]

- 搜索路径：选中的「x 池」在**真实筹码重放态**上、用**和子树相同的菜单**重算出 `to`；全下 = hero 真实全部筹码。
- 翻牌 blueprint 路径：在 200BB 重放态上用 blueprint 菜单算 `to`（底池是真实的，筹码是 200BB 的）。
- 然后 ÷ 筹码倍数四舍五入，夹进桌面给的 [min_raise, max_raise]；全下直接给 `max_raise`。加注不合法 → 改跟注 / 过牌（`:raise_as_call` / `:raise_as_check`），
  选中弃牌但能过牌 → 过牌（`:fold_as_check`）。driver 再做一次合法性检查。

---

## 5. 画像在翻后哪里起作用

**画像是什么**：进程内 `Profiler`，每手结束 driver 发一次 observe 累积，按玩家名记翻前各局面的频率和 VPIP / PFR，按 n/(n+k) 向人群均值收缩
（`shrink_k = 50`、`cell_shrink_k = 30`、`strength_alpha = 0.5`，见 `driver.log` 的 `exploit ON` 一行）。一手之内不变。

**实盘配置下只在脱锚路径** [事实 + 实测]：实盘没开 `--exploit-anchored`，锚定搜索不读画像。实盘 range 日志里所有 `anchored` 条目的对手座 `coef` / `width` 都是 false。
开了 `--exploit-anchored` 后锚定路径也用倾斜，见 §8.3。

**第一次进入范围** = 本手**第一次脱锚求解的那条街的根范围**：

- 翻前就脱锚（平跟池等，绝大多数）→ 翻牌起点：对手翻前步被倾斜系数替换（`exploit_tilt`），没被倾斜接管的有画像对手再做宽度修正；
- 翻牌查 blueprint、转牌才脱锚（如 `000549`）→ 翻牌没有子树，跨街后验不命中 → 转牌起点用前缀 reach + 倾斜，画像在转牌进入；
- 翻牌已经锚定**搜索**过、转牌 / 河牌才脱锚 → 跨街后验命中（锚定的翻牌解），前缀和画像都不算 → **这手全程没有画像**。

**之后怎么带下去**：转牌、河牌用跨街后验，起点是上一街子树解的根范围（已经乘过倾斜），再乘上一街真实动作的概率。画像不重复加，
避免每街都把翻后动作信息冲淡一半（`subgame.rs:3834` 注释）。代价是上一街没用画像时下一街也不用。

**宽度修正实际很少起作用**：开倾斜后，被倾斜接管的座跳过宽度修正（`exploit_snapshot` :4710）。实盘 range 日志里宽度修正只出现 2 次，都是 §8.1 的 bug 造成的（修后不会再这样出现）。

**暖启动**（2026-11-02 起，wopo driver 默认开）：driver 启动后、第一手之前，把历史 `wopo_hh.jsonl` 里每手按实盘同一格式当 observe
发给 advisor（`tools/wopo_play.py` `--warm-hh`，缺省读 run 目录上一级下所有 `*/wopo_hh.jsonl`，`off` 关闭），进程内画像不再空表起步。
此前实盘每个进程空表起步。advisor 自己的 `--warm-profile-hh` 读的是 OpenPoker hh（ante 写死 0），不能用于 wopo。

---

## 6. 两个实盘例子

### 6.1 锚定：`ante-table:hand:000621`（`artifacts/wopo_ante200_9b_tilt_live_20261101/`）

**牌局**（hh / play.log）：7 人发牌（7 号空座）、10/20、ante 20。hero 0 号 BB，开局 3388（169BB），A♥9♦。1 号弃，2 号（真实 UTG+1，开局 11203 = 560BB）
加注到 150（7.5BB），其余弃，hero 跟。翻牌 K♠A♣4♠（底池 450），hero 过牌，2 号下注 300，hero 全下 3218，2 号跟，亮 4♥4♦（暗三），hero 输 3388。

**翻前**：7 人桌，影子先替幻影 UTG 弃牌，2 号进的是 8 人树的树座 5（range 日志 `tree_seat: 5`）。影子里每人都交 ante，开池前底池 9.5BB，
1/3 池开池 = 4.5BB，所以 7.5BB 开池在影子里是 **4.5BB 开池**；hero 查的是「BB 面对 4.5BB 开池」：call 0.79 / raise1pot 0.21，抽到跟注 [事实]。
到翻牌，真实底池 22.5BB，影子底池只有 17.5BB（8 份 ante + 4.5 + 4.5 + 0.5）[计算]。

**翻牌 hero 第一次决策**（过牌）：锚定。码深闸：hero 与 2 号开局有效码深 169BB，在 120–280BB 内，没人全下 → 过；本街还没人下注 → 尺寸闸过。
查 blueprint：check 0.98，`source = blueprint`。街起点预热因此跳过，range 日志里这手没有翻牌预热条目 [事实 + 实测]。

**翻牌 hero 第二次决策**（面对 300）：

1. **尺寸闸不过** [推断，依据如下]。300 进 450，真实池比例 0.667；同样 15BB 在影子里是 15 / 17.5 = 0.857 池，落在 0.5 和 1 两档之间，
   pseudo-harmonic 取到 1 池的概率约 77% [计算]。映射到 1 池 → 比值 0.667 < 0.75 → 不过；映射到 0.5 池 → 比值 1.333，刚好在上限内（666 ≤ 666.5）→ 过。
   码深闸已过、`source` 是 `search` 不是 `search:bp_placeholder`，所以只能是映射到了 1 池、尺寸闸不过。开池被压成 4.5BB → 影子底池偏小 →
   同一个注在影子里显得更大，误差在这里叠了两层。
2. **锚定搜索**，没有预热 → 现解，`decide_ms = 5008`，`solve_updates = 382592`。子树根 = 翻牌起点（真实筹码：hero 剩 3218、2 号剩 10733，底池 450）；
   在手 2 人、第二大剩余栈 ÷ 底池 = 7.2 ≤ 40 → 菜单 {0.5, 1}（`menu: 0.5+1pot`）。
3. **范围**：range 日志 `path: anchored, range_src: blueprint_estimate`。2 号 = 树座 5 的开池 σ（再混 25% 均匀），有效宽度约 29% 的组合 [计算，按 range 日志反推]；
   4♥4♦ 等三个 44 组合权重 2417（满权重 2418，在开池范围里）。hero = BB 面对 4.5BB 开池的跟注 σ。2 号的画像没有用（`coef / width` 都是 false）。
4. **读数**：hero 访问 4102 次 ≥ 200。分布 call 0.739 / raise0.5pot 0.120 / raise1pot 0.070 / allin 0.070。
5. **护栏没拦**：A9 在 K A 4 上是顶对，坚果度 0.952 [计算] ≥ 0.75，不算弱牌。
6. **剪枝没拦**：SPR = 3218 ÷（750 + 300）= 3.1 < 6 → 全下门槛 5%，0.070 保留；分布里没有弃牌 [计算]。
7. 抽中 allin → 全下 = `max_raise` 3218。

**这手说明什么**：每一步都按设计走了，全下是混合策略里 7% 的那一支。值得注意的是三处输入误差：对手的 7.5BB 开池按 4.5BB 开池估范围、
翻牌下注尺寸在影子里被放大、对手画像没参与。它们不一定改变这次结果（对手范围里 44 本来就在），但都是锚定路径的系统性误差。

### 6.2 脱锚（平跟池）：`ante-table:hand:000651`（同目录）

**牌局**：8 人发牌，hero 0 号 BTN，开局 3765（188BB），A♦J♦。3–6 号弃，7 号（CO，开局 5320）平跟 20；hero 隔离加注（建议 100，人加到 140）；
SB 弃，2 号 BB（开局 5000）跟，7 号跟。翻牌 A♠K♦9♦（底池 590）三家过牌；转牌 J♠ 两家过牌、hero 下注 590、BB 跟、CO 弃；
河牌 5♣ BB 过牌，hero 建议全下 3015、人下注 1800，BB 弃。

**翻前**：7 号的平跟 → `is_limp_gap` → 影子记弃牌。hero 的决策：锚定 + 有平跟映射 + 翻前 → 平跟池启发式，`limp_heuristic:raise`，
隔离加注额 (3 + 1 家平跟) + 1BB(ante 程度 L=1) = 5BB = 100 [事实，见翻前篇 §3.3]。之后影子里 hero 的加注是「BTN 开池」，SB 弃、BB 跟；
7 号（影子已弃）再跟注 → **`limp_as_fold:limper_continued`**，`synced_node` = BB 跟注之后 [推断：按动作序和代码推出，日志不记原因]。

**翻牌**（`search:unanchored`）：

1. 预热在翻牌起点按脱锚解好；决策 1356ms [实测]。
2. 范围（range 日志 `path: unanchored, range_src: prefix_reach, hero_coef: true`）：
   - hero：前缀里 hero 的「BTN 开池」那一步被拿掉，换成平跟池启发式的 0/1 系数（BTN 面对一家平跟时隔离加注的牌）。人加到 140 而不是 100，
     动作类型一样，所以系数照用、没有 mismatch；
   - 7 号、2 号：`coef: true` —— 他们的翻前步（平跟、平跟后跟注隔离加注；BB 跟注）整段被倾斜系数替换，用的是各自画像在对应局面格的频率；
   - 宽度修正：两人都被倾斜接管，跳过。`resp.exploit` 列了 1、3、4、5、6 号，这些座已弃牌，修正对他们不起作用（§8.6）。
3. 子树：在手 3 人，第二大剩余栈 4840 ÷ 底池 590 = 8.2 → {0.5, 1}。check 0.955，访问 1338 次，抽到过牌。

**转牌**（`search:unanchored`，335ms，命中预热）：range 日志 `cross_street_posterior`，即翻牌那份解的根范围（已含倾斜）× 三家翻牌都过牌的概率；
不再做倾斜和宽度修正（`exploit: null`）。AJ 两对，bet1pot 0.597 / bet0.5pot 0.365 / check 0.035，抽到 1 池 = 590。坚果度 0.971 [计算]，护栏不管。

**河牌**（`search:unanchored`，3275ms）：范围 = 转牌解的后验（BB 过牌后跟注了 1 池下注）。0.5pot 0.048 / 1pot 0.270 / allin 0.682，
SPR = 3015 ÷ 1770 = 1.7，全下门槛 5%，抽到全下 → 3015。坚果度 0.967 [计算]。河牌树小，hero 访问 27 万次。

**这手说明什么**：脱锚的转牌 / 河牌和锚定一样靠跨街后验；画像只在翻牌起点进入一次，之后随后验带下去。

---

## 7. 实盘统计 [实测]

**数据**：全部 6 个 `artifacts/wopo*/wopo_actions_log.jsonl`，翻后 advisor 回复共 310 条（含 41 条人已先行动、建议过期的）。
各轮配置不同：10-28 两轮没开画像（`wopo_ante_live_20261028` 是 100BB blueprint），10-29 只开宽度修正，10-31 起开倾斜；
10-28、10-29 的请求没有 `hand_id`，按（底牌, 庄位, 开局栈）分手。统计脚本只读日志。

### 7.1 按街 × 来源

| 街 | blueprint | blueprint:low_hero_visits | 锚定搜索 | 锚定 giveup | 脱锚搜索 | 脱锚 giveup | fallback | 合计 |
|---|---|---|---|---|---|---|---|---|
| 翻牌 | 24 | 3 | 48 | 6 | 64 | 20 | 0 | 165 |
| 转牌 | 0 | 0 | 41 | 1 | 46 | 7 | 0 | 95 |
| 河牌 | 0 | 0 | 20 | 0 | 30 | 0 | 0 | 50 |
| 合计 | 24 | 3 | 109 | 7 | 140 | 27 | 0 | 310 |

（`:weak_commit_guard`、`:hero_coef_mismatch` 等后缀并入对应类。）

- 锚定占 46%（143 条），脱锚 54%（167 条）。
- 翻牌锚定 81 次：两道闸都过、直接查 blueprint 24 次（30%）；去搜索 57 次（其中 3 次访问数不足又退回 blueprint）。
  去搜索的 57 次按开局栈估 [推断]：约 51 次码深闸不过（有效码深不在 120–280BB 或有人全下），6 次码深闸过、只能是尺寸闸不过
  （`search:bp_placeholder` 一次都没出现；`000621` 是其中之一）。
- giveup 34 次：访问数不足 16（锚定 4、脱锚 12）、节点超限 8 + 建树前计数超限 2（都在脱锚路径）、没访问到 5、真栈导航失败 2、
  hero 自身 reach 为 0 1。其中 9 次被兜底改成跟注（`nut_call` / `pot_odds_call` / `draw_call` 各 3）。
- 搜索求解量中位数（`solve_updates` / hero 访问数）：翻牌 锚定 64 万 / 7145、脱锚 74 万 / 4281；转牌 80 万 / 11372、124 万 / 10757；
  河牌 273 万 / 9.5 万、300 万 / 14.6 万。菜单：翻牌脱锚 64 次里 28 次是 {1} 池（在手人数多或筹码深），其余街绝大多数是 {0.5, 1}。
- `hero_coef_mismatch`：脱锚 167 条里 50 条（17 手）。查 play.log，都是人没照翻前建议打（例：`000611` 建议弃牌、人加注；`000657` 建议弃牌、人跟注）。
  全部牌局 825 次 hero 行动里有 130 次「不同于建议」。

### 7.2 锚定还是脱锚，在哪条街定下

| | 手数 |
|---|---|
| 有翻后决策的手 | 124 |
| 全程锚定 | 58（第一个翻后决策都在翻牌） |
| 全程脱锚 | 63（第一个翻后决策都在翻牌，即翻前就脱锚） |
| 先锚定后脱锚 | 3 |
| 先脱锚后锚定 | 0 |

先锚定后脱锚的 3 手：

| 手 | 转折 | 原因 [推断] |
|---|---|---|
| 10-29 Q♣T♣ | 翻牌锚定 → 转牌脱锚 | 对手转牌加到 260BB，超过 200BB 重放态的筹码（`replay_illegal`） |
| `_b100` `000549` | 翻牌 blueprint → 转牌脱锚 | 对手面对过牌弃牌（`structural_gap`），转牌搜索随后导航失败 giveup |
| 10-28 A♥8♣ | 转牌锚定 → 河牌脱锚 | 13.5BB 短码翻牌跟注全下，200BB 重放里他还在行动（`replay_seat_mismatch`） |

全程脱锚的 63 手 + 后脱锚的 3 手，翻前「首个加注前有人跟注」的有 55 手 [推断：按动作序判，含 SB 补齐和补盲者]；其余有入池超过 4 人（树的入池上限）等情况。

---

## 8. 已知问题与局限

### 8.1 街内有对手弃牌后，hero 第二次决策会换范围重解（bug，已修）[事实 + 实测]

`exploit_tilt` 判断「已弃牌、跳过」用的是整段动作历史（`tools/openpoker_advisor.rs:2461`：`req.actions` 里该座有任何 `fold`），
但子树根在本街起点，那时他还在手。于是翻牌（第一次脱锚的那条街）里某个对手弃牌后，hero 再决策时：该对手不再被倾斜，
转而被 `exploit_snapshot` 做宽度修正 → 范围变了 → 缓存 key 变了 → 重新求解一棵新树。

实盘 `wopo_ante200_9b_tilt_live_20261101`：
- `000611`：翻牌第一次决策预热命中；7 号随后弃牌，hero 面对下注的第二次决策重解 5022ms，访问数 142 < 200 → `search_giveup`，建议弃牌（人加注了）；
- `000663`：2 号翻牌弃牌后，第二次决策重解，「hero 信息集未被访问」→ giveup。
range 日志里这两次 `trigger: decide` 的条目，该座 `coef: false, width: true`，而同街预热条目是 `coef: true`。

后果：一条街出现两份解（破坏「每街一份解」）、多花一次时间预算、新解的访问数更容易不够。

**修法**（`c7a9e2c` 之后）：新函数 `actions_before_street_start` 用 `truncate_request_to_street` 的真栈重放找到本街起点，
`exploit_tilt` 只看这之前的弃牌；重放失败退回整段历史（旧口径）。翻前就弃牌的座照旧跳过。
测试 `exploit_tilt_ignores_fold_within_street`：翻牌起点与「SB 下注、大盲弃牌」之后两份请求算出同一份系数。

### 8.2 锚定路径不读画像 [事实]

实盘配置下（没开 `--exploit-anchored`），锚定局面占翻后决策近一半，对手的画像在这些局面完全不起作用（§5）。另外锚定后才脱锚的手（§7.2 那 3 手的类型）因为跨街后验优先，也不会再加画像。

### 8.3 锚定局面用画像：`--exploit-anchored`（2026-11-02 实现，默认关，未验证）[事实]

需 `--exploit-tilt on`。做法：

- `anchored_tilt`（`tools/openpoker_advisor.rs`）调同一个 `exploit_tilt`，`synced_node` 取锚定的影子节点：决策时 = hero 当前节点，预热时 = 街起点节点。
  锚定时对手的翻前动作全在影子路径上，本街起点前的弃牌也一样，两处算出同一份系数 → 预热命中决策的 key。
- `subgame_search_cached_tilt` / `subgame_search_prewarm` 接 `AnchoredTilt`（`src/training/subgame.rs`）：blueprint estimate 时，被接管座的翻前影子步
  从 `range_decisions` 删掉、估完再乘系数（均匀平滑之前），翻牌及以后的影子步照旧按 blueprint σ；hero 不动。
- 只改 blueprint estimate 这一档：跨街后验命中时不用（画像已随上一街的根范围带下来），不做宽度修正。求解、导航、翻牌访问数不足退 blueprint 都不变。
  实盘配置下它主要作用在翻牌搜索的根范围；翻牌查了 blueprint 的手，作用在转牌根范围。
- range 日志里锚定条目的 `coef` 会标出被倾斜的座。
- 顺带缓解 §8.5：对手翻前全下那一步用全下宽度表，不再按 200BB 全下的 σ 估。

测试 `prewarm_anchored_exploit_tilt_hits`：预热被决策命中；只有对手被倾斜；翻前全在影子路径上、没有全下的局面，锚定开倾斜与 `--search-force-unanchored`
下的脱锚倾斜算出逐座相同的根范围（两条路前缀相同时，锚定倾斜没有多乘 / 漏乘）；关旗与旧行为相同、key 不同。

**预期风险**：它给的根范围与 `--search-force-unanchored` 基本相同，§3.5 那组补算里 force 的转牌 / 河牌变差，原因在倾斜后的范围、不在求解路径，
所以这个开关很可能有同样的问题。

**没验证效果**：11-02 本打算用调研 §7.6 的亮牌补算比较开 / 关，只用 10-31 之后的三轮（41 手、76 个亮牌对手），落在锚定局面的样本只有几十个，
分不出好坏，没跑完。默认保持关；等 10-31 之后的 wopo 亮牌样本多了再比。

### 8.4 尺寸映射误差会叠加 [事实 + 推断]

- 翻前任何非全下开池在影子里都是 4.5BB（翻前篇 §3.2）。对手开大时，锚定路径按标准开池估他的范围，且翻牌影子底池偏小，翻牌下注在影子里显得更大（§6.1）。
- 尺寸闸用「真实池比例 ÷ 影子池比例」判断，分子分母的底池不是同一个，开池尺寸偏离越大，闸越容易误判。
- 翻牌 blueprint 路径的出牌基准态是 200BB 重放态：护栏判「≥ 一半筹码」、剪枝的 SPR 都按 200BB 算，hero 实际筹码少时会偏松 [推断]。
- 访问数不足退回 blueprint 时不看尺寸闸（§3.2）。

### 8.5 锚定范围对短码全下按 200BB 全下估 [推断]（开 `--exploit-anchored` 时有名字的对手不再如此，§8.3）

锚定路径估范围不跳过全下标签（脱锚会跳过）。只要重放没失同步，短码全下在影子里就是 200BB 全下，他的范围按「200BB 全下」的 σ 估，偏窄。
多人底池里有人短码全下、其余人继续时会走到这里。

### 8.6 几个日志字段的含义

- `hero_coef_mismatch`：脱锚路径里 hero 某个翻前步，按平跟池启发式重算时 hero 真实底牌会做出另一种动作——实盘基本都是人没照建议打。
  该步退回旧做法：映射弃牌步按无信息、仍在影子路径上的步按 blueprint σ。hero 范围因此比真实的宽或偏。
- `:weak_commit_guard`：只要挪了任意概率就加，大多数只挪了 1% 以下（§4.4），不代表护栏改变了动作。
- `resp.exploit`：列的是「有画像、没被倾斜接管」的座位，**包括已弃牌的座**，不能用来判断谁的范围被改了；要看 range 日志的 `coef` / `width`。

### 8.7 画像进程内统计、重启清零 [事实]（2026-11-02 已改：wopo driver 默认暖启动，见 §5）

11-02 之前的实盘：advisor 每次重启画像从空表开始；一手结束才更新。

### 8.8 hero 偏离建议时锚定路径的 hero 范围不对 [推断]

锚定路径的 hero 范围 = hero 每一步按 blueprint σ 的概率。人没照 blueprint 打（例如 blueprint 100% 弃的牌跟了）时，hero 真实底牌的权重接近 0，
hero 范围不再是「他实际会这样打的牌」。实盘有 1 次 `search_giveup:unsolved:...hero 自身 reach 在每次扫描时都为 0` 可能就是这种。

### 8.9 访问数不足仍常见 [实测]

16 次 giveup + 3 次退 blueprint 是因为 hero 信息集访问不足 200 次，集中在翻牌多人底池。门槛 200 是 10-19 本机近似实验定的，不是生产标定（:211 注释）。

---

## 附：代码位置索引（`c7a9e2c`）

| 内容 | 位置 |
|---|---|
| 主决策（翻后部分） | `tools/openpoker_advisor.rs` `decide_with` :2700；重放 :2803；脱锚分支 :2870；翻牌两道闸 :2931；`want_search` :2953；force_unanchored :2997；锚定搜索 :3105；访问数不足退 blueprint :3173；护栏 :3243；抽样 :3253；换算 :3290 |
| 锚定判定 | `lockstep_replay` :1419，`lockstep_step` :1266，`shadow_drifted` :1328，`is_limp_gap` :1338，`advance_shadow_by_applied` `src/training/blueprint_advisor.rs:311`，pseudo-harmonic `src/abstraction/action.rs:281` |
| 真实筹码重放 | `build_real_auth` :4424，`real_stacks_config` :5045 |
| 翻牌两道闸 | `flop_blueprint_stack_gate` :4517，`flop_blueprint_size_gate` :4554，常量 :217 / :223 |
| 脱锚搜索 | `decide_search_unanchored` :4755，`unanchored_range_inputs` :2595，`unanchored_prefix_decisions` :1555，`hero_limp_coef` :1671，`hero_limp_route` :1635，`limp_raisers` :1754 |
| 画像 | `exploit_tilt` :2430（弃牌判断 :2461），`limp_tilt` :2218，`exploit_snapshot` :4710，`pick_shape` :4677，`preflop_entry_kinds` :4594，`apply_exploit_width_prior` `subgame.rs:1722`，`warm_profile_from_hh` :6042 |
| 锚定子树求解 | `subgame_search_cached_inner` `src/training/subgame.rs:2655`（锚定范围 :2773，跨街 :2828） |
| 脱锚子树求解 | `subgame_search_unanchored_cached_inner` `subgame.rs:3622`（跨街命中不做宽度修正 :3834），`SUBTREE_PRECOUNT_MIN_ACTIVE` :1336 |
| 范围 | `estimate_range` `subgame.rs:1518`，`decisions_on_path` :1435，`cross_street_posterior_range` :1952，`mix_lambda_for_seat` :1583，`check_blueprint_coverage` :1256 |
| crop 补解 | `search_with_crop_backfill` :1078，`backfill_prev_street` :1131，`validate_crop_runtime` :5575 |
| 预热 / 缓存 | `prewarm_inner` :788，`SubgameSolveCache` `subgame.rs:2146` |
| 菜单 | `deep_menu_for` `src/training/nlhe_betting_tree.rs:445`，`deep_wide_then_single_pot` :386，阈值 :413 / :420 |
| 求解 / 读数 | `solve_subgame` `subgame.rs:3092`，`navigate_subtree_by_real_actions` :3392，`read_current_strategy` :3209，`min_hero_visits` 默认 `tools/openpoker_advisor.rs:211` |
| 护栏 / 剪枝 | `weak_commit_guard` :5318，`is_weak_postflop_hand` :5276，`commits_stack` :5289，`current_board_nuttiness` :3626，常量 :5216 / :5221；`prune_for_sampling` :5185，`postflop_spr` :5160，常量 :5142 起 |
| 兜底 | `fallback_with_floor` :3776，`pot_odds_from_auth` :3678，`draw_odds_floor_hit` :3717，`giveup_source` :444 |
| 出牌换算 | `outgoing_action` `blueprint_advisor.rs:244`，`action_to_response` :3521，`raise_to_op` :5108 |
| 开关定义 | `SearchRuntime` :708，`DEFAULT_SEARCH_FORCE_UNANCHORED` :191 |

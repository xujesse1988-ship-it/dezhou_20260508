# OpenPoker 实时搜索的手牌范围估计

代码口径：2026-10-31，仓库版本 `bbc54fa`。

> 下文的「默认」都指 `openpoker_advisor` 自身的参数默认值，不是实盘配置。wopo 实盘（`tools/wopo_play.py`，2026-10-29 那轮的 `common_args.sh`）用的是 `--search-trigger all-postflop`（翻后全部搜索）、`--search-flop-prefer-blueprint on`、`--search-limp-as-fold on`、`--exploit on`，且加载只含翻前和翻牌的裁剪版 blueprint；`--exploit-tilt` 还没在实盘开过。对照实盘日志时：平跟池按「平跟当弃牌」一节理解；转牌、河牌的根 range 主要来自上一街的跨街后验，拿不到时补解上一街。wopo driver 开了 `--search` 就默认写范围日志（见「配置和诊断入口」）。

`openpoker_advisor` 为每个仍在手的玩家构造手牌范围（range），再按这些范围抽取底牌，运行实时子博弈搜索。范围来自历史行动在 blueprint（离线训练策略）或上一街搜索策略中的概率，表示模型对各手牌组合的相对权重。

**advisor 默认从当前街起点建立子博弈。根 range 使用之前各街的行动，当前街已发生的行动用于定位子博弈中的决策节点。** 因此，日志里的根 range 和当前下注之后的条件范围需要区分。

## 范围的表示和计算

每个在手座位的 range 对应全部 `C(52, 2) = 1326` 种具体底牌组合。例如 A♥K♦ 和 A♥K♣ 是两个组合。范围中的权重先排除与公共牌冲突的组合，再归一化。均匀范围指可用具体组合等权，不是 169 类起手牌各占相同比例。

对玩家 `s` 的候选底牌 `h`，根据行动历史计算范围的基本形式是：

```text
range_s(h) ∝ prior_s(h) × ∏ σ(a_t | h, node_t)
                         t 为玩家 s 自己的历史决策
```

其中 `σ(a_t | h, node_t)` 表示：模型认为玩家在历史节点 `node_t` 拿着 `h` 时，做出已观察动作 `a_t` 的概率。越常走这条行动线的手牌，更新后的权重越高。

每个座位只累乘自己的动作概率。对手的行动通过历史节点影响查询上下文，例如“面对 UTG 加注时 BTN 跟注”的节点；不会把 UTG 的动作概率直接乘到 BTN 的 range 上。

查询历史策略时，每个节点使用该街的公共牌前缀重新计算手牌桶：翻前无公共牌，翻牌三张，转牌四张，河牌五张。计算转牌起始范围时，翻牌动作仍按翻牌三张公共牌查询；新转牌用于排除不可能的底牌组合。

实现入口为 `subgame.rs::estimate_range`。翻前使用无损的 169 类起手牌索引，翻后使用相应桶表，因此范围虽然按具体组合保存，翻后动作概率仍受桶抽象影响。

## 范围来源的选择顺序

锚定搜索指真实行动能在 blueprint 影子树上持续映射；脱锚搜索指映射发生结构性失败，搜索改用真栈重放得到的公共状态。映射过程见 [OpenPoker 真实牌局到抽象影子树的映射](openpoker_shadow_tree_mapping.md)。

在 advisor 的正常搜索路径中，范围来源按以下顺序选择：

| 条件 | 使用的范围 | 调试日志中的来源 |
|---|---|---|
| 跨街复用开启，且有可用的同手紧前一街搜索结果 | 上一街根 range 乘以上一街实际行动在搜索平均策略中的概率 | `cross_street_posterior` |
| 无可用跨街结果，且 blueprint 能正常重放 | 按之前各街的 blueprint 动作概率估计 | `blueprint_estimate` |
| 无可用跨街结果，且已经脱锚，但保留了当前街之前的同步前缀，或有 hero / 对手的翻前系数 | 按有效前缀中的 blueprint 动作概率估计，再乘翻前系数 | `prefix_reach` |
| 脱锚且没有可用前缀、也没有任何翻前系数，或关闭前缀范围估计 | 均匀抽取可用底牌 | `uniform` |

翻前系数有两种：hero 由平跟启发式决定的翻前步（见「平跟当弃牌」），以及开了 `--exploit-limp-tilt` / `--exploit-tilt` 时被倾斜接管的对手（见「对手翻前范围倾斜」）。它们只进脱锚档一的前缀范围；跨街后验命中时不再算，翻前信息已经随上一街的根范围带过来。

跨街复用要求缓存属于同一手牌、公共牌恰好从上一街增加一张，且上一街实际动作线可以在缓存子树上导航。锚定和脱锚搜索的结果均可复用，也允许两种路径之间跨街复用。

实际下注金额可能不在抽象菜单中，历史动作会先映射成相应树上的动作分支，再读取该分支的概率。因此条件化仍有动作抽象误差。

## 翻牌到河牌的连续示例

假设六人桌最终剩下 UTG 和 BTN，hero 是 BTN：

```text
翻前：UTG raise，BTN call，其余玩家 fold
翻牌：K♠ 8♦ 3♠      UTG bet，BTN call
转牌：2♥             UTG check，BTN bet，UTG call
河牌：A♠             UTG bet，轮到 BTN
```

以下示例假设翻前映射正常，翻牌和转牌均获得可复用的搜索结果，且各街搜索触发条件满足。advisor 默认触发面只覆盖翻牌尚未起注的决策点，判据是本街所有座位的最大投入为零，前面有人 check 也可能满足；需要搜索转牌和河牌时，使用 `--search-trigger all-postflop`。

**表中动作概率是演示数字，不是 checkpoint 的实测值。** 为展示连乘关系，三张表暂时省略归一化和默认的均匀平滑，列出的是候选组合的相对权重，不是完整范围中的最终概率。完整计算包含所有合法组合。

### 翻牌根据翻前行动建立范围

进入 `K♠ 8♦ 3♠` 时，UTG 的范围根据翻前加注估计：

```text
UTG 翻牌起始范围(h) ∝ σ_blueprint(UTG 翻前 raise | h)
```

| UTG 候选底牌 | 翻前 raise 概率 | 翻牌起始相对权重 |
|---|---:|---:|
| A♥K♦ | 90% | 0.90 |
| Q♠J♠ | 60% | 0.60 |
| 7♥6♥ | 10% | 0.10 |
| K♠Q♥ | 与公共牌 K♠ 冲突 | 0 |

A♥K♦ 比 7♥6♥ 获得更高权重，是因为模型认为它更常在 UTG 位置加注。公共牌先排除撞牌组合，不会因为某手牌在翻牌上成了强牌，就直接提高其先验权重。

BTN 使用自己在相应节点的跟注概率：

```text
BTN 翻牌起始范围(h)
  ∝ σ_blueprint(面对 UTG 翻前 raise，BTN call | h)
```

因此同一块公共牌上，UTG 是加注范围，BTN 是跟注范围。若翻前有多轮加注和跟注，则各座累乘自己在相应历史节点的所有动作概率。

翻牌上 UTG 下注后，BTN 的实时决策读取“UTG bet 后的 BTN 节点”。这次下注没有再乘进翻牌根 range，它已作为当前街子博弈路径的一部分被处理。

### 转牌根据翻牌搜索策略更新范围

进入 `2♥` 时，跨街更新使用翻牌搜索得到的平均策略：

```text
UTG 转牌起始范围(h)
  ∝ UTG 翻牌起始范围(h)
  × σ_flop_search(UTG bet | h)
```

| UTG 候选底牌 | 翻牌起始权重 | 翻牌 bet 概率 | 转牌更新后的相对权重 |
|---|---:|---:|---:|
| A♥K♦，顶对 | 0.90 | 80% | 0.72 |
| Q♠J♠，同花听牌 | 0.60 | 70% | 0.42 |
| 7♥6♥，空气 | 0.10 | 20% | 0.02 |

这条下注线保留了模型中经常下注的成牌和听牌，也保留了一部分诈唬。它不是按牌力直接截取“强牌前若干名”。

BTN 根据自己的翻牌跟注更新：

```text
BTN 转牌起始范围(h)
  ∝ BTN 翻牌起始范围(h)
  × σ_flop_search(面对 UTG bet，BTN call | h)
```

所有包含新公共牌 2♥ 的候选组合都会被排除。上一街根 range 已携带翻前信息，这里只乘翻牌动作，不再重复乘翻前动作。

### 河牌根据转牌整条行动线更新范围

转牌实际行动为 `UTG check → BTN bet → UTG call`。UTG 做了两个决策，河牌起始范围需要乘两个概率：

```text
UTG 河牌起始范围(h)
  ∝ UTG 转牌起始范围(h)
  × σ_turn_search(UTG check | h)
  × σ_turn_search(面对 BTN bet，UTG call | h)
```

| UTG 候选底牌 | 转牌起始权重 | check 概率 | 随后 call 概率 | 河牌更新后的相对权重 |
|---|---:|---:|---:|---:|
| A♥K♦ | 0.72 | 60% | 90% | 0.3888 |
| Q♠J♠ | 0.42 | 80% | 70% | 0.2352 |
| 7♥6♥ | 0.02 | 50% | 5% | 0.0005 |

BTN 只乘自己转牌下注的概率：

```text
BTN 河牌起始范围(h)
  ∝ BTN 转牌起始范围(h)
  × σ_turn_search(UTG check 后，BTN bet | h)
```

河牌 `A♠` 带来两类不同影响：

- **排除不可能的底牌**：任何包含 A♠ 的候选组合，例如 A♠Q♠，权重归零。
- **改变牌力和行动价值**：Q♠J♠ 成为同花，A♥K♦ 成为两对。搜索在新的牌面上重新计算策略，不会仅因成牌就额外乘一个“强牌奖励权重”。

UTG 河牌下注后，BTN 查询河牌搜索树中该下注之后的节点，河牌根 range 保持原来的街起点含义。

## 翻牌使用 blueprint 时的转牌估计

**如果翻牌没有可复用的搜索结果，而且历史可以正常映射到 blueprint，转牌起始范围就根据翻前和翻牌的 blueprint 节点策略计算。** 不要求上一街也执行过实时搜索。

例如：

```text
翻前：UTG raise，BTN call
翻牌：UTG bet，BTN raise，UTG call
转牌：开始实时搜索
```

此时：

```text
UTG 转牌起始范围(h)
  ∝ σ_blueprint(翻前 UTG raise | h)
  × σ_blueprint(翻牌 UTG bet | h)
  × σ_blueprint(翻牌面对 BTN raise，UTG call | h)

BTN 转牌起始范围(h)
  ∝ σ_blueprint(翻前面对 UTG raise，BTN call | h)
  × σ_blueprint(翻牌面对 UTG bet，BTN raise | h)
```

每个概率都来自对应历史节点，不能用一个通用的 bet 或 call 概率替代整条线。转牌动作尚未进入根范围；已发生的转牌行动用于当前街子树导航。

同理，河牌搜索没有可复用的转牌结果、但完整 blueprint 路径可用时，会重新按翻前、翻牌和转牌的 blueprint 概率计算范围。这条回退路径不会自动保留更早某次搜索的后验。

范围来源由缓存和路径可用性决定，不单由上一街动作输出标签决定。例如预热也可能产生可复用的搜索结果。`--search-flop-prefer-blueprint on` 会让锚定翻牌优先查 blueprint，并跳过相应的翻牌搜索预热；前提是本手无人全下，hero 与每个未弃牌对手的有效码深在 60–140BB 内，且本街每次下注 / 加注（含 hero 自己的）的「真实池比例 ÷ 映射到 blueprint 树上的池比例」都在 0.75–1.33 内（例如 1/3 池下注被映射成满池就不过），否则翻牌照常搜索。

## 平滑和无信号处理

advisor 默认对对手范围混入 25% 均匀范围：

```text
r_final(h) = 0.75 × r_estimated(h) + 0.25 × u(h)

u(h) = 不撞当前公共牌的具体组合上的均匀分布
```

这一步发生在 blueprint 范围估计（及翻前系数相乘）之后，或跨街后验归一化之后；画像宽度修正在它之后。跨街复用时，上一街根范围已包含上一街的平滑，本街更新后会再次对对手应用配置的平滑。hero 不参与该混合。

参数为 `--search-range-uniform-mix`，advisor 默认 `0.25`，设置为 `0` 可关闭。底层 `SubgameSearchConfig::default()` 的默认值是 `0.0`，advisor 在解析参数时覆盖为自己的生产默认值。

撞公共牌的组合在估计范围和均匀分布中都为零，平滑不会把它恢复为合法组合。无信号情况按下列方式处理：

- blueprint 查询返回空、维度不符或当前动作概率非法时，该动作使用合法动作数的倒数作为均匀概率。
- 跨街查询也有上述兜底；若某个已观察动作在剩余正权候选牌上的最大概率小于 `1e-12`，跳过该动作这一列，保留其余信息。
- 某座的估计权重全零时，平滑函数保持全零；采样时退为可用组合上的均匀抽牌。
- 翻前系数与 blueprint 前缀相乘后全零（两者互相矛盾）时，保留相乘前的范围，不乘系数。
- 采样时某座的正权组合全部与已用牌冲突，也退为当前可用组合上的均匀抽牌。

## 脱锚和 limp 的现有行为

脱锚后保留的 `synced_node` 是失败动作之前最后一个同步节点。前缀范围只使用该节点之前、且属于当前街之前的决策，失败动作及其后的行动不进入这份 blueprint 前缀估计。

这一路还跳过前缀中的 `AllIn` tag：blueprint 的 100BB 全下语义未必适用于真实短码全下。跨街搜索后验没有这个跳过规则，它使用上一街子树自身的策略。

### UTG 首动作 limp

在满桌、blueprint 不支持 UTG open-limp 的情况下：

```text
UTG limp → HJ raise → BTN call → UTG call → flop
```

第一个 limp 就导致映射失败，保留节点为翻前根节点，有效决策前缀为空。翻牌又没有更早的翻后搜索结果可复用，因此进入搜索时 `ranges_opt = None`，所有仍在手的座位，包括 hero，使用均匀抽牌。

HJ 的 raise、BTN 的 call 和 UTG 后续的 call 都不进入翻牌根范围估计。真实动作仍用于真栈重放，决定底池、筹码、在手人数和行动顺序；范围信息与公共状态信息的处理不同。

如果翻牌搜索成功并缓存，转牌可以用该均匀起点下的翻牌策略条件化，河牌再沿转牌策略更新。范围会随翻后行动改变，但丢失的翻前信息不会被补回。

这里的“空前缀”针对满桌首动作断开。短桌可能先重放幻影座 fold，或其他位置 limp 前已有真实动作，前缀未必为空；各座仍只使用其中属于自己的决策，没有自身有效动作的座位不会从这段前缀获得有信息的范围。

以上是默认（`--search-limp-as-fold off`、不开 `--exploit-tilt`）的行为。开了 `--exploit-tilt` 时，断点之后的翻前决策用「替身节点」查参考策略，这个例子里 HJ、BTN、UTG 的翻前动作都能进入范围（见「对手翻前范围倾斜」）。

### 平跟当弃牌（`--search-limp-as-fold on`，默认关）

开启后，非 SB 的开池 / 跟入平跟在 100BB 影子里记为弃牌，真实局照常跟注；这名平跟者成为「影子已弃、真实在手」的座位。之后：

- 平跟者面对加注弃牌：只推真实局，两边重新一致，整手回到锚定路径（翻后走 blueprint / 锚定搜索）。
- 平跟者再跟注或加注：失同步，原因 `limp_as_fold:limper_continued`，前缀包含此前全部已同步动作。
- 平跟者还在手就进了翻牌（如 SB 补齐 + BB 过牌）：失同步，原因 `limp_as_fold:limper_in_pot`，前缀包含全部翻前动作（含 SB 补齐和 BB 过牌）。
- 其余漂移（如平跟后其余人全弃、影子已结束但真实局轮到 BB）：原因加 `limp_as_fold:` 前缀。

上面的例子开启后：UTG 的平跟按 UTG 开池节点的弃牌概率估范围（≈ 该位置开池范围之外的牌，再按 `--search-range-uniform-mix` 混入均匀），HJ 的加注按 UTG 已弃后的 HJ 开池节点估，BTN 的跟注按面对 HJ 开池的节点估；UTG 后续的 call 是断点，不进入前缀。平跟后在翻前又加注的对手，那一步映射弃牌也从前缀里拿掉（「开池范围之外的牌」已被他自己的加注推翻），按无信息处理。

翻前不受这个开关影响：只要发生过映射，翻前决策一律交给 `limp_heuristic`（按位置 × 平跟人数配的范围），不使用影子对上的 blueprint 开池节点。原因见 `docs/temp/openpoker_limp_handling_research_2026_10_16.md` §4。

**hero 的平跟步**（`hero_limp_coef`）：hero 由 `limp_heuristic` 决定的翻前步不按 blueprint 算——hero 被映射成弃牌的那一步按 blueprint 弃牌概率算，会把 hero 真实底牌所在牌型压到接近 0。这些步从前缀里拿掉，改为对 1326 个组合各重跑一次同一个 `limp_heuristic`，动作类型和实际一致记 1、否则 0，各步相乘后作为 hero 的范围系数（hero 不混均匀）。重跑时 hero 真实底牌算出 0（分流或合法集重建与当时不一致）→ 该步退回旧做法，响应来源追加 `:hero_coef_mismatch`。其余偏离 blueprint 的 hero 翻前步（PM-17 4bet、翻前全下启发式、底池赔率地板）仍按 blueprint 算，见调研 §2.5。

### 裁剪 blueprint 的区别

若范围估计需要读取已经被裁掉的 blueprint 街，代码会触发覆盖守卫，尝试补解紧前一街，再通过跨街后验重试；必要时向更早街递归补解。补解仍不能提供后验时，advisor 显式返回 `search_giveup:crop_no_posterior:*`。

这种“所需策略数据被裁掉”的失败不会静默转换为均匀范围，与 limp 空前缀回退不同。

## 已实现的可选画像修正

### 翻前宽度修正（`--exploit on|vpip`）

`--exploit` 默认关闭。开启后，仅在脱锚搜索决策路径上（配 `--search-force-unanchored on` 时，命中触发面的锚定搜索也算在内；只因 blueprint 均匀占位而搜索、又没命中触发面的决策仍走锚定，不用画像），对见过的对手使用翻前画像调整范围。没有样本量门槛，改为收缩（2026-10-29）：设该玩家已有 `n` 手、`k = --exploit-shrink-k`（默认 50），VPIP / PFR 取 `(n·实测 + k·人群) / (n + k)`（人群 = 除该玩家和 hero 之外所有名字的合计），混合强度取 `--exploit-strength × n/(n+k)`。默认下 10 手约 1/6 强度、50 手半强度、150 手 3/4 强度。

- `--exploit vpip` 使用 VPIP 宽度和 Chen 起手牌排序构造目标分布。
- `--exploit on` 结合本手翻前被动或主动入池方式选择 `CallBand` 或 `RaiseBand`，入池方式未知时退为 VPIP 形状。
- 画像只有全局 VPIP / PFR，描述不了的线这一座不修正（两种模式都是，2026-10-29 修）：盲位 / 补盲者的过牌、补齐和防守跟注；面对加注再加注（3bet 及以上，含平跟后再加注）；面对 ≥2 次加注跟注（含开池者跟 3bet）。可修正的只剩非盲位平跟 / 平跟一次加注（`CallBand`）和开池加注（`RaiseBand`）。入池方式按当时合法集判动作，短码全下只够跟注算跟注；画像统计在 driver 带投入额时同样把它算跟注、不算 PFR。原因见 `docs/temp/preflop_range_offanchor_research_2026_10_29.md` §2.1 / §2.2。
- 目标分布与原范围按配置强度 × 收缩权重混合，默认强度为 0.5，上限为 0.8。hero 不调整。
- 只在用翻前前缀估出的范围上混一次。转牌 / 河牌命中跨街后验时不再混：后验是从上一街已经混过的范围、按对手的真实动作推出来的，再混会每街把一半质量拉回只看翻前宽度的分布，冲掉翻后动作信息（2026-10-21 修）。上一街没剥削（锚定解、当时还没有画像）时本街也不剥削。

宽度修正遇到 `ranges_opt = None` 会跳过，所以只开 `--exploit` 时，满桌 UTG 首动作 limp 的空前缀场景仍是均匀起点。被下面的翻前倾斜接管的座位不再做宽度修正（同一份画像不用两次）。脱锚预热和决策用同一份画像快照（同一手内画像不变，observe 在手末才发），预热的解能被决策命中；预热请求须带 `names`。

### 对手翻前范围倾斜（`--exploit-limp-tilt` / `--exploit-tilt`，默认关）

宽度修正是「混合」：目标分布只看全局 VPIP / PFR，和这手牌的具体行动线无关。倾斜改成「条件化」：对手每个翻前决策都按他在该局面的实测频率改一次概率，再沿行动线连乘。设计和评审见 `docs/temp/preflop_range_offanchor_research_2026_10_29.md` §4、§7。

**局面格统计**（`opponent_profile.rs` `preflop_spots`，画像和 advisor 用同一份分类，与 `opponent_analysis/spots.py` 逐条对拍过）：每个翻前决策按动作前的局面归格，统计加注 / 跟（含平跟、补齐、盲位过牌）/ 弃三类次数。

| 局面类 | 条件 | 格 |
|---|---|---|
| `Unopened` | 没人加注、非大盲 / 补盲者 | 位置组（EP / MP / CO / BTN / SB）× 前面有没有人平跟 |
| `BlindOption` | 没人加注、轮到大盲或补盲者 | 是不是大盲 × 前面跟注 ≥ 2 人 |
| `VsOpen` | 本手第一个动作、面对恰好一次加注 | 位置大类 × 加注后有没有人跟 |
| `LimpVsRaise` | 平跟（或补盲过牌）后第一次面对恰好一次加注 | 位置大类 |
| `Vs3bet` | 开池者第一次面对 3bet | 位置大类 |

面对 4bet、冷跟 3bet 等不归格。observe 需带 `button_seat / dealt_seats / posts`（wopo driver 已发），缺了这手不计格。

**频率收缩**（`Profiler::spot_freqs`）：人群本格（不含本人和 hero）先按 10 条伪样本往 blueprint 边际收；再乘「本人整体松紧」（他在同一局面类其余格的实测相对人群期望的比值，按 `cell_shrink_k` 往 1 收）；最后本人本格按 `cell_shrink_k`（30）往上一层收。哪一层没有样本就等于上一层；本人和人群都没有样本时不倾斜。

**倾斜**：参考策略 `σ_ref(a|h)` 是某个 blueprint 节点上三类动作的概率（类内各加注尺寸合并），按

```text
σ_opp(a | h) ∝ σ_ref(a | h) · exp(θ_a)，θ 使 Σ_h prior(h)·σ_opp(a|h) / Σ_h prior(h) = 收缩后的频率 f(a)
```

解出 θ（迭代缩放，最小 KL 投影），这一步的系数 = `σ_opp(实际动作类 | h)`。`prior` 是该座在这一步之前的系数乘积（首个动作为均匀）。直观上：blueprint 决定「哪些牌更可能这样打」，实测频率决定「这样打的牌有多少」。节点上没有的动作类（如大盲 option 没有弃牌）不参与，频率在其余类上重新归一。

blueprint 在边界以下的概率只剩 1e-4 量级的残余值，排序不可靠，所以参考策略先套一个按 Chen 序单调的底值（`1e-3`）：加注类强牌高、弃牌类弱牌高；跟注类只在面对一次加注（`VsOpen`、`LimpVsRaise`）时套（强牌高），面对 3bet 时不套——那里跟注的低概率区装的是要 4bet 的强牌。

- **`--exploit-limp-tilt on`**（需 `--search-limp-as-fold on`）：只处理对手首入平跟那一步（被平跟当弃牌映射掉的步）。参考策略在开池节点上合成：加注 = blueprint 全部加注概率，其余按 Chen 序种子拆成平跟和弃牌（blueprint 没有平跟）。系数代替原来的「blueprint 弃牌概率」。
- **`--exploit-tilt on`**（只需 `--exploit on|vpip`）：对每个有名字、本手没弃牌的对手，按时间序处理他的**全部翻前决策**，结果代替他翻前的 blueprint 前缀 reach 和宽度修正；开了它就不再单独走 `--exploit-limp-tilt`。每一步的参考节点：
  - 影子同步的步：用影子里实际走到的节点；
  - 断点之后的步：用「替身节点」（`spot_substitute`）——从树根按脚本走一条规范路径（相关座位按真实动作，加注映射到最近的抽象档，其余座弃牌，平跟者当弃牌），走到同位置、同局面类的节点。例如平跟后面对隔离加注，用「大盲面对同一次加注」的节点。树里复现不了的线不给节点；
  - 首入用上面的平跟合成参考（SB 补齐节点本身有跟注时直接用该节点）；
  - 主动全下：不倾斜，用 `allin_range_width` 全下宽度表（与 hero 面对全下用同一张表）当系数，表外给 `1e-3`；
  - 没归格 / 找不到节点：在同步前缀里的照旧乘原始 σ（AllIn tag 照旧跳过），不在的按无信息处理。

  至少一步用上了参考节点或宽度表的座才接管：他在前缀里的全部翻前决策从前缀里删掉，翻后决策不动。

两个开关都只作用于脱锚档一的前缀范围，系数乘在 `estimate_range` 之后、均匀平滑之前，所以平滑仍是最后一道兜底。有对手被接管时即使前缀为空也会构造范围（没被接管的座仍是均匀），所以开了 `--exploit-tilt` 后满桌 UTG 首动作 limp 也不再是纯均匀起点。系数只读请求、画像快照和 blueprint，预热和决策算出同一份，进 solve 缓存 key。

离线回放（调研 §7.1–§7.5）：wopo 三轮里平跟后跟隔离加注的范围从「开池范围之外的杂色小牌」变成中段牌；按实盘参数回放 194 条翻后决策，120 条脱锚决策中 15 条动作改变，倾斜本身耗时可忽略。已知局限：排序仍来自 blueprint，平跟再加注这种真人习惯（平跟陷阱 AA）表达不了。

## 范围如何进入搜索

每次 MCCFR 迭代从街起点重新抽取隐藏牌。有范围时，按座位顺序做加权抽牌，排除公共牌和已经为其他座位抽出的牌；没有范围时均匀重发隐藏牌。弃牌座位没有底牌范围，仍在手的全下座位也需要底牌参与收益计算。

**hero 在求解阶段同样按自己的 range 抽牌。** 求解完成后，才使用 hero 的真实底牌和当前公共牌确定信息集，读取当前节点的平均策略。根范围构造时只按公共牌排除组合，hero 的真实两张牌没有被固定进所有模拟；每次模拟的重复牌约束由该次抽出的整组底牌保证。

范围按各座位独立保存，玩家之间的牌张关联主要通过顺序抽牌时的冲突排除近似处理，没有保存完整的多人联合后验。默认动作模型为同一套 blueprint 或搜索策略，因此它描述的是模型假设下的范围，真人策略偏离、动作抽象和有限搜索迭代都可能带来估计误差。

## 配置和诊断入口

| 配置 | advisor 默认值 | 与范围估计的关系 |
|---|---|---|
| `--search` | 关闭 | 开启实时搜索 |
| `--search-trigger` | `flop-first-unraised` | `all-postflop` 才覆盖转牌和河牌搜索触发面 |
| `resolve_root` | `RoundStart` | advisor 使用街起点范围；底层另保留 `CurrentDecision` 模式 |
| `--search-range-uniform-mix` | `0.25` | 对手范围的均匀混合权重 |
| `--search-unanchored-prefix-reach` | `on` | 脱锚时使用同步成功的历史前缀 |
| `--search-unanchored-cross-street` | `on` | 同时控制锚定和脱锚两路的跨街复用 |
| `--search-flop-prefer-blueprint` | `off` | 开启时，锚定翻牌优先使用 blueprint（须无人全下、有效码深 60–140BB、本街下注尺寸映射比在 0.75–1.33 内） |
| `--search-force-unanchored` | `off` | 开启时，影子同步、命中触发面的锚定搜索也改走脱锚路径（前缀范围跳过 AllIn、按真实动作导航、可叠加 `--exploit`；翻牌访问量不足不再退回 blueprint）；不改哪些决策搜索 |
| `--search-limp-as-fold` | `off` | 开启时平跟在影子里记为弃牌，平跟池翻后搜索获得范围前缀 |
| `--exploit` | `off` | 可选对手画像修正（翻前宽度修正） |
| `--exploit-shrink-k` | `50` | 宽度修正的收缩伪样本数 |
| `--exploit-limp-tilt` | `off` | 只倾斜对手首入平跟那一步；需 `--exploit` 与 `--search-limp-as-fold on` |
| `--exploit-tilt` | `off` | 对手全部翻前决策走倾斜流水线；需 `--exploit` |
| `--range-log <path>` | 不写 | 每次新 solve 写一行各座根范围；需 `--search`。wopo driver 开了 `--search` 时默认写到行动文件同目录的 `wopo_ranges.jsonl`，`--range-log off` 关 |

底层锚定搜索另保留 `ResolveRoot::CurrentDecision` 模式，会把当前街已发生的动作也纳入 blueprint 根范围估计，并从当前决策点建树。脱锚搜索只支持 `RoundStart`；本文逐街示例使用的也是 advisor 的 `RoundStart` 路径。

添加 `--debug-log` 后，stderr 会输出本次根范围的来源，以及有显式范围向量时各座的非零组合数、权重和、权重最高的六个组合和 13×13 起手牌格（每格 = 该牌型平均权重相对全表最大值，`0`–`9`，`.` = 不到 5%）。`ranges_opt = None` 的纯均匀路径只标记 `range_src=uniform`，没有逐座向量摘要。开了倾斜时另有 `limp_tilt:` / `exploit_tilt:` 行（每步的局面格、参考节点来源、目标与收缩频率、θ），以及 `spot_substitute:` 行（每个对手翻前决策的替身节点和树上路径）。

**范围日志**（`--range-log`）用于事后分析，不需要开 `--debug-log`。每个决策 / 预热请求之后看 solve 缓存，出现新 solve（同街预热与决策命中同一个 solve 只写一次，solve 失败不写）就追加一行 JSON：

- 请求标识：`hand_id`（driver 开了范围日志就在请求里带上，可与 hh 对上）、hero 底牌、公共牌、街、动作数、`trigger`（`prewarm` / `decide`）、响应来源；
- 范围来源：`path`（`anchored` / `unanchored`）、`range_src`（同上表）、`hero_coef`；
- `seats`：每个在手座的 OpenPoker 座位、名字、是否 hero、`coef`（被翻前倾斜接管）、`width`（做了宽度修正）、`range`（这次 solve 实际用的 1326 个权重，归一后乘 1e6 取整）。`range_src = uniform` 时为空。

文件开头（每次进程启动）有一行表头，`combos` 列出 1326 个组合的牌面，`range` 下标按它。只读缓存，不影响决策；写失败只告警。实测平均约 20KB / 行，1000 手约 40MB。

这些日志表示子博弈根范围，不能直接当作当前街下注之后另行导出的后验。范围向量进入求解缓存 key；范围变化会使求解缓存失效。同一街输入一致时可以复用搜索结果，再沿实际动作线读取相应节点。

## 代码索引

| 文件 | 主要函数及用途 |
|---|---|
| [tools/openpoker_advisor.rs](../tools/openpoker_advisor.rs) | `lockstep_replay` 保存同步前缀；`decide_search_unanchored` 接入脱锚范围；`unanchored_range_inputs` 汇总前缀决策、hero 系数和对手倾斜（预热与决策共用）；`search_with_crop_backfill` 补解裁剪缺失街；`parse_args` 设置 advisor 默认参数 |
| [tools/openpoker_advisor.rs](../tools/openpoker_advisor.rs) | `hero_limp_coef` hero 平跟步系数；`limp_tilt` / `exploit_tilt` 对手翻前倾斜；`preflop_real_steps` + `req_preflop_spots` 本手翻前局面格；`spot_substitute` 替身节点；`limp_reference` / `class_reference` 参考策略；`solve_tilt` 解 θ；`exploit_snapshot` 宽度修正画像快照；`RangeLog` 范围日志 |
| [src/training/subgame.rs](../src/training/subgame.rs) | `all_hole_combos` 枚举组合；`estimate_range` 查询 blueprint；`cross_street_posterior_range` 更新跨街后验；`mix_range_with_uniform` 平滑；`apply_exploit_width_prior` 应用画像 |
| [src/training/subgame.rs](../src/training/subgame.rs) | `PrefixReach` 前缀决策 + hero / 对手系数；`SubgameNlheGame::root` 和 `sample_holes_from_ranges` 抽牌；`query_at` 用真实底牌查询策略；`debug_dump_ranges` 输出根范围摘要；`SubgameSolveCache::current_solve` + `SolveRangeMeta` 供范围日志读取 |
| [src/training/nlhe.rs](../src/training/nlhe.rs) | `info_set_for_cards` 计算候选底牌在历史节点上的信息集 |
| [src/training/opponent_profile.rs](../src/training/opponent_profile.rs) | `ExploitConfig` 画像参数；`Profiler::profile_for` 宽度修正用的 VPIP / PFR；`preflop_spots` 翻前局面分类；`Profiler::spot_freqs` 分层收缩的局面频率 |

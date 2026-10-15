# OpenPoker 实时搜索的手牌范围估计

代码口径：2026-10-15，仓库版本 `f566185`。

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
| 无可用跨街结果，且已经脱锚，但保留了当前街之前的同步前缀 | 只按该有效前缀中的 blueprint 动作概率估计 | `prefix_reach` |
| 脱锚且没有可用前缀，或关闭前缀范围估计 | 均匀抽取可用底牌 | `uniform` |

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

范围来源由缓存和路径可用性决定，不单由上一街动作输出标签决定。例如预热也可能产生可复用的搜索结果。`--search-flop-prefer-blueprint on` 会让锚定翻牌优先查 blueprint，并跳过相应的翻牌搜索预热。

## 平滑和无信号处理

advisor 默认对对手范围混入 25% 均匀范围：

```text
r_final(h) = 0.75 × r_estimated(h) + 0.25 × u(h)

u(h) = 不撞当前公共牌的具体组合上的均匀分布
```

这一步发生在 blueprint 范围估计之后，或跨街后验归一化之后。跨街复用时，上一街根范围已包含上一街的平滑，本街更新后会再次对对手应用配置的平滑。hero 不参与该混合。

参数为 `--search-range-uniform-mix`，advisor 默认 `0.25`，设置为 `0` 可关闭。底层 `SubgameSearchConfig::default()` 的默认值是 `0.0`，advisor 在解析参数时覆盖为自己的生产默认值。

撞公共牌的组合在估计范围和均匀分布中都为零，平滑不会把它恢复为合法组合。无信号情况按下列方式处理：

- blueprint 查询返回空、维度不符或当前动作概率非法时，该动作使用合法动作数的倒数作为均匀概率。
- 跨街查询也有上述兜底；若某个已观察动作在剩余正权候选牌上的最大概率小于 `1e-12`，跳过该动作这一列，保留其余信息。
- 某座的估计权重全零时，平滑函数保持全零；采样时退为可用组合上的均匀抽牌。
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

### 裁剪 blueprint 的区别

若范围估计需要读取已经被裁掉的 blueprint 街，代码会触发覆盖守卫，尝试补解紧前一街，再通过跨街后验重试；必要时向更早街递归补解。补解仍不能提供后验时，advisor 显式返回 `search_giveup:crop_no_posterior:*`。

这种“所需策略数据被裁掉”的失败不会静默转换为均匀范围，与 limp 空前缀回退不同。

## 已实现的可选画像修正

`--exploit` 默认关闭。开启后，仅在脱锚搜索决策路径上，对满足样本量和稳定性门槛的对手使用翻前画像调整范围；默认门槛包括至少 150 手、VPIP 标准误不超过 0.05、近窗漂移不超过 0.08。

- `--exploit vpip` 使用 VPIP 宽度和 Chen 起手牌排序构造目标分布。
- `--exploit on` 在 PFR 也满足门槛时，结合本手翻前被动或主动入池方式选择 `CallBand` 或 `RaiseBand`；否则退为 VPIP 形状。
- 目标分布与原范围按配置强度混合，默认强度为 0.5，上限为 0.8。hero 不调整。

当前实现遇到 `ranges_opt = None` 会跳过画像修正，所以满桌 UTG 首动作 limp 的空前缀场景不会因开启 `--exploit` 而改变均匀起点。脱锚预热路径目前也不应用画像修正。

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
| `--search-flop-prefer-blueprint` | `off` | 开启时，锚定翻牌优先使用 blueprint |
| `--exploit` | `off` | 可选对手画像修正 |

底层锚定搜索另保留 `ResolveRoot::CurrentDecision` 模式，会把当前街已发生的动作也纳入 blueprint 根范围估计，并从当前决策点建树。脱锚搜索只支持 `RoundStart`；本文逐街示例使用的也是 advisor 的 `RoundStart` 路径。

添加 `--debug-log` 后，stderr 会输出本次根范围的来源，以及有显式范围向量时各座的非零组合数、权重和、权重最高的六个组合。`ranges_opt = None` 的纯均匀路径只标记 `range_src=uniform`，没有逐座向量摘要。

这些日志表示子博弈根范围，不能直接当作当前街下注之后另行导出的后验。范围向量进入求解缓存 key；范围变化会使求解缓存失效。同一街输入一致时可以复用搜索结果，再沿实际动作线读取相应节点。

## 代码索引

| 文件 | 主要函数及用途 |
|---|---|
| [tools/openpoker_advisor.rs](../tools/openpoker_advisor.rs) | `lockstep_replay` 保存同步前缀；`decide_search_unanchored` 接入脱锚范围；`search_with_crop_backfill` 补解裁剪缺失街；`parse_args` 设置 advisor 默认参数 |
| [src/training/subgame.rs](../src/training/subgame.rs) | `all_hole_combos` 枚举组合；`estimate_range` 查询 blueprint；`cross_street_posterior_range` 更新跨街后验；`mix_range_with_uniform` 平滑；`apply_exploit_width_prior` 应用画像 |
| [src/training/subgame.rs](../src/training/subgame.rs) | `SubgameNlheGame::root` 和 `sample_holes_from_ranges` 抽牌；`query_at` 用真实底牌查询策略；`debug_dump_ranges` 输出根范围摘要 |
| [src/training/nlhe.rs](../src/training/nlhe.rs) | `info_set_for_cards` 计算候选底牌在历史节点上的信息集 |
| [src/training/opponent_profile.rs](../src/training/opponent_profile.rs) | `ExploitConfig` 和 `Profiler::profile_for` 定义画像参数及使用门槛 |
